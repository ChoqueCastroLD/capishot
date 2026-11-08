//! The graphics context, lost and avoided.
//!
//! The window (egui and the canvas) draws through one WebGL or WebGPU context. When the browser
//! drops it (`webglcontextlost`: a weak GPU that took seconds to build shaders, Windows resetting
//! the driver; a WebGPU device lost, as phones short of memory do, which closed the page) nothing
//! can draw any more, and eframe can't make a new one: the page went black for good. Now the loss
//! is caught ([`install`], `gpu_status::check_window`): every open document is written to crash
//! recovery, then the page asks ([`ask_reload`]) whether to reload with the graphics card again
//! (it usually comes back) or on the CPU canvas (`?cpu=1`, this visit only: slower drawing; the
//! AI models run in their own worker either way). The documents reopen and a notice says what
//! happened. Nothing is switched to the CPU for good: Preferences › Performance › Rendering does
//! that.
//!
//! [`weak_start`]: a WebGL renderer known to be too weak (software rendering, old Intel HD/GMA,
//! textures under 8192 px) starts on the CPU canvas.

use std::sync::atomic::{AtomicBool, Ordering};

use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;

/// Set by the `webglcontextlost` listener; the next frame handles it.
static LOST: AtomicBool = AtomicBool::new(false);
/// The reload question is on the page.
static ASKED: AtomicBool = AtomicBool::new(false);

/// Times the context was lost in this tab (it survives the reload).
const COUNT_KEY: &str = "capishot.gpuLost";
/// What the reloaded page tells the user: "gpu" or "cpu" (how the person chose to reload).
const NOTICE_KEY: &str = "capishot.gpuLostNotice";

/// Listens for the canvas's context loss: the event's default is prevented (so the browser may
/// give the context back) and [`lost`] is set.
pub fn install(canvas: &web_sys::HtmlCanvasElement) {
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        e.prevent_default();
        LOST.store(true, Ordering::Release);
        log::error!("photocraft-web: the graphics context was lost");
        // Now: the reload that follows doesn't wait for a statistics flush.
        let stage = photocraft_ui_egui::telemetry::stage_name();
        crate::usage_stats::send_crash(&photocraft_ui_egui::telemetry::crash_event("gpu_lost", None, None, Some("context_lost"), Some(stage), Some(crate::breadcrumb::memory_bytes() >> 20)));
    });
    let _ = canvas.add_event_listener_with_callback("webglcontextlost", cb.as_ref().unchecked_ref());
    cb.forget();
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |_: web_sys::Event| log::info!("photocraft-web: the graphics context is back"));
    let _ = canvas.add_event_listener_with_callback("webglcontextrestored", cb.as_ref().unchecked_ref());
    cb.forget();
}

/// The context was lost: nothing can be drawn any more.
pub fn lost() -> bool {
    LOST.load(Ordering::Acquire)
}

fn session() -> Option<web_sys::Storage> {
    web_sys::window()?.session_storage().ok().flatten()
}

/// The loss's bookkeeping, once: the counter (losses in this tab) and the usage event (`cause`:
/// "context_lost" for WebGL, "device_lost" for WebGPU). Returns the count.
pub fn note(app: &mut photocraft_ui_egui::PhotocraftApp, cause: &str) -> u32 {
    let n = losses() + 1;
    if let Some(s) = session() {
        let _ = s.set_item(COUNT_KEY, &n.to_string());
    }
    photocraft_ui_egui::gpu_status::record_gpu_lost(app, cause, n, None);
    n
}

fn losses() -> u32 {
    session().and_then(|s| s.get_item(COUNT_KEY).ok().flatten()).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0)
}

/// Reloads the page, on the CPU canvas (`cpu`) or with the graphics card, and tells the reloaded
/// page which ([`take_notice`]).
fn reload(cpu: bool) {
    let Some(loc) = web_sys::window().map(|w| w.location()) else { return };
    let Ok(href) = loc.href() else { return };
    if let Some(s) = session() {
        let _ = s.set_item(NOTICE_KEY, if cpu { "cpu" } else { "gpu" });
    }
    let url = if cpu { photocraft_ui_egui::gpu_status::cpu_url(&href) } else { photocraft_ui_egui::gpu_status::gpu_url(&href) };
    let _ = loc.replace(&url);
}

