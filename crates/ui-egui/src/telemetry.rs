//! Anonymous usage statistics: the web build and the official Windows and Linux desktop builds.
//!
//! The platform shell installs a [`Recorder`] with [`start`] once the user's preference
//! (Preferences › General › Send anonymous usage statistics) and the platform's privacy signals
//! allow it, then drains [`take_batch`] every 30 s, off the frame on the desktop
//! (`apps/photocraft/src/usage_stats.rs`) and when the page hides on the web
//! (`apps/photocraft-web/src/usage_stats.rs`). Until then every hook is a relaxed atomic load and
//! nothing else.
//!
//! What is recorded (schema `v: 2`; the collector is a separate service):
//! a random install id and session id, coarse environment buckets, command ids counted per
//! batch (never their params), tool switches, open/save/export kinds with size buckets and
//! durations, error classes (command id + kind, never the message), slow-command timings, and
//! (schema 2) frame-time percentiles per canvas tool: how long the app took per frame while a
//! tool was dragged on the canvas or the view zoomed/scrolled (`frames`, see [`Recorder::frame`]).
//! Schema 3 adds the device class and memory bucket to `session_start`, and to `frames` the true
//! frame rate (intervals between painted frames), dropped frames, the layer-count bucket, the
//! number of gestures and the split of the worst frame (input wait, app logic, UI layout, paint
//! and GPU submission; see [`FrameSample`]).
//! Schema 4 adds `freeze` (the page's main thread blocked for 200 ms or more, with the command,
//! tool and background job running) and `unclean_exit`; schema 5 adds to `freeze` the part of the
//! frame that took longest (`phase`, a [`crate::perf_span`] id) and the user's last action
//! (`action`, a command id or `tool.<Name>`), so no freeze is anonymous.
//! Schema 7 adds the desktop app (`client`), the install channel and hardware names to
//! `session_start`, every command's time per page size (`cmd_perf`), AI runs (`ai_run`), memory
//! high-water marks (`memory`) and the first edit of a session (`edited`).
//! Schema 8 adds `crash` (a panic, a script error, a lost device or an uncaught GPU error, sent the
//! moment it happens: [`crash_event`], [`crash_batch`]), to `unclean_exit` what the app was doing
//! (`stage`, [`Stage`]), the browser's memory and how many unclean exits in a row (`loop`), and to
//! `gpu_lost` its `reason`. A panic's location is kept only inside this source tree
//! ([`crash_location`]), and no byte of a panic's or a driver's message leaves: each is reduced to a
//! fixed code ([`panic_code`], [`gpu_code`]).
//! Without a schema change (the collector refuses a whole batch for an unknown event or field), a
//! canvas press a tool dropped goes out as a `command` with the id `toolDrop.<reason>.tool.<Name>`
//! and source `tool` ([`Recorder::tool_drop`]): the reason is a fixed code, never free text.
//! Never: file names, paths, layer or font names, document content, typed text, error messages.
//! [`Recorder::take_batch`] enforces it a last time: an event with a field outside [`FIELDS`] or a
//! value that looks like a path or a file name is dropped.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Map, Value, json};

/// Version of the batch schema the collector validates.
pub const SCHEMA_VERSION: u32 = 8;
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
/// A batch's events stay at most this many (the collector refuses a whole batch above it).
pub const MAX_BATCH_EVENTS: usize = 300;
/// Distinct frame-statistics keys (canvas tools, `view`) per batch.
pub const MAX_FRAME_KEYS: usize = 24;
/// Frame-time histogram resolution: one bucket per millisecond, the last one open-ended.
const FRAME_BUCKETS: usize = 250;
/// A frame at least this slow is a visible hitch (`long` in `frames`).
pub const LONG_FRAME_MS: f64 = 50.0;
/// The `frames` key of view navigation (wheel zoom and scroll, pinch) without a tool drag.
pub const VIEW_FRAMES: &str = "view";
/// Distinct (command, page size) timings per batch (`cmd_perf`).
pub const MAX_COMMAND_TIMES: usize = 64;
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

/// What was running when the page froze: the last command, the tool, a background job, the part
/// of the frame that took longest (`phase`, a [`crate::perf_span`] id) and the last action the user
/// took however long ago (`action`: a command or a tool switch).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FreezeContext {
    pub command: Option<String>,
    pub tool: Option<String>,
    pub job: Option<String>,
    pub megapixels: Option<f64>,
    pub phase: Option<&'static str>,
    pub action: Option<String>,
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
    /// The desktop app: no browser, `os` instead of the user agent's.
    pub native: bool,
    /// The OS family on the desktop (`windows`, `linux`, …).
    pub os: &'static str,
    /// How this copy was installed: `web-st`, `web-threads`, `msi`, `zip`, `appimage`, `deb`,
    /// `tar` or `source`.
    pub channel: &'static str,
    /// OS name and version ([`os_version_token`]; in browsers parsed from the user agent).
    pub os_version: String,
    /// CPU model, GPU model and driver, as reported: only sent through [`hardware_name`].
    pub cpu: String,
    pub gpu_model: String,
    pub gpu_driver: String,
    /// Installed RAM in GB (0 = not known), bucketed with [`ram_bucket`]. Desktop only.
    pub ram_gb: f64,
    /// Free space where Capishot keeps its data, in GB (`None` = not known), bucketed with
    /// [`disk_bucket`]. Desktop only.
    pub free_disk_gb: Option<f64>,
}

/// The `session_start` event for `env`.
pub fn session_start_event(env: &Environment) -> Value {
    let (browser, major) = if env.native { ("native", 0) } else { browser_family(&env.user_agent) };
    let os = if env.native { env.os } else { os_family(&env.user_agent) };
    let mut e = json!({
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
        "os": os,
        "screen": screen_bucket(env.screen.0, env.screen.1),
        "lang": sanitize_token(&env.language, 12).unwrap_or_else(|| "other".into()),
        "device": if matches!(env.device, "mobile" | "tablet" | "desktop") { env.device } else { "unknown" },
        "mem": memory_bucket(env.memory_gb),
    });
    if matches!(env.channel, "web-st" | "web-threads" | "msi" | "zip" | "appimage" | "deb" | "tar" | "source") {
        e["channel"] = json!(env.channel);
    }
    if let Some(v) = os_version_token(&env.os_version) {
        e["os_ver"] = json!(v);
    }
    for (key, value) in [("cpu", &env.cpu), ("gpu_model", &env.gpu_model), ("gpu_driver", &env.gpu_driver)] {
        if let Some(name) = hardware_name(value) {
            e[key] = json!(name);
        }
    }
    if env.native {
        e["ram"] = json!(ram_bucket(env.ram_gb));
        e["disk"] = json!(env.free_disk_gb.map_or("unknown", disk_bucket));
    }
    e
}

