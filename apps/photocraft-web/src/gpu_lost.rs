//! The graphics context, lost and avoided.
//!
//! The window (egui and the canvas) draws through one WebGL or WebGPU context. When the browser
//! drops it (`webglcontextlost`: a weak GPU that took seconds to build shaders, Windows resetting
//! the driver) nothing can draw any more, and eframe can't make a new one: the page went black
//! for good. Now the loss is caught ([`install`]): unsaved documents are written to crash
//! recovery, and the page reloads on the CPU canvas (`?cpu=1`), where they reopen and a notice
//! says what happened. A second loss in the same tab (or a canvas pipeline that took over 2 s,
//! `gpu_status::warm_up`) keeps this device on the CPU canvas (Preferences › Performance ›
//! Rendering), with a notice saying how to undo it. A fresh visit tries the GPU again unless that
//! preference is set.
//!
//! [`weak_start`]: a WebGL renderer known to be too weak (software rendering, old Intel HD/GMA,
//! textures under 8192 px) starts on the CPU canvas.

use std::sync::atomic::{AtomicBool, Ordering};

use js_sys::{Function, Reflect};
use wasm_bindgen::prelude::*;

/// Set by the `webglcontextlost` listener; the next frame handles it.
static LOST: AtomicBool = AtomicBool::new(false);

/// Times the context was lost in this tab (it survives the reload).
const COUNT_KEY: &str = "capishot.gpuLost";
/// What the reloaded page tells the user: "lost" (this time) or "kept" (CPU from now on).
const NOTICE_KEY: &str = "capishot.gpuLostNotice";

/// Listens for the canvas's context loss: the event's default is prevented (so the browser may
/// give the context back) and [`lost`] is set.
pub fn install(canvas: &web_sys::HtmlCanvasElement) {
    let cb = Closure::<dyn FnMut(web_sys::Event)>::new(move |e: web_sys::Event| {
        e.prevent_default();
        LOST.store(true, Ordering::Release);
        log::error!("photocraft-web: the graphics context was lost");
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

/// The loss's bookkeeping, once: the counter, the usage event, the notice for the reloaded page,
/// and on the second loss the CPU canvas from now on. Returns the count.
pub fn note(app: &mut photocraft_ui_egui::PhotocraftApp) -> u32 {
    let n = session().and_then(|s| s.get_item(COUNT_KEY).ok().flatten()).and_then(|v| v.parse::<u32>().ok()).unwrap_or(0) + 1;
    if let Some(s) = session() {
        let _ = s.set_item(COUNT_KEY, &n.to_string());
        let _ = s.set_item(NOTICE_KEY, if n >= 2 { "kept" } else { "lost" });
    }
    photocraft_ui_egui::gpu_status::record_gpu_lost(app, "context_lost", n, None);
    if n >= 2 {
        let _ = app.run("prefs.set", serde_json::json!({"values": {"performance.renderingMode": "cpu"}}));
    }
    n
}

/// Reloads the page on the CPU canvas.
pub fn reload_on_cpu() {
    let Some(loc) = web_sys::window().map(|w| w.location()) else { return };
    let Ok(href) = loc.href() else { return };
    let _ = loc.replace(&photocraft_ui_egui::gpu_status::cpu_url(&href));
}

/// After a reload for a lost context: the notice to show (once).
pub fn take_notice() -> Option<&'static str> {
    let s = session()?;
    let v = s.get_item(NOTICE_KEY).ok().flatten()?;
    let _ = s.remove_item(NOTICE_KEY);
    match v.as_str() {
        "lost" => Some("lost"),
        "kept" => Some("kept"),
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
