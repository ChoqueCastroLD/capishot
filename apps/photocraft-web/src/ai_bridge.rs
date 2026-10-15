//! Image › Upscale with AI… in the browser: the engine's [`Upscaler`] backed by the AI Web
//! Worker (`apps/photocraft-ai-worker`), a second wasm module this page loads only when an
//! upscale runs, so the main module doesn't carry the inference stack.
//!
//! The page says where things are with two `<meta>` tags (relative URLs resolve against the
//! page; packaging/web-ai/build.sh writes the first, the host the second):
//!
//! ```html
//! <meta name="capishot-ai-worker" content="ai/photocraft_ai_worker-<hash>.js">
//! <meta name="capishot-ai-models" content="https://example.com/capishot/models/">
//! ```
//!
//! A cross-origin isolated page (the threads build) also names the worker's threads variant,
//! which runs the CPU kernels on a rayon pool of nested Web Workers (CPU only: no WebGPU in it):
//!
//! ```html
//! <meta name="capishot-ai-worker-threads" content="ai/<hash>/photocraft_ai_worker_mt.js">
//! ```
//!
//! Runs go to the plain worker when WebGPU may be used and to the threads variant when the run is
//! on the CPU (no WebGPU adapter on this page, or WebGPU failed). Without the worker the command
//! is unavailable; without a models URL only models already in the browser's Cache Storage run. The worker is a module worker started from a one-line
//! `blob:` bootstrap that imports the wasm-bindgen glue; it stays alive between runs (the model
//! stays on the GPU) and is terminated to cancel. Messages: see the worker's module docs.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use js_sys::{Array, Float32Array, Object, Reflect};
use photocraft_ai::cache::WEB_CACHE;
use photocraft_ai::catalog::ModelInfo;
use photocraft_ai::profile::{DeviceProfile, GpuClass};
use photocraft_ai::service::{Done, Hooks, ModelState, Request, Upscaler};
use photocraft_ai::{AiError, Planar};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// What the page configured plus what we learned (shared with the engine's job thread).
#[derive(Default)]
struct Shared {
    worker_url: Option<String>,
    /// The threads variant (only on cross-origin isolated pages).
    worker_mt_url: Option<String>,
    /// Does this page have a WebGPU adapter (`None`: not known yet)?
    gpu: Option<bool>,
    /// The machine, as the page sees it (filled by `probe`).
    profile: DeviceProfile,
    /// Bytes in the models' Cache Storage, last time we asked.
    storage: Option<u64>,
    /// When we last asked (ms since the epoch): the UI asks every frame, Cache Storage at most
    /// every two seconds.
    storage_asked_ms: f64,
    /// The threads variant failed to start: use the plain worker for everything.
    mt_broken: bool,
    models_base: Option<String>,
    /// Model id → is it in Cache Storage.
    cached: HashMap<&'static str, bool>,
    checking: HashSet<&'static str>,
    device: String,
    /// The worker crashed while starting WebGPU: run on the CPU from now on.
    cpu_only: bool,
}

/// See the module docs.
pub struct WebUpscaler {
    shared: Arc<Mutex<Shared>>,
}

/// Manga LaMa (`edit.inpaintAi`, the Remove tool's AI mode) through the same worker.
pub struct WebInpainter {
    up: WebUpscaler,
}