/// A CPU or GPU name or driver version as it may leave the machine: what comes before the first
/// slash, backslash or colon (`GeForce GTX 1050 Ti/PCIe/SSE2`), of the characters such names use,
/// at most 64. `None` when nothing is left. A path can't pass, whatever a driver reports.
pub fn hardware_name(raw: &str) -> Option<String> {
    let head = raw.split(['/', '\\', ':']).next().unwrap_or("");
    let clean: String = head.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '(' | ')' | '+' | '@' | '#' | ',' | '-')).take(64).collect();
    let clean = clean.trim().trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim_end();
    (!clean.is_empty()).then(|| clean.to_string())
}

/// An OS name and version as it may leave the machine: `[A-Za-z0-9._ -]`, at most 24 characters.
/// `None` for anything with a path separator: filtering those out would leave the path's words.
pub fn os_version_token(raw: &str) -> Option<String> {
    if raw.contains(['/', '\\', ':']) {
        return None;
    }
    let clean: String = raw.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '.' | '_' | '-')).take(24).collect();
    let clean = clean.trim().trim_start_matches(|c: char| !c.is_ascii_alphanumeric()).trim_end();
    (!clean.is_empty()).then(|| clean.to_string())
}

/// The OS version a user agent states (`Windows NT 10.0`, `Android 14`, `Mac OS X 10_15_7`,
/// `iPhone OS 17_4`), if any.
pub fn os_version_from_ua(ua: &str) -> String {
    for token in ["Windows NT ", "Android ", "Mac OS X ", "iPhone OS ", "CPU OS ", "CrOS x86_64 "] {
        if let Some(at) = ua.find(token) {
            let rest = ua.get(at + token.len()..).unwrap_or("");
            let version: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_')).take(16).collect();
            return version.replace('_', ".");
        }
    }
    String::new()
}

/// Installed RAM (GB) as a bucket.
pub fn ram_bucket(gb: f64) -> &'static str {
    match gb {
        g if !g.is_finite() || g <= 0.0 => "unknown",
        g if g < 2.0 => "<2",
        g if g < 4.0 => "2-4",
        g if g < 8.0 => "4-8",
        g if g < 16.0 => "8-16",
        g if g < 32.0 => "16-32",
        g if g < 64.0 => "32-64",
        _ => "64+",
    }
}

