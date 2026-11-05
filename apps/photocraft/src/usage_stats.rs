//! Anonymous usage statistics on the desktop: the native side of
//! [`photocraft_ui_egui::telemetry`] (what is recorded is documented there and in README › Privacy).
//!
//! Only the official Windows and Linux packages (`update_check::channel_name`, built by the
//! packaging scripts), only while
//! Preferences › General › Send anonymous usage statistics is on. `DO_NOT_TRACK=1` turns them off,
//! as `CAPISHOT_TELEMETRY_URL=off` does; that variable also points them at another collector.
//! CI and control-channel launches don't count unless it is set.
//!
//! Nothing here runs on a frame but a few atomic loads: the hardware is read, batches are built and
//! POSTed, and the crash breadcrumb is written on one background thread, which holds no queue of its
//! own (the recorder's is bounded). A failed POST is dropped, never retried. The window's frames
//! can't wait on any of it: the recorder's lock is only ever tried.
//!
//! The first run shows a notice with a link to the option; nothing leaves until it was on screen
//! for a flush period or dismissed.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use eframe::App as _;
use eframe::egui;
use photocraft_ui_egui::telemetry::{self, Environment, FrameProbe, Recorder};
use photocraft_ui_egui::{PhotocraftApp, notices};
use serde_json::{Value, json};

/// Capishot's collector.
const ENDPOINT: &str = "https://capichan.com/capishot/api/t";
/// Overrides [`ENDPOINT`]; `off` turns the statistics off.
const URL_VAR: &str = "CAPISHOT_TELEMETRY_URL";
/// The random install id, in the config folder.
const ID_FILE: &str = "telemetry-id";
/// Breadcrumbs of running copies, in the config folder.
const CRUMB_DIR: &str = "telemetry-crumbs";
/// How often the background thread wakes: the breadcrumb's heartbeat.
const BEAT: Duration = Duration::from_secs(5);
/// A breadcrumb this old whose process is gone belongs to a copy that died.
const DEAD_SECS: u64 = 30;
/// How long closing the window may wait for the last batch.
const LAST_BATCH_WAIT: Duration = Duration::from_millis(800);

/// The collector to send to, if any. `url`: [`URL_VAR`]; `official`: a package we publish, built
/// for release; `quiet`: CI or a control-channel launch; `dnt`: `DO_NOT_TRACK` is set.
fn endpoint(url: Option<&str>, official: bool, quiet: bool, dnt: bool) -> Option<String> {
    if dnt {
        return None;
    }
    match url.map(str::trim) {
        Some("" | "off" | "0") => None,
        Some(url) => Some(url.to_string()),
        None => (official && !quiet).then(|| ENDPOINT.to_string()),
    }
}

/// `DO_NOT_TRACK`'s convention: set to anything but empty or `0`.
fn do_not_track(value: Option<&str>) -> bool {
    value.is_some_and(|v| !v.trim().is_empty() && v.trim() != "0")
}

/// 128 random bits as hex, if the OS has any to give.
fn random_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The install id kept in `dir`, created at first use. A damaged file is replaced; without a place
/// to keep it (`None`) every session counts as a new install.
fn install_id(dir: Option<&Path>) -> Option<String> {
    let file = dir.map(|d| d.join(ID_FILE));
    if let Some(file) = &file
        && let Ok(text) = std::fs::read_to_string(file)
    {
        let id = text.trim();
        if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
            return Some(id.to_string());
        }
    }
    let id = random_id()?;
    if let Some(file) = &file {
        if let Some(parent) = file.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(file, &id);
    }
    Some(id)
}

/// What the hardware reports, read on the background thread (it takes a while).
#[derive(Clone, Debug, Default)]
struct Hardware {
    os_version: String,
    cpu: String,
    cores: u32,
    ram_gb: f64,
    free_disk_gb: Option<f64>,
}