/// The upscaler and the inpainter for this page, if it names an AI worker (one worker runs both).
pub fn from_page() -> Option<(Arc<dyn Upscaler>, Arc<dyn photocraft_ai_lama::service::Inpainter>)> {
    let worker_url = meta_url("capishot-ai-worker")?;
    let models_base = meta_url("capishot-ai-models").map(|u| if u.ends_with('/') { u } else { format!("{u}/") });
    let isolated = Reflect::get(&js_sys::global(), &"crossOriginIsolated".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    let worker_mt_url = if isolated { meta_url("capishot-ai-worker-threads") } else { None };
    let shared = Arc::new(Mutex::new(Shared { worker_url: Some(worker_url), worker_mt_url, models_base, ..Default::default() }));
    probe(shared.clone(), worker_mt_url_present(&shared));
    Some((Arc::new(WebUpscaler { shared: shared.clone() }), Arc::new(WebInpainter { up: WebUpscaler { shared } })))
}

fn worker_mt_url_present(shared: &Arc<Mutex<Shared>>) -> bool {
    shared.lock().unwrap_or_else(PoisonError::into_inner).worker_mt_url.is_some()
}

/// The device profile: the WebGPU adapter (once), `navigator.deviceMemory`,
/// `hardwareConcurrency` and a mobile user agent; CPU runs go to the threads variant when there
/// is no adapter.
fn probe(shared: Arc<Mutex<Shared>>, cpu_threads: bool) {
    let nav = web_sys::window().map(|w| w.navigator());
    let num = |k: &str| nav.as_ref().and_then(|n| Reflect::get(n, &k.into()).ok()).and_then(|v| v.as_f64()).filter(|v| v.is_finite() && *v > 0.0);
    let ua = nav.as_ref().and_then(|n| n.user_agent().ok()).unwrap_or_default();
    // userAgentData.mobile where the browser has it (Chromium), else the user agent string.
    let ua_mobile = nav
        .as_ref()
        .and_then(|n| Reflect::get(n, &"userAgentData".into()).ok())
        .filter(|d| !d.is_undefined() && !d.is_null())
        .and_then(|d| Reflect::get(&d, &"mobile".into()).ok())
        .and_then(|m| m.as_bool());
    {
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        s.profile.memory_gb = num("deviceMemory").map(|m| m as f32);
        s.profile.cores = num("hardwareConcurrency").map(|c| c as usize);
        s.profile.mobile = ua_mobile.unwrap_or(false) || photocraft_ai::profile::mobile_user_agent(&ua);
        s.profile.cpu_threads = cpu_threads;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let found = async {
            let gpu = Reflect::get(&web_sys::window()?.navigator(), &"gpu".into()).ok().filter(|g| !g.is_undefined() && !g.is_null())?;
            let f: js_sys::Function = Reflect::get(&gpu, &"requestAdapter".into()).ok()?.dyn_into().ok()?;
            let p: js_sys::Promise = f.call0(&gpu).ok()?.dyn_into().ok()?;
            let a = JsFuture::from(p).await.ok()?;
            if a.is_null() || a.is_undefined() {
                return Some((GpuClass::None, String::new()));
            }
            let info = Reflect::get(&a, &"info".into()).ok().filter(|i| !i.is_undefined() && !i.is_null());
            let field = |k: &str| info.as_ref().and_then(|i| Reflect::get(i, &k.into()).ok()).and_then(|v| v.as_string()).unwrap_or_default();
            let fallback = info
                .as_ref()
                .and_then(|i| Reflect::get(i, &"isFallbackAdapter".into()).ok())
                .and_then(|v| v.as_bool())
                .or_else(|| Reflect::get(&a, &"isFallbackAdapter".into()).ok().and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let (vendor, arch, desc) = (field("vendor"), field("architecture"), field("description"));
            let class = GpuClass::from_adapter(fallback, &vendor, &arch, &desc);
            let name = [desc, vendor, arch].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" ");
            Some((class, name))
        }
        .await
        .unwrap_or((GpuClass::None, String::new()));
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        s.gpu.get_or_insert(found.0.usable());
        s.profile.gpu = found.0;
        s.profile.gpu_name = found.1;
    });
}