/// Free disk space (GB) as a bucket.
pub fn disk_bucket(gb: f64) -> &'static str {
    match gb {
        g if !g.is_finite() || g < 0.0 => "unknown",
        g if g < 10.0 => "<10GB",
        g if g < 50.0 => "10-50GB",
        g if g < 200.0 => "50-200GB",
        g if g < 1000.0 => "200GB-1TB",
        _ => "1TB+",
    }
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
    /// `web` or `desktop` ([`Recorder::desktop`]).
    client: &'static str,
    install_id: String,
    session_id: String,
    seq: u64,
    started_ms: f64,
    active_s: u64,
    last_interaction_ms: Option<f64>,
    last_heartbeat_ms: f64,
    commands: BTreeMap<(String, Source), u32>,
    errors: BTreeMap<(String, &'static str), u32>,
    /// Dropped canvas presses since the last batch, by (reason code, tool id).
    tool_drops: BTreeMap<(&'static str, String), u32>,
    events: Vec<Value>,
    perf_queued: usize,
    freezes_queued: usize,
    frames: BTreeMap<String, FrameHist>,
    dropped: u32,
    ended: bool,
    /// The display's frame period as observed (the shortest interval between painted frames
    /// seen, at least [`MIN_VSYNC_MS`]): what a dropped frame is measured against.
    vsync_ms: f64,
    /// Every command's time since the last batch, per page-size bucket (a short list: looked up
    /// without allocating).
    command_times: Vec<(String, Option<&'static str>, TimeHist)>,
    /// Memory high-water marks (MB), and the ones last reported.
    ram_mb: u64,
    gpu_mb: u64,
    reported_memory: (u64, u64),
}

/// Times on a log scale (three buckets per doubling; [`TimeHist::add`] caps a time at an hour), fixed size.
#[derive(Clone, Debug)]
struct TimeHist {
    buckets: [u32; TimeHist::LEN],
    n: u32,
    max_ms: f64,
}

impl TimeHist {
    const LEN: usize = 80;

    fn new() -> Self {
        Self { buckets: [0; Self::LEN], n: 0, max_ms: 0.0 }
    }

    fn add(&mut self, ms: f64) {
        let ms = if ms.is_finite() && ms > 0.0 { ms.min(3_600_000.0) } else { 0.0 };
        let bucket = (((ms + 1.0).log2() * 3.0) as usize).min(Self::LEN - 1);
        if let Some(slot) = self.buckets.get_mut(bucket) {
            *slot = slot.saturating_add(1);
        }
        self.n = self.n.saturating_add(1);
        self.max_ms = self.max_ms.max(ms);
    }

    /// The `p` quantile (0..1): its bucket's upper edge, never above the largest time seen.
    fn quantile(&self, p: f64) -> u64 {
        let want = ((f64::from(self.n) * p).ceil() as u64).max(1);
        let mut seen = 0u64;
        for (b, &c) in self.buckets.iter().enumerate() {
            seen += u64::from(c);
            if seen >= want {
                let edge = 2f64.powf((b as f64 + 1.0) / 3.0) - 1.0;
                return round_ms(edge.min(self.max_ms));
            }
        }
        round_ms(self.max_ms)
    }

    fn merge(&mut self, other: &TimeHist) {
        for (a, b) in self.buckets.iter_mut().zip(other.buckets.iter()) {
            *a = a.saturating_add(*b);
        }
        self.n = self.n.saturating_add(other.n);
        self.max_ms = self.max_ms.max(other.max_ms);
    }
}

/// Every field a batch's events may carry. A field outside this list drops its event: adding one
/// is a decision (and a line in README › Privacy), not a side effect.
pub const FIELDS: &[&str] = &[
    "e", "app", "commit", "threads", "coi", "cores", "gpu", "gpu_vendor", "browser", "browser_major", "os", "screen", "lang", "device", "mem", "channel", "os_ver",
    "cpu", "gpu_model", "gpu_driver", "ram", "disk", "active_s", "total_s", "id", "src", "n", "kind", "bytes", "mp", "layers", "ms", "ok", "op", "tool", "p50", "p95",
    "max", "long", "lat50", "lat95", "fps", "int95", "drop", "g", "w_in", "w_logic", "w_ui", "w_paint", "cmd", "job", "phase", "action", "api", "cause", "memMB", "upMin",
    "docs", "model", "region", "fell_back", "lite", "ram_mb", "gpu_mb", "loc", "msg", "reason", "stage", "js_mb", "dev_gb", "loop",
];

/// Fields holding a command or tool id, which [`sanitize_command_id`] already reduced to known ids
/// (some of which end like a file name: `file.export.png`).
const ID_FIELDS: &[&str] = &["id", "op", "cmd", "job", "action", "tool"];

/// Whether an event may leave the machine: known fields only, and no string that looks like a
/// path, a file name or free text.
fn is_clean_event(event: &Value) -> bool {
    let Some(fields) = event.as_object() else { return false };
    fields.iter().all(|(key, value)| {
        FIELDS.contains(&key.as_str())
            && match value {
                Value::String(text) => is_clean_text(key, text),
                Value::Array(_) | Value::Object(_) => false,
                _ => true,
            }
    })
}

fn is_clean_text(key: &str, text: &str) -> bool {
    // A crash's location is the one value shaped like a path: only this tree's sources pass.
    if key == "loc" {
        return is_crash_location(text);
    }
    text.len() <= 80 && !text.contains(['/', '\\', ':', '\n']) && (ID_FIELDS.contains(&key) || file_kind(text) == "other")
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
            client: "web",
            install_id: sanitize_token(install_id, 64).unwrap_or_default(),
            session_id: sanitize_token(session_id, 64).unwrap_or_default(),
            seq: 0,
            started_ms: now_ms,
            active_s: 0,
            last_interaction_ms: None,
            last_heartbeat_ms: now_ms,
            commands: BTreeMap::new(),
            errors: BTreeMap::new(),
            tool_drops: BTreeMap::new(),
            events: Vec::new(),
            perf_queued: 0,
            freezes_queued: 0,
            frames: BTreeMap::new(),
            dropped: 0,
            ended: false,
            vsync_ms: DEFAULT_VSYNC_MS,
            command_times: Vec::new(),
            ram_mb: 0,
            gpu_mb: 0,
            reported_memory: (0, 0),
        }
    }

    /// This session's batches say they come from the desktop app.
    pub fn desktop(mut self) -> Self {
        self.client = "desktop";
        self
    }

    /// One run of a command, whatever it took: summarised per id and page size at the batch.
    pub fn command_time(&mut self, id: &str, ms: f64, megapixels: Option<f64>) {
        if self.ended || !ms.is_finite() {
            return;
        }
        let mp = megapixels.map(mp_bucket_of);
        if let Some((_, _, hist)) = self.command_times.iter_mut().find(|(i, m, _)| i == id && *m == mp) {
            hist.add(ms);
        } else if self.command_times.len() >= MAX_COMMAND_TIMES {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            let mut hist = TimeHist::new();
            hist.add(ms);
            self.command_times.push((id.to_string(), mp, hist));
        }
    }

    /// The first change to a document in this session.
    pub fn edited(&mut self) {
        self.event(json!({"e": "edited"}));
    }

    /// One AI job: `model` a catalog id, `device` from [`ai_device_class`], `region` the size of
    /// what it worked on.
    pub fn ai_run(&mut self, run: &AiRun<'_>) {
        let mut e = json!({"e": "ai_run", "id": sanitize_command_id(run.command), "ms": round_ms(run.ms), "ok": run.ok});
        if let Some(model) = run.model.and_then(|m| sanitize_token(m, 40)) {
            e["model"] = json!(model);
        }
        if let Some(device) = run.device {
            e["device"] = json!(device);
        }
        if let Some(mp) = run.megapixels {
            e["region"] = json!(mp_bucket_of(mp));
        }
        if let Some(fell_back) = run.fell_back {
            e["fell_back"] = json!(fell_back);
        }
        if let Some(lite) = run.lite {
            e["lite"] = json!(lite);
        }
        if let Some(why) = run.fallback {
            e["reason"] = json!(why);
        }
        self.event(e);
    }

    /// The process's memory now (RSS on the desktop, the wasm memory on the web), in MB.
    pub fn note_ram(&mut self, mb: u64) {
        self.ram_mb = self.ram_mb.max(mb);
    }

    /// The canvas textures' memory now, in MB.
    pub fn note_gpu(&mut self, mb: u64) {
        self.gpu_mb = self.gpu_mb.max(mb);
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
        for (key, value) in [("cmd", &ctx.command), ("job", &ctx.job), ("action", &ctx.action)] {
            if let Some(v) = value {
                e[key] = json!(sanitize_action_id(v));
            }
        }
        if let Some(tool) = &ctx.tool {
            e["tool"] = json!(sanitize_tool_id(tool));
        }
        if let Some(phase) = ctx.phase.and_then(|p| sanitize_token(p, 40)) {
            e["phase"] = json!(phase);
        }
        if let Some(mp) = ctx.megapixels {
            e["mp"] = json!(mp_bucket_of(mp));
        }
        self.event(e);
    }

    /// A canvas press with `tool` (its [`tool_id`]) came to nothing for `reason`, a fixed code
    /// (`canvas::press_watch`). Counted per batch, under [`MAX_KEYS`] distinct keys.
    pub fn tool_drop(&mut self, reason: &'static str, tool: &str) {
        let key = (reason, sanitize_tool_id(tool));
        if self.tool_drops.len() < MAX_KEYS || self.tool_drops.contains_key(&key) {
            let n = self.tool_drops.entry(key).or_default();
            *n = n.saturating_add(1);
        }
    }

    /// The GPU canvas gave way to the CPU one: `api` "webgl" or "webgpu" (or a native backend),
    /// `gpu` the renderer class ([`gpu_vendor_class`]), `cause` "context_lost" (the black-canvas
    /// class: the browser dropped the graphics context), "slow_pipelines" (a canvas pipeline took
    /// over 2 s to build) or "weak_gpu" (the start-up check chose the CPU canvas), `phase` what
    /// the last frame spent its time on, `n` the times it happened this session, `ms` how long
    /// the slow step took. Each kind at most once a batch.
    pub fn gpu_lost(&mut self, api: &str, gpu: &str, cause: &str, phase: Option<&str>, n: u32, ms: Option<f64>) {
        let mut e = json!({"e": "gpu_lost", "api": sanitize_token(api, 16).unwrap_or_else(|| "unknown".into()), "gpu": sanitize_token(gpu, 16).unwrap_or_else(|| "unknown".into()),
            "cause": sanitize_token(cause, 24).unwrap_or_else(|| "unknown".into()), "n": n.min(99)});
        if let Some(phase) = phase.and_then(|p| sanitize_token(p, 40)) {
            e["phase"] = json!(phase);
        }
        if let Some(ms) = ms.filter(|m| m.is_finite()) {
            e["ms"] = json!(round_ms(ms));
        }
        if self.events.iter().any(|x| x.get("e") == e.get("e") && x.get("cause") == e.get("cause")) {
            return;
        }
        self.event(e);
    }

    /// Why the `gpu_lost` of `cause` queued in this batch happened: a [`gpu_code`].
    pub fn gpu_lost_reason(&mut self, cause: &str, reason: &str) {
        if let Some(e) = self.events.iter_mut().rev().find(|e| e.get("e").and_then(Value::as_str) == Some("gpu_lost") && e.get("cause").and_then(Value::as_str) == Some(cause)) {
            e["reason"] = json!(gpu_code(reason));
        }
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
        self.events.is_empty() && self.commands.is_empty() && self.errors.is_empty() && self.tool_drops.is_empty() && self.frames.is_empty() && self.command_times.is_empty() && self.dropped == 0
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
            let id = if source == Source::Tool { sanitize_tool_id(&id) } else { sanitize_command_id(&id) };
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
        events.extend(std::mem::take(&mut self.tool_drops).into_iter().map(|((reason, tool), n)| json!({"e": "command", "id": format!("toolDrop.{reason}.{tool}"), "src": "tool", "n": n})));
        for (key, h) in std::mem::take(&mut self.frames) {
            if h.n == 0 {
                continue;
            }
            let key = if key == VIEW_FRAMES {
                key
            } else if key.starts_with("tool.") {
                sanitize_tool_id(&key)
            } else {
                "other".into()
            };
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
        let mut times: Vec<(String, Option<&'static str>, TimeHist)> = Vec::new();
        for (id, mp, hist) in std::mem::take(&mut self.command_times) {
            let id = sanitize_command_id(&id);
            if let Some((_, _, merged)) = times.iter_mut().find(|(i, m, _)| *i == id && *m == mp) {
                merged.merge(&hist);
            } else {
                times.push((id, mp, hist));
            }
        }
        events.extend(times.into_iter().map(|(id, mp, h)| {
            let mut e = json!({"e": "cmd_perf", "id": id, "n": h.n, "p50": h.quantile(0.5), "p95": h.quantile(0.95), "max": round_ms(h.max_ms)});
            if let Some(mp) = mp {
                e["mp"] = json!(mp);
            }
            e
        }));
        // Memory only when a high-water mark grew by at least 5% (and 16 MB) since it was reported.
        let grew = |now: u64, then: u64| now > then.saturating_add((then / 20).max(16));
        if grew(self.ram_mb, self.reported_memory.0) || grew(self.gpu_mb, self.reported_memory.1) {
            self.reported_memory = (self.ram_mb, self.gpu_mb);
            let mut e = json!({"e": "memory", "ram_mb": self.ram_mb});
            if self.gpu_mb > 0 {
                e["gpu_mb"] = json!(self.gpu_mb);
            }
            events.push(e);
        }
        self.perf_queued = 0;
        self.freezes_queued = 0;
        let mut dropped = std::mem::take(&mut self.dropped);
        // The last line of defence: nothing but known fields and harmless values leaves.
        let before = events.len();
        events.retain(is_clean_event);
        dropped = dropped.saturating_add((before - events.len()) as u32);
        if events.len() > MAX_BATCH_EVENTS {
            dropped = dropped.saturating_add((events.len() - MAX_BATCH_EVENTS) as u32);
            events.truncate(MAX_BATCH_EVENTS);
        }
        self.seq = self.seq.saturating_add(1);
        loop {
            let mut batch = Map::new();
            batch.insert("v".into(), json!(SCHEMA_VERSION));
            batch.insert("client".into(), json!(self.client));
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
        // Why it stopped, by its class: memory (a phone's worker killed or out of wasm memory), a
        // panic, a file of another build it couldn't fetch.
        if has(&["out of memory", "memory access out of bounds", "allocation failed", "could not allocate", "array buffer allocation"]) {
            "ai.worker_oom"
        } else if has(&["unreachable", "panicked"]) {
            "ai.worker_panic"
        } else if has(&["failed to fetch", "404", "networkerror"]) {
            "ai.worker_load"
        } else {
            "ai.worker_crash"
        }
    } else if has(&["gpu couldn't run", "webgpu", "wgpu"]) {
        "ai.gpu"
    } else if has(&["still running"]) {
        // Refused while a job has the document: its message offers to cancel, nothing was.
        "busy"
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
/// A tool's id ([`tool_id`]) as it is, anything else `tool.other`.
pub fn sanitize_tool_id(id: &str) -> String {
    if crate::state::Tool::ALL.iter().any(|t| tool_id(*t) == id) { id.to_string() } else { "tool.other".into() }
}

/// A command id ([`sanitize_command_id`]) or a tool switch ([`sanitize_tool_id`]).
pub fn sanitize_action_id(id: &str) -> String {
    if id.starts_with("tool.") && !is_known_command(id) { sanitize_tool_id(id) } else { sanitize_command_id(id) }
}

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
/// This session's first edit was recorded.
static EDITED: AtomicBool = AtomicBool::new(false);

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
        EDITED.store(false, Ordering::Relaxed);
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

/// This installation's support code: the start of its telemetry install id, grouped
/// (`a1b2-c3d4-e5f6`), which finds every session it sent. `None` while usage statistics are off.
pub fn support_code() -> Option<String> {
    let id = with(|r| r.install_id.clone())?;
    let hex: String = id.chars().filter(char::is_ascii_hexdigit).take(12).collect();
    (hex.len() == 12).then(|| format!("{}-{}-{}", &hex[0..4], &hex[4..8], &hex[8..12]))
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

    /// Record the outermost command's outcome.
    pub fn finish<T>(self, id: &str, result: &Result<T, String>, megapixels: impl FnOnce() -> Option<f64>) {
        if !self.outer {
            return;
        }
        let ms = now_ms() - self.start_ms;
        let source = self.source;
        let mp = megapixels();
        with(|r| {
            r.command(id, source);
            if let Err(e) = result {
                r.error(id, e);
            }
            r.perf(id, ms, mp);
            r.command_time(id, ms, mp);
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

// ------------------------------------------------------------------ what both shells report

/// Every frame, from either shell: tool switches, that the user interacted, the first edit.
pub fn observe_frame(app: &crate::PhotocraftApp, ctx: &egui::Context) {
    if !is_active() {
        return;
    }
    observe_tool(app.ui.tool);
    if ctx.input(|i| i.events.iter().any(is_interaction)) {
        let now = now_ms();
        with(|r| r.interaction(now));
    }
    if !EDITED.load(Ordering::Relaxed) && app.session.active().is_some_and(photocraft_engine::DocState::is_dirty) {
        EDITED.store(true, Ordering::Relaxed);
        with(Recorder::edited);
    }
}

fn is_interaction(e: &egui::Event) -> bool {
    matches!(
        e,
        egui::Event::PointerButton { .. }
            | egui::Event::PointerMoved(_)
            | egui::Event::MouseWheel { .. }
            | egui::Event::Key { .. }
            | egui::Event::Text(_)
            | egui::Event::Touch { .. }
            | egui::Event::Zoom(_)
    )
}

/// A document was opened: `file` is only used for its extension; never its name, the document's
/// layer names or anything else in it.
pub fn record_doc_open(file: &str, bytes: usize, result: Result<&photocraft_doc::Document, &String>, ms: f64) {
    record_doc_open_stats(file, bytes, result.map(|doc| Some((doc.size.width, doc.size.height, doc.layer_count()))), ms);
}

/// [`record_doc_open`] from the document's size and layer count (`None`: it waits for a prompt).
pub fn record_doc_open_stats(file: &str, bytes: usize, result: Result<Option<(u32, u32, usize)>, &String>, ms: f64) {
    if !is_active() {
        return;
    }
    let mut e = json!({"e": "doc_open", "kind": file_kind(file), "bytes": bytes_bucket(bytes), "ms": round_ms(ms), "ok": result.is_ok()});
    if let Ok(Some((width, height, layers))) = result {
        e["mp"] = json!(mp_bucket(width, height));
        e["layers"] = json!(layers_bucket(layers));
    }
    with(|r| {
        r.event(e);
        if let Err(message) = result {
            r.error("file.open", message);
        }
    });
}

/// A document was encoded for saving (layered formats) or exporting (flat images): `file` is only
/// used for its extension, `bytes` is the encoded size when known.
pub fn record_doc_saved(file: &str, (width, height): (u32, u32), layers: usize, bytes: Option<usize>, ok: Result<(), &String>, ms: f64) {
    if !is_active() {
        return;
    }
    let kind = file_kind(file);
    let (event, id) = if is_document_kind(kind) { ("doc_save", "file.save") } else { ("export", "file.export") };
    let mut e = json!({
        "e": event, "kind": kind, "mp": mp_bucket(width, height),
        "layers": layers_bucket(layers), "ms": round_ms(ms), "ok": ok.is_ok(),
    });
    if let Some(bytes) = bytes {
        e["bytes"] = json!(bytes_bucket(bytes));
    }
    with(|r| {
        r.event(e);
        if let Err(message) = ok {
            r.error(id, message);
        }
    });
}

/// One AI job for [`Recorder::ai_run`].
#[derive(Clone, Copy, Debug, Default)]
pub struct AiRun<'a> {
    pub command: &'a str,
    pub model: Option<&'a str>,
    pub device: Option<&'static str>,
    pub megapixels: Option<f64>,
    pub ms: f64,
    pub ok: bool,
    pub fell_back: Option<bool>,
    pub lite: Option<bool>,
    /// Why it left the GPU, as a class: never the message.
    pub fallback: Option<&'static str>,
}

/// The class of a GPU failure's message, for [`AiRun::fallback`].
fn fallback_class(message: &str) -> &'static str {
    let m = message.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if has(&["out of memory", "not enough memory", "e_outofmemory", "0x8007000e", "allocation"]) {
        "oom"
    } else if has(&["device lost", "device was lost", "removed", "hung", "0x887a00"]) {
        "lost"
    } else if has(&["loadlibrary", "couldn't load", "could not load", "dll", "not found", "onnxruntime"]) {
        "library"
    } else if has(&["session", "operator", "not implemented", "unsupported"]) {
        "model"
    } else {
        "other"
    }
}

/// The jobs that are AI runs.
fn is_ai_job(command: &str) -> bool {
    matches!(command, photocraft_engine::ai_cmds::COMMAND | photocraft_engine::ai_inpaint_cmds::ID | photocraft_engine::remove_ai_cmds::ID)
}

/// Where an AI run ended as a class, from the reply's `device` (a free string: the adapter's name
/// on some paths) and `cpu`.
pub fn ai_device_class(device: &str, cpu: bool) -> &'static str {
    let d = device.to_ascii_lowercase();
    if cpu {
        "cpu"
    } else if d.contains("directml") {
        "directml"
    } else if d.contains("webgpu") {
        "webgpu"
    } else if d.contains("webgl") {
        "webgl"
    } else if d.contains("wgpu") {
        "wgpu"
    } else if d.contains("gpu") {
        "gpu"
    } else {
        "other"
    }
}

/// An AI job ended: `reply` its result (`None`: it failed), `doc_mp` the document's size for
/// replies that don't state their region. Cancelled jobs aren't reported.
pub fn record_ai_job(command: &str, reply: Option<&Value>, doc_mp: Option<f64>, ms: f64) {
    if !is_active() || !is_ai_job(command) {
        return;
    }
    // Nothing to fill: the model never ran.
    if reply.and_then(|r| r.get("changed")).and_then(Value::as_bool) == Some(false) {
        return;
    }
    let text = |key: &str| reply.and_then(|r| r.get(key)).and_then(Value::as_str);
    let flag = |key: &str| reply.and_then(|r| r.get(key)).and_then(Value::as_bool);
    let region = reply.and_then(|r| r.get("region")).and_then(Value::as_array).and_then(|a| {
        let side = |i: usize| a.get(i).and_then(Value::as_f64);
        Some(side(2)? * side(3)? / 1_000_000.0)
    });
    let run = AiRun {
        command,
        model: text("model"),
        device: text("device").map(|d| ai_device_class(d, flag("cpu").unwrap_or(false))),
        megapixels: region.or(doc_mp),
        ms,
        ok: reply.is_some(),
        fell_back: flag("fellBack"),
        lite: flag("lite"),
        fallback: reply.and_then(|r| r.get("notices")).map(|n| fallback_class(&n.to_string())),
    };
    with(|r| r.ai_run(&run));
}

/// A previous run of this install died without closing, as `unclean_exit`: its last command and
/// tool, memory high-water mark, uptime and open documents (counts only). `panic`: it reported a
/// panic on the way (`None`: not known, the web's breadcrumb).
pub fn unclean_exit_event(cmd: Option<&str>, tool: Option<&str>, mem_mb: Option<u64>, up_min: Option<u64>, docs: Option<u64>, panic: Option<bool>) -> Value {
    let mut e = json!({
        "e": "unclean_exit",
        "cmd": cmd.filter(|c| !c.is_empty()).map(sanitize_action_id),
        "tool": tool.and_then(|t| sanitize_token(t, 40)),
        "memMB": mem_mb,
        "upMin": up_min,
        "docs": docs,
    });
    if let Some(panic) = panic {
        e["src"] = json!(if panic { "panic" } else { "killed" });
    }
    e
}

/// Adds to an `unclean_exit` what the crash breadcrumb knows beyond [`unclean_exit_event`]: the
/// last [`Stage`], the browser's memory (`js_mb`: `performance.memory`, `dev_gb`:
/// `navigator.deviceMemory`) and the unclean exits in a row, this one included. A breadcrumb is
/// read back from storage, so nothing in it is trusted.
pub fn unclean_exit_detail(event: &mut Value, stage: Option<&str>, js_mb: Option<u64>, dev_gb: Option<f64>, crash_loop: Option<u64>) {
    if let Some(stage) = stage.filter(|s| Stage::is_name(s)) {
        event["stage"] = json!(stage);
    }
    if let Some(mb) = js_mb {
        event["js_mb"] = json!(mb.min(10_000_000));
    }
    if let Some(gb) = dev_gb.filter(|g| g.is_finite() && *g > 0.0) {
        event["dev_gb"] = json!(gb.min(1024.0));
    }
    if let Some(n) = crash_loop.filter(|n| *n > 0) {
        event["loop"] = json!(n.min(1000));
    }
}

// ------------------------------------------------------------------ crash detail (schema 8)

/// What the app was doing, for a crash report: the last stage entered wins (several can overlap),
/// and leaving one goes back to where the page rests: [`Stage::Picker`] or [`Stage::Reading`] while
/// the file picker is out, [`Stage::Hidden`] in the background, otherwise [`Stage::Idle`]. A page
/// that died resting in `Picker` or `Hidden` was reclaimed or closed from the background, which is
/// not a crash ([`Stage::is_background`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Boot,
    FirstFrame,
    Idle,
    Opening,
    Decoding,
    Composing,
    Prewarm,
    RecoveryLoad,
    RecoveryCheckpoint,
    Ai,
    /// The browser's file picker is out, which on a phone puts the page behind it.
    Picker,
    /// The picked files are being read in.
    Reading,
    /// The page is in the background: no frames, so no heartbeat.
    Hidden,
}

impl Stage {
    const ALL: [Stage; 13] = [
        Stage::Boot,
        Stage::FirstFrame,
        Stage::Idle,
        Stage::Opening,
        Stage::Decoding,
        Stage::Composing,
        Stage::Prewarm,
        Stage::RecoveryLoad,
        Stage::RecoveryCheckpoint,
        Stage::Ai,
        Stage::Picker,
        Stage::Reading,
        Stage::Hidden,
    ];

    /// Its name in `stage` (the collector accepts any `[a-z_]{1,24}`).
    pub fn name(self) -> &'static str {
        match self {
            Stage::Boot => "boot",
            Stage::FirstFrame => "first_frame",
            Stage::Idle => "idle",
            Stage::Opening => "opening",
            Stage::Decoding => "decoding",
            Stage::Composing => "composing",
            Stage::Prewarm => "prewarm",
            Stage::RecoveryLoad => "recovery_load",
            Stage::RecoveryCheckpoint => "recovery_checkpoint",
            Stage::Ai => "ai",
            Stage::Picker => "picker",
            Stage::Reading => "reading",
            Stage::Hidden => "hidden",
        }
    }

    fn is_name(name: &str) -> bool {
        Self::ALL.iter().any(|s| s.name() == name)
    }

    /// A page last seen in stage `name` wasn't doing anything of its own: the phone took it back
    /// behind the picker or in the background, or the user closed it from there.
    pub fn is_background(name: &str) -> bool {
        name == Stage::Picker.name() || name == Stage::Hidden.name()
    }
}

/// The stage, and where the page rests (a state of its own for the tests: stages change from every
/// test that opens a document).
struct Stages {
    now: AtomicU8,
    /// [`Stage::Picker`] or [`Stage::Reading`] while the file picker's work is going on, else `Idle`.
    page: AtomicU8,
    hidden: AtomicBool,
}

impl Stages {
    const fn new() -> Self {
        Self { now: AtomicU8::new(Stage::Boot as u8), page: AtomicU8::new(Stage::Idle as u8), hidden: AtomicBool::new(false) }
    }

    fn name(&self) -> &'static str {
        let now = self.now.load(Ordering::Relaxed);
        Stage::ALL.iter().find(|s| **s as u8 == now).map_or("idle", |s| s.name())
    }

    /// Where the page rests when no work is going on.
    fn resting(&self) -> Stage {
        let page = self.page.load(Ordering::Relaxed);
        if page == Stage::Picker as u8 {
            Stage::Picker
        } else if page == Stage::Reading as u8 {
            Stage::Reading
        } else if self.hidden.load(Ordering::Relaxed) {
            Stage::Hidden
        } else {
            Stage::Idle
        }
    }

    /// Whether this is a change.
    fn enter(&self, stage: Stage) -> bool {
        self.now.swap(stage as u8, Ordering::Relaxed) != stage as u8
    }

    /// Back to where the page rests, if still in `stage`: that resting stage, when it needs writing
    /// now (resting away from the front, the page draws no frame to write it at).
    fn leave(&self, stage: Stage) -> Option<Stage> {
        let rest = self.resting();
        let left = self.now.compare_exchange(stage as u8, rest as u8, Ordering::Relaxed, Ordering::Relaxed).is_ok();
        (left && rest != Stage::Idle).then_some(rest)
    }

    /// Goes to where the page now rests, unless it is at work: that work's end comes back to it.
    /// The new stage, if it changed.
    fn settle(&self) -> Option<Stage> {
        let rest = self.resting();
        let now = self.now.load(Ordering::Relaxed);
        let resting = [Stage::Boot, Stage::Idle, Stage::Picker, Stage::Reading, Stage::Hidden].iter().any(|s| *s as u8 == now);
        (resting && now != rest as u8 && self.now.compare_exchange(now, rest as u8, Ordering::Relaxed, Ordering::Relaxed).is_ok()).then_some(rest)
    }

    fn set_page(&self, stage: Option<Stage>) -> Option<Stage> {
        self.page.store(stage.unwrap_or(Stage::Idle) as u8, Ordering::Relaxed);
        self.settle()
    }

    fn set_hidden(&self, hidden: bool) -> Option<Stage> {
        self.hidden.store(hidden, Ordering::Relaxed);
        self.settle()
    }
}

static STAGES: Stages = Stages::new();
/// Told when a stage is entered: the web's breadcrumb, which must be written before the work that
/// may kill the page, not at the next heartbeat.
static STAGE_HOOK: OnceLock<fn(&'static str)> = OnceLock::new();

/// See [`STAGE_HOOK`].
pub fn set_stage_hook(hook: fn(&'static str)) {
    let _ = STAGE_HOOK.set(hook);
}

fn tell_stage_hook(stage: Option<Stage>) {
    if let (Some(stage), Some(hook)) = (stage, STAGE_HOOK.get()) {
        hook(stage.name());
    }
}

/// The app enters `stage`. Costs an atomic store (and the hook, on a change).
pub fn set_stage(stage: Stage) {
    tell_stage_hook(STAGES.enter(stage).then_some(stage));
}

/// The app left `stage`; a later stage entered meanwhile stays.
pub fn leave_stage(stage: Stage) {
    tell_stage_hook(STAGES.leave(stage));
}

/// The file picker came out ([`Stage::Picker`]), its files are being read ([`Stage::Reading`]), or
/// that is over (`None`).
pub fn set_page_stage(stage: Option<Stage>) {
    tell_stage_hook(STAGES.set_page(stage));
}

/// The page went to the background or came back.
pub fn set_page_hidden(hidden: bool) {
    tell_stage_hook(STAGES.set_hidden(hidden));
}

/// The current stage's name.
pub fn stage_name() -> &'static str {
    STAGES.name()
}

/// In `stage` until dropped ([`stage_scope`]).
pub struct StageScope(Stage);

impl Drop for StageScope {
    fn drop(&mut self) {
        leave_stage(self.0);
    }
}

impl StageScope {
    /// Leaves `stage` when dropped, for a stage already entered.
    pub fn leaving(stage: Stage) -> Self {
        Self(stage)
    }
}

/// Enters `stage` until the returned guard drops.
pub fn stage_scope(stage: Stage) -> StageScope {
    set_stage(stage);
    StageScope(stage)
}

/// An AI job ended: its stage is over.
pub fn leave_ai_stage(command: &str) {
    if is_ai_job(command) || crate::ai_progress::is_ai(command, &Value::Null) {
        leave_stage(Stage::Ai);
    }
}

/// A `crash`'s `loc` for code outside this tree (a dependency, the standard library): their paths
/// name the machine that built them, and a location in them says little anyway.
pub const EXTERNAL_LOCATION: &str = "external";

/// `crates/gpu/src/health.rs:12` for a source of this tree (as the compiler names one: relative to
/// the workspace), anything else [`EXTERNAL_LOCATION`].
pub fn crash_location(file: &str, line: u32) -> String {
    let location = format!("{}:{line}", file.replace('\\', "/"));
    if is_crash_location(&location) { location } else { EXTERNAL_LOCATION.to_string() }
}

/// Whether `text` is a `loc`: [`EXTERNAL_LOCATION`] or `<crates|apps|xtask>/…/<file>.rs:<line>`.
fn is_crash_location(text: &str) -> bool {
    if text == EXTERNAL_LOCATION {
        return true;
    }
    let Some((file, line)) = text.rsplit_once(':') else { return false };
    text.len() <= 100
        && !line.is_empty()
        && line.len() <= 7
        && line.bytes().all(|b| b.is_ascii_digit())
        && (file.starts_with("crates/") || file.starts_with("apps/") || file.starts_with("xtask/"))
        && file.ends_with(".rs")
        && !file.contains("..")
        && !file.contains("//")
        && file.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
}

/// What a panic's message is reduced to. Only ever one of these fixed strings: a message may quote
/// a file or layer name, so none of its text leaves.
const PANIC_CODES: &[&str] =
    &["unwrap_none", "unwrap_err", "index_oob", "slice_range", "str_boundary", "div_zero", "overflow", "capacity", "alloc", "refcell", "poisoned", "assert", "unreachable", "unimplemented", "other"];
/// What a GPU driver's or wgpu's message is reduced to, likewise.
const GPU_CODES: &[&str] = &["oom", "device_removed", "device_hung", "device_reset", "timeout", "limit", "invalid_object", "destroyed", "other"];

/// The first bytes of a message, lower case, to classify.
fn message_head(message: &str) -> String {
    message.chars().take(300).collect::<String>().to_ascii_lowercase()
}

/// The class of a panic's message.
pub fn panic_code(message: &str) -> &'static str {
    let m = message_head(message);
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["option::unwrap()"]) {
        "unwrap_none"
    } else if has(&["result::unwrap()"]) {
        "unwrap_err"
    } else if has(&["index out of bounds"]) {
        "index_oob"
    } else if has(&["char boundary", "byte index"]) {
        "str_boundary"
    } else if has(&["range end index", "range start index", "slice index"]) {
        "slice_range"
    } else if has(&["divide by zero", "divisor of zero"]) {
        "div_zero"
    } else if has(&["capacity overflow"]) {
        "capacity"
    } else if has(&["with overflow"]) {
        "overflow"
    } else if has(&["memory allocation", "out of memory"]) {
        "alloc"
    } else if has(&["already borrowed", "already mutably borrowed"]) {
        "refcell"
    } else if has(&["poison"]) {
        "poisoned"
    } else if has(&["assertion"]) {
        "assert"
    } else if has(&["entered unreachable"]) {
        "unreachable"
    } else if has(&["not implemented", "not yet implemented"]) {
        "unimplemented"
    } else {
        "other"
    }
}

/// The class of a GPU driver's or wgpu's message.
pub fn gpu_code(message: &str) -> &'static str {
    let m = message_head(message);
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["out of memory", "out-of-memory"]) {
        "oom"
    } else if has(&["removed"]) {
        "device_removed"
    } else if has(&["hung"]) {
        "device_hung"
    } else if has(&["reset"]) {
        "device_reset"
    } else if has(&["timeout", "timed out", "tdr"]) {
        "timeout"
    } else if has(&["limit", "exceeds", "too large"]) {
        "limit"
    } else if has(&["destroyed"]) {
        "destroyed"
    } else if has(&["invalid"]) {
        "invalid_object"
    } else {
        "other"
    }
}