fn read_hardware(data_dir: Option<&Path>) -> Hardware {
    use sysinfo::{CpuRefreshKind, Disks, RefreshKind, System};
    let sys = System::new_with_specifics(RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing()).with_memory(sysinfo::MemoryRefreshKind::nothing().with_ram()));
    let os_version = [System::name(), System::os_version()].into_iter().flatten().collect::<Vec<_>>().join(" ");
    let free_disk_gb = data_dir.and_then(|dir| {
        let disks = Disks::new_with_refreshed_list();
        let disk = disks.list().iter().filter(|d| dir.starts_with(d.mount_point())).max_by_key(|d| d.mount_point().as_os_str().len())?;
        Some(disk.available_space() as f64 / 1e9)
    });
    Hardware {
        os_version,
        cpu: sys.cpus().first().map(|c| c.brand().to_string()).unwrap_or_default(),
        cores: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
        ram_gb: sys.total_memory() as f64 / 1e9,
        free_disk_gb,
    }
}

/// This process's resident memory, in MB.
fn resident_mb(sys: &mut sysinfo::System, pid: sysinfo::Pid) -> Option<u64> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), false, ProcessRefreshKind::nothing().with_memory());
    Some(sys.process(pid)?.memory() >> 20)
}

// ------------------------------------------------------------------ the crash breadcrumb

/// What the breadcrumb says: updated from the frame every few seconds, written by the thread.
#[derive(Clone, Debug, Default, PartialEq)]
struct Crumb {
    cmd: String,
    tool: String,
    docs: u64,
    mem_mb: u64,
}

static PANIC_MARK: OnceLock<PathBuf> = OnceLock::new();

/// From the panic hook: marks this run's breadcrumb, so that if it dies of the panic the next run
/// can say so. Only the fact: not the message, which may name a file.
pub fn note_panic() {
    if let Some(mark) = PANIC_MARK.get() {
        let _ = std::fs::write(mark, b"");
    }
}

fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// `<pid>.json` and its panic mark in `dir`.
fn crumb_files(dir: &Path, pid: u32) -> (PathBuf, PathBuf) {
    (dir.join(format!("{pid}.json")), dir.join(format!("{pid}.panic")))
}

fn write_crumb(file: &Path, crumb: &Crumb, started: u64, now: u64) {
    let body = json!({"t": now, "start": started, "cmd": crumb.cmd, "tool": crumb.tool, "docs": crumb.docs, "memMB": crumb.mem_mb});
    let _ = std::fs::write(file, body.to_string());
}

/// Removes this run's breadcrumb: it ended normally.
fn remove_crumb(file: &Path, mark: &Path) {
    let _ = std::fs::remove_file(file);
    let _ = std::fs::remove_file(mark);
}

/// The breadcrumbs in `dir` of runs that died (`alive`: that process still runs; a stale file of a
/// running one is a suspended machine), as `unclean_exit` events; their files are removed. A damaged
/// file is removed and not counted. At most a few: a folder can't flood a batch.
fn collect_crumbs(dir: &Path, now: u64, alive: &dyn Fn(u32) -> bool) -> Vec<Value> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut events = Vec::new();
    for entry in entries.flatten() {
        let file = entry.path();
        if file.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(pid) = file.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<u32>().ok()) else {
            let _ = std::fs::remove_file(&file);
            continue;
        };
        let (_, mark) = crumb_files(dir, pid);
        let crumb: Option<Value> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok());
        let beat = crumb.as_ref().and_then(|c| c.get("t")).and_then(Value::as_u64).unwrap_or(0);
        if now.saturating_sub(beat) < DEAD_SECS || alive(pid) {
            continue;
        }
        if let Some(c) = crumb.filter(|_| events.len() < 8) {
            let text = |key: &str| c.get(key).and_then(Value::as_str);
            let number = |key: &str| c.get(key).and_then(Value::as_u64);
            let up = beat.saturating_sub(number("start").unwrap_or(beat)) / 60;
            events.push(telemetry::unclean_exit_event(text("cmd"), text("tool"), number("memMB"), Some(up), number("docs"), Some(mark.exists())));
        }
        remove_crumb(&file, &mark);
    }
    events
}

// ------------------------------------------------------------------ the background thread