/// Refreshes the Cache Storage size (`navigator.storage.estimate()` covers the whole origin, so
/// the models' entries are summed instead).
fn refresh_storage(shared: Arc<Mutex<Shared>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let total = async {
            let caches = web_sys::window()?.caches().ok()?;
            let cache: web_sys::Cache = JsFuture::from(caches.open(WEB_CACHE)).await.ok()?.dyn_into().ok()?;
            let keys: Array = JsFuture::from(cache.keys()).await.ok()?.dyn_into().ok()?;
            let mut sum = 0u64;
            for k in keys.iter() {
                let url = Reflect::get(&k, &"url".into()).ok().and_then(|u| u.as_string()).unwrap_or_default();
                let file = url.rsplit('/').next().unwrap_or_default();
                sum += photocraft_ai::catalog::MODELS.iter().find(|m| m.file == file).map_or(0, |m| m.bytes);
            }
            Some(sum)
        }
        .await;
        shared.lock().unwrap_or_else(PoisonError::into_inner).storage = total;
    });
}

fn meta_url(name: &str) -> Option<String> {
    let doc = web_sys::window()?.document()?;
    let el = doc.query_selector(&format!("meta[name=\"{name}\"]")).ok()??;
    let content = el.get_attribute("content")?.trim().to_string();
    if content.is_empty() {
        return None;
    }
    let base = doc.base_uri().ok()??;
    web_sys::Url::new_with_base(&content, &base).ok().map(|u| u.href())
}

impl WebUpscaler {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Looks the model up in Cache Storage (async; [`ModelState::Checking`] until it answers).
    fn check(&self, model: &'static ModelInfo, base: String) {
        let shared = self.shared.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let url = model.url(&base);
            let hit = async {
                let caches = web_sys::window()?.caches().ok()?;
                let cache: web_sys::Cache = JsFuture::from(caches.open(WEB_CACHE)).await.ok()?.dyn_into().ok()?;
                let r = JsFuture::from(cache.match_with_str(&url)).await.ok()?;
                Some(r.is_instance_of::<web_sys::Response>())
            }
            .await
            .unwrap_or(false);
            let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
            s.cached.insert(model.id, hit);
            s.checking.remove(model.id);
        });
    }
}

impl Upscaler for WebUpscaler {
    fn model_state(&self, model: &ModelInfo) -> ModelState {
        let Some(model) = photocraft_ai::catalog::model(model.id) else { return ModelState::Unavailable(format!("unknown model {}", model.id)) };
        let mut s = self.lock();
        let Some(base) = s.models_base.clone() else {
            return ModelState::Unavailable("AI models are not hosted on this site yet".into());
        };
        match s.cached.get(model.id) {
            Some(true) => ModelState::Ready,
            Some(false) => ModelState::NeedsDownload,
            None => {
                if s.checking.insert(model.id) {
                    drop(s);
                    self.check(model, base);
                }
                ModelState::Checking
            }
        }
    }

    fn device(&self) -> String {
        self.lock().device.clone()
    }

    fn profile(&self) -> DeviceProfile {
        self.lock().profile.clone()
    }

    fn storage_used(&self) -> Option<u64> {
        let now = js_sys::Date::now();
        let (stale, used) = {
            let mut s = self.lock();
            let stale = now - s.storage_asked_ms > 2000.0;
            if stale {
                s.storage_asked_ms = now;
            }
            (stale, s.storage)
        };
        if stale {
            refresh_storage(self.shared.clone());
        }
        used
    }

    fn clear_downloads(&self) -> photocraft_ai::Result<()> {
        // A live worker keeps its model in memory; it goes too.
        let busy = LIVE.with(|l| l.borrow().as_ref().is_some_and(|l| l.run.is_some()));
        if busy {
            return Err(AiError::Cache("an AI run is in progress".into()));
        }
        kill(AiError::Cancelled);
        let shared = self.shared.clone();
        {
            let mut s = self.lock();
            s.cached.clear();
            s.storage = Some(0);
        }
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(caches) = web_sys::window().and_then(|w| w.caches().ok()) {
                let _ = JsFuture::from(caches.delete(WEB_CACHE)).await;
            }
            shared.lock().unwrap_or_else(PoisonError::into_inner).cached.clear();
        });
        Ok(())
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        let Some(t) = self.target(req.opts.cpu, req.opts.threads) else { return done(Err(AiError::Device(NO_WORKER.into()))) };
        let msg = run_message(&req, &t.base, t.cpu);
        let (h2, h3) = (hooks.clone(), hooks);
        let run = Run {
            model: req.model.id,
            reply: Some(Reply::Upscale(done)),
            progress: Box::new(move |f, s| h2.progress(f, s)),
            cancelled: Box::new(move || h3.cancelled()),
            shared: self.shared.clone(),
            started_gpu: false,
        };
        if let Err(e) = start(&t, msg, run) {
            log::error!("AI worker: {e}");
        }
    }
}