/// A panic's location and class, from the panic hook.
pub fn panic_report(info: &std::panic::PanicHookInfo<'_>) -> (String, &'static str) {
    let location = info.location().map_or_else(|| EXTERNAL_LOCATION.to_string(), |l| crash_location(l.file(), l.line()));
    let payload = info.payload();
    let message = payload.downcast_ref::<&str>().copied().or_else(|| payload.downcast_ref::<String>().map(String::as_str)).unwrap_or("");
    (location, panic_code(message))
}

/// A `crash` event. `kind`: "panic", "gpu_lost" or "gpu_error" ("js" comes from the page's loader,
/// `boot.js`); `loc` a [`crash_location`], `msg` a [`panic_code`] or [`gpu_code`], `reason` what the
/// GPU API said (`unknown`, `destroyed`, `context_lost`, `validation`, `out_of_memory`, `internal`),
/// `stage` a [`Stage`] name. These may come back from a breadcrumb in storage, so whatever isn't
/// exactly that is left out: the collector refuses a whole batch for one bad value.
pub fn crash_event(kind: &str, loc: Option<&str>, msg: Option<&str>, reason: Option<&str>, stage: Option<&str>, mem_mb: Option<u64>) -> Value {
    const REASONS: &[&str] = &["unknown", "destroyed", "context_lost", "validation", "out_of_memory", "internal"];
    let mut e = json!({"e": "crash", "kind": if matches!(kind, "panic" | "gpu_lost" | "gpu_error") { kind } else { "panic" }});
    if let Some(loc) = loc.filter(|l| is_crash_location(l)) {
        e["loc"] = json!(loc);
    }
    if let Some(msg) = msg.filter(|m| PANIC_CODES.contains(m) || GPU_CODES.contains(m)) {
        e["msg"] = json!(msg);
    }
    if let Some(reason) = reason.filter(|r| REASONS.contains(r)) {
        e["reason"] = json!(reason);
    }
    if let Some(stage) = stage.filter(|s| Stage::is_name(s)) {
        e["stage"] = json!(stage);
    }
    if let Some(mb) = mem_mb {
        e["memMB"] = json!(mb.min(1_000_000));
    }
    e
}

