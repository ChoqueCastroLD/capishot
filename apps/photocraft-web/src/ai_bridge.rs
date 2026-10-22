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
//!
//! Downloads (the AI Model Manager, `ai.models.*`) are separate from runs: [`WebModelStore`]
//! queues them and hands them, one at a time, to a second instance of the plain worker (the
//! downloader), which fetches, hashes, verifies and writes Cache Storage; the page only gets
//! a progress message at most ten times a second. Cancelling terminates the downloader; it is
//! also terminated when the queue is empty. At startup one Cache Storage listing tells which
//! models are already here (and, from the checksum header the downloader writes, which are
//! outdated), so the tools know at once whether their model is downloaded.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use js_sys::{Array, Float32Array, Object, Reflect};
use photocraft_ai::cache::{WEB_CACHE, WEB_SHA_HEADER};
use photocraft_ai::catalog::{MODELS, ModelInfo};
use photocraft_ai::models::{Cancelled, ModelStore, Queue, Status, Storage};
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
    /// WebGPU failed a run (or crashed the worker): run on the CPU from now on, and on this
    /// adapter in later sessions too ([`GPU_FAILED_KEY`]).
    cpu_only: bool,
    /// Files in Cache Storage with another checksum than the catalog's (an older release).
    outdated: HashSet<&'static str>,
    /// The AI Model Manager's downloads.
    queue: Queue,
    /// `navigator.storage.estimate()`: the origin's usage and quota, last time we asked.
    estimate: Option<(u64, u64)>,
    /// When we last asked (ms since the epoch).
    estimate_asked_ms: f64,
}

/// See the module docs.
pub struct WebUpscaler {
    shared: Arc<Mutex<Shared>>,
}

/// Manga LaMa (`edit.inpaintAi`, the Remove tool's AI mode) through the same worker.
pub struct WebInpainter {
    up: WebUpscaler,
}

/// Generative Fill (`edit.generativeFill`) through the same worker (WebGPU only).
pub struct WebGenFill {
    up: WebUpscaler,
}

/// The AI services this page offers.
pub struct Services {
    pub upscaler: Arc<dyn Upscaler>,
    pub inpainter: Arc<dyn photocraft_ai_lama::service::Inpainter>,
    pub genfill: Arc<dyn photocraft_ai_sd::service::GenFill>,
    pub models: Arc<dyn ModelStore>,
}