const NO_WORKER: &str = "the AI worker isn't available here (it runs on the page's main thread, in builds that include it)";

/// Where a run goes.
struct Target {
    url: String,
    base: String,
    /// Skip WebGPU.
    cpu: bool,
    /// The threads variant.
    threads: bool,
    /// Its rayon pool size (0: one per core).
    pool: usize,
}

impl WebUpscaler {
    /// Which worker runs the next request (`cpu`: the user chose the CPU; `pool`: CPU threads,
    /// 0 = one per core); `None` off the main thread (the threads build's pool workers can't own
    /// the page's AI worker) or without a worker.
    fn target(&self, cpu: bool, pool: usize) -> Option<Target> {
        web_sys::window()?;
        let s = self.lock();
        let base = s.models_base.clone().unwrap_or_default();
        let cpu = cpu || s.cpu_only || s.gpu == Some(false);
        if cpu
            && !s.mt_broken
            && let Some(url) = s.worker_mt_url.clone()
        {
            return Some(Target { url, base, cpu, threads: true, pool });
        }
        Some(Target { url: s.worker_url.clone()?, base, cpu, threads: false, pool: 0 })
    }
}

impl photocraft_ai_lama::service::Inpainter for WebInpainter {
    fn model_state(&self) -> photocraft_ai_lama::service::ModelState {
        self.model_state_of(photocraft_ai::catalog::LAMA_ID)
    }

    fn model_state_of(&self, id: &str) -> photocraft_ai_lama::service::ModelState {
        use photocraft_ai_lama::service::ModelState as M;
        let Some(m) = photocraft_ai::catalog::model(id) else { return M::Unavailable(format!("{id} isn't in the catalog")) };
        match self.up.model_state(m) {
            ModelState::Ready => M::Ready,
            ModelState::NeedsDownload => M::NeedsDownload { bytes: m.bytes },
            ModelState::Checking => M::Checking,
            ModelState::Unavailable(why) => M::Unavailable(why),
        }
    }

    fn device(&self) -> String {
        self.up.device()
    }

    fn run(&self, req: photocraft_ai_lama::service::Request, hooks: Arc<dyn photocraft_ai_lama::service::Hooks>, done: photocraft_ai_lama::service::Done) {
        use photocraft_ai_lama::LamaError;
        let Some(model) = photocraft_ai::catalog::model(req.model) else { return done(Err(LamaError::Unavailable(format!("{} isn't in the catalog", req.model)))) };
        let Some(target) = self.up.target(req.cpu, req.threads) else { return done(Err(LamaError::Unavailable(NO_WORKER.into()))) };
        let (base, cpu) = (target.base.clone(), target.cpu);
        let t = &req.tile;
        let rgb = Float32Array::from(t.rgb.as_slice());
        let mask = Float32Array::from(t.mask.as_slice());
        let o = Object::new();
        for (k, v) in [
            ("type", JsValue::from("inpaint")),
            ("id", 1.into()),
            ("model", model.id.into()),
            ("unload", req.unload.into()),
            ("base", base.into()),
            ("download", req.allow_download.into()),
            ("cpu", cpu.into()),
            ("width", (t.w as f64).into()),
            ("height", (t.h as f64).into()),
            ("mask", mask.into()),
        ] {
            let _ = Reflect::set(&o, &k.into(), &v);
        }
        let buffer: JsValue = rgb.buffer().into();
        let _ = Reflect::set(&o, &"rgb".into(), &rgb.into());
        let (h2, h3) = (hooks.clone(), hooks);
        let run = Run {
            model: model.id,
            reply: Some(Reply::Inpaint(done)),
            progress: Box::new(move |f, s| h2.progress(f, s)),
            cancelled: Box::new(move || h3.cancelled()),
            shared: self.up.shared.clone(),
            started_gpu: false,
        };
        if let Err(e) = start(&target, (o, buffer), run) {
            log::error!("AI worker: {e}");
        }
    }
}