/// `event` alone as a batch's JSON body, to send right away and without the recorder: a dying page
/// may have panicked with the recorder's lock held, and its queue would never be flushed. `None`
/// for an event that may not leave ([`is_clean_event`]).
pub fn crash_batch(install_id: &str, session_id: &str, client: &str, event: &Value) -> Option<String> {
    if !is_clean_event(event) {
        return None;
    }
    let (iid, sid) = (sanitize_token(install_id, 64)?, sanitize_token(session_id, 64)?);
    Some(json!({"v": SCHEMA_VERSION, "client": client, "iid": iid, "sid": sid, "seq": 1, "events": [event]}).to_string())
}

/// Where GPU faults go ([`install_gpu_fault_reports`]).
static CRASH_SINK: OnceLock<fn(Value)> = OnceLock::new();
/// The kinds of GPU fault reported so far: each once per run, as a driver can repeat one per frame.
static GPU_FAULTS_SEEN: AtomicU8 = AtomicU8::new(0);

/// Hands a `crash` event for every kind of fault of a watched device to `sink`, as it happens: the
/// page may reload right after a lost device.
pub fn install_gpu_fault_reports(sink: fn(Value)) {
    let _ = CRASH_SINK.set(sink);
    photocraft_gpu::health::set_fault_observer(gpu_fault);
}