/// The upscaler, the inpainter and the model store for this page, if it names an AI worker (one
/// worker runs both; downloads use a second instance).
pub fn from_page() -> Option<Services> {
    let worker_url = meta_url("capishot-ai-worker")?;
    let models_base = meta_url("capishot-ai-models").map(|u| if u.ends_with('/') { u } else { format!("{u}/") });
    let isolated = Reflect::get(&js_sys::global(), &"crossOriginIsolated".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    let worker_mt_url = if isolated { meta_url("capishot-ai-worker-threads") } else { None };
    let shared = Arc::new(Mutex::new(Shared { worker_url: Some(worker_url), worker_mt_url, models_base, ..Default::default() }));
    probe(shared.clone(), worker_mt_url_present(&shared));
    scan(shared.clone());
    Some(Services {
        upscaler: Arc::new(WebUpscaler { shared: shared.clone() }),
        inpainter: Arc::new(WebInpainter { up: WebUpscaler { shared: shared.clone() } }),
        genfill: Arc::new(WebGenFill { up: WebUpscaler { shared: shared.clone() } }),
        models: Arc::new(WebModelStore { shared }),
    })
}

/// The query the downloader puts on the cache keys of an unfinished download's parts.
const PART_QUERY: &str = "?capishot-part=";

/// Which models are already in Cache Storage (one listing at startup; entries under another
/// models URL don't count: the worker looks files up by URL).
fn scan(shared: Arc<Mutex<Shared>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let base = shared.lock().unwrap_or_else(PoisonError::into_inner).models_base.clone();
        let Some(base) = base else { return };
        let mut partial: Vec<(&'static str, u64)> = Vec::new();
        let found = async {
            let caches = web_sys::window()?.caches().ok()?;
            let cache: web_sys::Cache = JsFuture::from(caches.open(WEB_CACHE)).await.ok()?.dyn_into().ok()?;
            let keys: Array = JsFuture::from(cache.keys()).await.ok()?.dyn_into().ok()?;
            let mut found: Vec<(&'static str, bool)> = Vec::new();
            for k in keys.iter() {
                let url = Reflect::get(&k, &"url".into()).ok().and_then(|u| u.as_string()).unwrap_or_default();
                // A part kept from an unfinished download: it resumes from there.
                if let Some((file_url, _)) = url.split_once(PART_QUERY) {
                    if let Some(m) = MODELS.iter().find(|m| m.url(&base) == file_url) {
                        let size = async {
                            let r: web_sys::Response = JsFuture::from(cache.match_with_str(&url)).await.ok()?.dyn_into().ok()?;
                            let b: web_sys::Blob = JsFuture::from(r.blob().ok()?).await.ok()?.dyn_into().ok()?;
                            Some(b.size() as u64)
                        }
                        .await
                        .unwrap_or(0);
                        partial.push((m.file, size));
                    }
                    continue;
                }
                let Some(m) = MODELS.iter().find(|m| m.url(&base) == url) else { continue };
                // The checksum header (written by the downloader); older entries have none.
                let sha = async {
                    let r: web_sys::Response = JsFuture::from(cache.match_with_str(&url)).await.ok()?.dyn_into().ok()?;
                    r.headers().get(WEB_SHA_HEADER).ok()?
                }
                .await;
                found.push((m.file, sha.is_some_and(|h| h != m.sha256)));
            }
            Some(found)
        }
        .await
        .unwrap_or_default();
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        let mut sums: HashMap<&'static str, u64> = HashMap::new();
        for (f, n) in partial.drain(..) {
            *sums.entry(f).or_default() += n;
        }
        for (f, n) in sums {
            if !found.iter().any(|(x, _)| *x == f) {
                s.queue.mark_partial(f, n);
            }
        }
        for m in MODELS {
            let hit = found.iter().find(|(f, _)| *f == m.file);
            s.cached.insert(m.id, hit.is_some_and(|(_, outdated)| !outdated));
            if hit.is_some_and(|(_, outdated)| *outdated) {
                s.outdated.insert(m.file);
            }
        }
    });
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
        // WebGPU failed on this adapter before: the CPU, until the adapter changes.
        if found.0.usable() {
            match local_storage().and_then(|l| l.get_item(GPU_FAILED_KEY).ok().flatten()) {
                Some(name) if name == found.1 => s.cpu_only = true,
                Some(_) => forget_gpu_failure(),
                None => {}
            }
        }
        s.profile.gpu = found.0;
        s.profile.gpu_name = found.1;
    });
}

/// `localStorage` key: the name of the WebGPU adapter that failed a run (see [`gpu_failed`]).
const GPU_FAILED_KEY: &str = "capishot.ai.gpuFailed";

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

fn forget_gpu_failure() {
    if let Some(l) = local_storage() {
        let _ = l.remove_item(GPU_FAILED_KEY);
    }
}