/// The `run` message (the pixels are transferred, not copied again).
fn run_message(req: &Request, base: &str, cpu: bool) -> (Object, JsValue) {
    let data = Float32Array::from(req.input.data.as_slice());
    let o = Object::new();
    let set = |k: &str, v: JsValue| {
        let _ = Reflect::set(&o, &k.into(), &v);
    };
    set("type", "run".into());
    set("id", 1.into());
    set("model", req.model.id.into());
    set("base", base.into());
    set("download", req.allow_download.into());
    set("grayscale", req.grayscale.into());
    set("cpu", cpu.into());
    set("width", (req.input.width as f64).into());
    set("height", (req.input.height as f64).into());
    set("channels", (req.input.channels as f64).into());
    set("unload", req.opts.unload.into());
    if let Some(t) = req.opts.tiles {
        set("tile", (t.tile as f64).into());
        set("overlap", (t.overlap as f64).into());
    }
    let buffer: JsValue = data.buffer().into();
    set("data", data.into());
    (o, buffer)
}

/// Where a run's result goes.
enum Reply {
    Upscale(Done),
    Inpaint(photocraft_ai_lama::service::Done),
}

/// Reports a run's progress (`0..=1`, status).
type ProgressFn = Box<dyn Fn(f32, &str)>;

/// The run in flight.
struct Run {
    model: &'static str,
    reply: Option<Reply>,
    progress: ProgressFn,
    cancelled: Box<dyn Fn() -> bool>,
    shared: Arc<Mutex<Shared>>,
    /// The worker reported it was starting the device (a crash after this is a GPU problem).
    started_gpu: bool,
}

impl Run {
    fn finish(&mut self, r: photocraft_ai::Result<Planar>) {
        use photocraft_ai_lama::LamaError;
        match self.reply.take() {
            Some(Reply::Upscale(done)) => done(r),
            Some(Reply::Inpaint(done)) => done(match r {
                Ok(p) => Ok(p.data),
                Err(AiError::Cancelled) => Err(LamaError::Cancelled),
                Err(e) => Err(LamaError::Device(e.to_string())),
            }),
            None => {}
        }
    }
}

/// The live worker, its handlers and the run it serves (main thread only).
struct Live {
    worker: web_sys::Worker,
    /// The threads variant.
    threads: bool,
    pool: usize,
    ready: bool,
    /// The run message waiting for `ready`.
    queued: Option<(Object, JsValue)>,
    run: Option<Run>,
    timer: Option<i32>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
    _tick: Closure<dyn FnMut()>,
}

thread_local! {
    static LIVE: RefCell<Option<Live>> = const { RefCell::new(None) };
    /// Handlers of killed workers: dropped only when the next worker starts, never from inside
    /// one of their own invocations (or while queued events of the old worker still arrive).
    static RETIRED: RefCell<Vec<Live>> = const { RefCell::new(Vec::new()) };
}

