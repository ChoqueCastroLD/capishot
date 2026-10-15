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
//! `{type: "run", id, model, base, download, grayscale, cpu, width, height, channels, data}`:
//! upscale `data` (a `Float32Array`, planar, transferred) with catalog model `model`, fetching
//! its weights from `base` when not cached and `download` is true; `cpu` skips WebGPU.
//!
//! Worker → main thread:
//! * `{type: "ready"}` once loaded;
//! * `{type: "progress", id, fraction, status, device}`;
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
use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_ai::runtime::Runtime;
use photocraft_ai::upscale::{Loaded, default_tiles, upscale};
use photocraft_ai::{AiError, Planar, Progress, Result};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Cache, DedicatedWorkerGlobalScope, MessageEvent, ReadableStreamDefaultReader, Response};

#[derive(Default)]
struct State {
    rt: Option<Runtime>,
    device: String,
    loaded: Option<Rc<Loaded>>,
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

/// Installs the message handler and announces itself.
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        post(&obj(&[("type", "panic".into()), ("message", info.to_string().into())]), None);
    }));
    let on = Closure::<dyn FnMut(MessageEvent)>::new(|e: MessageEvent| {
        let data = e.data();
        if get(&data, "type").as_string().as_deref() == Some("run") {
            wasm_bindgen_futures::spawn_local(run(data));
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
}

impl Posting {
    fn send(&self, fraction: f32, status: &str) {
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
    let mut p = Posting { id, lo: 0.0, status: String::new(), device: String::new() };
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
    let out = upscale(&rt, &loaded, &input, default_tiles(model), &p).await?;
    Ok((out, p.device))
}

/// The device: WebGPU when the browser offers an adapter (checked first, so a missing one is a
/// clean fallback, not a failure inside the runtime), else the CPU.
async fn runtime(cpu: bool) -> Result<Runtime> {
    if let Some(rt) = STATE.with(|s| s.borrow().rt.clone()) {
        return Ok(rt);
    }
    let (rt, device) = if !cpu && has_adapter().await {
        match Runtime::gpu().await {
            Ok(rt) => (rt, "WebGPU".to_string()),
            Err(e) => {
                web_sys::console::warn_1(&format!("AI worker: WebGPU failed ({e}); using the CPU").into());
                (Runtime::cpu(), "CPU (WebGPU failed)".to_string())
            }
        }
    } else {
        (Runtime::cpu(), if cpu { "CPU".to_string() } else { "CPU (no WebGPU)".to_string() })
    };
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.rt = Some(rt.clone());
        s.device = device;
    });
    Ok(rt)
}

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

/// The verified weights: from Cache Storage, else downloaded (with consent) and cached.
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
        return Err(AiError::Download(format!("{} isn't downloaded yet", model.title)));
    }
    if base.is_empty() {
        return Err(AiError::Download("model downloads are not configured on this site".into()));
    }
    let bytes = fetch(&url, model, p).await?;
    verify(model, &bytes)?;
    if let Some(cache) = &cache {
        let arr = Uint8Array::from(bytes.as_slice());
        match Response::new_with_opt_buffer_source(Some(&arr)) {
            Ok(resp) => {
                if let Err(e) = JsFuture::from(cache.put_with_str(&url, &resp)).await {
                    // Out of quota, private mode: it still runs, it just downloads next time.
                    web_sys::console::warn_1(&format!("AI worker: couldn't cache {}: {}", model.file, js_err(e)).into());
                }
            }
            Err(e) => web_sys::console::warn_1(&js_err(e).into()),
        }
    }
    Ok(bytes)
}

async fn open_cache() -> Option<Cache> {
    let caches = scope().caches().ok()?;
    JsFuture::from(caches.open(WEB_CACHE)).await.ok()?.dyn_into::<Cache>().ok()
}

/// Streams `url` with progress, never reading more than the model's size.
async fn fetch(url: &str, model: &ModelInfo, p: &Posting) -> Result<Vec<u8>> {
    let err = |e: String| AiError::Download(format!("{url}: {e}"));
    let resp: Response = JsFuture::from(scope().fetch_with_str(url)).await.map_err(|e| err(js_err(e)))?.dyn_into().map_err(|_| err("not a response".into()))?;
    if !resp.ok() {
        return Err(err(format!("HTTP {}", resp.status())));
    }
    let body = resp.body().ok_or_else(|| err("no body".into()))?;
    let reader: ReadableStreamDefaultReader = body.get_reader().dyn_into().map_err(|_| err("no reader".into()))?;
    let mut out: Vec<u8> = Vec::with_capacity(usize::try_from(model.bytes).unwrap_or(0));
    let status = format!("Downloading {} ({:.0} MB)", model.title, model.bytes as f64 / 1e6);
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|e| err(js_err(e)))?;
        if get(&chunk, "done").as_bool().unwrap_or(true) {
            break;
        }
        let v: Uint8Array = get(&chunk, "value").dyn_into().map_err(|_| err("bad chunk".into()))?;
        if out.len() as u64 + u64::from(v.length()) > model.bytes {
            let _ = reader.cancel();
            return Err(err("larger than expected".into()));
        }
        let at = out.len();
        out.resize(at + v.length() as usize, 0);
        if let Some(dst) = out.get_mut(at..) {
            v.copy_to(dst);
        }
        p.send(0.12 * out.len() as f32 / model.bytes.max(1) as f32, &status);
    }
    Ok(out)
}
