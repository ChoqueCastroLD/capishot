//! Anonymous usage statistics (the web build only; nothing is recorded or sent elsewhere).
//!
//! The platform shell installs a [`Recorder`] with [`start`] once the user's preference
//! (Preferences › General › Send anonymous usage statistics) and the browser's privacy signals
//! allow it, then drains [`take_batch`] every 30 s and when the page hides. Until then every hook
//! is a relaxed atomic load and nothing else.
//!
//! What is recorded (schema `v: 2`, see `/root/capishot-telemetry/README.md` on the host):
//! a random install id and session id, coarse environment buckets, command ids counted per
//! batch (never their params), tool switches, open/save/export kinds with size buckets and
//! durations, error classes (command id + kind, never the message), slow-command timings, and
//! (schema 2) frame-time percentiles per canvas tool: how long the app took per frame while a
//! tool was dragged on the canvas or the view zoomed/scrolled (`frames`, see [`Recorder::frame`]).
//! Schema 3 adds the device class and memory bucket to `session_start`, and to `frames` the true
//! frame rate (intervals between painted frames), dropped frames, the layer-count bucket, the
//! number of gestures and the split of the worst frame (input wait, app logic, UI layout, paint
//! and GPU submission; see [`FrameSample`]).
//! Never: file names, paths, document content, typed text, error messages.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value, json};

/// Version of the batch schema the collector validates.
pub const SCHEMA_VERSION: u32 = 4;
/// Events queued between flushes; later ones are dropped (and counted).
pub const MAX_EVENTS: usize = 200;
/// Distinct (command, source) and (command, error) keys aggregated between flushes.
pub const MAX_KEYS: usize = 400;
/// Slow-command samples per batch.
pub const MAX_PERF: usize = 20;
/// Freeze reports per batch.
pub const MAX_FREEZES: usize = 20;
/// A main-thread block at least this long is a freeze (`freeze`, schema v4).
pub const FREEZE_MS: f64 = 200.0;
/// Only commands at least this slow are sampled.
pub const PERF_MIN_MS: f64 = 100.0;
/// A batch's JSON stays under this (the collector accepts 64 KB; `fetch` keepalive allows 64 KB).
pub const MAX_BATCH_BYTES: usize = 48 * 1024;
/// Distinct frame-statistics keys (canvas tools, `view`) per batch.
pub const MAX_FRAME_KEYS: usize = 24;
/// Frame-time histogram resolution: one bucket per millisecond, the last one open-ended.
const FRAME_BUCKETS: usize = 250;
/// A frame at least this slow is a visible hitch (`long` in `frames`).
pub const LONG_FRAME_MS: f64 = 50.0;
/// The `frames` key of view navigation (wheel zoom and scroll, pinch) without a tool drag.
pub const VIEW_FRAMES: &str = "view";
/// Heartbeat period; a period counts as active when the user interacted during it.
pub const HEARTBEAT_MS: f64 = 60_000.0;
/// Flush period.
pub const FLUSH_MS: f64 = 30_000.0;
/// Session times are capped at a week (the collector's limit).
pub const MAX_SECONDS: u64 = 7 * 86_400;

/// How a freeze was seen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreezeSource {
    /// The browser's `longtask` performance entries.
    LongTask,
    /// One app frame (input, logic and drawing) ran that long.
    Frame,
}

/// What was running when the page froze: the last command, the tool, a background job.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FreezeContext {
    pub command: Option<String>,
    pub tool: Option<String>,
    pub job: Option<String>,
    pub megapixels: Option<f64>,
}

/// Where a command came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Menu,
    Shortcut,
    Palette,
    /// Panels, option bars, dialogs, canvas gestures: anything that calls the app directly.
    Panel,
    /// Tool switches.
    Tool,
    /// The embedding API or control channel.
    Automation,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Menu => "menu",
            Source::Shortcut => "shortcut",
            Source::Palette => "palette",
            Source::Panel => "panel",
            Source::Tool => "tool",
            Source::Automation => "automation",
        }
    }
}

/// The browser and device, as coarse classes, for `session_start`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Environment {
    /// The threads build (shared memory and a Web Worker pool).
    pub threads: bool,
    pub cross_origin_isolated: bool,
    /// `navigator.hardwareConcurrency` (capped at 256).
    pub cores: u32,
    /// `webgpu`, `webgl`, `cpu` or `unknown`.
    pub gpu: &'static str,
    /// [`gpu_vendor_class`].
    pub gpu_vendor: &'static str,
    /// Parsed into a browser family, major version and OS family; never sent as is.
    pub user_agent: String,
    /// Screen size in CSS pixels, bucketed with [`screen_bucket`].
    pub screen: (u32, u32),
    /// UI language code (`es`, `pt-br`, …).
    pub language: String,
    /// [`device_class`]: `mobile`, `tablet`, `desktop` or `unknown`.
    pub device: &'static str,
    /// `navigator.deviceMemory` in GB (0 = not reported), bucketed with [`memory_bucket`].
    pub memory_gb: f64,
}