fn start(target: &Target, msg: (Object, JsValue), run: Run) -> Result<(), String> {
    let busy = LIVE.with(|l| l.borrow().as_ref().is_some_and(|l| l.run.is_some()));
    if busy {
        let mut run = run;
        run.finish(Err(AiError::Inference("another AI upscale is still running".into())));
        return Ok(());
    }
    // The other variant (or another pool size) is alive: it goes (models stay in Cache Storage).
    let other = LIVE.with(|l| l.borrow().as_ref().is_some_and(|l| l.threads != target.threads || l.pool != target.pool));
    if other {
        kill(AiError::Cancelled);
    }
    let exists = LIVE.with(|l| l.borrow().is_some());
    if !exists && let Err(e) = spawn(&target.url, target.threads, target.pool) {
        let mut run = run;
        run.finish(Err(AiError::Device(e.clone())));
        return Err(e);
    }
    let send = LIVE.with(|l| {
        let mut l = l.borrow_mut();
        let live = l.as_mut()?;
        live.run = Some(run);
        // Cancellation: the job's flag, checked four times a second.
        if live.timer.is_none()
            && let Some(w) = web_sys::window()
        {
            live.timer = w.set_interval_with_callback_and_timeout_and_arguments_0(live._tick.as_ref().unchecked_ref(), 250).ok();
        }
        if live.ready {
            Some((live.worker.clone(), msg))
        } else {
            live.queued = Some(msg);
            None
        }
    });
    if let Some((worker, msg)) = send {
        post(&worker, msg);
    }
    Ok(())
}

/// Sends the run message (outside any borrow of [`LIVE`]: a failure ends the run).
fn post(worker: &web_sys::Worker, (msg, buffer): (Object, JsValue)) {
    if let Err(e) = worker.post_message_with_transfer(&msg, &Array::of1(&buffer)) {
        end_run(|run| run.finish(Err(AiError::Inference(format!("couldn't send the image to the AI worker: {e:?}")))));
    }
}

/// Starts the worker module from a `blob:` bootstrap (a module worker can't take the wasm-bindgen
/// glue's URL directly: it must call its `init`). The threads variant then starts from
/// `startAi(threads)` (0: one rayon worker per core).
fn spawn(url: &str, threads: bool, pool: usize) -> Result<(), String> {
    let js = |e: JsValue| format!("{e:?}");
    let src = format!("const m = await import({});\nawait m.default();\nif (m.startAi) m.startAi({pool});\n", serde_json::Value::String(url.to_string()));
    let parts = Array::of1(&JsValue::from_str(&src));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("text/javascript");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(js)?;
    let blob_url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let wopts = web_sys::WorkerOptions::new();
    wopts.set_type(web_sys::WorkerType::Module);
    RETIRED.with(|r| r.borrow_mut().clear());
    let worker = web_sys::Worker::new_with_options(&blob_url, &wopts).map_err(js)?;
    let _ = web_sys::Url::revoke_object_url(&blob_url);
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| on_message(e.data()));
    let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(|e: web_sys::ErrorEvent| {
        e.prevent_default();
        crashed(&format!("the AI worker stopped: {}", e.message()));
    });
    let tick = Closure::<dyn FnMut()>::new(poll_cancel);
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    LIVE.with(|l| {
        *l.borrow_mut() = Some(Live { worker, threads, pool, ready: false, queued: None, run: None, timer: None, _on_message: on_message, _on_error: on_error, _tick: tick });
    });
    Ok(())
}

fn get(v: &JsValue, k: &str) -> JsValue {
    Reflect::get(v, &k.into()).unwrap_or(JsValue::UNDEFINED)
}

