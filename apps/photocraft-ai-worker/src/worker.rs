//! Capishot's AI Web Worker: a second wasm module, loaded only when Image › Upscale with AI… runs,
//! so the main bundle doesn't carry the inference stack (burn, cubecl, the WebGPU kernels).
//!
//! It runs photocraft-ai models on the user's GPU through WebGPU (`navigator.gpu` in the worker),
//! falling back to the CPU when there is none, and keeps the weight files in Cache Storage
//! ([`photocraft_ai::cache::WEB_CACHE`], keyed by URL; verified by sha256 before use).
//!
//! ## Protocol (plain objects through `postMessage`)
//!
//! Main thread → worker:
//! * `{type: "run", id, model, base, download, grayscale, cpu, width, height, channels, data}`:
//!   upscale `data` (a `Float32Array`, planar, transferred) with catalog model `model`, fetching
//!   its weights from `base` when not cached and `download` is true; `cpu` skips WebGPU;
//!   optional `tile` and `overlap` override the model's tiling; `unload` frees the model after.
//! * `{type: "inpaint", id, model, base, download, cpu, width, height, rgb, mask}`: an
//!   inpainting model (`model`: a catalog id, Manga LaMa by default, or MI-GAN) on one tile
//!   (`rgb` interleaved, `mask` 0/1, both `Float32Array`s, transferred); the reply's `data`
//!   is the interleaved RGB output (`channels` 3). `unload` frees the model after the run.
//!
//! * `{type: "genfill", …}`: Generative Fill (see [`genfill`]); the tier's files are streamed
//!   from Cache Storage into GPU buffers (never whole in the module's memory) and stay loaded.
//!
//! * `{type: "download", model, base}`: downloads catalog model `model`'s weight file from
//!   `base` into Cache Storage (streamed, hashed as it arrives, verified, then stored with an
//!   `x-capishot-sha256` header), for the AI Model Manager. The page runs downloads in a worker
//!   of their own (one at a time), so they go on while models run and cancelling one is
//!   `Worker.terminate()`.
//!
//! Worker → main thread:
//! * `{type: "ready"}` once loaded;
//! * `{type: "progress", id, fraction, status, device}` (at most ten a second);
//! * `{type: "dl-progress", model, done, total}` (at most ten a second), then
//!   `{type: "dl-done", model}` or `{type: "dl-error", model, message}`;
//! * `{type: "done", id, width, height, channels, data, device}` (`data` transferred);
//! * `{type: "error", id, message}`;
//! * `{type: "panic", message}` just before the module aborts (the worker is then dead).
//!
//! Cancelling is `Worker.terminate()`: immediate even inside a GPU tile; the next run starts a
//! new worker (weights come back from Cache Storage).

use std::cell::RefCell;
use std::rc::Rc;

use js_sys::{Array, Float32Array, Object, Reflect, Uint8Array};
use photocraft_ai::cache::{WEB_CACHE, verify};
use photocraft_ai::models::Throttle;
use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_ai::runtime::Runtime;
use photocraft_ai::upscale::{Loaded, default_tiles, upscale};
use photocraft_ai::{AiError, Planar, Progress, Result};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, Cache, DedicatedWorkerGlobalScope, MessageEvent, ReadableStreamDefaultReader, Response};

#[derive(Default)]
struct State {
    rt: Option<Runtime>,
    /// Threads variant: the rayon pool size asked for by the page (0: one per core).
    #[cfg(target_feature = "atomics")]
    threads: usize,
    /// Threads variant: the pool is up (started with the CPU runtime, never before: rayon would
    /// otherwise build its one-thread fallback pool for good).
    #[cfg(target_feature = "atomics")]
    pool: usize,
    device: String,
    loaded: Option<Rc<Loaded>>,
    /// The loaded inpainting model and its catalog id.
    inpaint: Option<(&'static str, Rc<photocraft_ai_lama::Generator>)>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

fn scope() -> DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

fn post(msg: &Object, transfer: Option<&JsValue>) {
    let r = match transfer {
        Some(t) => scope().post_message_with_transfer(msg, &Array::of1(t)),
        None => scope().post_message(msg),
    };
    if let Err(e) = r {
        web_sys::console::error_2(&"AI worker: postMessage failed".into(), &e);
    }
}

fn obj(fields: &[(&str, JsValue)]) -> Object {
    let o = Object::new();
    for (k, v) in fields {
        let _ = Reflect::set(&o, &(*k).into(), v);
    }
    o
}

fn js_err(e: JsValue) -> String {
    e.as_string().or_else(|| e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message()))).unwrap_or_else(|| format!("{e:?}"))
}

