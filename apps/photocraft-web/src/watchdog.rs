//! Freeze watchdog: reports to the usage statistics every time the page's main thread was blocked
//! for at least 200 ms (`telemetry::FREEZE_MS`), with what was running then
//! ([`PhotocraftApp::freeze_context`]). Two views of the same thing: the browser's long tasks
//! (Chrome and Edge), and the app's own frames that took that long (every browser).

use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::telemetry::{self, FreezeSource};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/watchdog.js")]
extern "C" {
    fn wd_start();
    fn wd_take() -> js_sys::Array;
}

/// Frame timing for the watchdog.
#[derive(Default)]
pub struct Watchdog {
    frame_start: Option<f64>,
}

impl Watchdog {
    pub fn new() -> Self {
        wd_start();
        Self::default()
    }

    /// A frame begins (the shell's `logic`).
    pub fn begin(&mut self) {
        self.frame_start = Some(crate::frame_perf::now_ms());
    }

    /// The frame ended (the shell's `ui`): report it if it froze the page, and any long tasks
    /// the browser saw since.
    pub fn end(&mut self, app: &PhotocraftApp) {
        let frame = self.frame_start.take().map(|t| crate::frame_perf::now_ms() - t);
        let long: Vec<f64> = wd_take().iter().filter_map(|v| v.as_f64()).collect();
        if frame.is_none_or(|ms| ms < telemetry::FREEZE_MS) && long.is_empty() {
            return;
        }
        let ctx = app.freeze_context();
        if let Some(ms) = frame.filter(|ms| *ms >= telemetry::FREEZE_MS) {
            log::warn!("frame took {ms:.0} ms ({ctx:?})");
            telemetry::with(|r| r.freeze(FreezeSource::Frame, ms, &ctx));
        }
        for ms in long {
            telemetry::with(|r| r.freeze(FreezeSource::LongTask, ms, &ctx));
        }
    }
}