/// The `session_start` event for `env`.
pub fn session_start_event(env: &Environment) -> Value {
    let (browser, major) = browser_family(&env.user_agent);
    json!({
        "e": "session_start",
        "app": photocraft_engine::build_info::VERSION,
        "commit": photocraft_engine::build_info::short_sha().unwrap_or("dev"),
        "threads": env.threads,
        "coi": env.cross_origin_isolated,
        "cores": env.cores.min(256),
        "gpu": env.gpu,
        "gpu_vendor": env.gpu_vendor,
        "browser": browser,
        "browser_major": major,
        "os": os_family(&env.user_agent),
        "screen": screen_bucket(env.screen.0, env.screen.1),
        "lang": sanitize_token(&env.language, 12).unwrap_or_else(|| "other".into()),
        "device": if matches!(env.device, "mobile" | "tablet" | "desktop") { env.device } else { "unknown" },
        "mem": memory_bucket(env.memory_gb),
    })
}

/// The device class from the user agent, the number of touch points and the screen's shorter
/// side in CSS pixels: phones say `Mobi` (or are iPhones); tablets are touch devices that don't
/// (Android tablets, iPads, which also pose as a Mac with touch points).
pub fn device_class(ua: &str, max_touch_points: u32, screen_short_side: u32) -> &'static str {
    if ua.is_empty() {
        return "unknown";
    }
    if ua.contains("iPhone") || ua.contains("iPod") || (ua.contains("Mobi") && !ua.contains("iPad")) {
        return "mobile";
    }
    let ipad = ua.contains("iPad") || (ua.contains("Macintosh") && max_touch_points > 1);
    let android_tablet = ua.contains("Android");
    if ipad || android_tablet || ua.contains("Tablet") {
        return if screen_short_side > 0 && screen_short_side < 480 { "mobile" } else { "tablet" };
    }
    "desktop"
}

/// `navigator.deviceMemory` (GB, rounded down to a power of two by the browser) as a bucket.
pub fn memory_bucket(gb: f64) -> &'static str {
    match gb {
        g if !g.is_finite() || g <= 0.0 => "unknown",
        g if g < 1.0 => "<1",
        g if g < 2.0 => "1",
        g if g < 4.0 => "2",
        g if g < 8.0 => "4",
        _ => "8+",
    }
}

/// Where a frame's main-thread time went (ms): `input` how long the oldest input it consumed had
/// waited when it started, `logic` the app's logic (tools, commands, canvas refresh), `ui` the
/// panels' and canvas's layout, `paint` tessellation, painting and GPU submission.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FrameParts {
    pub input: f64,
    pub logic: f64,
    pub ui: f64,
    pub paint: f64,
}

/// One frame of a canvas gesture for [`Recorder::frame_sample`].
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameSample<'a> {
    /// The tool dragged ([`tool_id`]) or [`VIEW_FRAMES`].
    pub key: &'a str,
    /// The frame's main-thread time (logic + layout + paint).
    pub ms: f64,
    /// How long the oldest pointer move it consumed waited until the frame was painted.
    pub latency_ms: Option<f64>,
    /// Since the previous painted frame (the display's frame rate while the gesture runs).
    pub interval_ms: Option<f64>,
    pub megapixels: Option<f64>,
    /// Layers in the document (groups count).
    pub layers: Option<usize>,
    /// The first frame of a gesture (a press, or the first wheel step).
    pub gesture_start: bool,
    pub parts: Option<FrameParts>,
}

/// The queue and aggregates of one session. Pure data: the platform shell owns the clock and
/// the transport.
#[derive(Debug)]
pub struct Recorder {
    install_id: String,
    session_id: String,
    seq: u64,
    started_ms: f64,
    active_s: u64,
    last_interaction_ms: Option<f64>,
    last_heartbeat_ms: f64,
    commands: BTreeMap<(String, Source), u32>,
    errors: BTreeMap<(String, &'static str), u32>,
    events: Vec<Value>,
    perf_queued: usize,
    freezes_queued: usize,
    frames: BTreeMap<String, FrameHist>,
    dropped: u32,
    ended: bool,
    /// The display's frame period as observed (the shortest interval between painted frames
    /// seen, at least [`MIN_VSYNC_MS`]): what a dropped frame is measured against.
    vsync_ms: f64,
}

/// Intervals shorter than this are back-to-back event frames, not the display's period.
const MIN_VSYNC_MS: f64 = 5.5;
/// The period assumed until a shorter one is seen (60 Hz).
const DEFAULT_VSYNC_MS: f64 = 1000.0 / 60.0;

/// Frame times (and input-to-paint latencies) of one key since the last batch, as 1 ms histograms:
/// fixed size, nothing allocated per frame.
#[derive(Clone, Debug)]
struct FrameHist {
    frames: Box<[u32; FRAME_BUCKETS]>,
    latency: Box<[u32; FRAME_BUCKETS]>,
    n: u32,
    latency_n: u32,
    long: u32,
    max_ms: f64,
    latency_max_ms: f64,
    mp: Option<&'static str>,
    /// Intervals between painted frames (schema 3).
    intervals: Box<[u32; FRAME_BUCKETS]>,
    interval_n: u32,
    interval_sum_ms: f64,
    interval_max_ms: f64,
    /// Display frames missed (an interval of k periods misses k − 1).
    missed: u32,
    gestures: u32,
    layers: Option<&'static str>,
    /// The slowest frame's split.
    worst: Option<FrameParts>,
}

impl FrameHist {
    fn new() -> Self {
        Self {
            frames: Box::new([0; FRAME_BUCKETS]),
            latency: Box::new([0; FRAME_BUCKETS]),
            n: 0,
            latency_n: 0,
            long: 0,
            max_ms: 0.0,
            latency_max_ms: 0.0,
            mp: None,
            intervals: Box::new([0; FRAME_BUCKETS]),
            interval_n: 0,
            interval_sum_ms: 0.0,
            interval_max_ms: 0.0,
            missed: 0,
            gestures: 0,
            layers: None,
            worst: None,
        }
    }