fn get(msg: &JsValue, k: &str) -> JsValue {
    Reflect::get(msg, &k.into()).unwrap_or(JsValue::UNDEFINED)
}

/// Threads variant: the bootstrap calls this after loading the module, with the rayon pool size
/// it wants (0: one per core). The pool itself starts with the first CPU run.
#[cfg(target_feature = "atomics")]
#[wasm_bindgen(js_name = startAi)]
pub fn start_ai(threads: u32) {
    STATE.with(|s| s.borrow_mut().threads = threads as usize);
    start();
}

/// Installs the message handler and announces itself.
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        post(&obj(&[("type", "panic".into()), ("message", info.to_string().into())]), None);
    }));
    let on = Closure::<dyn FnMut(MessageEvent)>::new(|e: MessageEvent| {
        let data = e.data();
        match get(&data, "type").as_string().as_deref() {
            Some("run") => wasm_bindgen_futures::spawn_local(run(data)),
            Some("inpaint") => wasm_bindgen_futures::spawn_local(inpaint(data)),
            Some("download") => wasm_bindgen_futures::spawn_local(download(data)),
            Some("genfill") => wasm_bindgen_futures::spawn_local(genfill(data)),
            _ => {}
        }
    });
    scope().set_onmessage(Some(on.as_ref().unchecked_ref()));
    on.forget();
    post(&obj(&[("type", "ready".into())]), None);
}

struct Posting {
    id: f64,
    lo: f32,
    status: String,
    device: String,
    /// At most ten messages a second; a new status always goes.
    throttle: RefCell<(Throttle, String)>,
}

impl Posting {
    fn new(id: f64) -> Self {
        Self { id, lo: 0.0, status: String::new(), device: String::new(), throttle: RefCell::new((Throttle::default(), String::new())) }
    }

    fn send(&self, fraction: f32, status: &str) {
        {
            let mut t = self.throttle.borrow_mut();
            let changed = t.1 != status;
            if !t.0.ready(now_ms()) && !changed {
                return;
            }
            if changed {
                status.clone_into(&mut t.1);
            }
        }
        post(
            &obj(&[
                ("type", "progress".into()),
                ("id", self.id.into()),
                ("fraction", fraction.into()),
                ("status", status.into()),
                ("device", self.device.as_str().into()),
            ]),
            None,
        );
    }
}

impl Progress for Posting {
    fn report(&self, done: u64, total: u64, _: &str) {
        let f = if total == 0 { 0.0 } else { done as f32 / total as f32 };
        self.send(self.lo + (1.0 - self.lo) * f, &self.status);
    }
}

/// Milliseconds on the worker's clock.
fn now_ms() -> f64 {
    js_sys::Reflect::get(&js_sys::global(), &"performance".into())
        .ok()
        .and_then(|p| p.dyn_into::<web_sys::Performance>().ok())
        .map_or_else(js_sys::Date::now, |p| p.now())
}

async fn run(msg: JsValue) {
    let id = get(&msg, "id").as_f64().unwrap_or(0.0);
    match run_inner(&msg, id).await {
        Ok((out, device)) => {
            let data = Float32Array::from(out.data.as_slice());
            let buffer: JsValue = data.buffer().into();
            post(
                &obj(&[
                    ("type", "done".into()),
                    ("id", id.into()),
                    ("width", (out.width as f64).into()),
                    ("height", (out.height as f64).into()),
                    ("channels", (out.channels as f64).into()),
                    ("data", data.into()),
                    ("device", device.into()),
                ]),
                Some(&buffer),
            );
        }
        Err(e) => post(&obj(&[("type", "error".into()), ("id", id.into()), ("message", e.to_string().into())]), None),
    }
}