fn on_message(data: JsValue) {
    let ty = get(&data, "type").as_string().unwrap_or_default();
    match ty.as_str() {
        "ready" => {
            let send = LIVE.with(|l| {
                let mut l = l.borrow_mut();
                let live = l.as_mut()?;
                live.ready = true;
                live.queued.take().map(|msg| (live.worker.clone(), msg))
            });
            if let Some((worker, msg)) = send {
                post(&worker, msg);
            }
        }
        "progress" => {
            let fraction = get(&data, "fraction").as_f64().unwrap_or(0.0) as f32;
            let status = get(&data, "status").as_string().unwrap_or_default();
            let device = get(&data, "device").as_string().unwrap_or_default();
            with_run(|run| {
                if status.starts_with("Preparing") || status.starts_with("Upscaling") {
                    run.started_gpu = true;
                }
                if !device.is_empty() {
                    let mut s = run.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    // The plain worker found no usable WebGPU: CPU runs go to the threads variant.
                    if device.starts_with("CPU (") {
                        s.gpu = Some(false);
                    }
                    s.device = device.clone();
                }
                (run.progress)(fraction, &status);
            });
        }
        "done" => {
            let dim = |k: &str| get(&data, k).as_f64().unwrap_or(0.0) as usize;
            let pixels = get(&data, "data").dyn_into::<Float32Array>().map(|a| a.to_vec()).unwrap_or_default();
            let result = Planar::new(dim("width"), dim("height"), dim("channels"), pixels);
            end_run(|run| {
                if result.is_ok() {
                    run.shared.lock().unwrap_or_else(PoisonError::into_inner).cached.insert(run.model, true);
                }
                run.finish(result);
            });
        }
        "error" => {
            let message = get(&data, "message").as_string().unwrap_or_else(|| "the AI worker failed".into());
            end_run(|run| run.finish(Err(AiError::Inference(message))));
        }
        "panic" => {
            let message = get(&data, "message").as_string().unwrap_or_default();
            crashed(&format!("the AI worker crashed: {message}"));
        }
        _ => {}
    }
}

fn with_run(f: impl FnOnce(&mut Run)) {
    LIVE.with(|l| {
        if let Some(run) = l.borrow_mut().as_mut().and_then(|l| l.run.as_mut()) {
            f(run);
        }
    });
}

/// Ends the run in flight (the worker stays for the next one).
fn end_run(f: impl FnOnce(&mut Run)) {
    let run = LIVE.with(|l| {
        let mut l = l.borrow_mut();
        let live = l.as_mut()?;
        if let (Some(t), Some(w)) = (live.timer.take(), web_sys::window()) {
            w.clear_interval_with_handle(t);
        }
        live.run.take()
    });
    if let Some(mut run) = run {
        f(&mut run);
    }
}

/// Drops the worker (cancelled, crashed) and ends its run with `why`.
fn kill(why: AiError) {
    let live = LIVE.with(|l| l.borrow_mut().take());
    let Some(mut live) = live else { return };
    live.worker.set_onmessage(None);
    live.worker.set_onerror(None);
    live.worker.terminate();
    if let (Some(t), Some(w)) = (live.timer.take(), web_sys::window()) {
        w.clear_interval_with_handle(t);
    }
    let run = live.run.take();
    RETIRED.with(|r| r.borrow_mut().push(live));
    if let Some(mut run) = run {
        run.finish(Err(why));
    }
}

fn crashed(message: &str) {
    log::error!("{message}");
    // The threads variant didn't come up (no nested workers, no memory): plain worker from now on.
    let mt = LIVE.with(|l| l.borrow().as_ref().map(|l| (l.threads, l.ready)));
    if let Some((true, ready)) = mt {
        with_run(|run| {
            let mut s = run.shared.lock().unwrap_or_else(PoisonError::into_inner);
            if !ready || s.device.is_empty() {
                s.mt_broken = true;
            }
        });
    }
    with_run(|run| {
        // Crashed while bringing up WebGPU: use the CPU from now on.
        if run.started_gpu && run.shared.lock().unwrap_or_else(PoisonError::into_inner).device.is_empty() {
            run.shared.lock().unwrap_or_else(PoisonError::into_inner).cpu_only = true;
        }
    });
    kill(AiError::Inference(message.to_string()));
}

fn poll_cancel() {
    let cancelled = LIVE.with(|l| l.borrow().as_ref().and_then(|l| l.run.as_ref()).is_some_and(|r| (r.cancelled)()));
    if cancelled {
        kill(AiError::Cancelled);
    }
}