    fn bucket(ms: f64) -> usize {
        (ms.max(0.0) as usize).min(FRAME_BUCKETS - 1)
    }

    /// The `p` quantile (0..1) of `hist` holding `n` samples, in whole milliseconds: the upper edge
    /// of the bucket it falls in (the open last bucket reports `max`).
    fn quantile(hist: &[u32; FRAME_BUCKETS], n: u32, p: f64, max: f64) -> u64 {
        let want = ((n as f64 * p).ceil() as u64).max(1);
        let mut seen = 0u64;
        for (b, &c) in hist.iter().enumerate() {
            seen += c as u64;
            if seen >= want {
                return if b + 1 == FRAME_BUCKETS { round_ms(max) } else { (b as u64 + 1).min(round_ms(max).max(1)) };
            }
        }
        round_ms(max)
    }
}

impl Recorder {
    /// A new session at `now_ms` (any monotonic-enough millisecond clock).
    pub fn new(install_id: &str, session_id: &str, now_ms: f64) -> Self {
        Self {
            install_id: sanitize_token(install_id, 64).unwrap_or_default(),
            session_id: sanitize_token(session_id, 64).unwrap_or_default(),
            seq: 0,
            started_ms: now_ms,
            active_s: 0,
            last_interaction_ms: None,
            last_heartbeat_ms: now_ms,
            commands: BTreeMap::new(),
            errors: BTreeMap::new(),
            events: Vec::new(),
            perf_queued: 0,
            freezes_queued: 0,
            frames: BTreeMap::new(),
            dropped: 0,
            ended: false,
            vsync_ms: DEFAULT_VSYNC_MS,
        }
    }

    /// One frame's time while the canvas was being worked: `key` is the tool dragged (its
    /// [`tool_id`]) or [`VIEW_FRAMES`]; `ms` the frame's main-thread time (app update + paint);
    /// `latency_ms` how long the oldest pointer move the frame consumed waited until the frame was
    /// painted; `megapixels` the document's size. Summarised per key as percentiles at the batch.
    pub fn frame(&mut self, key: &str, ms: f64, latency_ms: Option<f64>, megapixels: Option<f64>) {
        self.frame_sample(&FrameSample { key, ms, latency_ms, megapixels, ..Default::default() });
    }

    /// One frame of a canvas gesture with everything schema 3 reports (see [`FrameSample`]).
    pub fn frame_sample(&mut self, f: &FrameSample<'_>) {
        let (key, ms, latency_ms, megapixels) = (f.key, f.ms, f.latency_ms, f.megapixels);
        if self.ended || !ms.is_finite() || ms < 0.0 {
            return;
        }
        // The display's period: the shortest interval between painted frames seen so far.
        let interval = f.interval_ms.filter(|i| i.is_finite() && *i > 0.0 && *i < 1000.0 && !f.gesture_start);
        if let Some(i) = interval
            && i >= MIN_VSYNC_MS
        {
            self.vsync_ms = self.vsync_ms.min(i);
        }
        let vsync = self.vsync_ms;
        if !self.frames.contains_key(key) {
            if self.frames.len() >= MAX_FRAME_KEYS {
                return;
            }
            self.frames.insert(key.to_string(), FrameHist::new());
        }
        let Some(h) = self.frames.get_mut(key) else { return };
        h.n = h.n.saturating_add(1);
        if let Some(slot) = h.frames.get_mut(FrameHist::bucket(ms)) {
            *slot = slot.saturating_add(1);
        }
        if ms >= LONG_FRAME_MS {
            h.long = h.long.saturating_add(1);
        }
        h.max_ms = h.max_ms.max(ms.min(3_600_000.0));
        if let Some(l) = latency_ms.filter(|l| l.is_finite() && *l >= 0.0) {
            h.latency_n = h.latency_n.saturating_add(1);
            h.latency_max_ms = h.latency_max_ms.max(l.min(3_600_000.0));
            if let Some(slot) = h.latency.get_mut(FrameHist::bucket(l)) {
                *slot = slot.saturating_add(1);
            }
        }
        if let Some(mp) = megapixels {
            h.mp = Some(mp_bucket_of(mp));
        }
        if let Some(n) = f.layers {
            h.layers = Some(layers_bucket(n));
        }
        if f.gesture_start {
            h.gestures = h.gestures.saturating_add(1);
        }
        if let Some(i) = interval {
            h.interval_n = h.interval_n.saturating_add(1);
            h.interval_sum_ms += i;
            h.interval_max_ms = h.interval_max_ms.max(i);
            if let Some(slot) = h.intervals.get_mut(FrameHist::bucket(i)) {
                *slot = slot.saturating_add(1);
            }
            let periods = (i / vsync).round();
            if periods >= 2.0 {
                h.missed = h.missed.saturating_add((periods - 1.0).min(1000.0) as u32);
            }
        }
        if let Some(parts) = f.parts
            && h.max_ms <= ms.min(3_600_000.0)
        {
            h.worst = Some(parts);
        }
    }