async fn run_inner(msg: &JsValue, id: f64) -> Result<(Planar, String)> {
    let name = get(msg, "model").as_string().unwrap_or_default();
    let model = catalog::model(&name).ok_or(AiError::UnknownModel(name))?;
    let base = get(msg, "base").as_string().unwrap_or_default();
    let download = get(msg, "download").as_bool().unwrap_or(false);
    let grayscale = get(msg, "grayscale").as_bool().unwrap_or(false);
    let cpu = get(msg, "cpu").as_bool().unwrap_or(false);
    let dim = |k: &str| get(msg, k).as_f64().filter(|v| v.is_finite() && *v >= 1.0).map(|v| v as usize).ok_or_else(|| AiError::Image(format!("bad {k}")));
    let (w, h, c) = (dim("width")?, dim("height")?, dim("channels")?);
    let data: Float32Array = get(msg, "data").dyn_into().map_err(|_| AiError::Image("no pixel data".into()))?;
    let input = Planar::new(w, h, c, data.to_vec())?;
    let mut p = Posting::new(id);
    let bytes = weights(model, &base, download, &p).await?;
    p.send(0.12, "Preparing the model");
    let rt = runtime(cpu).await?;
    p.device = STATE.with(|s| s.borrow().device.clone());
    let reuse = STATE.with(|s| s.borrow().loaded.clone()).filter(|l| l.id == model.id && l.grayscale == grayscale);
    let loaded = match reuse {
        Some(l) => l,
        None => {
            STATE.with(|s| s.borrow_mut().loaded = None);
            let l = Rc::new(Loaded::new(&rt, model, &bytes, grayscale)?);
            STATE.with(|s| s.borrow_mut().loaded = Some(l.clone()));
            l
        }
    };
    drop(bytes);
    p.lo = 0.15;
    p.status = format!("Upscaling with {} ({})", model.title, p.device);
    let mut tiles = default_tiles(model);
    let n = |k: &str| get(msg, k).as_f64().filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as usize);
    if let Some(t) = n("tile").filter(|t| (32..=1024).contains(t)) {
        tiles.tile = t;
    }
    if let Some(o) = n("overlap").filter(|o| o * 2 < tiles.tile) {
        tiles.overlap = o;
    }
    let out = upscale(&rt, &loaded, &input, tiles, &p).await;
    if get(msg, "unload").as_bool().unwrap_or(false) {
        STATE.with(|s| s.borrow_mut().loaded = None);
    }
    Ok((out?, p.device))
}

/// The device: WebGPU when the browser offers an adapter (checked first, so a missing one is a
/// clean fallback, not a failure inside the runtime), else the CPU. The threads variant is
/// CPU-only.
async fn runtime(cpu: bool) -> Result<Runtime> {
    if let Some(rt) = STATE.with(|s| s.borrow().rt.clone()) {
        return Ok(rt);
    }
    #[cfg(not(target_feature = "atomics"))]
    let gpu = if !cpu && has_adapter().await {
        match Runtime::gpu().await {
            Ok(rt) => Ok(rt),
            Err(e) => {
                web_sys::console::warn_1(&format!("AI worker: WebGPU failed ({e}); using the CPU").into());
                Err(" (WebGPU failed)")
            }
        }
    } else {
        Err(if cpu { "" } else { " (no WebGPU)" })
    };
    #[cfg(target_feature = "atomics")]
    let gpu: std::result::Result<Runtime, &str> = Err(if cpu { "" } else { " (no WebGPU)" });
    let (rt, device) = match gpu {
        Ok(rt) => (rt, "WebGPU".to_string()),
        Err(why) => {
            let threads = cpu_pool().await;
            let what = if threads > 1 { format!("CPU, {threads} threads") } else { "CPU".to_string() };
            (Runtime::cpu(), format!("{what}{why}"))
        }
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.rt = Some(rt.clone());
        s.device = device;
    });
    Ok(rt)
}

