//! Capishot's host contract example: a tiny egui app that embeds Capishot through `capishot-host`
//! the way an outside host does (its own Cargo workspace, native and wasm32). If this builds and
//! runs, a host written the same way does too.
//!
//! - [`script`]: the contract's steps against the engine API alone (open a PSD from bytes, set a
//!   tool, Remove with AI, undo, add a text, save), shared by the app and `tests/contract.rs`.
//! - [`app`]: the eframe app (host-drawn layer list and buttons, Capishot's canvas).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod app;
pub mod script;
mod view;

pub use app::HostApp;

/// The browser entry (the binary's `main` on wasm32): starts the app on the page's
/// `<canvas id="host_canvas">` (index.html).
#[cfg(target_arch = "wasm32")]
pub fn start_web() {
    use wasm_bindgen::JsCast;

    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    wasm_bindgen_futures::spawn_local(async {
        let canvas = web_sys::window()
            .and_then(|w| w.document())
            .and_then(|d| d.get_element_by_id("host_canvas"))
            .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok());
        let Some(canvas) = canvas else {
            log::error!("no <canvas id=\"host_canvas\"> on the page");
            return;
        };
        let started = eframe::WebRunner::new()
            .start(canvas, eframe::WebOptions::default(), Box::new(|cc| Ok(Box::new(HostApp::new(cc)))))
            .await;
        if let Err(e) = started {
            log::error!("the app did not start: {e:?}");
        }
    });
}