/// WebGPU failed a run: CPU from now on, remembered for this adapter (a new GPU or browser
/// that names it differently tries WebGPU again).
fn gpu_failed(shared: &Arc<Mutex<Shared>>) {
    let name = {
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        s.cpu_only = true;
        s.profile.gpu_name.clone()
    };
    if let Some(l) = local_storage() {
        let _ = l.set_item(GPU_FAILED_KEY, &name);
    }
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
        let s = self.lock();
        let mut p = s.profile.clone();
        // WebGPU failed here: choose as for a machine without a usable GPU (the lite model on
        // Quality Auto).
        if s.cpu_only {
            p.gpu = GpuClass::None;
        }
        p
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
        kill_downloader();
        let shared = self.shared.clone();
        {
            let mut s = self.lock();
            s.queue.cancel(None);
            for m in MODELS {
                s.cached.insert(m.id, false);
            }
            s.outdated.clear();
            s.storage = Some(0);
        }
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(caches) = web_sys::window().and_then(|w| w.caches().ok()) {
                let _ = JsFuture::from(caches.delete(WEB_CACHE)).await;
            }
            let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
            for m in MODELS {
                s.cached.insert(m.id, false);
            }
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
            gpu: !t.cpu && !t.threads,
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
        let (base, cpu, gpu) = (target.base.clone(), target.cpu, !target.cpu && !target.threads);
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
            gpu,
        };
        if let Err(e) = start(&target, (o, buffer), run) {
            log::error!("AI worker: {e}");
        }
    }
}

impl photocraft_ai_sd::service::GenFill for WebGenFill {
    fn model_state(&self, tier: photocraft_ai_sd::service::Tier) -> photocraft_ai_sd::service::ModelState {
        use photocraft_ai_sd::service::ModelState as M;
        let Some(files) = photocraft_ai_sd::files::tier(tier) else { return M::Unavailable(format!("the {} tier isn't available yet", tier.id())) };
        let (mut missing, mut checking) = (0u64, false);
        for &f in &files.files {
            match self.up.model_state(f) {
                ModelState::Ready => {}
                ModelState::NeedsDownload => missing += f.bytes,
                ModelState::Checking => checking = true,
                ModelState::Unavailable(why) => return M::Unavailable(why),
            }
        }
        if checking {
            M::Checking
        } else if missing > 0 {
            M::NeedsDownload { bytes: missing }
        } else {
            M::Ready
        }
    }

    fn device(&self) -> String {
        self.up.device()
    }