/// Asks, over the page (the window can't draw any more), whether to reload with the graphics card
/// or on the CPU. `saved`: every document reached crash recovery. A tab that lost it more than
/// once suggests the CPU. Once.
pub fn ask_reload(saved: bool) {
    if ASKED.swap(true, Ordering::AcqRel) {
        return;
    }
    let t = photocraft_ui_egui::i18n::t;
    let again = losses() >= 2;
    let Some(doc) = web_sys::window().and_then(|w| w.document()) else { return reload(again) };
    let Some(body) = doc.body() else { return reload(again) };
    let Ok(el) = doc.create_element("div") else { return reload(again) };
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let button = |id: &str, label: &str, main: bool| {
        format!(
            "<button id=\"{id}\" style=\"display:block;width:100%;margin-top:10px;padding:12px;font:inherit;border-radius:8px;cursor:pointer;\
             border:1px solid #5a8dee;background:{};color:{}\">{}</button>",
            if main { "#3b6fd8" } else { "transparent" },
            if main { "#fff" } else { "#cfd8e6" },
            esc(label)
        )
    };
    let (gpu, cpu) = (t("Reload with the graphics card"), t("Reload on the CPU (slower drawing)"));
    el.set_inner_html(&format!(
        "<div style=\"max-width:420px;margin:12vh auto;padding:24px;border-radius:12px;background:#23272f;color:#e6e9ef;\
         font:15px/1.45 system-ui,sans-serif;box-shadow:0 8px 40px #000a\"><h2 style=\"margin:0 0 10px;font-size:18px\">{}</h2><p style=\"margin:0 0 6px\">{}</p>{}{}{}</div>",
        esc(t("The graphics card stopped responding")),
        esc(if saved { t("Your documents are saved and will reopen as they were.") } else { t("Your documents will reopen from their last autosave.") }),
        if again { format!("<p style=\"margin:6px 0 0;color:#f0c674\">{}</p>", esc(t("It stopped again in this tab: the CPU is the safer choice."))) } else { String::new() },
        button("capishot-reload-gpu", gpu, !again),
        button("capishot-reload-cpu", cpu, again),
    ));
    let _ = el.set_attribute("style", "position:fixed;inset:0;z-index:2147483647;background:#000c;overflow:auto");
    let _ = body.append_child(&el);
    for (id, on_cpu) in [("capishot-reload-gpu", false), ("capishot-reload-cpu", true)] {
        if let Some(b) = doc.get_element_by_id(id) {
            let cb = Closure::<dyn FnMut()>::new(move || reload(on_cpu));
            let _ = b.add_event_listener_with_callback("click", cb.as_ref().unchecked_ref());
            cb.forget();
        }
    }
}

/// After a reload for a lost context: the notice to show (once).
pub fn take_notice() -> Option<&'static str> {
    let s = session()?;
    let v = s.get_item(NOTICE_KEY).ok().flatten()?;
    let _ = s.remove_item(NOTICE_KEY);
    match v.as_str() {
        "gpu" => Some("gpu"),
        "cpu" => Some("cpu"),
        _ => None,
    }
}

/// The WebGL renderer's name (unmasked when the browser allows) and its largest texture side,
/// from a throwaway context.
pub fn webgl_renderer() -> Option<(String, u32)> {
    let doc = web_sys::window()?.document()?;
    let canvas: web_sys::HtmlCanvasElement = doc.create_element("canvas").ok()?.dyn_into().ok()?;
    let gl = canvas.get_context("webgl2").ok()??;
    let call = |obj: &JsValue, name: &str, arg: &JsValue| -> Option<JsValue> {
        let f: Function = Reflect::get(obj, &name.into()).ok()?.dyn_into().ok()?;
        f.call1(obj, arg).ok()
    };
    let gl: JsValue = gl.into();
    let renderer = call(&gl, "getExtension", &"WEBGL_debug_renderer_info".into())
        .filter(|e| !e.is_null())
        .and_then(|_| call(&gl, "getParameter", &JsValue::from(0x9246)))
        .and_then(|v| v.as_string())
        .or_else(|| call(&gl, "getParameter", &JsValue::from(0x1F01)).and_then(|v| v.as_string()))
        .unwrap_or_default();
    let max = call(&gl, "getParameter", &JsValue::from(0x0D33)).and_then(|v| v.as_f64()).unwrap_or(0.0) as u32;
    // Free the throwaway context now rather than when it is collected.
    if let Some(lose) = call(&gl, "getExtension", &"WEBGL_lose_context".into()).filter(|e| !e.is_null()) {
        let _ = call(&lose, "loseContext", &JsValue::UNDEFINED);
    }
    Some((renderer, max))
}