    /// Queue an event; past [`MAX_EVENTS`] it is dropped and counted.
    pub fn event(&mut self, event: Value) {
        if self.ended {
            return;
        }
        if self.events.len() >= MAX_EVENTS {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.events.push(event);
        }
    }

    /// Count a command id (sanitized when the batch is built).
    pub fn command(&mut self, id: &str, source: Source) {
        bump(&mut self.commands, (id, source), &mut self.dropped, |(id, s)| (id.to_string(), s));
    }

    /// Count a failed command, classified by [`error_kind`]; the message itself is not kept.
    pub fn error(&mut self, id: &str, message: &str) {
        let kind = error_kind(message);
        bump(&mut self.errors, (id, kind), &mut self.dropped, |(id, k)| (id.to_string(), k));
    }

    /// A slow-command sample (only at least [`PERF_MIN_MS`], at most [`MAX_PERF`] per batch).
    pub fn perf(&mut self, op: &str, ms: f64, megapixels: Option<f64>) {
        if !ms.is_finite() || ms < PERF_MIN_MS || self.perf_queued >= MAX_PERF {
            return;
        }
        self.perf_queued += 1;
        let mut e = json!({"e": "perf", "op": sanitize_command_id(op), "ms": round_ms(ms)});
        if let Some(mp) = megapixels {
            e["mp"] = json!(mp_bucket_of(mp));
        }
        self.event(e);
    }

    /// The page's main thread was blocked for `ms` (`longtask`: the browser saw a long task;
    /// `frame`: one app frame ran that long), while `ctx` ran. Only at least [`FREEZE_MS`], at
    /// most [`MAX_FREEZES`] per batch.
    pub fn freeze(&mut self, src: FreezeSource, ms: f64, ctx: &FreezeContext) {
        if !ms.is_finite() || ms < FREEZE_MS || self.freezes_queued >= MAX_FREEZES {
            return;
        }
        self.freezes_queued += 1;
        let src = match src {
            FreezeSource::LongTask => "longtask",
            FreezeSource::Frame => "frame",
        };
        let mut e = json!({"e": "freeze", "src": src, "ms": round_ms(ms)});
        for (key, value) in [("cmd", &ctx.command), ("tool", &ctx.tool), ("job", &ctx.job)] {
            if let Some(v) = value {
                e[key] = json!(sanitize_command_id(v));
            }
        }
        if let Some(mp) = ctx.megapixels {
            e["mp"] = json!(mp_bucket_of(mp));
        }
        self.event(e);
    }

    /// The user pressed, moved, scrolled or typed.
    pub fn interaction(&mut self, now_ms: f64) {
        self.last_interaction_ms = Some(now_ms);
    }

    /// Call periodically: once a [`HEARTBEAT_MS`] period has passed, a period with interaction
    /// while `visible` adds to active time and queues a `heartbeat`.
    pub fn tick(&mut self, now_ms: f64, visible: bool) {
        let elapsed = now_ms - self.last_heartbeat_ms;
        if self.ended || !elapsed.is_finite() || elapsed < HEARTBEAT_MS {
            return;
        }
        let since = self.last_heartbeat_ms;
        self.last_heartbeat_ms = now_ms;
        let interacted = self.last_interaction_ms.is_some_and(|t| t >= since);
        if visible && interacted {
            // A throttled timer can fire late; never credit more than two periods.
            self.active_s = self.active_s.saturating_add((elapsed.min(2.0 * HEARTBEAT_MS) / 1000.0).round() as u64);
            let total = self.total_s(now_ms);
            self.event(json!({"e": "heartbeat", "active_s": self.active_s.min(MAX_SECONDS), "total_s": total}));
        }
    }

