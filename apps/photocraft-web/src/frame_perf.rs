//! Frame timing in the browser: how long each frame keeps the page's main thread busy while the
//! canvas is worked (a tool dragged on it, the view zoomed or scrolled), and how long a pointer
//! move waits until a frame that consumed it is painted.
//!
//! - Always: per-tool percentiles go to the anonymous usage statistics (`frames` events,
//!   [`photocraft_ui_egui::telemetry::Recorder::frame`]) when they are on.
//! - With `?perf=1` in the page URL: a console line per gesture (`capishot-perf {json}`) and
//!   `photocraftPerf.take()`, which returns (as JSON) every frame recorded since the last call,
//!   for scripted measurements.
//!
//! A frame's time is eframe's own measure (`frame.info().cpu_usage`: the app's update, egui's
//! tessellation and the painter, i.e. the WebGL/WebGPU command submission), reported one frame
//! later; `upd` splits off the app's update (`logic` + `ui`). With `?perf=1`, `gpu` is how long
//! the GPU still worked after the update (submit to `onSubmittedWorkDone` of everything queued
//! by then: the frame's document refresh and the previous frame's paint): a GPU-bound page
//! shows it next to long `int` (`requestAnimationFrame` interval) and dropped frames.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::telemetry;
use serde_json::json;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast as _, JsValue};

/// Frames kept for `photocraftPerf.take()` (`?perf=1` only); older ones are dropped.
const MAX_RECORDS: usize = 20_000;

/// High-resolution time in milliseconds, on the same epoch on every thread (`timeOrigin + now()`):
/// the clock [`photocraft_ui_egui::gpu_canvas::now_ms`] reads on the web.
pub fn now_ms() -> f64 {
    thread_local! {
        static PERFORMANCE: Option<(web_sys::Performance, f64)> = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("performance"))
            .ok()
            .and_then(|p| p.dyn_into::<web_sys::Performance>().ok())
            .map(|p| {
                let origin = p.time_origin();
                (p, origin)
            });
    }
    PERFORMANCE.with(|p| p.as_ref().map_or_else(js_sys::Date::now, |(p, origin)| origin + p.now()))
}

/// The page's own clock (`performance.now()`, the epoch of `Event.timeStamp`).
fn page_now() -> f64 {
    web_sys::window().and_then(|w| w.performance()).map_or(0.0, |p| p.now())
}

/// A frame of the gesture being reported (`?perf=1`).
struct GestureFrame {
    ms: f64,
    update_ms: f64,
    latency_ms: Option<f64>,
    gpu_ms: Option<f64>,
    interval_ms: Option<f64>,
}

/// One display refresh at 60 Hz (dropped frames are counted against it).
const VSYNC_MS: f64 = 1000.0 / 60.0;

/// Frames a record waits for its GPU completion before it is written without one.
const GPU_WAIT_FRAMES: u8 = 8;

/// Milliseconds from the end of a frame's update until the GPU finished what was queued then
/// (`f64` bits; `u64::MAX` while pending).
type GpuFence = Arc<AtomicU64>;

/// One frame, from its start until eframe reports its time (in the next frame).
#[derive(Clone, Debug)]
struct Frame {
    /// Start of the frame (`performance.now()`).
    start: f64,
    /// Since the previous frame's start.
    interval: Option<f64>,
    /// The app's update: `logic` + `ui`.
    update_ms: f64,
    /// Timestamp of the oldest pointer move this frame consumed.
    input: Option<f64>,
    /// What the canvas was doing: a tool id (`tool.Brush`) or `view`.
    key: Option<String>,
    megapixels: Option<f64>,
    /// `?perf=1`: GPU completion of the work queued by the end of the update.
    gpu: Option<GpuFence>,
}