fn gpu_fault(fault: photocraft_gpu::health::FaultReport<'_>) {
    let bit = match (fault.lost, fault.reason) {
        (true, "destroyed") => 1,
        (true, _) => 2,
        (false, "validation") => 4,
        (false, "out_of_memory") => 8,
        (false, _) => 16,
    };
    if GPU_FAULTS_SEEN.fetch_or(bit, Ordering::Relaxed) & bit != 0 {
        return;
    }
    if let Some(sink) = CRASH_SINK.get() {
        sink(crash_event(if fault.lost { "gpu_lost" } else { "gpu_error" }, None, Some(gpu_code(fault.message)), Some(fault.reason), Some(stage_name()), None));
    }
}

/// The tool working the canvas (`tool.Brush`), or `view` for wheel zoom/scroll and pinch over it,
/// written into `out` (no allocation once it is long enough: called per frame). `false`: neither.
pub fn canvas_activity(app: &crate::PhotocraftApp, ctx: &egui::Context, out: &mut String) -> bool {
    use std::fmt::Write as _;
    out.clear();
    if let Some(tool) = app.canvas_gesture_tool(ctx) {
        return write!(out, "tool.{tool:?}").is_ok();
    }
    let over_canvas = ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| app.last_canvas_rect.contains(p)));
    let navigating = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::MouseWheel { .. } | egui::Event::Zoom(_))));
    if over_canvas && navigating {
        out.push_str(VIEW_FRAMES);
        return true;
    }
    false
}