/// Starts the CPU kernels' rayon pool (threads variant) and returns its size; 1 without threads.
async fn cpu_pool() -> usize {
    #[cfg(target_feature = "atomics")]
    {
        let (asked, up) = STATE.with(|s| (s.borrow().threads, s.borrow().pool));
        if up > 0 {
            return up;
        }
        let n = pool_size(asked, scope().navigator().hardware_concurrency());
        match JsFuture::from(wasm_bindgen_rayon::init_thread_pool(n)).await {
            Ok(_) => {
                STATE.with(|s| s.borrow_mut().pool = n);
                n
            }
            Err(e) => {
                // No nested workers (or no memory for them): one thread, as before.
                web_sys::console::warn_1(&format!("AI worker: no thread pool ({}); one CPU thread", js_err(e)).into());
                STATE.with(|s| s.borrow_mut().pool = 1);
                1
            }
        }
    }
    #[cfg(not(target_feature = "atomics"))]
    1
}

/// The pool size: what the page asked for, else one per core; at most 16 (more workers than
/// that cost memory and startup time for little gain on 512 px tiles).
#[cfg(target_feature = "atomics")]
fn pool_size(asked: usize, cores: f64) -> usize {
    let cores = if cores.is_finite() && cores >= 1.0 { cores as usize } else { 4 };
    if asked > 0 { asked.min(64) } else { cores.clamp(1, 16) }
}

#[cfg(not(target_feature = "atomics"))]
async fn has_adapter() -> bool {
    let gpu = get(&get(&js_sys::global(), "navigator"), "gpu");
    if gpu.is_undefined() || gpu.is_null() {
        return false;
    }
    let Ok(f) = get(&gpu, "requestAdapter").dyn_into::<js_sys::Function>() else { return false };
    let Ok(p) = f.call0(&gpu) else { return false };
    let Ok(p) = p.dyn_into::<js_sys::Promise>() else { return false };
    matches!(JsFuture::from(p).await, Ok(a) if !a.is_null() && !a.is_undefined())
}

/// The verified weights: from Cache Storage, else downloaded (with consent, for agents; the
/// editor's UI downloads in the AI Model Manager instead) and cached.
async fn weights(model: &'static ModelInfo, base: &str, download: bool, p: &Posting) -> Result<Vec<u8>> {
    let url = model.url(base);
    let cache = open_cache().await;
    if let Some(cache) = &cache
        && let Ok(hit) = JsFuture::from(cache.match_with_str(&url)).await
        && let Ok(resp) = hit.dyn_into::<Response>()
    {
        p.send(0.0, &format!("Loading {}", model.title));
        if let Ok(buf) = resp.array_buffer()
            && let Ok(buf) = JsFuture::from(buf).await
        {
            let bytes = Uint8Array::new(&buf).to_vec();
            if verify(model, &bytes).is_ok() {
                return Ok(bytes);
            }
        }
        let _ = JsFuture::from(cache.delete_with_str(&url)).await;
    }
    if !download {
        return Err(AiError::Download(format!("{} isn't downloaded", model.title)));
    }
    if base.is_empty() {
        return Err(AiError::Download("model downloads are not configured on this site".into()));
    }
    let status = format!("Downloading {} ({:.0} MB)", model.title, model.bytes as f64 / 1e6);
    let blob = fetch_verified(&url, model, &mut |done, total| p.send(0.12 * done as f32 / total.max(1) as f32, &status)).await?;
    if let Some(cache) = &cache
        && let Err(e) = store(cache, &url, model, &blob).await
    {
        // Out of quota, private mode: it still runs, it just downloads next time.
        web_sys::console::warn_1(&format!("AI worker: couldn't cache {}: {e}", model.file).into());
    }
    let buf = JsFuture::from(blob.array_buffer()).await.map_err(|e| AiError::Download(js_err(e)))?;
    Ok(Uint8Array::new(&buf).to_vec())
}

async fn open_cache() -> Option<Cache> {
    let caches = scope().caches().ok()?;
    JsFuture::from(caches.open(WEB_CACHE)).await.ok()?.dyn_into::<Cache>().ok()
}

