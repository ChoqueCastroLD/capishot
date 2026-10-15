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
//! Without the worker the command is unavailable; without a models URL only models already in
//! the browser's Cache Storage run. The worker is a module worker started from a one-line
//! `blob:` bootstrap that imports the wasm-bindgen glue; it stays alive between runs (the model
//! stays on the GPU) and is terminated to cancel. Messages: see the worker's module docs.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};

use js_sys::{Array, Float32Array, Object, Reflect};
use photocraft_ai::cache::WEB_CACHE;
use photocraft_ai::catalog::ModelInfo;
use photocraft_ai::service::{Done, Hooks, ModelState, Request, Upscaler};
use photocraft_ai::{AiError, Planar};
use wasm_bindgen::prelude::*;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// What the page configured plus what we learned (shared with the engine's job thread).
#[derive(Default)]
struct Shared {
    worker_url: Option<String>,
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

/// The upscaler for this page, if it names an AI worker.
pub fn from_page() -> Option<Arc<dyn Upscaler>> {
    let worker_url = meta_url("capishot-ai-worker")?;
    let models_base = meta_url("capishot-ai-models").map(|u| if u.ends_with('/') { u } else { format!("{u}/") });
    let shared = Shared { worker_url: Some(worker_url), models_base, ..Default::default() };
    Some(Arc::new(WebUpscaler { shared: Arc::new(Mutex::new(shared)) }))
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

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        let (url, base, cpu) = {
            let s = self.lock();
            (s.worker_url.clone(), s.models_base.clone().unwrap_or_default(), s.cpu_only)
        };
        let Some(url) = url else { return done(Err(AiError::Device("the AI worker isn't part of this build".into()))) };
        let msg = run_message(&req, &base, cpu);
        let run = Run { model: req.model.id, done: Some(done), hooks, shared: self.shared.clone(), started_gpu: false };
        if let Err(e) = start(&url, msg, run) {
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
    let buffer: JsValue = data.buffer().into();
    set("data", data.into());
    (o, buffer)
}

/// The run in flight.
struct Run {
    model: &'static str,
    done: Option<Done>,
    hooks: Arc<dyn Hooks>,
    shared: Arc<Mutex<Shared>>,
    /// The worker reported it was starting the device (a crash after this is a GPU problem).
    started_gpu: bool,
}

impl Run {
    fn finish(&mut self, r: photocraft_ai::Result<Planar>) {
        if let Some(done) = self.done.take() {
            done(r);
        }
    }
}

/// The live worker, its handlers and the run it serves (main thread only).
struct Live {
    worker: web_sys::Worker,
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
}

fn start(url: &str, msg: (Object, JsValue), run: Run) -> Result<(), String> {
    let busy = LIVE.with(|l| l.borrow().as_ref().is_some_and(|l| l.run.is_some()));
    if busy {
        let mut run = run;
        run.finish(Err(AiError::Inference("another AI upscale is still running".into())));
        return Ok(());
    }
    let exists = LIVE.with(|l| l.borrow().is_some());
    if !exists && let Err(e) = spawn(url) {
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
/// glue's URL directly: it must call its `init`).
fn spawn(url: &str) -> Result<(), String> {
    let js = |e: JsValue| format!("{e:?}");
    let src = format!("import init from {};\nawait init();\n", serde_json::Value::String(url.to_string()));
    let parts = Array::of1(&JsValue::from_str(&src));
    let opts = web_sys::BlobPropertyBag::new();
    opts.set_type("text/javascript");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &opts).map_err(js)?;
    let blob_url = web_sys::Url::create_object_url_with_blob(&blob).map_err(js)?;
    let wopts = web_sys::WorkerOptions::new();
    wopts.set_type(web_sys::WorkerType::Module);
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
        *l.borrow_mut() = Some(Live { worker, ready: false, queued: None, run: None, timer: None, _on_message: on_message, _on_error: on_error, _tick: tick });
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
                    run.shared.lock().unwrap_or_else(PoisonError::into_inner).device = device.clone();
                }
                run.hooks.progress(fraction, &status);
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
    live.worker.terminate();
    if let (Some(t), Some(w)) = (live.timer.take(), web_sys::window()) {
        w.clear_interval_with_handle(t);
    }
    if let Some(mut run) = live.run.take() {
        run.finish(Err(why));
    }
}

fn crashed(message: &str) {
    log::error!("{message}");
    with_run(|run| {
        // Crashed while bringing up WebGPU: use the CPU from now on.
        if run.started_gpu && run.shared.lock().unwrap_or_else(PoisonError::into_inner).device.is_empty() {
            run.shared.lock().unwrap_or_else(PoisonError::into_inner).cpu_only = true;
        }
    });
    kill(AiError::Inference(message.to_string()));
}

fn poll_cancel() {
    let cancelled = LIVE.with(|l| l.borrow().as_ref().and_then(|l| l.run.as_ref()).is_some_and(|r| r.hooks.cancelled()));
    if cancelled {
        kill(AiError::Cancelled);
    }
}