pub struct FramePerf {
    debug: bool,
    pending_input: Rc<Cell<Option<f64>>>,
    current: Option<Frame>,
    last: Option<Frame>,
    last_start: Option<f64>,
    /// eframe's last reported frame time: a frame whose logic ran in an event handler reports
    /// none, so an unchanged value is not counted again.
    last_cpu: Option<f32>,
    records: Rc<RefCell<Vec<serde_json::Value>>>,
    /// The current gesture's frames (`?perf=1`).
    gesture: Vec<GestureFrame>,
    gesture_key: Option<String>,
    /// Finished frames waiting for their GPU completion (`?perf=1`), with eframe's time and age.
    awaiting_gpu: VecDeque<(Frame, Option<f64>, u8)>,
}

impl FramePerf {
    /// Listens for pointer moves on `canvas`; `debug` is `?perf=1`.
    pub fn new(canvas: &web_sys::HtmlCanvasElement, debug: bool) -> Self {
        let pending_input: Rc<Cell<Option<f64>>> = Rc::default();
        let pending = pending_input.clone();
        let cb = Closure::<dyn FnMut(web_sys::PointerEvent)>::new(move |e: web_sys::PointerEvent| {
            let t = e.time_stamp();
            pending.set(Some(pending.get().map_or(t, |p| p.min(t))));
        });
        if canvas.add_event_listener_with_callback("pointermove", cb.as_ref().unchecked_ref()).is_ok() {
            cb.forget();
        }
        let records: Rc<RefCell<Vec<serde_json::Value>>> = Rc::default();
        if debug {
            expose(records.clone());
            log::info!("photocraft-web: frame timing on (?perf=1): photocraftPerf.take()");
        }
        Self {
            debug,
            pending_input,
            current: None,
            last: None,
            last_start: None,
            last_cpu: None,
            records,
            gesture: Vec::new(),
            gesture_key: None,
            awaiting_gpu: VecDeque::new(),
        }
    }

    /// Start of a frame (`App::logic`); `cpu_usage` is eframe's time of the previous frame.
    pub fn begin(&mut self, cpu_usage: Option<f32>) {
        let start = page_now();
        let fresh = cpu_usage.filter(|c| Some(*c) != self.last_cpu);
        self.last_cpu = cpu_usage.or(self.last_cpu);
        if let Some(frame) = self.last.take() {
            let total = fresh.map(|s| f64::from(s) * 1000.0);
            if frame.gpu.is_some() {
                self.awaiting_gpu.push_back((frame, total, 0));
            } else {
                self.finish(frame, total, None);
            }
        }
        // Frames in order, once their GPU work is done (or it took too long to wait for).
        for f in &mut self.awaiting_gpu {
            f.2 = f.2.saturating_add(1);
        }
        while let Some((frame, _, age)) = self.awaiting_gpu.front() {
            let gpu = frame.gpu.as_ref().map(|g| g.load(Ordering::Relaxed)).filter(|b| *b != u64::MAX).map(f64::from_bits);
            if gpu.is_none() && *age < GPU_WAIT_FRAMES {
                break;
            }
            let Some((frame, total, _)) = self.awaiting_gpu.pop_front() else { break };
            self.finish(frame, total, gpu);
        }
        let interval = self.last_start.map(|t| start - t);
        self.last_start = Some(start);
        self.current = Some(Frame { start, interval, update_ms: 0.0, input: self.pending_input.take(), key: None, megapixels: None, gpu: None });
    }

    /// End of the app's update (`App::ui`): what the canvas was doing this frame.
    pub fn end_update(&mut self, app: &PhotocraftApp, ctx: &egui::Context) {
        let Some(mut frame) = self.current.take() else { return };
        frame.update_ms = page_now() - frame.start;
        frame.key = canvas_activity(app, ctx);
        if frame.key.is_some() {
            frame.megapixels = app.session.active().map(|st| telemetry::megapixels(st.doc.size.width, st.doc.size.height));
        }
        if self.debug {
            let fence: GpuFence = Arc::new(AtomicU64::new(u64::MAX));
            let done = fence.clone();
            let t0 = page_now();
            if app.on_gpu_done(move || done.store((page_now() - t0).max(0.0).to_bits(), Ordering::Relaxed)) {
                frame.gpu = Some(fence);
            }
        }
        self.last = Some(frame);
    }