/// Streams `url`, never reading more than the model's size, hashing each chunk as it arrives
/// (no second pass over 100 MB) and keeping the chunks on the JavaScript side (no copy into the
/// module's memory); `progress(done, total)` is called at most ten times a second. The result
/// is verified (size and sha256).
async fn fetch_verified(url: &str, model: &ModelInfo, progress: &mut dyn FnMut(u64, u64)) -> Result<Blob> {
    use sha2::Digest;
    let err = |e: String| AiError::Download(format!("{url}: {e}"));
    let resp: Response = JsFuture::from(scope().fetch_with_str(url)).await.map_err(|e| err(js_err(e)))?.dyn_into().map_err(|_| err("not a response".into()))?;
    if !resp.ok() {
        return Err(err(format!("HTTP {}", resp.status())));
    }
    let body = resp.body().ok_or_else(|| err("no body".into()))?;
    let reader: ReadableStreamDefaultReader = body.get_reader().dyn_into().map_err(|_| err("no reader".into()))?;
    let chunks = Array::new();
    let mut hash = sha2::Sha256::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut done = 0u64;
    let mut throttle = Throttle::default();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|e| err(js_err(e)))?;
        if get(&chunk, "done").as_bool().unwrap_or(true) {
            break;
        }
        let v: Uint8Array = get(&chunk, "value").dyn_into().map_err(|_| err("bad chunk".into()))?;
        let n = v.length() as usize;
        done = done.saturating_add(n as u64);
        if done > model.bytes {
            let _ = reader.cancel();
            return Err(err("larger than expected".into()));
        }
        buf.resize(n, 0);
        v.copy_to(&mut buf);
        hash.update(&buf);
        chunks.push(&v);
        if throttle.ready(now_ms()) {
            progress(done, model.bytes);
        }
    }
    progress(done, model.bytes);
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != model.bytes {
        return Err(AiError::Checksum { expected: format!("{} bytes", model.bytes), actual: format!("{done} bytes") });
    }
    if actual != model.sha256 {
        return Err(AiError::Checksum { expected: model.sha256.to_string(), actual });
    }
    Blob::new_with_u8_array_sequence(&chunks).map_err(|e| err(js_err(e)))
}

/// Puts a verified file in Cache Storage, with its checksum in a header (the page reads it to
/// tell an outdated file from the catalog's).
async fn store(cache: &Cache, url: &str, model: &ModelInfo, blob: &Blob) -> std::result::Result<(), String> {
    let headers = web_sys::Headers::new().map_err(js_err)?;
    for (k, v) in [("content-type", "application/octet-stream".to_string()), ("content-length", model.bytes.to_string()), (photocraft_ai::cache::WEB_SHA_HEADER, model.sha256.to_string())] {
        headers.set(k, &v).map_err(js_err)?;
    }
    let init = web_sys::ResponseInit::new();
    init.set_headers(&headers);
    let resp = Response::new_with_opt_blob_and_init(Some(blob), &init).map_err(js_err)?;
    JsFuture::from(cache.put_with_str(url, &resp)).await.map(|_| ()).map_err(js_err)
}

/// The AI Model Manager's download (see the protocol in the module docs).
async fn download(msg: JsValue) {
    let name = get(&msg, "model").as_string().unwrap_or_default();
    let reply = |ty: &str, extra: &[(&str, JsValue)]| {
        let mut fields = vec![("type", JsValue::from(ty)), ("model", JsValue::from(name.as_str()))];
        fields.extend(extra.iter().cloned());
        post(&obj(&fields), None);
    };
    let r = async {
        let model = catalog::model(&name).ok_or_else(|| AiError::UnknownModel(name.clone()))?;
        let base = get(&msg, "base").as_string().unwrap_or_default();
        if base.is_empty() {
            return Err(AiError::Download("model downloads are not configured on this site".into()));
        }
        let cache = open_cache().await.ok_or_else(|| AiError::Cache("this browser can't store models here (Cache Storage is unavailable, as in some private windows)".into()))?;
        let url = model.url(&base);
        let blob = fetch_verified(&url, model, &mut |done, total| {
            reply("dl-progress", &[("done", (done as f64).into()), ("total", (total as f64).into())]);
        })
        .await?;
        store(&cache, &url, model, &blob).await.map_err(|e| AiError::Cache(format!("couldn't store {} ({e}); free some space or allow this site more storage", model.title)))
    }
    .await;
    match r {
        Ok(()) => reply("dl-done", &[]),
        Err(e) => reply("dl-error", &[("message", e.to_string().into())]),
    }
}

