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
//! later; `upd` splits off the app's update (`logic` + `ui`), and the rest is `paint`. eframe
//! paints from `requestAnimationFrame`, so the interval between two frames of a gesture is the
//! display's real frame rate: an interval of k display periods missed k − 1 frames. Each frame
//! records its split (`in`: how long its oldest pointer move waited before the frame started,
//! `logic`, `ui`, `paint`); `?perf=1` also counts the app's work per gesture (thumbnails rebuilt,
//! Layers panel rows laid out, panel recomputes: `photocraft_ui_egui::perf_counters`) and exposes
//! the counters as `photocraftPerf.counters()`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

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
    interval_ms: Option<f64>,
    parts: telemetry::FrameParts,
}

/// One frame, from its start until eframe reports its time (in the next frame).
#[derive(Clone, Debug)]
struct Frame {
    /// Start of the frame (`performance.now()`).
    start: f64,
    /// Since the previous frame's start.
    interval: Option<f64>,
    /// The app's update: `logic` + `ui`.
    update_ms: f64,
    /// The app's logic (`App::logic`).
    logic_ms: f64,
    /// Timestamp of the oldest pointer move this frame consumed.
    input: Option<f64>,
    /// What the canvas was doing: a tool id (`tool.Brush`) or `view`.
    key: Option<String>,
    megapixels: Option<f64>,
    layers: Option<usize>,
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
    /// The previous finished frame's key: a frame whose key differs starts a gesture.
    prev_key: Option<String>,
    /// The app's work counters at the start of the current gesture, and now (`?perf=1`).
    counters_at_start: Vec<(&'static str, u64)>,
    counters: Rc<RefCell<Vec<(&'static str, u64)>>>,
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
        let counters: Rc<RefCell<Vec<(&'static str, u64)>>> = Rc::default();
        if debug {
            expose(records.clone(), counters.clone());
            log::info!("photocraft-web: frame timing on (?perf=1): photocraftPerf.take(), photocraftPerf.counters()");
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
            prev_key: None,
            counters_at_start: Vec::new(),
            counters,
        }
    }

    /// Start of a frame (`App::logic`); `cpu_usage` is eframe's time of the previous frame.
    pub fn begin(&mut self, cpu_usage: Option<f32>) {
        let start = page_now();
        let fresh = cpu_usage.filter(|c| Some(*c) != self.last_cpu);
        self.last_cpu = cpu_usage.or(self.last_cpu);
        if let Some(frame) = self.last.take() {
            self.finish(frame, fresh.map(|s| f64::from(s) * 1000.0));
        }
        let interval = self.last_start.map(|t| start - t);
        self.last_start = Some(start);
        self.current = Some(Frame { start, interval, update_ms: 0.0, logic_ms: 0.0, input: self.pending_input.take(), key: None, megapixels: None, layers: None });
    }

    /// End of the app's logic (`App::logic`).
    pub fn end_logic(&mut self) {
        if let Some(frame) = self.current.as_mut() {
            frame.logic_ms = page_now() - frame.start;
        }
    }

    /// End of the app's update (`App::ui`): what the canvas was doing this frame.
    pub fn end_update(&mut self, app: &PhotocraftApp, ctx: &egui::Context) {
        let Some(mut frame) = self.current.take() else { return };
        frame.update_ms = page_now() - frame.start;
        frame.key = canvas_activity(app, ctx);
        if frame.key.is_some() {
            frame.megapixels = app.session.active().map(|st| telemetry::megapixels(st.doc.size.width, st.doc.size.height));
            frame.layers = app.session.active().map(|st| layer_count(&st.doc.layers));
        }
        if self.debug {
            *self.counters.borrow_mut() = app.perf_counters.snapshot();
        }
        self.last = Some(frame);
    }

    fn finish(&mut self, frame: Frame, total_ms: Option<f64>) {
        // The oldest move waited from its event until this frame's paint was submitted.
        let latency = frame.input.zip(total_ms).map(|(t, ms)| (frame.start + ms - t).max(0.0));
        let parts = telemetry::FrameParts {
            input: frame.input.map_or(0.0, |t| (frame.start - t).max(0.0)),
            logic: frame.logic_ms,
            ui: (frame.update_ms - frame.logic_ms).max(0.0),
            paint: total_ms.map_or(0.0, |ms| (ms - frame.update_ms).max(0.0)),
        };
        let gesture_start = frame.key.is_some() && frame.key != self.prev_key;
        self.prev_key = frame.key.clone();
        if let (Some(key), Some(ms)) = (&frame.key, total_ms) {
            let sample = telemetry::FrameSample {
                key,
                ms,
                latency_ms: latency,
                interval_ms: frame.interval,
                megapixels: frame.megapixels,
                layers: frame.layers,
                gesture_start,
                parts: Some(parts),
            };
            telemetry::with(|r| r.frame_sample(&sample));
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
                "ms": total_ms.map(round2), "lat": latency.map(round2), "key": frame.key,
                "in": round2(parts.input), "logic": round2(parts.logic), "ui": round2(parts.ui), "paint": round2(parts.paint),
            }));
        }
        if frame.key != self.gesture_key {
            self.report_gesture();
            self.gesture_key = frame.key.clone();
            self.counters_at_start = self.counters.borrow().clone();
        }
        if frame.key.is_some()
            && let Some(ms) = total_ms
        {
            let interval_ms = frame.interval.filter(|_| !gesture_start);
            self.gesture.push(GestureFrame { ms, update_ms: frame.update_ms, latency_ms: latency, interval_ms, parts });
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
        // The display's rate and the frames it missed, against the shortest interval seen.
        let intervals: Vec<f64> = frames.iter().filter_map(|f| f.interval_ms).filter(|i| *i < 1000.0).collect();
        let vsync = intervals.iter().copied().filter(|i| *i >= 5.5).fold(1000.0 / 60.0, f64::min);
        let missed: f64 = intervals.iter().map(|i| ((i / vsync).round() - 1.0).max(0.0)).sum();
        let fps = if intervals.is_empty() { None } else { Some(round2(1000.0 * intervals.len() as f64 / intervals.iter().sum::<f64>())) };
        let worst = frames.iter().max_by(|a, b| a.ms.total_cmp(&b.ms)).map(|f| {
            json!({"ms": round2(f.ms), "in": round2(f.parts.input), "logic": round2(f.parts.logic), "ui": round2(f.parts.ui), "paint": round2(f.parts.paint)})
        });
        let now = self.counters.borrow().clone();
        let work: serde_json::Map<String, serde_json::Value> = now
            .iter()
            .filter_map(|(k, v)| {
                let d = v.saturating_sub(self.counters_at_start.iter().find(|c| c.0 == *k).map_or(0, |c| c.1));
                (d > 0).then(|| ((*k).to_string(), json!(d)))
            })
            .collect();
        let line = json!({
            "key": key, "frames": frames.len(),
            "ms": pick(|f| Some(f.ms)), "upd": pick(|f| Some(f.update_ms)), "lat": pick(|f| f.latency_ms),
            "fps": fps, "dropped": missed, "worst": worst, "work": work,
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

/// Layers in the document, groups and their contents included (no allocation: called per frame).
fn layer_count(layers: &[photocraft_doc::Layer]) -> usize {
    fn rec(layers: &[photocraft_doc::Layer], depth: usize) -> usize {
        if depth > 64 {
            return 0;
        }
        layers.iter().map(|l| 1 + l.children().map_or(0, |c| rec(c, depth + 1))).sum()
    }
    rec(layers, 0)
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

/// `photocraftPerf.take()` on the page: the recorded frames since the last call, as JSON;
/// `photocraftPerf.counters()`: the app's work counters (monotonic), as JSON.
fn expose(records: Rc<RefCell<Vec<serde_json::Value>>>, counters: Rc<RefCell<Vec<(&'static str, u64)>>>) {
    let Some(window) = web_sys::window() else { return };
    let api = js_sys::Object::new();
    let take = Closure::<dyn Fn() -> String>::new(move || serde_json::Value::Array(std::mem::take(&mut *records.borrow_mut())).to_string());
    let read = Closure::<dyn Fn() -> String>::new(move || {
        let map: serde_json::Map<String, serde_json::Value> = counters.borrow().iter().map(|(k, v)| ((*k).to_string(), json!(v))).collect();
        serde_json::Value::Object(map).to_string()
    });
    if js_sys::Reflect::set(&api, &JsValue::from_str("take"), &take.into_js_value()).is_ok()
        && js_sys::Reflect::set(&api, &JsValue::from_str("counters"), &read.into_js_value()).is_ok()
    {
        let _ = js_sys::Reflect::set(&window, &JsValue::from_str("photocraftPerf"), &api);
    }
}