    /// The page is going away: queue `session_end` once.
    pub fn end(&mut self, now_ms: f64) {
        if self.ended {
            return;
        }
        // The minute in progress counts when the user interacted in it (the tab may already be
        // hidden while the page goes away).
        let partial = now_ms - self.last_heartbeat_ms;
        if partial.is_finite() && partial > 0.0 && self.last_interaction_ms.is_some_and(|t| t >= self.last_heartbeat_ms) {
            self.active_s = self.active_s.saturating_add((partial.min(HEARTBEAT_MS) / 1000.0).round() as u64);
        }
        let total = self.total_s(now_ms);
        self.event(json!({"e": "session_end", "active_s": self.active_s.min(MAX_SECONDS), "total_s": total}));
        self.ended = true;
    }

    pub fn active_seconds(&self) -> u64 {
        self.active_s
    }

    fn total_s(&self, now_ms: f64) -> u64 {
        let s = (now_ms - self.started_ms) / 1000.0;
        if s.is_finite() && s > 0.0 { (s.round() as u64).min(MAX_SECONDS) } else { 0 }
    }

    /// Nothing waiting to be sent?
    pub fn is_empty(&self) -> bool {
        self.events.is_empty() && self.commands.is_empty() && self.errors.is_empty() && self.frames.is_empty() && self.dropped == 0
    }