async fn inpaint(msg: JsValue) {
    let id = get(&msg, "id").as_f64().unwrap_or(0.0);
    match inpaint_inner(&msg, id).await {
        Ok((out, w, h, device)) => {
            let data = Float32Array::from(out.as_slice());
            let buffer: JsValue = data.buffer().into();
            post(
                &obj(&[
                    ("type", "done".into()),
                    ("id", id.into()),
                    ("width", (w as f64).into()),
                    ("height", (h as f64).into()),
                    ("channels", 3.0.into()),
                    ("data", data.into()),
                    ("device", device.into()),
                ]),
                Some(&buffer),
            );
        }
        Err(e) => post(&obj(&[("type", "error".into()), ("id", id.into()), ("message", e.into())]), None),
    }
}

async fn inpaint_inner(msg: &JsValue, id: f64) -> std::result::Result<(Vec<f32>, usize, usize, String), String> {
    let name = get(msg, "model").as_string().unwrap_or_else(|| catalog::LAMA_ID.to_string());
    let model = catalog::model(&name).filter(|m| m.content == catalog::Content::Inpaint).ok_or_else(|| format!("{name} isn't an inpainting model"))?;
    let base = get(msg, "base").as_string().unwrap_or_default();
    let download = get(msg, "download").as_bool().unwrap_or(false);
    let cpu = get(msg, "cpu").as_bool().unwrap_or(false);
    let dim = |k: &str| get(msg, k).as_f64().filter(|v| v.is_finite() && *v >= 1.0).map(|v| v as usize).ok_or_else(|| format!("bad {k}"));
    let (w, h) = (dim("width")?, dim("height")?);
    let arr = |k: &str| get(msg, k).dyn_into::<Float32Array>().map(|a| a.to_vec()).map_err(|_| format!("no {k}"));
    let (rgb, mask) = (arr("rgb")?, arr("mask")?);
    let mut p = Posting::new(id);
    let reuse = STATE.with(|s| s.borrow().inpaint.clone()).filter(|(i, _)| *i == model.id);
    let lama = match reuse {
        Some((_, l)) => l,
        None => {
            STATE.with(|s| s.borrow_mut().inpaint = None);
            let bytes = weights(model, &base, download, &p).await.map_err(|e| e.to_string())?;
            p.send(0.12, "Preparing the model");
            let rt = runtime(cpu).await.map_err(|e| e.to_string())?;
            let l = Rc::new(photocraft_ai_lama::Generator::from_safetensors(&bytes, rt.device()).map_err(|e| e.to_string())?);
            STATE.with(|s| s.borrow_mut().inpaint = Some((model.id, l.clone())));
            l
        }
    };
    p.device = STATE.with(|s| s.borrow().device.clone());
    p.send(0.2, &format!("Cleaning with {} ({})", model.title, p.device));
    let out = lama.generate_async(&rgb, &mask, w, h).await.map_err(|e| e.to_string());
    if get(msg, "unload").as_bool().unwrap_or(false) {
        STATE.with(|s| s.borrow_mut().inpaint = None);
    }
    Ok((out?, w, h, p.device))
}

// ------------------------------------------------------------------ Generative Fill

// Generative Fill models loaded on the device, and the tier they serve.
#[cfg(not(target_feature = "atomics"))]
thread_local! {
    static GENFILL: RefCell<Option<(photocraft_ai_sd::service::Tier, Rc<photocraft_ai_sd::runner::Models>)>> = const { RefCell::new(None) };
}

