//! Ctrl + wheel in the browser.
//!
//! eframe's web runner learns that Ctrl is down only from key events on its focused canvas. A
//! Ctrl + wheel it receives while it believes Ctrl is up is, to it, a trackpad pinch (browsers
//! send a pinch as a wheel event with `ctrlKey` set), so it becomes `Event::Zoom` at a pinch
//! speed: ×2.7 for one Chrome mouse notch. This module watches the real Ctrl key on the whole
//! window, records each real Ctrl + wheel event before eframe sees it, and
//! [`restore`] swaps eframe's guesses for them at the start of the next frame, where the canvas
//! zooms them smoothly (`photocraft_ui_egui::wheel_nav`).
//!
//! It also keeps the browser's own page zoom from ever triggering over Capishot: Ctrl + wheel (and
//! a pinch) and Ctrl/⌘ + `=`, `+`, `-`, `0` are `preventDefault`ed on the window, in the capture
//! phase, with a non-passive listener (window wheel listeners are passive by default).

use std::cell::{Cell, RefCell};

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

thread_local! {
    /// The Ctrl key is really held (from key and pointer events, which carry the true state).
    static CTRL: Cell<bool> = const { Cell::new(false) };
    /// Real Ctrl + wheel events since the last frame, in order.
    static RECORDED: RefCell<Vec<egui::Event>> = const { RefCell::new(Vec::new()) };
}

/// Install the window listeners. Call once, before eframe's runner starts.
pub fn install() {
    let Some(window) = web_sys::window() else { return };
    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(true);
    opts.set_passive(false);

    let on_wheel = Closure::<dyn FnMut(web_sys::WheelEvent)>::new(|e: web_sys::WheelEvent| {
        if !e.ctrl_key() {
            return;
        }
        // Never the browser's page zoom.
        e.prevent_default();
        if !CTRL.with(Cell::get) {
            // A pinch: eframe's own `Event::Zoom` is right for it.
            return;
        }
        let unit = match e.delta_mode() {
            web_sys::WheelEvent::DOM_DELTA_PIXEL => egui::MouseWheelUnit::Point,
            web_sys::WheelEvent::DOM_DELTA_LINE => egui::MouseWheelUnit::Line,
            web_sys::WheelEvent::DOM_DELTA_PAGE => egui::MouseWheelUnit::Page,
            _ => return,
        };
        let delta = -egui::vec2(e.delta_x() as f32, e.delta_y() as f32);
        if !(delta.x.is_finite() && delta.y.is_finite()) {
            return;
        }
        let modifiers = egui::Modifiers { alt: e.alt_key(), ctrl: true, shift: e.shift_key(), mac_cmd: e.meta_key(), command: true };
        RECORDED.with(|r| {
            let mut r = r.borrow_mut();
            // A runaway queue (frames stalled) is dropped rather than grown without bound.
            if r.len() < 1024 {
                r.push(egui::Event::MouseWheel { source: egui::MouseWheelSource::Unknown, unit, delta, phase: egui::TouchPhase::Move, modifiers });
            }
        });
    });
    if window.add_event_listener_with_callback_and_add_event_listener_options("wheel", on_wheel.as_ref().unchecked_ref(), &opts).is_ok() {
        on_wheel.forget();
    }

    let on_key = Closure::<dyn FnMut(web_sys::KeyboardEvent)>::new(|e: web_sys::KeyboardEvent| {
        CTRL.with(|c| c.set(e.ctrl_key()));
        let page_zoom_key = matches!(e.key().as_str(), "=" | "+" | "-" | "_" | "0");
        if e.type_() == "keydown" && (e.ctrl_key() || e.meta_key()) && page_zoom_key {
            // Zoom In / Zoom Out / Fit on Screen are the canvas's; the page never zooms.
            e.prevent_default();
        }
    });
    for kind in ["keydown", "keyup"] {
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(kind, on_key.as_ref().unchecked_ref(), &opts);
    }
    on_key.forget();

    let on_pointer = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(|e: web_sys::MouseEvent| CTRL.with(|c| c.set(e.ctrl_key())));
    for kind in ["pointermove", "pointerdown", "pointerup"] {
        let _ = window.add_event_listener_with_callback_and_add_event_listener_options(kind, on_pointer.as_ref().unchecked_ref(), &opts);
    }
    on_pointer.forget();

    let on_blur = Closure::<dyn FnMut(web_sys::Event)>::new(|_e: web_sys::Event| CTRL.with(|c| c.set(false)));
    if window.add_event_listener_with_callback("blur", on_blur.as_ref().unchecked_ref()).is_ok() {
        on_blur.forget();
    }
}

/// Put the real Ctrl + wheel events recorded since the last frame back into `raw`
/// (`App::raw_input_hook`).
pub fn restore(raw: &mut egui::RawInput) {
    let recorded = RECORDED.with(|r| std::mem::take(&mut *r.borrow_mut()));
    if !recorded.is_empty() {
        photocraft_ui_egui::wheel_nav::restore_ctrl_wheel(&mut raw.events, recorded);
    }
}