    /// Everything since the last batch as the JSON body to POST, or `None` when there is
    /// nothing. The queue and aggregates are cleared either way (a failed send is not retried).
    pub fn take_batch(&mut self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut events = std::mem::take(&mut self.events);
        let mut merged: BTreeMap<(String, &'static str), u32> = BTreeMap::new();
        for ((id, source), n) in std::mem::take(&mut self.commands) {
            let id = if source == Source::Tool { id } else { sanitize_command_id(&id) };
            let slot = merged.entry((id, source.as_str())).or_default();
            *slot = slot.saturating_add(n);
        }
        events.extend(merged.into_iter().map(|((id, src), n)| json!({"e": "command", "id": id, "src": src, "n": n})));
        let mut errors: BTreeMap<(String, &'static str), u32> = BTreeMap::new();
        for ((id, kind), n) in std::mem::take(&mut self.errors) {
            let slot = errors.entry((sanitize_command_id(&id), kind)).or_default();
            *slot = slot.saturating_add(n);
        }
        events.extend(errors.into_iter().map(|((id, kind), n)| json!({"e": "error", "id": id, "kind": kind, "n": n})));
        for (key, h) in std::mem::take(&mut self.frames) {
            if h.n == 0 {
                continue;
            }
            let key = if key == VIEW_FRAMES || key.starts_with("tool.") { key } else { "other".into() };
            let mut e = json!({
                "e": "frames", "tool": key, "n": h.n,
                "p50": FrameHist::quantile(&h.frames, h.n, 0.5, h.max_ms),
                "p95": FrameHist::quantile(&h.frames, h.n, 0.95, h.max_ms),
                "max": round_ms(h.max_ms), "long": h.long,
            });
            if h.latency_n > 0 {
                e["lat50"] = json!(FrameHist::quantile(&h.latency, h.latency_n, 0.5, h.latency_max_ms));
                e["lat95"] = json!(FrameHist::quantile(&h.latency, h.latency_n, 0.95, h.latency_max_ms));
            }
            if let Some(mp) = h.mp {
                e["mp"] = json!(mp);
            }
            if h.interval_n > 0 && h.interval_sum_ms > 0.0 {
                e["fps"] = json!(((1000.0 * f64::from(h.interval_n) / h.interval_sum_ms).round() as u64).min(1000));
                e["int95"] = json!(FrameHist::quantile(&h.intervals, h.interval_n, 0.95, h.interval_max_ms));
                e["drop"] = json!(h.missed);
            }
            if let Some(l) = h.layers {
                e["layers"] = json!(l);
            }
            if h.gestures > 0 {
                e["g"] = json!(h.gestures);
            }
            if let Some(w) = h.worst {
                e["w_in"] = json!(round_ms(w.input));
                e["w_logic"] = json!(round_ms(w.logic));
                e["w_ui"] = json!(round_ms(w.ui));
                e["w_paint"] = json!(round_ms(w.paint));
            }
            events.push(e);
        }
        self.perf_queued = 0;
        self.freezes_queued = 0;
        let mut dropped = std::mem::take(&mut self.dropped);
        self.seq = self.seq.saturating_add(1);
        loop {
            let mut batch = Map::new();
            batch.insert("v".into(), json!(SCHEMA_VERSION));
            batch.insert("iid".into(), json!(self.install_id));
            batch.insert("sid".into(), json!(self.session_id));
            batch.insert("seq".into(), json!(self.seq));
            if dropped > 0 {
                batch.insert("dropped".into(), json!(dropped));
            }
            batch.insert("events".into(), Value::Array(events.clone()));
            let text = Value::Object(batch).to_string();
            if text.len() <= MAX_BATCH_BYTES || events.is_empty() {
                return Some(text);
            }
            // Too big: drop from the end (aggregates last) until it fits.
            let cut = events.len().div_ceil(4).max(1);
            events.truncate(events.len().saturating_sub(cut));
            dropped = dropped.saturating_add(cut as u32);
        }
    }
}

fn bump<K: Ord, B>(map: &mut BTreeMap<K, u32>, key: B, dropped: &mut u32, own: impl FnOnce(B) -> K) {
    let key = own(key);
    if let Some(n) = map.get_mut(&key) {
        *n = n.saturating_add(1);
    } else if map.len() >= MAX_KEYS {
        *dropped = dropped.saturating_add(1);
    } else {
        map.insert(key, 1);
    }
}

fn round_ms(ms: f64) -> u64 {
    if ms.is_finite() && ms > 0.0 { ms.min(3_600_000.0).round() as u64 } else { 0 }
}

// ------------------------------------------------------------------ classification

/// An error message's class: never the message itself.
pub fn error_kind(message: &str) -> &'static str {
    let m = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    // AI runs on the GPU first: a WebGPU limit the kernel needed more of, a lost device, a
    // crashed AI worker, another GPU failure (`AiError::Gpu`).
    if has(&["shared memory budget", "hardware allows", "required feature is unavailable", "max_compute_workgroup", "maxcomputeworkgroup"]) {
        "ai.gpu_limit"
    } else if has(&["device lost", "device was lost", "devicelost"]) {
        "ai.gpu_lost"
    } else if has(&["ai worker crashed", "ai worker stopped"]) {
        "ai.worker_crash"
    } else if has(&["gpu couldn't run", "webgpu", "wgpu"]) {
        "ai.gpu"
    } else if has(&["cancel"]) {
        "cancelled"
    } else if has(&["not available", "disabled"]) {
        "disabled"
    } else if has(&["unsupported", "not supported", "not implemented", "not yet"]) {
        "unsupported"
    } else if has(&["no document", "no active document", "open a document", "no open document"]) {
        "no_document"
    } else if has(&["no layer", "no selection", "no active layer", "nothing to", "no path"]) {
        "no_target"
    } else if has(&["out of memory", "too large", "too big", "exceeds", "limit"]) {
        "too_large"
    } else if has(&["permission", "denied", "not allowed", "not authorized"]) {
        "denied"
    } else if has(&["decode", "corrupt", "malformed", "unexpected end", "truncated", "invalid file", "not a ", "signature"]) {
        "decode"
    } else if has(&["param", "invalid", "expected", "missing", "out of range", "must be", "unknown"]) {
        "invalid_params"
    } else {
        "other"
    }
}

/// A known command id as is; anything else is reduced to its first segment (`plugin.other`),
/// so an id can never carry a name the user typed.
pub fn sanitize_command_id(id: &str) -> String {
    if is_known_command(id) {
        return id.to_string();
    }
    let head = id.split('.').next().unwrap_or("");
    match sanitize_token(head, 24) {
        Some(h) if !h.is_empty() && is_known_prefix(&h) => format!("{h}.other"),
        _ => "other".into(),
    }
}

fn is_known_command(id: &str) -> bool {
    id.len() <= 80
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
        && (photocraft_engine::commands::find(id).is_some()
            || crate::menus::UI_COMMANDS.iter().any(|c| c.0 == id)
            || crate::menu_catalog::CATALOG.iter().any(|c| c.3 == id))
}

fn is_known_prefix(head: &str) -> bool {
    let dotted = format!("{head}.");
    photocraft_engine::commands::command_specs().iter().any(|c| c.id.starts_with(&dotted))
        || crate::menus::UI_COMMANDS.iter().any(|c| c.0.starts_with(&dotted))
        || crate::menu_catalog::CATALOG.iter().any(|c| c.3.starts_with(&dotted))
}

/// `[A-Za-z0-9._-]{1,max}`, or `None`.
fn sanitize_token(s: &str, max: usize) -> Option<String> {
    (!s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))).then(|| s.to_string())
}

/// A tool's telemetry id (`tool.Brush`).
pub fn tool_id(tool: crate::state::Tool) -> String {
    format!("tool.{tool:?}")
}

/// A file's kind from its extension, from a fixed list; anything else is `other`.
pub fn file_kind(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "pcraft" => "pcraft",
        "psd" | "psdt" => "psd",
        "psb" => "psb",
        "png" => "png",
        "jpg" | "jpeg" => "jpg",
        "webp" => "webp",
        "gif" => "gif",
        "tif" | "tiff" => "tif",
        "bmp" => "bmp",
        "tga" => "tga",
        "ico" => "ico",
        "qoi" => "qoi",
        "exr" => "exr",
        "hdr" => "hdr",
        "heic" | "heif" | "hif" => "heic",
        "avif" => "avif",
        "svg" | "svgz" => "svg",
        "ora" => "ora",
        "pdn" => "pdn",
        "pdf" => "pdf",
        "af" | "afdesign" | "afphoto" | "afpub" => "affinity",
        "dng" | "cr2" | "cr3" | "nef" | "nrw" | "arw" | "pef" | "orf" | "rw2" | "raf" => "raw",
        "pbm" | "pgm" | "ppm" | "pam" | "pfm" => "netpbm",
        "fits" | "fit" | "fts" => "fits",
        "abr" | "grd" | "aco" | "ase" | "pat" | "asl" => "preset",
        _ => "other",
    }
}