/// `{type: "genfill", id, tier, base, download, cpu, unload, width, height, rgb, mask, prompt,
/// negative, variants, seed, steps, strength, guidance}` → `{type: "gf-done", id, width, height,
/// variants: [Float32Array…], device}` (interleaved RGB per variant, transferred) or `error`.
async fn genfill(msg: JsValue) {
    let id = get(&msg, "id").as_f64().unwrap_or(0.0);
    #[cfg(not(target_feature = "atomics"))]
    let r = genfill_inner(&msg, id).await;
    #[cfg(target_feature = "atomics")]
    let r: std::result::Result<(Vec<Vec<f32>>, usize, usize, String), String> = Err("Generative Fill runs on WebGPU, not in the CPU threads worker".into());
    match r {
        Ok((outs, w, h, device)) => {
            let arr = Array::new();
            let transfer = Array::new();
            for o in &outs {
                let a = Float32Array::from(o.as_slice());
                transfer.push(&a.buffer());
                arr.push(&a);
            }
            let o = obj(&[
                ("type", "gf-done".into()),
                ("id", id.into()),
                ("width", (w as f64).into()),
                ("height", (h as f64).into()),
                ("variants", arr.into()),
                ("device", device.into()),
            ]);
            if let Err(e) = scope().post_message_with_transfer(&o, &transfer) {
                web_sys::console::error_2(&"AI worker: postMessage failed".into(), &e);
            }
        }
        Err(e) => post(&obj(&[("type", "error".into()), ("id", id.into()), ("message", e.into())]), None),
    }
}

#[cfg(not(target_feature = "atomics"))]
async fn genfill_inner(msg: &JsValue, id: f64) -> std::result::Result<(Vec<Vec<f32>>, usize, usize, String), String> {
    use photocraft_ai_sd::runner::{Loader, generate, precision};
    use photocraft_ai_sd::service::Tier;
    let tier = get(msg, "tier").as_string().and_then(|t| Tier::from_id(&t)).ok_or("bad tier")?;
    let files = photocraft_ai_sd::files::tier(tier).ok_or_else(|| format!("the {} tier isn't available yet", tier.id()))?;
    let base = get(msg, "base").as_string().unwrap_or_default();
    let download = get(msg, "download").as_bool().unwrap_or(false);
    let cpu = get(msg, "cpu").as_bool().unwrap_or(false);
    let num = |k: &str| get(msg, k).as_f64().filter(|v| v.is_finite() && *v >= 0.0);
    let dim = |k: &str| num(k).filter(|v| *v >= 1.0).map(|v| v as usize).ok_or_else(|| format!("bad {k}"));
    let (w, h) = (dim("width")?, dim("height")?);
    let arr = |k: &str| get(msg, k).dyn_into::<Float32Array>().map(|a| a.to_vec()).map_err(|_| format!("no {k}"));
    let (rgb, mask) = (arr("rgb")?, arr("mask")?);
    let text = |k: &str| get(msg, k).as_string().unwrap_or_default();
    let mut p = Posting::new(id);
    let reuse = GENFILL.with(|g| g.borrow().clone()).filter(|(t, _)| *t == tier);
    let models = match reuse {
        Some((_, m)) => m,
        None => {
            GENFILL.with(|g| *g.borrow_mut() = None);
            let rt = runtime(cpu).await.map_err(|e| e.to_string())?;
            if rt.accel() != photocraft_ai::runtime::Accel::Gpu {
                return Err("Generative Fill needs WebGPU (no usable GPU here); use Clean with AI instead".into());
            }
            let dtype = precision(rt.device(), false);
            let mut loader = Loader::new(rt.device(), dtype).map_err(|e| e.to_string())?;
            let total: u64 = files.bytes().max(1);
            let mut done = 0u64;
            for &f in &files.files {
                if download && !is_cached(f, &base).await {
                    let status = format!("Downloading {}", f.title);
                    let start = done;
                    let url = f.url(&base);
                    let blob =
                        fetch_verified(&url, f, &mut |d, _| p.send(0.15 * (start + d) as f32 / total as f32, &status)).await.map_err(|e| e.to_string())?;
                    if let Some(cache) = open_cache().await {
                        store(&cache, &url, f, &blob).await.map_err(|e| format!("couldn't store {}: {e}", f.title))?;
                    }
                }
                let start = done;
                stream_into(&mut loader, f, &base, &mut |d| p.send(0.15 * (start + d) as f32 / total as f32, "Loading Generative Fill")).await?;
                done += f.bytes;
            }
            p.send(0.16, "Preparing Generative Fill");
            let m = Rc::new(loader.finish().map_err(|e| e.to_string())?);
            GENFILL.with(|g| *g.borrow_mut() = Some((tier, m.clone())));
            m
        }
    };
    p.device = STATE.with(|s| s.borrow().device.clone());
    let req = photocraft_ai_sd::service::Request {
        rgb,
        mask,
        width: w,
        height: h,
        prompt: text("prompt"),
        negative: text("negative"),
        variants: num("variants").map_or(1, |v| v as usize),
        seed: num("seed").map_or(0, |v| v as u64),
        steps: num("steps").map_or(0, |v| v as usize),
        strength: num("strength").unwrap_or(1.0),
        guidance: num("guidance").unwrap_or(0.0) as f32,
        tier,
        allow_download: download,
        cpu,
        threads: 0,
        unload: false,
    };
    let params = photocraft_ai_sd::runner::params(&req, &models);
    let total = params.steps.max(1) as f32;
    let status = format!("Generating ({})", p.device);
    p.send(0.18, &status);
    let out = generate(&models, &params, &mut |d, _| {
        p.send(0.18 + 0.8 * d as f32 / total, &status);
        true
    })
    .await
    .map_err(|e| e.to_string());
    if get(msg, "unload").as_bool().unwrap_or(false) {
        GENFILL.with(|g| *g.borrow_mut() = None);
    }
    Ok((out?, w, h, p.device))
}

