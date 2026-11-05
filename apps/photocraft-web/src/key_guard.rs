//! Held keys the browser never releases.
//!
//! egui forgets its held keys when `Event::WindowFocused(false)` arrives, but eframe's web runner
//! sends that only when the canvas stops being the document's active element or the page hides.
//! Alt+Tab, the browser's own UI (a sidebar, a mouse gesture) or the window's system menu take the
//! keyboard and leave the canvas active, so the `keyup` of a key down at that moment goes nowhere:
//! a Space stays the Hand tool and a tool letter stays sprung until the key is pressed again, and
//! no tool does what it should meanwhile. This module sees the window lose focus itself and hands
//! egui the event at the start of the next frame.
//!
//! The modifiers are the pointer's own at each press, from the capture phase: eframe only sends
//! the ones that differ from what it last sent, and the event above resets egui's without it knowing.

use std::cell::Cell;

use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;

thread_local! {
    /// The window lost focus or the page was hidden since the last frame.
    static LOST: Cell<bool> = const { Cell::new(false) };
    /// The modifiers of a press since the last frame.
    static PRESS: Cell<Option<egui::Modifiers>> = const { Cell::new(None) };
}

/// Install the window listeners. Call once, before eframe's runner starts.
pub fn install() {
    let Some(window) = web_sys::window() else { return };

    let on_lost = Closure::<dyn FnMut(web_sys::Event)>::new(|_e: web_sys::Event| LOST.with(|l| l.set(true)));
    for kind in ["blur", "pagehide"] {
        let _ = window.add_event_listener_with_callback(kind, on_lost.as_ref().unchecked_ref());
    }
    on_lost.forget();

    if let Some(document) = window.document() {
        let hidden_doc = document.clone();
        let on_visibility = Closure::<dyn FnMut(web_sys::Event)>::new(move |_e: web_sys::Event| {
            if hidden_doc.hidden() {
                LOST.with(|l| l.set(true));
            }
        });
        if document.add_event_listener_with_callback("visibilitychange", on_visibility.as_ref().unchecked_ref()).is_ok() {
            on_visibility.forget();
        }
    }

    let opts = web_sys::AddEventListenerOptions::new();
    opts.set_capture(true);
    let on_press = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(|e: web_sys::MouseEvent| {
        let command = e.ctrl_key() || e.meta_key();
        let modifiers = egui::Modifiers { alt: e.alt_key(), ctrl: e.ctrl_key(), shift: e.shift_key(), mac_cmd: e.meta_key(), command };
        PRESS.with(|p| p.set(Some(modifiers)));
    });
    if window.add_event_listener_with_callback_and_add_event_listener_options("pointerdown", on_press.as_ref().unchecked_ref(), &opts).is_ok() {
        on_press.forget();
    }
}

/// Put what the browser said since the last frame ahead of `raw`'s events (`App::raw_input_hook`):
/// keys are released before a press they would otherwise turn into another tool.
pub fn restore(raw: &mut egui::RawInput) {
    if let Some(modifiers) = PRESS.with(Cell::take) {
        raw.events.insert(0, egui::Event::ModifiersChanged(modifiers));
    }
    if LOST.with(|l| l.replace(false)) {
        raw.events.insert(0, egui::Event::WindowFocused(false));
    }
}