/// Layered document formats count as saves; flat images as exports.
pub fn is_document_kind(kind: &str) -> bool {
    matches!(kind, "pcraft" | "psd" | "psb" | "ora")
}

pub fn megapixels(width: u32, height: u32) -> f64 {
    f64::from(width) * f64::from(height) / 1_000_000.0
}

pub fn mp_bucket(width: u32, height: u32) -> &'static str {
    mp_bucket_of(megapixels(width, height))
}

fn mp_bucket_of(mp: f64) -> &'static str {
    match mp {
        m if m < 1.0 => "<1",
        m if m < 4.0 => "1-4",
        m if m < 12.0 => "4-12",
        m if m < 24.0 => "12-24",
        m if m < 50.0 => "24-50",
        _ => "50+",
    }
}

pub fn layers_bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1 => "1",
        2..=5 => "2-5",
        6..=20 => "6-20",
        21..=100 => "21-100",
        _ => "100+",
    }
}

pub fn bytes_bucket(n: usize) -> &'static str {
    const MB: usize = 1 << 20;
    match n {
        n if n < MB => "<1MB",
        n if n < 10 * MB => "1-10MB",
        n if n < 50 * MB => "10-50MB",
        n if n < 200 * MB => "50-200MB",
        _ => "200MB+",
    }
}

/// Screen size class by its longer side in CSS pixels.
pub fn screen_bucket(width: u32, height: u32) -> &'static str {
    match width.max(height) {
        0 => "unknown",
        1..=1023 => "<1024",
        1024..=1365 => "1024-1365",
        1366..=1919 => "1366-1919",
        1920..=2559 => "1920-2559",
        _ => "2560+",
    }
}

/// Browser family and major version from a user agent string.
pub fn browser_family(ua: &str) -> (&'static str, u32) {
    // Order matters: Edge, Opera and Samsung also say "Chrome"; Chrome also says "Safari".
    const TOKENS: &[(&str, &str)] = &[
        ("Edg/", "edge"),
        ("EdgA/", "edge"),
        ("EdgiOS/", "edge"),
        ("OPR/", "opera"),
        ("SamsungBrowser/", "samsung"),
        ("Firefox/", "firefox"),
        ("FxiOS/", "firefox"),
        ("CriOS/", "chrome"),
        ("Chrome/", "chrome"),
    ];
    for (token, family) in TOKENS {
        if let Some(at) = ua.find(token) {
            return (family, major_after(ua.get(at + token.len()..).unwrap_or("")));
        }
    }
    if ua.contains("Safari/")
        && let Some(at) = ua.find("Version/")
    {
        return ("safari", major_after(ua.get(at + "Version/".len()..).unwrap_or("")));
    }
    ("other", 0)
}

fn major_after(s: &str) -> u32 {
    let digits: String = s.chars().take_while(char::is_ascii_digit).take(4).collect();
    digits.parse().unwrap_or(0)
}

/// Operating system family from a user agent string.
pub fn os_family(ua: &str) -> &'static str {
    if ua.contains("Windows") {
        "windows"
    } else if ua.contains("Android") {
        "android"
    } else if ua.contains("iPhone") || ua.contains("iPad") || ua.contains("iPod") {
        "ios"
    } else if ua.contains("CrOS") {
        "chromeos"
    } else if ua.contains("Mac OS X") || ua.contains("Macintosh") {
        "macos"
    } else if ua.contains("Linux") || ua.contains("X11") {
        "linux"
    } else {
        "other"
    }
}

/// GPU vendor class from the adapter's PCI vendor id and name (browsers often hide both).
pub fn gpu_vendor_class(vendor: u32, name: &str) -> &'static str {
    match vendor {
        0x10de => return "nvidia",
        0x1002 | 0x1022 => return "amd",
        0x8086 => return "intel",
        0x106b => return "apple",
        0x13b5 => return "arm",
        0x5143 => return "qualcomm",
        0x1010 => return "imgtec",
        0x1414 => return "microsoft",
        _ => {}
    }
    let n = name.to_lowercase();
    let has = |w: &str| n.contains(w);
    if has("nvidia") || has("geforce") || has("quadro") || has("rtx") {
        "nvidia"
    } else if has("amd") || has("radeon") || has("ati ") {
        "amd"
    } else if has("intel") {
        "intel"
    } else if has("apple") {
        "apple"
    } else if has("mali") || has("arm") {
        "arm"
    } else if has("adreno") || has("qualcomm") {
        "qualcomm"
    } else if has("powervr") || has("imagination") {
        "imgtec"
    } else if has("swiftshader") || has("llvmpipe") || has("software") || has("basic render") {
        "software"
    } else if n.trim().is_empty() {
        "unknown"
    } else {
        "other"
    }
}