/// Whether `model`'s file is in Cache Storage.
#[cfg(not(target_feature = "atomics"))]
async fn is_cached(model: &ModelInfo, base: &str) -> bool {
    let Some(cache) = open_cache().await else { return false };
    matches!(JsFuture::from(cache.match_with_str(&model.url(base))).await, Ok(v) if !v.is_undefined() && !v.is_null())
}

/// Streams one cached file into `loader` chunk by chunk (never whole in the wasm heap), hashing
/// it on the way; a file whose checksum doesn't match is deleted and reported.
#[cfg(not(target_feature = "atomics"))]
async fn stream_into(
    loader: &mut photocraft_ai_sd::runner::Loader,
    model: &'static ModelInfo,
    base: &str,
    progress: &mut dyn FnMut(u64),
) -> std::result::Result<(), String> {
    use sha2::Digest;
    let url = model.url(base);
    let missing = || format!("{} isn't downloaded", model.title);
    let cache = open_cache().await.ok_or_else(missing)?;
    let resp: Response = JsFuture::from(cache.match_with_str(&url)).await.ok().and_then(|r| r.dyn_into().ok()).ok_or_else(missing)?;
    let body = resp.body().ok_or_else(missing)?;
    let reader: ReadableStreamDefaultReader = body.get_reader().dyn_into().map_err(|_| "no reader".to_string())?;
    let mut parser = photocraft_ai_sd::stream::StreamParser::new();
    let mut sink: Option<photocraft_ai_sd::runner::FileSink> = None;
    let mut hash = sha2::Sha256::new();
    let mut buf: Vec<u8> = Vec::new();
    let mut done = 0u64;
    let mut throttle = Throttle::default();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(js_err)?;
        if get(&chunk, "done").as_bool().unwrap_or(true) {
            break;
        }
        let v: Uint8Array = get(&chunk, "value").dyn_into().map_err(|_| "bad chunk".to_string())?;
        buf.resize(v.length() as usize, 0);
        v.copy_to(&mut buf);
        done = done.saturating_add(buf.len() as u64);
        if done > model.bytes {
            let _ = reader.cancel();
            break;
        }
        hash.update(&buf);
        let ts = parser.push(&buf).map_err(|e| e.to_string())?;
        if sink.is_none() && !parser.metadata().is_empty() {
            sink = Some(loader.open(parser.metadata()).map_err(|e| e.to_string())?);
        }
        if let Some(s) = sink.as_mut() {
            for t in &ts {
                s.tensor(t).map_err(|e| e.to_string())?;
            }
        }
        if throttle.ready(now_ms()) {
            progress(done);
        }
    }
    let actual: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
    if done != model.bytes || actual != model.sha256 || !parser.finished() {
        let _ = JsFuture::from(cache.delete_with_str(&url)).await;
        return Err(format!("{} is damaged; download it again in Window › AI Models…", model.title));
    }
    let sink = sink.ok_or_else(|| format!("{} isn't a Generative Fill file", model.title))?;
    loader.close(sink).map_err(|e| e.to_string())?;
    Ok(())
}