enum Message {
    /// Send what is waiting now.
    Flush,
    /// A session started: write its breadcrumb from now on.
    Session(Arc<Mutex<Crumb>>),
    /// Statistics were turned off: no more breadcrumbs.
    Stop,
    /// The app is closing: send this last batch and say when done.
    Last(Option<String>, Sender<()>),
}

/// What the frame and the thread share.
#[derive(Default)]
struct Shared {
    /// The window has the keyboard focus: a period's interaction counts as active time.
    focused: AtomicBool,
    /// When the session started (`unix_secs`).
    started: AtomicU64,
    hardware: OnceLock<Hardware>,
    install_id: OnceLock<String>,
    /// Breadcrumbs of runs that died, found when the thread started.
    crashes: Mutex<Vec<Value>>,
}

fn post(url: &str, body: &str) {
    if let Err(e) = photocraft_fontfetch::post_json(url, body) {
        log::debug!("usage statistics: {e}");
    }
}

fn run_thread(url: String, config: Option<PathBuf>, shared: Arc<Shared>, rx: mpsc::Receiver<Message>) {
    use sysinfo::{Pid, System};
    let pid_number = std::process::id();
    let pid = Pid::from_u32(pid_number);
    let mut sys = System::new();
    let crumb_dir = config.as_ref().map(|d| d.join(CRUMB_DIR));
    let files = crumb_dir.as_deref().map(|dir| {
        let _ = std::fs::create_dir_all(dir);
        crumb_files(dir, pid_number)
    });
    if let Some((_, mark)) = &files {
        let _ = PANIC_MARK.set(mark.clone());
    }
    if let Some(dir) = &crumb_dir {
        let alive = |p: u32| System::new_with_specifics(sysinfo::RefreshKind::nothing().with_processes(sysinfo::ProcessRefreshKind::nothing())).process(Pid::from_u32(p)).is_some();
        *shared.crashes.lock().unwrap_or_else(PoisonError::into_inner) = collect_crumbs(dir, unix_secs(), &alive);
    }
    if let Some(id) = install_id(config.as_deref()) {
        let _ = shared.install_id.set(id);
    }
    let _ = shared.hardware.set(read_hardware(config.as_deref()));
    let mut crumb: Option<Arc<Mutex<Crumb>>> = None;
    let mut beats = 0u64;
    let beats_per_flush = (telemetry::FLUSH_MS / 1000.0 / BEAT.as_secs_f64()).round().max(1.0) as u64;
    loop {
        let flush_now = match rx.recv_timeout(BEAT) {
            Ok(Message::Flush) => true,
            Ok(Message::Session(c)) => {
                crumb = Some(c);
                beats = 0;
                false
            }
            Ok(Message::Stop) => {
                crumb = None;
                if let Some((file, mark)) = &files {
                    remove_crumb(file, mark);
                }
                false
            }
            Ok(Message::Last(body, done)) => {
                if let Some(body) = body {
                    post(&url, &body);
                }
                if let Some((file, mark)) = &files {
                    remove_crumb(file, mark);
                }
                let _ = done.send(());
                return;
            }
            Err(RecvTimeoutError::Timeout) => {
                beats += 1;
                beats % beats_per_flush == 0
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if let (Some(c), Some((file, _))) = (&crumb, &files) {
            let snapshot = c.lock().unwrap_or_else(PoisonError::into_inner).clone();
            write_crumb(file, &snapshot, shared.started.load(Ordering::Relaxed), unix_secs());
        }
        if !flush_now || !telemetry::is_active() {
            continue;
        }
        let ram = resident_mb(&mut sys, pid);
        if let (Some(c), Some(mb)) = (&crumb, ram) {
            let mut c = c.lock().unwrap_or_else(PoisonError::into_inner);
            c.mem_mb = c.mem_mb.max(mb);
        }
        let visible = shared.focused.load(Ordering::Relaxed);
        let now = telemetry::now_ms();
        telemetry::with(|r| {
            r.tick(now, visible);
            if let Some(mb) = ram {
                r.note_ram(mb);
            }
        });
        if let Some(body) = telemetry::take_batch() {
            // Turned off while the batch was being built: it stays here.
            if telemetry::is_active() {
                post(&url, &body);
            }
        }
    }
}

// ------------------------------------------------------------------ the shell

/// The app with its statistics: forwards everything to [`PhotocraftApp`], and adds the frame hooks.
pub struct Shell {
    app: PhotocraftApp,
    tx: Sender<Message>,
    shared: Arc<Shared>,
    probe: FrameProbe,
    /// Session on, as last applied.
    enabled: bool,
    session_started_ms: f64,
    crumb: Arc<Mutex<Crumb>>,
    last_crumb_ms: f64,
    last_gpu_ms: f64,
}

impl Shell {
    /// `Ok`: this copy may send statistics (the preference still decides, frame by frame), and a
    /// background thread is running. `Err` hands the app back: nothing will ever be sent.
    pub fn new(app: PhotocraftApp, portable: bool, automated: bool) -> Result<Self, Box<PhotocraftApp>> {
        let env_var = |name: &str| std::env::var(name).ok();
        let channel = crate::update_check::channel_name(portable);
        // The packaging scripts bake in the commit: `cargo build --release` on Windows is no MSI.
        let official = channel.is_some() && !cfg!(debug_assertions) && photocraft_engine::build_info::git_sha().is_some();
        let quiet = automated || std::env::var_os("CI").is_some();
        let Some(url) = endpoint(env_var(URL_VAR).as_deref(), official, quiet, do_not_track(env_var("DO_NOT_TRACK").as_deref())) else {
            return Err(Box::new(app));
        };
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared::default());
        let config = crate::services::config_dir();
        let thread_shared = shared.clone();
        let spawned = std::thread::Builder::new().name("capishot-usage-stats".into()).spawn(move || run_thread(url, config, thread_shared, rx));
        if let Err(e) = spawned {
            log::info!("usage statistics not started: {e}");
            return Err(Box::new(app));
        }
        Ok(Self {
            app,
            tx,
            shared,
            probe: FrameProbe::default(),
            enabled: false,
            session_started_ms: 0.0,
            crumb: Arc::default(),
            last_crumb_ms: 0.0,
            last_gpu_ms: 0.0,
        })
    }

    /// Every frame: follows the preference at once; starts a session once the thread has read the
    /// hardware and the window knows its adapter.
    fn sync(&mut self, ctx: &egui::Context) {
        let want = self.app.session.prefs().general.send_anonymous_usage_statistics;
        if want != self.enabled {
            if want {
                self.start_session(ctx);
            } else {
                telemetry::stop();
                self.enabled = false;
                let _ = self.tx.send(Message::Stop);
            }
        }
        if !telemetry::is_active() {
            return;
        }
        telemetry::observe_frame(&self.app, ctx);
        self.shared.focused.store(ctx.input(|i| i.focused), Ordering::Relaxed);
        let now = telemetry::now_ms();
        if now - self.last_crumb_ms >= 2000.0 {
            self.last_crumb_ms = now;
            // Only tried: the thread holds it for a copy of a few short strings.
            if let Ok(mut crumb) = self.crumb.try_lock() {
                crumb.cmd.clone_from(&self.app.perf.last_command);
                crumb.tool.clear();
                crumb.tool.push_str(telemetry::tool_id(self.app.ui.tool).trim_start_matches("tool."));
                crumb.docs = self.app.session.documents().len() as u64;
            }
        }
        if now - self.last_gpu_ms >= telemetry::FLUSH_MS {
            self.last_gpu_ms = now;
            let gpu = telemetry::canvas_memory_mb(&self.app);
            telemetry::with(|r| r.note_gpu(gpu));
        }
    }

    fn start_session(&mut self, ctx: &egui::Context) {
        let (Some(hardware), Some(install)) = (self.shared.hardware.get(), self.shared.install_id.get()) else { return };
        let info = &self.app.perf.gpu_info;
        if info.backend.is_empty() {
            return; // the window's device isn't up yet
        }
        let Some(session) = random_id() else { return };
        // On a first run the notice comes first. It stays until dismissed.
        notices::usage_statistics_notice(&mut self.app);
        let info = &self.app.perf.gpu_info;
        let monitor = ctx.input(|i| i.viewport().monitor_size).unwrap_or_default();
        let env = Environment {
            native: true,
            os: native_os(),
            channel: crate::update_check::channel_name(crate::app_dirs::current().mode == crate::app_dirs::Mode::Portable).unwrap_or("source"),
            device: "desktop",
            threads: true,
            cores: hardware.cores,
            gpu: canvas_api(&info.canvas, &info.backend),
            gpu_vendor: telemetry::gpu_vendor_class(0, &info.adapter),
            gpu_model: info.adapter.clone(),
            gpu_driver: info.driver.clone(),
            os_version: hardware.os_version.clone(),
            cpu: hardware.cpu.clone(),
            ram_gb: hardware.ram_gb,
            free_disk_gb: hardware.free_disk_gb,
            memory_gb: hardware.ram_gb,
            screen: (monitor.x as u32, monitor.y as u32),
            language: photocraft_ui_egui::i18n::current().code().to_string(),
            ..Default::default()
        };
        let now = telemetry::now_ms();
        let mut recorder = Recorder::new(install, &session, now).desktop();
        recorder.event(telemetry::session_start_event(&env));
        for event in std::mem::take(&mut *self.shared.crashes.lock().unwrap_or_else(PoisonError::into_inner)) {
            recorder.event(event);
        }
        telemetry::start(recorder);
        // A busy recorder: the next frame tries again.
        if !telemetry::is_active() {
            return;
        }
        self.enabled = true;
        self.session_started_ms = now;
        self.shared.started.store(unix_secs(), Ordering::Relaxed);
        let _ = self.tx.send(Message::Session(self.crumb.clone()));
        // At once, not at the first tick: a run that dies within seconds is still counted. Not
        // before the notice was dismissed, though: until then the first tick is the first send.
        if notices::usage_statistics_notice_dismissed(&self.app) {
            let _ = self.tx.send(Message::Flush);
        }
    }
}

/// `gpu` in `session_start`: the graphics API drawing the canvas.
fn canvas_api(canvas: &str, backend: &str) -> &'static str {
    if canvas == "cpu" {
        return "cpu";
    }
    match backend.to_ascii_lowercase().as_str() {
        "vulkan" => "vulkan",
        "dx12" => "dx12",
        "metal" => "metal",
        "gl" => "gl",
        _ => "unknown",
    }
}