    fn finish(&mut self, frame: Frame, total_ms: Option<f64>, gpu_ms: Option<f64>) {
        // The oldest move waited from its event until this frame's paint was submitted.
        let latency = frame.input.zip(total_ms).map(|(t, ms)| (frame.start + ms - t).max(0.0));
        if let (Some(key), Some(ms)) = (&frame.key, total_ms) {
            telemetry::with(|r| r.frame(key, ms, latency, frame.megapixels));
        }
        if !self.debug {
            return;
        }
        {
            let mut records = self.records.borrow_mut();
            if records.len() >= MAX_RECORDS {
                records.drain(..MAX_RECORDS / 4);
            }
            records.push(json!({
                "t": round2(frame.start), "int": frame.interval.map(round2), "upd": round2(frame.update_ms),
                "ms": total_ms.map(round2), "lat": latency.map(round2), "key": frame.key, "gpu": gpu_ms.map(round2),
            }));
        }
        if frame.key != self.gesture_key {
            self.report_gesture();
            self.gesture_key = frame.key.clone();
        }
        if frame.key.is_some()
            && let Some(ms) = total_ms
        {
            self.gesture.push(GestureFrame { ms, update_ms: frame.update_ms, latency_ms: latency, gpu_ms, interval_ms: frame.interval });
        }
    }

    /// `?perf=1`: one console line per gesture with its percentiles.
    fn report_gesture(&mut self) {
        let frames = std::mem::take(&mut self.gesture);
        let Some(key) = self.gesture_key.take() else { return };
        if frames.is_empty() {
            return;
        }
        let pick = |f: fn(&GestureFrame) -> Option<f64>| {
            let mut v: Vec<f64> = frames.iter().filter_map(f).collect();
            v.sort_by(f64::total_cmp);
            json!({"p50": round2(quantile(&v, 0.5)), "p95": round2(quantile(&v, 0.95)), "max": round2(v.last().copied().unwrap_or(0.0))})
        };
        // Display refreshes without a new frame, between the gesture's frames.
        let dropped: f64 = frames.iter().skip(1).filter_map(|f| f.interval_ms).filter(|i| *i < 1000.0).map(|i| ((i / VSYNC_MS).round() - 1.0).max(0.0)).sum();
        let line = json!({
            "key": key, "frames": frames.len(),
            "ms": pick(|f| Some(f.ms)), "upd": pick(|f| Some(f.update_ms)), "lat": pick(|f| f.latency_ms),
            "gpu": pick(|f| f.gpu_ms), "int": pick(|f| f.interval_ms), "dropped": dropped,
        });
        log::info!("capishot-perf {line}");
    }
}

/// The tool working the canvas, or `view` for wheel zoom/scroll and pinch over it.
fn canvas_activity(app: &PhotocraftApp, ctx: &egui::Context) -> Option<String> {
    if let Some(tool) = app.canvas_gesture_tool(ctx) {
        return Some(telemetry::tool_id(tool));
    }
    let over_canvas = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| app.last_canvas_rect.contains(p)));
    let navigating = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. } | egui::Event::Zoom(_))));
    (over_canvas && navigating).then(|| telemetry::VIEW_FRAMES.to_string())
}

fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() as f64 * p).ceil() as usize).clamp(1, sorted.len()) - 1;
    sorted.get(i).copied().unwrap_or(0.0)
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// `photocraftPerf.take()` on the page: the recorded frames since the last call, as JSON.
fn expose(records: Rc<RefCell<Vec<serde_json::Value>>>) {
    let Some(window) = web_sys::window() else { return };
    let api = js_sys::Object::new();
    let take = Closure::<dyn Fn() -> String>::new(move || serde_json::Value::Array(std::mem::take(&mut *records.borrow_mut())).to_string());
    if js_sys::Reflect::set(&api, &JsValue::from_str("take"), &take.into_js_value()).is_ok() {
        let _ = js_sys::Reflect::set(&window, &JsValue::from_str("photocraftPerf"), &api);
    }
}