/// Layers in the document, groups and their contents included (no allocation: called per frame).
pub fn layer_count(layers: &[photocraft_doc::Layer]) -> usize {
    fn rec(layers: &[photocraft_doc::Layer], depth: usize) -> usize {
        if depth > 64 {
            return 0;
        }
        layers.iter().map(|l| 1 + l.children().map_or(0, |c| rec(c, depth + 1))).sum()
    }
    rec(layers, 0)
}

/// The canvas textures of every open document, in MB.
pub fn canvas_memory_mb(app: &crate::PhotocraftApp) -> u64 {
    app.session.documents().iter().map(|st| crate::doc_switch::canvas_bytes(app, st.doc.id)).sum::<u64>() >> 20
}

/// Frame timing on the desktop, the counterpart of the web shell's `FramePerf`: fed by the shell's
/// `logic` and `ui`, it reports `frames` while the canvas is worked and `freeze` for a frame of
/// [`FREEZE_MS`] or more. A frame's time is eframe's (`cpu_usage`), known one frame later.
#[derive(Debug, Default)]
pub struct FrameProbe {
    start: f64,
    logic_ms: f64,
    update_ms: f64,
    interval: Option<f64>,
    last_start: Option<f64>,
    last_cpu: Option<f32>,
    /// The finished frame waiting for eframe's time: its key is in `key`.
    pending: bool,
    key: String,
    prev_key: String,
    megapixels: Option<f64>,
    layers: Option<usize>,
}