fn native_os() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "other"
    }
}

impl eframe::App for Shell {
    fn on_exit(&mut self) {
        self.app.on_exit();
        if !telemetry::is_active() {
            let (done, _) = mpsc::channel();
            let _ = self.tx.send(Message::Last(None, done));
            return;
        }
        let now = telemetry::now_ms();
        telemetry::with(|r| r.end(now));
        // Closed before the notice had its time: nothing has left, and nothing does.
        let told = notices::usage_statistics_notice_dismissed(&self.app) || now - self.session_started_ms >= telemetry::FLUSH_MS;
        let body = if told { telemetry::take_batch() } else { None };
        telemetry::stop();
        let (done, wait) = mpsc::channel();
        if self.tx.send(Message::Last(body, done)).is_ok() {
            let _ = wait.recv_timeout(LAST_BATCH_WAIT);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.probe.begin(frame.info().cpu_usage);
        self.app.logic(ctx, frame);
        self.probe.end_logic();
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        let ctx = ui.ctx().clone();
        self.probe.end_update(&self.app, &ctx);
        self.sync(&ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_official_builds_send_unless_told_otherwise() {
        assert_eq!(endpoint(None, true, false, false).as_deref(), Some(ENDPOINT));
        assert_eq!(endpoint(None, false, false, false), None, "a build from source");
        assert_eq!(endpoint(None, true, true, false), None, "CI or a control-channel launch");
        assert_eq!(endpoint(Some("off"), true, false, false), None);
        assert_eq!(endpoint(Some(" 0 "), true, false, false), None);
        assert_eq!(endpoint(Some(""), true, false, false), None);
        assert_eq!(endpoint(Some("http://127.0.0.1:9/t"), false, true, false).as_deref(), Some("http://127.0.0.1:9/t"));
        assert_eq!(endpoint(Some("http://127.0.0.1:9/t"), true, false, true), None, "DO_NOT_TRACK wins");
        assert_eq!(endpoint(None, true, false, true), None);
    }

    #[test]
    fn do_not_track_follows_its_convention() {
        for (value, want) in [(None, false), (Some(""), false), (Some("0"), false), (Some("1"), true), (Some("true"), true)] {
            assert_eq!(do_not_track(value), want, "{value:?}");
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("capishot-usage-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_install_id_stays_and_a_damaged_one_is_replaced() {
        let dir = temp_dir("id");
        let id = install_id(Some(&dir)).unwrap();
        assert_eq!(id.len(), 32);
        assert_eq!(install_id(Some(&dir)).unwrap(), id);
        std::fs::write(dir.join(ID_FILE), "/home/someone").unwrap();
        let replaced = install_id(Some(&dir)).unwrap();
        assert_ne!(replaced, id);
        assert_eq!(std::fs::read_to_string(dir.join(ID_FILE)).unwrap(), replaced);
        assert_ne!(install_id(None).unwrap(), install_id(None).unwrap(), "nowhere to keep it");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_breadcrumb_left_by_a_dead_run_is_reported_once() {
        let dir = temp_dir("crumb");
        let crumb = Crumb { cmd: "edit.inpaintAi".into(), tool: "Brush".into(), docs: 2, mem_mb: 900 };
        let (dead, dead_mark) = crumb_files(&dir, 111);
        write_crumb(&dead, &crumb, 1000, 1000 + 600);
        std::fs::write(&dead_mark, b"").unwrap();
        let (killed, _) = crumb_files(&dir, 222);
        write_crumb(&killed, &crumb, 1000, 1000 + 600);
        let (running, _) = crumb_files(&dir, 333);
        write_crumb(&running, &crumb, 1000, 1000 + 600);
        let (recent, _) = crumb_files(&dir, 444);
        write_crumb(&recent, &crumb, 1000, 5000);
        std::fs::write(dir.join("555.json"), "{ not json").unwrap();
        std::fs::write(dir.join("page.psd.json"), "{}").unwrap();
        let events = collect_crumbs(&dir, 5010, &|pid| pid == 333);
        assert_eq!(events.len(), 2, "{events:?}");
        let by_src = |src: &str| events.iter().find(|e| e["src"] == src).unwrap();
        assert_eq!(by_src("panic")["cmd"], "edit.inpaintAi");
        assert_eq!(by_src("panic")["upMin"], 10);
        assert_eq!(by_src("killed")["memMB"], 900);
        assert!(running.exists() && recent.exists(), "still running");
        assert!(!dead.exists() && !dead_mark.exists() && !killed.exists() && !dir.join("555.json").exists() && !dir.join("page.psd.json").exists());
        assert!(collect_crumbs(&dir, 5011, &|pid| pid == 333).is_empty(), "once");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_folder_of_breadcrumbs_cannot_flood_a_batch() {
        let dir = temp_dir("flood");
        for pid in 1000..1040 {
            write_crumb(&crumb_files(&dir, pid).0, &Crumb::default(), 0, 1);
        }
        assert_eq!(collect_crumbs(&dir, 100_000, &|_| false).len(), 8);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "the rest are removed");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_canvas_api_comes_from_the_backend() {
        assert_eq!(canvas_api("gpu", "Vulkan"), "vulkan");
        assert_eq!(canvas_api("gpu", "dx12"), "dx12");
        assert_eq!(canvas_api("cpu", "vulkan"), "cpu");
        assert_eq!(canvas_api("gpu", "other"), "unknown");
    }
}