    fn run(&self, req: photocraft_ai_sd::service::Request, hooks: Arc<dyn photocraft_ai_sd::service::Hooks>, done: photocraft_ai_sd::service::Done) {
        use photocraft_ai_sd::SdError;
        let Some(first) = photocraft_ai_sd::files::tier(req.tier).and_then(|f| f.files.first().copied()) else {
            return done(Err(SdError::Unavailable(format!("the {} tier isn't available yet", req.tier.id()))));
        };
        // Standard and up: WebGPU, the plain worker. Ultra-light also runs on the CPU (the threads
        // worker on cross-origin isolated pages).
        let ultra = matches!(req.tier, photocraft_ai_sd::service::Tier::Ultra | photocraft_ai_sd::service::Tier::Fast);
        let target = if ultra { self.up.target(req.cpu, req.threads) } else { self.up.target(false, 0).map(|t| Target { threads: false, pool: 0, ..t }) };
        let Some(target) = target else { return done(Err(SdError::Unavailable(NO_WORKER.into()))) };
        let rgb = Float32Array::from(req.rgb.as_slice());
        let mask = Float32Array::from(req.mask.as_slice());
        let o = Object::new();
        for (k, v) in [
            ("type", JsValue::from("genfill")),
            ("id", 1.into()),
            ("tier", req.tier.id().into()),
            ("unload", req.unload.into()),
            ("base", target.base.clone().into()),
            ("download", req.allow_download.into()),
            ("cpu", target.cpu.into()),
            ("width", (req.width as f64).into()),
            ("height", (req.height as f64).into()),
            ("prompt", req.prompt.as_str().into()),
            ("negative", req.negative.as_str().into()),
            ("variants", (req.variants as f64).into()),
            ("seed", (req.seed as f64).into()),
            ("steps", (req.steps as f64).into()),
            ("strength", req.strength.into()),
            ("guidance", f64::from(req.guidance).into()),
            ("f32", req.f32.into()),
            ("mask", mask.into()),
        ] {
            let _ = Reflect::set(&o, &k.into(), &v);
        }
        let buffer: JsValue = rgb.buffer().into();
        let _ = Reflect::set(&o, &"rgb".into(), &rgb.into());
        let (h2, h3) = (hooks.clone(), hooks);
        let run = Run {
            model: first.id,
            reply: Some(Reply::GenFill(done)),
            progress: Box::new(move |f, s| h2.progress(f, s)),
            cancelled: Box::new(move || h3.cancelled()),
            shared: self.up.shared.clone(),
            started_gpu: false,
            gpu: !target.cpu && !target.threads,
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
    GenFill(photocraft_ai_sd::service::Done),
}

fn sd_err(e: AiError) -> photocraft_ai_sd::SdError {
    use photocraft_ai_sd::SdError;
    match e {
        AiError::Cancelled => SdError::Cancelled,
        AiError::Inference(m) if m.contains("isn't downloaded") => SdError::Unavailable(m),
        // Generative Fill has no CPU fallback in the browser (minutes per image): say so.
        AiError::Gpu(m) => SdError::Device(format!("the GPU failed ({m}); try fewer variants or a smaller selection, or use Clean with AI")),
        other => SdError::Device(other.to_string()),
    }
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
    /// It may run on WebGPU (the plain worker, not asked for the CPU): a crash or a GPU error
    /// then ends it with [`AiError::Gpu`], which the engine runs again on the CPU.
    gpu: bool,
}

impl Run {
    fn finish(&mut self, r: photocraft_ai::Result<Planar>) {
        use photocraft_ai_lama::LamaError;
        match self.reply.take() {
            Some(Reply::Upscale(done)) => done(r),
            Some(Reply::Inpaint(done)) => done(match r {
                Ok(p) => Ok(p.data),
                Err(AiError::Cancelled) => Err(LamaError::Cancelled),
                Err(AiError::Gpu(e)) => Err(LamaError::Gpu(e)),
                Err(e) => Err(LamaError::Device(e.to_string())),
            }),
            Some(Reply::GenFill(done)) => done(match r {
                Ok(p) => Ok(vec![p.data]),
                Err(e) => Err(sd_err(e)),
            }),
            None => {}
        }
    }

    /// Generative Fill's variants.
    fn finish_variants(&mut self, r: photocraft_ai::Result<Vec<Vec<f32>>>) {
        match self.reply.take() {
            Some(Reply::GenFill(done)) => done(r.map_err(sd_err)),
            Some(other) => {
                self.reply = Some(other);
                self.finish(Err(AiError::Inference("unexpected Generative Fill reply".into())));
            }
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
    RETIRED.with(|r| r.borrow_mut().clear());
    let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(|e: web_sys::MessageEvent| on_message(e.data()));
    let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(|e: web_sys::ErrorEvent| {
        e.prevent_default();
        crashed(&format!("the AI worker stopped: {}", e.message()));
    });
    let worker = module_worker(url, pool, &on_message, &on_error)?;
    let tick = Closure::<dyn FnMut()>::new(poll_cancel);
    LIVE.with(|l| {
        *l.borrow_mut() = Some(Live { worker, threads, pool, ready: false, queued: None, run: None, timer: None, _on_message: on_message, _on_error: on_error, _tick: tick });
    });
    Ok(())
}

/// A module worker running the wasm-bindgen glue at `url` (`pool`: the threads variant's rayon
/// pool size), with its handlers.
fn module_worker(
    url: &str,
    pool: usize,
    on_message: &Closure<dyn FnMut(web_sys::MessageEvent)>,
    on_error: &Closure<dyn FnMut(web_sys::ErrorEvent)>,
) -> Result<web_sys::Worker, String> {
    let js = |e: JsValue| format!("{e:?}");
    let src = format!("const m = await import({});\nawait m.default();\nif (m.startAi) m.startAi({pool});\n", serde_json::Value::String(url.to_string()));
    let parts = Array::of1(&JsValue::from_str(&src));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("text/javascript");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(js)?;
    let blob_url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let wopts = web_sys::WorkerOptions::new();
    wopts.set_type(web_sys::WorkerType::Module);
    let worker = web_sys::Worker::new_with_options(&blob_url, &wopts).map_err(js)?;
    let _ = web_sys::Url::revoke_object_url(&blob_url);
    worker.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    worker.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    Ok(worker)
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
        "gf-done" => {
            let variants: Vec<Vec<f32>> = get(&data, "variants")
                .dyn_into::<Array>()
                .map(|a| a.iter().filter_map(|v| v.dyn_into::<Float32Array>().ok()).map(|a| a.to_vec()).collect())
                .unwrap_or_default();
            let device = get(&data, "device").as_string().unwrap_or_default();
            end_run(|run| {
                {
                    let mut s = run.shared.lock().unwrap_or_else(PoisonError::into_inner);
                    if !device.is_empty() {
                        s.device = device.clone();
                    }
                }
                run.finish_variants(if variants.is_empty() { Err(AiError::Inference("Generative Fill returned nothing".into())) } else { Ok(variants) });
            });
        }
        "error" => {
            let message = get(&data, "message").as_string().unwrap_or_else(|| "the AI worker failed".into());
            let on_gpu = get(&data, "gpu").as_bool().unwrap_or(false) && LIVE.with(|l| l.borrow().as_ref().and_then(|l| l.run.as_ref()).is_some_and(|r| r.gpu));
            if on_gpu {
                // The GPU failed: CPU from now on. This worker (and its GPU state) goes first, so
                // the engine's retry on the CPU (started from `finish`) gets a fresh one.
                let run = take_run();
                kill(AiError::Cancelled);
                if let Some(mut run) = run {
                    gpu_failed(&run.shared);
                    run.finish(Err(AiError::Gpu(message)));
                }
                return;
            }
            end_run(|run| {
                // The model wasn't in Cache Storage after all (cleared since the startup scan).
                if message.contains("isn't downloaded")
                    && let Some(m) = photocraft_ai::catalog::model(run.model)
                {
                    set_cached(&mut run.shared.lock().unwrap_or_else(PoisonError::into_inner), m.file, false);
                }
                run.finish(Err(AiError::Inference(message)));
            });
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
    if let Some(mut run) = take_run() {
        f(&mut run);
    }
}

/// Takes the run in flight off the live worker (and stops its cancel timer).
fn take_run() -> Option<Run> {
    LIVE.with(|l| {
        let mut l = l.borrow_mut();
        let live = l.as_mut()?;
        if let (Some(t), Some(w)) = (live.timer.take(), web_sys::window()) {
            w.clear_interval_with_handle(t);
        }
        live.run.take()
    })
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
    // Crashed on WebGPU (bringing it up, or a GPU runtime panic in a run): the CPU from now on,
    // and the engine runs the request again there.
    let mut on_gpu = false;
    with_run(|run| {
        let device = run.shared.lock().unwrap_or_else(PoisonError::into_inner).device.clone();
        on_gpu = run.gpu && ((run.started_gpu && device.is_empty()) || device == "WebGPU");
        if on_gpu {
            gpu_failed(&run.shared);
        }
    });
    kill(if on_gpu { AiError::Gpu(message.to_string()) } else { AiError::Inference(message.to_string()) });
}

fn poll_cancel() {
    let cancelled = LIVE.with(|l| l.borrow().as_ref().and_then(|l| l.run.as_ref()).is_some_and(|r| (r.cancelled)()));
    if cancelled {
        kill(AiError::Cancelled);
    }
}

// ------------------------------------------------------------------ downloads (ai.models.*)

/// The AI Model Manager's store: Cache Storage, filled by the downloader worker (see the module
/// docs). Its methods act on the page's main thread (they return an error elsewhere).
pub struct WebModelStore {
    shared: Arc<Mutex<Shared>>,
}

impl WebModelStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Marks every catalog entry using `file` as cached or not.
fn set_cached(s: &mut Shared, file: &str, cached: bool) {
    for m in MODELS.iter().filter(|m| m.file == file) {
        s.cached.insert(m.id, cached);
    }
    if cached {
        s.outdated.retain(|f| *f != file);
    }
}

/// Asks `navigator.storage.estimate()` (at most every two seconds).
fn refresh_estimate(shared: &Arc<Mutex<Shared>>, force: bool) {
    let now = js_sys::Date::now();
    {
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if !force && now - s.estimate_asked_ms < 2000.0 {
            return;
        }
        s.estimate_asked_ms = now;
    }
    let shared = shared.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let est = async {
            let storage = Reflect::get(&web_sys::window()?.navigator(), &"storage".into()).ok().filter(|v| !v.is_undefined() && !v.is_null())?;
            let f: js_sys::Function = Reflect::get(&storage, &"estimate".into()).ok()?.dyn_into().ok()?;
            let p: js_sys::Promise = f.call0(&storage).ok()?.dyn_into().ok()?;
            let e = JsFuture::from(p).await.ok()?;
            let num = |k: &str| Reflect::get(&e, &k.into()).ok().and_then(|v| v.as_f64()).filter(|v| v.is_finite() && *v >= 0.0).map(|v| v as u64);
            Some((num("usage")?, num("quota")?))
        }
        .await;
        shared.lock().unwrap_or_else(PoisonError::into_inner).estimate = est;
    });
}

impl ModelStore for WebModelStore {
    fn available(&self) -> Result<(), String> {
        let s = self.lock();
        if s.models_base.is_none() {
            return Err("AI models are not hosted on this site yet".into());
        }
        if s.worker_url.is_none() {
            return Err(NO_WORKER.into());
        }
        Ok(())
    }

    fn status(&self, model: &ModelInfo) -> Status {
        let s = self.lock();
        if let Some(st) = s.queue.status(model) {
            return st;
        }
        if s.models_base.is_none() {
            return Status::Unavailable("AI models are not hosted on this site yet".into());
        }
        if s.outdated.contains(model.file) {
            return Status::UpdateAvailable;
        }
        match s.cached.get(model.id) {
            Some(true) => Status::Downloaded,
            Some(false) => Status::NotDownloaded,
            None => Status::Checking,
        }
    }

    fn download(&self, model: &'static ModelInfo) -> photocraft_ai::Result<()> {
        self.available().map_err(AiError::Download)?;
        if web_sys::window().is_none() {
            return Err(AiError::Download("downloads start from the page's main thread".into()));
        }
        self.lock().queue.enqueue(model);
        pump(&self.shared);
        Ok(())
    }

    fn cancel(&self, model: Option<&ModelInfo>) {
        let (c, urls) = {
            let mut s = self.lock();
            // What was kept of these downloads goes too.
            let urls: Vec<String> = match &s.models_base {
                Some(b) => MODELS.iter().filter(|m| model.is_none_or(|x| x.file == m.file)).map(|m| m.url(b)).collect(),
                None => Vec::new(),
            };
            (s.queue.cancel(model), urls)
        };
        if c == Cancelled::Active {
            kill_downloader();
        }
        if web_sys::window().is_some() {
            drop_parts(urls);
            pump(&self.shared);
        }
    }

    fn pause(&self, model: Option<&ModelInfo>) {
        let c = self.lock().queue.pause(model);
        if c == Cancelled::Active {
            // The downloader keeps the bytes in Cache Storage as they arrive.
            kill_downloader();
        }
        if web_sys::window().is_some() {
            pump(&self.shared);
        }
    }

    fn delete(&self, model: &ModelInfo) -> photocraft_ai::Result<()> {
        if web_sys::window().is_none() {
            return Err(AiError::Cache("models are deleted from the page's main thread".into()));
        }
        self.cancel(Some(model));
        let url = {
            let mut s = self.lock();
            s.queue.clear_failure(model.file);
            set_cached(&mut s, model.file, false);
            s.outdated.retain(|f| *f != model.file);
            s.models_base.clone().map(|b| model.url(&b))
        };
        let Some(url) = url else { return Ok(()) };
        let shared = self.shared.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Some(caches) = web_sys::window().and_then(|w| w.caches().ok())
                && let Ok(c) = JsFuture::from(caches.open(WEB_CACHE)).await
                && let Ok(c) = c.dyn_into::<web_sys::Cache>()
            {
                let _ = JsFuture::from(c.delete_with_str(&url)).await;
            }
            refresh_estimate(&shared, true);
        });
        Ok(())
    }

    fn storage(&self) -> Storage {
        if web_sys::window().is_some() {
            refresh_estimate(&self.shared, false);
        }
        let s = self.lock();
        let models = photocraft_ai::models::files().iter().filter(|m| s.cached.get(m.id) == Some(&true)).map(|m| m.bytes).sum();
        Storage { models: Some(models), used: s.estimate.map(|e| e.0), quota: s.estimate.map(|e| e.1) }
    }
}

/// Deletes the kept parts of unfinished downloads of `urls` (in the background).
fn drop_parts(urls: Vec<String>) {
    if urls.is_empty() {
        return;
    }
    wasm_bindgen_futures::spawn_local(async move {
        let Some(caches) = web_sys::window().and_then(|w| w.caches().ok()) else { return };
        let Ok(c) = JsFuture::from(caches.open(WEB_CACHE)).await else { return };
        let Ok(c) = c.dyn_into::<web_sys::Cache>() else { return };
        let Ok(keys) = JsFuture::from(c.keys()).await else { return };
        let Ok(keys) = keys.dyn_into::<Array>() else { return };
        for k in keys.iter() {
            let url = Reflect::get(&k, &"url".into()).ok().and_then(|u| u.as_string()).unwrap_or_default();
            if url.split_once(PART_QUERY).is_some_and(|(f, _)| urls.iter().any(|u| u == f)) {
                let _ = JsFuture::from(c.delete_with_str(&url)).await;
            }
        }
    });
}

/// The downloader worker (main thread only).
struct Downloader {
    worker: web_sys::Worker,
    ready: bool,
    /// The download message waiting for `ready`.
    queued: Option<Object>,
    _on_message: Closure<dyn FnMut(web_sys::MessageEvent)>,
    _on_error: Closure<dyn FnMut(web_sys::ErrorEvent)>,
}

thread_local! {
    static DOWNLOADER: RefCell<Option<Downloader>> = const { RefCell::new(None) };
    /// Handlers of terminated downloaders, dropped when the next one starts (never from inside
    /// one of their own invocations).
    static DL_RETIRED: RefCell<Vec<Downloader>> = const { RefCell::new(Vec::new()) };
}

/// Terminates the downloader (a cancelled download, an empty queue).
fn kill_downloader() {
    let d = DOWNLOADER.with(|d| d.borrow_mut().take());
    if let Some(d) = d {
        d.worker.set_onmessage(None);
        d.worker.set_onerror(None);
        d.worker.terminate();
        DL_RETIRED.with(|r| r.borrow_mut().push(d));
    }
}

/// Starts the next queued download when none runs (and frees the downloader when none is left).
fn pump(shared: &Arc<Mutex<Shared>>) {
    loop {
        let next = {
            let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
            if s.queue.active().is_some() {
                return;
            }
            s.queue.start_next().map(|file| (file, s.worker_url.clone(), s.models_base.clone()))
        };
        let Some((file, url, base)) = next else {
            kill_downloader();
            return;
        };
        let Some(model) = MODELS.iter().find(|m| m.file == file) else { continue };
        let (Some(url), Some(base)) = (url, base) else {
            shared.lock().unwrap_or_else(PoisonError::into_inner).queue.finish(file, Err("model downloads are not configured on this site".into()));
            continue;
        };
        let msg = Object::new();
        for (k, v) in [("type", JsValue::from("download")), ("model", model.id.into()), ("base", base.into())] {
            let _ = Reflect::set(&msg, &k.into(), &v);
        }
        match send_download(shared, &url, msg) {
            Ok(()) => return,
            Err(e) => {
                log::error!("AI downloader: {e}");
                shared.lock().unwrap_or_else(PoisonError::into_inner).queue.finish(file, Err(format!("couldn't start the download: {e}")));
            }
        }
    }
}

/// Sends a download message, starting the downloader first if needed.
fn send_download(shared: &Arc<Mutex<Shared>>, url: &str, msg: Object) -> Result<(), String> {
    if DOWNLOADER.with(|d| d.borrow().is_none()) {
        DL_RETIRED.with(|r| r.borrow_mut().clear());
        let (s1, s2) = (shared.clone(), shared.clone());
        let on_message = Closure::<dyn FnMut(web_sys::MessageEvent)>::new(move |e: web_sys::MessageEvent| on_download_message(&s1, e.data()));
        let on_error = Closure::<dyn FnMut(web_sys::ErrorEvent)>::new(move |e: web_sys::ErrorEvent| {
            e.prevent_default();
            downloader_failed(&s2, &format!("the download stopped: {}", e.message()));
        });
        let worker = module_worker(url, 0, &on_message, &on_error)?;
        DOWNLOADER.with(|d| *d.borrow_mut() = Some(Downloader { worker, ready: false, queued: None, _on_message: on_message, _on_error: on_error }));
    }
    let send = DOWNLOADER.with(|d| {
        let mut d = d.borrow_mut();
        let d = d.as_mut()?;
        if d.ready {
            Some(d.worker.clone())
        } else {
            d.queued = Some(msg.clone());
            None
        }
    });
    if let Some(w) = send {
        w.post_message(&msg).map_err(|e| format!("{e:?}"))?;
    }
    Ok(())
}

/// The active download failed outside the worker's own error reply (a crash): end it, go on.
fn downloader_failed(shared: &Arc<Mutex<Shared>>, why: &str) {
    log::error!("AI downloader: {why}");
    {
        let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(f) = s.queue.active() {
            s.queue.finish(f, Err(why.to_string()));
        }
    }
    kill_downloader();
    pump(shared);
}

fn on_download_message(shared: &Arc<Mutex<Shared>>, data: JsValue) {
    let ty = get(&data, "type").as_string().unwrap_or_default();
    let file = get(&data, "model").as_string().and_then(|id| photocraft_ai::catalog::model(&id)).map(|m| m.file);
    match ty.as_str() {
        "ready" => {
            let send = DOWNLOADER.with(|d| {
                let mut d = d.borrow_mut();
                let d = d.as_mut()?;
                d.ready = true;
                d.queued.take().map(|m| (d.worker.clone(), m))
            });
            if let Some((w, m)) = send
                && let Err(e) = w.post_message(&m)
            {
                downloader_failed(shared, &format!("{e:?}"));
            }
        }
        "dl-progress" => {
            let n = |k: &str| get(&data, k).as_f64().filter(|v| v.is_finite() && *v >= 0.0).map_or(0, |v| v as u64);
            if let Some(f) = file {
                shared.lock().unwrap_or_else(PoisonError::into_inner).queue.progress(f, n("done"), n("total"), js_sys::Date::now());
            }
        }
        "dl-done" => {
            if let Some(f) = file {
                let mut s = shared.lock().unwrap_or_else(PoisonError::into_inner);
                s.queue.finish(f, Ok(()));
                set_cached(&mut s, f, true);
            }
            refresh_estimate(shared, true);
            pump(shared);
        }
        "dl-error" => {
            let message = get(&data, "message").as_string().unwrap_or_else(|| "the download failed".into());
            if let Some(f) = file {
                shared.lock().unwrap_or_else(PoisonError::into_inner).queue.finish(f, Err(message));
            }
            pump(shared);
        }
        "panic" => {
            let message = get(&data, "message").as_string().unwrap_or_default();
            downloader_failed(shared, &format!("the downloader crashed: {message}"));
        }
        _ => {}
    }
}