// ------------------------------------------------------------------ the session's recorder

static ACTIVE: AtomicBool = AtomicBool::new(false);
static RECORDER: Mutex<Option<Recorder>> = Mutex::new(None);
static CLOCK: OnceLock<fn() -> f64> = OnceLock::new();

/// The shell's millisecond clock (the browser's `Date.now`); the shell installs it once.
pub fn set_clock(clock: fn() -> f64) {
    let _ = CLOCK.set(clock);
}

/// Milliseconds on the installed clock.
pub fn now_ms() -> f64 {
    CLOCK.get().map_or_else(crate::gpu_canvas::now_ms, |clock| clock())
}

/// The recorder, without ever blocking: on the web's main thread a contended lock would trap
/// (threads build), so a busy recorder simply misses this record.
fn recorder() -> Option<std::sync::MutexGuard<'static, Option<Recorder>>> {
    match RECORDER.try_lock() {
        Ok(guard) => Some(guard),
        Err(std::sync::TryLockError::Poisoned(e)) => Some(e.into_inner()),
        Err(std::sync::TryLockError::WouldBlock) => None,
    }
}

/// Start recording into `recorder` (replacing any session).
pub fn start(recorder: Recorder) {
    if let Some(mut slot) = self::recorder() {
        *slot = Some(recorder);
        ACTIVE.store(true, Ordering::Relaxed);
    }
}

/// Stop recording and drop whatever was not sent.
pub fn stop() {
    ACTIVE.store(false, Ordering::Relaxed);
    if let Some(mut slot) = recorder() {
        *slot = None;
    }
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Run `f` on the session's recorder, if recording.
pub fn with<R>(f: impl FnOnce(&mut Recorder) -> R) -> Option<R> {
    if !is_active() {
        return None;
    }
    recorder()?.as_mut().map(f)
}

/// The next batch to send, if recording and anything is waiting.
pub fn take_batch() -> Option<String> {
    with(Recorder::take_batch).flatten()
}

thread_local! {
    /// Nesting of command scopes: a menu item that runs an engine command counts once.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// The source the next outermost command is attributed to.
    static SOURCE: Cell<Option<Source>> = const { Cell::new(None) };
}

thread_local! {
    static LAST_TOOL: Cell<Option<crate::state::Tool>> = const { Cell::new(None) };
}

/// Call every frame: counts a tool switch (from the toolbar, a shortcut or a menu) as
/// `tool.<Name>` with source `tool`. The first frame only notes the starting tool.
pub fn observe_tool(tool: crate::state::Tool) {
    if !is_active() {
        return;
    }
    let previous = LAST_TOOL.with(|t| t.replace(Some(tool)));
    if previous.is_some_and(|p| p != tool) {
        with(|r| r.command(&tool_id(tool), Source::Tool));
    }
}

/// Run `f` with its commands attributed to `source` (menus, shortcuts, the palette).
pub fn with_source<R>(source: Source, f: impl FnOnce() -> R) -> R {
    let previous = SOURCE.with(|s| s.replace(Some(source)));
    let r = f();
    SOURCE.with(|s| s.set(previous));
    r
}

/// One command dispatch: the outermost one is counted (with its error class and, when slow,
/// its duration); nested dispatches are not.
#[must_use = "call finish with the command's result"]
pub struct Scope {
    entered: bool,
    outer: bool,
    source: Source,
    start_ms: f64,
}

impl Scope {
    /// `automation`: the app is running a control or embedding request.
    pub fn enter(automation: bool) -> Self {
        if !is_active() {
            return Self { entered: false, outer: false, source: Source::Panel, start_ms: 0.0 };
        }
        let depth = DEPTH.with(|d| {
            let depth = d.get();
            d.set(depth.saturating_add(1));
            depth
        });
        let source = if automation { Source::Automation } else { SOURCE.with(Cell::get).unwrap_or(Source::Panel) };
        Self { entered: true, outer: depth == 0, source, start_ms: now_ms() }
    }

    /// Record the outermost command's outcome. `megapixels` is asked only for slow commands.
    pub fn finish<T>(self, id: &str, result: &Result<T, String>, megapixels: impl FnOnce() -> Option<f64>) {
        if !self.outer {
            return;
        }
        let ms = now_ms() - self.start_ms;
        let source = self.source;
        let mp = if ms >= PERF_MIN_MS { megapixels() } else { None };
        with(|r| {
            r.command(id, source);
            if let Err(e) = result {
                r.error(id, e);
            }
            r.perf(id, ms, mp);
        });
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.entered {
            DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
        }
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
