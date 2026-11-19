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
    /// When the app's first frame began: long tasks that ended before it were the page's load.
    first_frame: Option<f64>,
}


impl Watchdog {
    pub fn new() -> Self {
        wd_start();
        Self::default()
    }

    /// A frame begins (the shell's `logic`).
    pub fn begin(&mut self) {
        let now = crate::frame_perf::now_ms();
        self.frame_start = Some(now);
        self.first_frame.get_or_insert(now);
        photocraft_ui_egui::perf_span::begin_frame();
    }

    /// The frame ended (the shell's `ui`): report it if it froze the page, and any long tasks
    /// the browser saw since.
    pub fn end(&mut self, app: &PhotocraftApp) {
        let now = crate::frame_perf::now_ms();
        let frame_start = self.frame_start.take();
        let frame = frame_start.map(|t| now - t);
        let raw: Vec<f64> = wd_take().iter().filter_map(|v| v.as_f64()).collect();
        let long: Vec<(f64, f64)> = raw.chunks_exact(2).map(|p| (p[0], p[1])).collect();
        if frame.is_none_or(|ms| ms < telemetry::FREEZE_MS) && long.is_empty() {
            return;
        }
        let mut ctx = app.freeze_context();
        // Most of the frame outside every span: say so rather than name a small part of it.
        if let Some(ms) = frame {
            let spans = photocraft_ui_egui::perf_span::frame();
            let covered: f64 = spans.iter().map(|s| s.1).sum();
            if ms - covered > spans.first().map_or(0.0, |s| s.1) {
                ctx.phase = Some("other");
            }
        }
        if let Some(ms) = frame.filter(|ms| *ms >= telemetry::FREEZE_MS) {
            let spans: Vec<String> = photocraft_ui_egui::perf_span::frame().iter().take(8).map(|(n, t)| format!("{n} {t:.0}")).collect();
            log::warn!("frame took {ms:.0} ms ({ctx:?}) spans: {}", spans.join(", "));
            telemetry::with(|r| r.freeze(FreezeSource::Frame, ms, &ctx));
        }
        for (start, ms) in long {
            let mut c = ctx.clone();
            c.phase = telemetry::long_task_phase(start, ms, self.first_frame, frame_start, now, ctx.phase);
            telemetry::with(|r| r.freeze(FreezeSource::LongTask, ms, &c));
        }
    }
}