impl FrameProbe {
    /// Start of a frame; `cpu_usage` is eframe's time of the previous one, in seconds.
    pub fn begin(&mut self, cpu_usage: Option<f32>) {
        if !is_active() {
            self.pending = false;
            return;
        }
        let start = now_ms();
        let fresh = cpu_usage.filter(|c| Some(*c) != self.last_cpu);
        self.last_cpu = cpu_usage.or(self.last_cpu);
        if std::mem::take(&mut self.pending) {
            self.finish(fresh.map(|s| f64::from(s) * 1000.0));
        }
        self.interval = self.last_start.map(|t| start - t);
        self.last_start = Some(start);
        self.start = start;
    }

    /// End of the app's logic.
    pub fn end_logic(&mut self) {
        if is_active() {
            self.logic_ms = now_ms() - self.start;
        }
    }

    /// End of the app's update: what the canvas was doing this frame; a freeze is reported here.
    pub fn end_update(&mut self, app: &crate::PhotocraftApp, ctx: &egui::Context) {
        if !is_active() {
            return;
        }
        self.update_ms = now_ms() - self.start;
        self.pending = true;
        if canvas_activity(app, ctx, &mut self.key) {
            self.megapixels = app.session.active().map(|st| megapixels(st.doc.size.width, st.doc.size.height));
            self.layers = app.session.active().map(|st| layer_count(&st.doc.layers));
        }
        if self.update_ms >= FREEZE_MS {
            let context = app.freeze_context();
            with(|r| r.freeze(FreezeSource::Frame, self.update_ms, &context));
        }
    }

    fn finish(&mut self, total_ms: Option<f64>) {
        let gesture_start = !self.key.is_empty() && self.key != self.prev_key;
        if let (false, Some(ms)) = (self.key.is_empty(), total_ms) {
            let parts = FrameParts { input: 0.0, logic: self.logic_ms, ui: (self.update_ms - self.logic_ms).max(0.0), paint: (ms - self.update_ms).max(0.0) };
            let sample = FrameSample {
                key: &self.key,
                ms,
                latency_ms: None,
                interval_ms: self.interval,
                megapixels: self.megapixels,
                layers: self.layers,
                gesture_start,
                parts: Some(parts),
            };
            with(|r| r.frame_sample(&sample));
        }
        std::mem::swap(&mut self.prev_key, &mut self.key);
    }
}

#[cfg(test)]
#[path = "telemetry_tests.rs"]
mod tests;
