//! The app's side of the module split (packaging/web-split/loader.js): the page loads part of
//! the module after the first frame, and `self.capishotSplit` says what has arrived. Commands
//! that need the rest wait for it on a job card (`photocraft_ui_egui::code_gate`) instead of the
//! page fetching it synchronously. Without the split (a debug build, the embed of an unsplit
//! build) there is no `capishotSplit`, and everything is ready.

use js_sys::{Function, Promise, Reflect};
use photocraft_ui_egui::code_gate::{CodeDone, CodeWait};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;

fn api() -> Option<JsValue> {
    let api = Reflect::get(&js_sys::global(), &JsValue::from_str("capishotSplit")).ok()?;
    (!api.is_undefined() && !api.is_null()).then_some(api)
}

fn method(api: &JsValue, name: &str) -> Option<Function> {
    Reflect::get(api, &JsValue::from_str(name)).ok()?.dyn_into::<Function>().ok()
}

/// Whether command `id` can run now.
fn ready(id: &str) -> bool {
    let Some(api) = api() else { return true };
    let Some(f) = method(&api, "ready") else { return true };
    f.call1(&api, &JsValue::from_str(id)).map(|v| v.as_bool() != Some(false)).unwrap_or(true)
}

/// Every chunk, with progress on the job.
fn wait(ctx: photocraft_engine::jobs::JobCtx, done: CodeDone) {
    let Some((api, f)) = api().and_then(|api| method(&api, "whenLoaded").map(|f| (api, f))) else {
        done(Ok(()));
        return;
    };
    let progress_ctx = ctx.clone();
    let on_progress = Closure::<dyn FnMut(f64)>::new(move |fraction: f64| {
        progress_ctx.progress(fraction as f32, "");
    });
    let promise = f.call1(&api, on_progress.as_ref()).ok().and_then(|p| p.dyn_into::<Promise>().ok());
    wasm_bindgen_futures::spawn_local(async move {
        let r = match promise {
            Some(p) => JsFuture::from(p).await.map(|_| ()).map_err(|e| {
                let msg = e.dyn_ref::<js_sys::Error>().map(|e| String::from(e.message())).unwrap_or_else(|| format!("{e:?}"));
                if msg.is_empty() { "network".to_string() } else { msg }
            }),
            None => Ok(()),
        };
        drop(on_progress);
        // Cancelled (Esc): the job is gone, nothing to report.
        if !ctx.cancelled() {
            done(r);
        }
    });
}

/// The loader for [`photocraft_ui_egui::Services::code_wait`].
pub fn code_wait() -> CodeWait {
    CodeWait { ready: Box::new(ready), wait: Box::new(wait) }
}

/// The app drew its first frame: the loader compiles and instantiates the chunks now.
pub fn first_frame() {
    if let Some(api) = api()
        && let Some(f) = method(&api, "firstFrame")
    {
        let _ = f.call0(&api);
    }
}
