//! The desktop updater behind the "update available" notice ([`crate::update_notice`]): Update
//! downloads the new version in the background (the progress card; Esc cancels), resuming an
//! interrupted transfer with Range requests, and refuses a file whose SHA-256 isn't the one the
//! manifest publishes. Then "Restart and Update" quits the usual way (unsaved documents ask
//! first) and, once the app has exited, a helper (this same executable run with
//! `--apply-update`) installs it and starts Capishot again:
//!
//! - an installed copy (the per-machine MSI): `msiexec /i <msi> /passive /norestart` (Windows asks
//!   for administrator rights);
//! - a portable copy (`portable.txt` beside the exe): the new ZIP, extracted beforehand, copied over
//!   the app's folder, its `CapishotData` (settings, presets, recovery) kept;
//! - Linux, an AppImage: the new AppImage replaces the file `$APPIMAGE` names (written beside it,
//!   then renamed over it);
//! - Linux, the tar.gz folder: the new tar.gz, extracted beforehand, copied over the app's folder
//!   like the Windows ZIP (each file renamed into place). A .deb install isn't updated here: apt
//!   owns its files, so the notice links the download page ([`crate::update_notice::Channel`]).
//!
//! What happened is written to `last-update.json` in the updates folder; the next launch reports a
//! failure or a cancelled administrator prompt. Downloads go to `%LOCALAPPDATA%\Capishot\updates`
//! (Linux: `$XDG_CACHE_HOME/capishot/updates`; a portable copy: `CapishotData/updates`).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// The helper's flag: `--apply-update <msi|zip|appimage|tar> <file-or-folder> --pid <n> --exe <path> --version <v>`.
pub const HELPER_FLAG: &str = "--apply-update";
/// The user data folder of a portable copy: never replaced.
pub const PORTABLE_DATA: &str = "CapishotData";
/// The result file the helper leaves for the next launch.
pub const RESULT_FILE: &str = "last-update.json";

/// How this copy is updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Msi,
    Zip,
    /// Linux: the new AppImage, put in place of `Plan::exe`.
    AppImage,
    /// Linux: the extracted tar.gz folder, copied over `Plan::exe`'s folder.
    Tar,
}

impl Kind {
    /// The helper's name for it.
    fn name(self) -> &'static str {
        match self {
            Self::Msi => "msi",
            Self::Zip => "zip",
            Self::AppImage => "appimage",
            Self::Tar => "tar",
        }
    }
}

/// What the helper does after the app exits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub kind: Kind,
    /// The MSI or AppImage, or the extracted portable folder (the one holding `capishot.exe`).
    pub source: PathBuf,
    /// The app to start again (for a portable copy: in the folder to replace; an AppImage: the
    /// file to replace).
    pub exe: PathBuf,
    pub version: String,
    /// Where the result file goes.
    pub updates_dir: PathBuf,
}

static PENDING: Mutex<Option<Plan>> = Mutex::new(None);

/// Ask for `plan` to run once the app has exited (the shell spawns the helper then).
pub fn set_pending(plan: Option<Plan>) {
    *PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = plan;
}

/// The update to install after exit, if one was asked for.
pub fn take_pending() -> Option<Plan> {
    PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
}

/// Whether an update waits for the app to exit.
pub fn pending() -> bool {
    PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner).is_some()
}

/// The folder downloads go to.
pub fn updates_dir(portable: bool) -> PathBuf {
    if portable && let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        return dir.join(PORTABLE_DATA).join("updates");
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        return env("LOCALAPPDATA").unwrap_or_else(std::env::temp_dir).join("Capishot").join("updates");
    }
    // XDG: a relative XDG_CACHE_HOME is ignored, as the spec asks.
    let cache = env("XDG_CACHE_HOME").filter(|p| p.is_absolute()).or_else(|| env("HOME").map(|h| h.join(".cache")));
    cache.unwrap_or_else(std::env::temp_dir).join("capishot").join("updates")
}

/// The helper's arguments for `plan`, run from the app with process id `pid`.
pub fn helper_args(plan: &Plan, pid: u32) -> Vec<OsString> {
    vec![
        HELPER_FLAG.into(),
        plan.kind.name().into(),
        plan.source.clone().into_os_string(),
        "--pid".into(),
        pid.to_string().into(),
        "--exe".into(),
        plan.exe.clone().into_os_string(),
        "--version".into(),
        plan.version.clone().into(),
        "--dir".into(),
        plan.updates_dir.clone().into_os_string(),
    ]
}

/// The helper's arguments read back: `None` when `args` (without the program) aren't a helper run.
pub fn parse_helper_args(args: &[OsString]) -> Option<Result<(Plan, u32), String>> {
    if args.first().is_none_or(|a| a != HELPER_FLAG) {
        return None;
    }
    let kind = match args.get(1).and_then(|a| a.to_str()) {
        Some("msi") => Kind::Msi,
        Some("zip") => Kind::Zip,
        Some("appimage") => Kind::AppImage,
        Some("tar") => Kind::Tar,
        other => return Some(Err(format!("unknown update kind {other:?}"))),
    };
    let Some(source) = args.get(2).map(PathBuf::from) else { return Some(Err("no update file".into())) };
    let mut pid = None;
    let (mut exe, mut version, mut dir) = (None, None, None);
    let mut i = 3;
    while i < args.len() {
        let value = args.get(i + 1).cloned();
        match (args[i].to_str(), value) {
            (Some("--pid"), Some(v)) => pid = v.to_str().and_then(|v| v.parse::<u32>().ok()),
            (Some("--exe"), Some(v)) => exe = Some(PathBuf::from(v)),
            (Some("--version"), Some(v)) => version = v.to_str().map(str::to_string),
            (Some("--dir"), Some(v)) => dir = Some(PathBuf::from(v)),
            (flag, _) => return Some(Err(format!("unexpected helper argument {flag:?}"))),
        }
        i += 2;
    }
    let (Some(pid), Some(exe), Some(version), Some(updates_dir)) = (pid, exe, version, dir) else {
        return Some(Err("the helper needs --pid, --exe, --version and --dir".into()));
    };
    Some(Ok((Plan { kind, source, exe, version, updates_dir }, pid)))
}

/// Starts the helper for `plan`, detached (it outlives this process).
pub fn spawn_helper(plan: &Plan) -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    // A portable copy's exe is replaced, and an AppImage's mount goes away when it exits: run the
    // helper from a copy outside them.
    let helper = if plan.kind != Kind::Msi {
        std::fs::create_dir_all(&plan.updates_dir)?;
        let copy = plan.updates_dir.join(format!("capishot-update-helper{}", std::env::consts::EXE_SUFFIX));
        std::fs::copy(&exe, &copy)?;
        copy
    } else {
        exe
    };
    let mut cmd = std::process::Command::new(helper);
    cmd.args(helper_args(plan, std::process::id()));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: not tied to this process or its console.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group: a terminal closing with the app doesn't take it down.
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ())
}

/// What the helper did, for the next launch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub version: String,
    pub ok: bool,
    /// The installer's exit code, when it ran.
    #[serde(default)]
    pub code: Option<i32>,
    #[serde(default)]
    pub message: String,
}

fn write_outcome(dir: &Path, o: &Outcome) {
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(dir.join(RESULT_FILE), serde_json::to_vec_pretty(o).unwrap_or_default());
}

/// The last helper run's outcome (read once: the file is removed).
pub fn take_outcome(dir: &Path) -> Option<Outcome> {
    let path = dir.join(RESULT_FILE);
    let o = serde_json::from_slice(&std::fs::read(&path).ok()?).ok();
    let _ = std::fs::remove_file(path);
    o
}

/// Whether process `pid` still runs.
fn alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .creation_flags(0x0800_0000)
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
    #[cfg(target_os = "linux")]
    {
        Path::new("/proc").join(pid.to_string()).exists()
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().is_ok_and(|s| s.success())
    }
}

/// Copies `from` to `to`. On Unix through a temporary file renamed over `to`, so a program still
/// mapped from the old file (or busy) keeps its copy and nothing is left half-written.
fn copy_file(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut name = to.file_name().unwrap_or_default().to_os_string();
        name.push(".capishot-update");
        let tmp = to.with_file_name(name);
        let r = std::fs::copy(from, &tmp).and_then(|_| std::fs::rename(&tmp, to));
        if r.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        r
    }
    #[cfg(not(unix))]
    {
        std::fs::copy(from, to).map(|_| ())
    }
}

/// Puts the downloaded AppImage `src` in place of `dst` (executable, renamed over it).
pub fn replace_appimage(src: &Path, dst: &Path) -> std::io::Result<()> {
    copy_file(src, dst)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Extracts the tar.gz at `file` into `dest` with the system's `tar` (entries with an absolute
/// path or `..` refused, links refused, owners not restored).
pub fn extract_tar(file: &Path, dest: &Path) -> Result<(), String> {
    let list = std::process::Command::new("tar").arg("-tvzf").arg(file).output().map_err(|e| format!("tar: {e}"))?;
    if !list.status.success() {
        return Err(format!("{}: not a readable tar.gz", file.display()));
    }
    let names = std::process::Command::new("tar").arg("-tzf").arg(file).output().map_err(|e| format!("tar: {e}"))?;
    for line in String::from_utf8_lossy(&list.stdout).lines() {
        // `tar -tv` starts each line with the entry's type: only files and folders are taken.
        if !(line.starts_with('-') || line.starts_with('d')) {
            return Err(format!("unsupported entry in the update: {line}"));
        }
    }
    for name in String::from_utf8_lossy(&names.stdout).lines() {
        let rel = Path::new(name);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("unsafe path in the update: {name}"));
        }
    }
    let _ = std::fs::remove_dir_all(dest);
    std::fs::create_dir_all(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let out = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(file)
        .arg("-C")
        .arg(dest)
        .arg("--no-same-owner")
        .output()
        .map_err(|e| format!("tar: {e}"))?;
    if !out.status.success() {
        return Err(format!("tar: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(())
}

/// Copies the extracted portable folder `src` over `dst`, keeping `dst`'s [`PORTABLE_DATA`].
/// Files still locked a moment after the app exited are retried.
pub fn swap_portable(src: &Path, dst: &Path) -> std::io::Result<usize> {
    fn copy_dir(src: &Path, dst: &Path, top: bool, n: &mut usize) -> std::io::Result<()> {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            let name = e.file_name();
            if top && name == PORTABLE_DATA {
                continue;
            }
            let (from, to) = (e.path(), dst.join(&name));
            if e.file_type()?.is_dir() {
                copy_dir(&from, &to, false, n)?;
            } else {
                let mut tries = 0;
                loop {
                    match copy_file(&from, &to) {
                        Ok(_) => break,
                        Err(e) if tries < 40 => {
                            tries += 1;
                            log::info!("update: {} busy ({e}); retrying", to.display());
                            std::thread::sleep(std::time::Duration::from_millis(250));
                        }
                        Err(e) => return Err(e),
                    }
                }
                *n += 1;
            }
        }
        Ok(())
    }
    let mut n = 0;
    copy_dir(src, dst, true, &mut n)?;
    Ok(n)
}

/// The folder in an extracted ZIP that holds the app (`capishot.exe`): the top one or its single
/// subfolder.
pub fn app_folder(extracted: &Path) -> Option<PathBuf> {
    let exe = format!("capishot{}", std::env::consts::EXE_SUFFIX);
    if extracted.join(&exe).is_file() || extracted.join("capishot.exe").is_file() {
        return Some(extracted.to_path_buf());
    }
    std::fs::read_dir(extracted).ok()?.filter_map(Result::ok).map(|e| e.path()).find(|p| p.join("capishot.exe").is_file() || p.join(&exe).is_file())
}

/// Extracts the ZIP at `zip` into `dest` (entries outside it refused).
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<(), String> {
    let bytes = std::fs::read(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let r = photocraft_format::zip::ZipReader::new(&bytes).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(dest);
    for e in &r.entries {
        let rel = Path::new(&e.name);
        if rel.is_absolute() || rel.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(format!("unsafe path in the update: {}", e.name));
        }
        let out = dest.join(rel);
        if e.name.ends_with('/') {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let data = r.read(e, 1 << 31).map_err(|e| e.to_string())?;
        std::fs::write(&out, data).map_err(|e| format!("{}: {e}", out.display()))?;
    }
    Ok(())
}

/// The helper: `None` when `args` aren't a helper run, else what it did. Waits for the app to
/// exit, installs, records the outcome and starts Capishot again.
pub fn run_helper(args: &[OsString]) -> Option<Result<(), String>> {
    let (plan, pid) = match parse_helper_args(args)? {
        Ok(p) => p,
        Err(e) => return Some(Err(e)),
    };
    let start = std::time::Instant::now();
    while alive(pid) && start.elapsed() < std::time::Duration::from_secs(180) {
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
    let outcome = match plan.kind {
        Kind::Msi => {
            let status = std::process::Command::new("msiexec").arg("/i").arg(&plan.source).args(["/passive", "/norestart"]).status();
            match status {
                Ok(s) => {
                    let code = s.code();
                    // 3010: installed, a reboot would complete it; 1602/1223: the user cancelled.
                    let ok = matches!(code, Some(0 | 3010));
                    let message = match code {
                        Some(1602 | 1223) => "The update was cancelled (the administrator prompt was declined).".to_string(),
                        Some(c) if !ok => format!("The installer failed (code {c})."),
                        _ => String::new(),
                    };
                    Outcome { version: plan.version.clone(), ok, code, message }
                }
                Err(e) => Outcome { version: plan.version.clone(), ok: false, code: None, message: format!("The installer didn't start: {e}") },
            }
        }
        Kind::Zip | Kind::Tar => {
            let dst = plan.exe.parent().map(Path::to_path_buf).unwrap_or_default();
            match swap_portable(&plan.source, &dst) {
                Ok(_) => Outcome { version: plan.version.clone(), ok: true, code: None, message: String::new() },
                Err(e) => Outcome { version: plan.version.clone(), ok: false, code: None, message: format!("The files couldn't be replaced: {e}") },
            }
        }
        Kind::AppImage => match replace_appimage(&plan.source, &plan.exe) {
            Ok(()) => Outcome { version: plan.version.clone(), ok: true, code: None, message: String::new() },
            Err(e) => {
                Outcome { version: plan.version.clone(), ok: false, code: None, message: format!("{} couldn't be replaced: {e}", plan.exe.display()) }
            }
        },
    };
    write_outcome(&plan.updates_dir, &outcome);
    let _ = std::process::Command::new(&plan.exe).spawn();
    Some(if outcome.ok { Ok(()) } else { Err(outcome.message) })
}

// ------------------------------------------------------------------ in the app

/// The command that downloads the offered update.
pub const DOWNLOAD: &str = crate::update_notice::UPDATE_NOW;
/// The command that quits and installs a downloaded update.
pub const APPLY: &str = "help.restartAndUpdate";
const READY_TAG: &str = "update-ready";

/// A downloaded update, ready to install.
#[derive(Clone, Debug)]
pub struct Ready {
    pub plan: Plan,
}

static READY: Mutex<Option<Ready>> = Mutex::new(None);

/// The downloaded update, if any.
pub fn ready() -> Option<Ready> {
    READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

/// [`DOWNLOAD`]: download the offered update in the background.
pub fn download(app: &mut PhotocraftApp) -> Result<Value, String> {
    let Some(update) = app.ui.update.clone() else { return Err(tl!("No update is available").into()) };
    let Some(sha) = update.sha256.clone().filter(|s| s.len() == 64) else {
        return Err(tl!("This update publishes no checksum: download it from the website").into());
    };
    let portable = app.services.update_check.as_ref().is_some_and(|c| c.portable);
    let channel = app.services.update_check.as_ref().map(crate::update_notice::UpdateCheck::channel).unwrap_or_default();
    let kind = match channel {
        crate::update_notice::Channel::Msi => Kind::Msi,
        crate::update_notice::Channel::Zip => Kind::Zip,
        crate::update_notice::Channel::AppImage => Kind::AppImage,
        crate::update_notice::Channel::Tar => Kind::Tar,
        crate::update_notice::Channel::Deb => return Err(tl!("This copy is updated by its package manager: download the update from the website").into()),
    };
    let dir = updates_dir(portable);
    let file = update.url.rsplit('/').next().filter(|f| !f.is_empty()).unwrap_or("capishot-update").to_string();
    let dest = dir.join(&file);
    let exe = match kind {
        // The AppImage file itself, not the binary in its (temporary) mount.
        Kind::AppImage => std::env::var_os("APPIMAGE").filter(|p| !p.is_empty()).map(PathBuf::from).ok_or_else(|| tl!("APPIMAGE isn't set: download the update from the website").to_string())?,
        _ => std::env::current_exe().map_err(|e| e.to_string())?,
    };
    let (url, size, version) = (update.url.clone(), update.size.unwrap_or(0), update.version.clone());
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(crate::update_notice::TAG));
    let label = crate::i18n::fmt(tl!("Downloading Capishot {version}"), &[("version", &version)]);
    let started = app.session.start_job(
        DOWNLOAD,
        json!({"version": version}),
        &label,
        false,
        move |ctx| {
            let http = craft_update::http::Http::new(&format!("Capishot/{}", photocraft_engine::build_info::VERSION))
                .map_err(photocraft_engine::EngineError::Other)?;
            let stop = craft_update::http::Stop::default();
            let r = http.download(&url, &dest, size, &sha, &stop, &mut |t| {
                if let craft_update::http::Transfer::Bytes { done, total } = t
                    && total > 0
                {
                    ctx.progress(done as f32 / total as f32, "");
                }
                if ctx.check().is_err() {
                    stop.cancel();
                }
            });
            if ctx.check().is_err() {
                return Err(photocraft_engine::EngineError::Cancelled);
            }
            r.map_err(photocraft_engine::EngineError::Other)?;
            let source = match kind {
                Kind::Msi | Kind::AppImage => dest.clone(),
                Kind::Zip => {
                    let out = dest.with_extension("extracted");
                    extract_zip(&dest, &out).map_err(photocraft_engine::EngineError::Other)?;
                    app_folder(&out).ok_or_else(|| photocraft_engine::EngineError::Other("capishot.exe isn't in the update".into()))?
                }
                Kind::Tar => {
                    let out = dest.with_extension("extracted");
                    extract_tar(&dest, &out).map_err(photocraft_engine::EngineError::Other)?;
                    app_folder(&out).ok_or_else(|| photocraft_engine::EngineError::Other("capishot isn't in the update".into()))?
                }
            };
            Ok(Plan { kind, source, exe, version, updates_dir: dir })
        },
        |_, plan: Plan| {
            let version = plan.version.clone();
            *READY.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Ready { plan });
            Ok(json!({"ready": true, "version": version}))
        },
    );
    started.map_err(|e| e.to_string())?;
    Ok(json!({"downloading": update.version}))
}

/// The end of a [`DOWNLOAD`] job: the status line says the update is ready, that the download
/// was cancelled, or why it failed; never "Downloading" any more.
pub fn on_download_end(app: &mut PhotocraftApp, label: &str, outcome: photocraft_engine::jobs::JobOutcome) {
    use photocraft_engine::jobs::JobOutcome;
    app.ui.status_error = false;
    match outcome {
        JobOutcome::Done(v) => {
            app.ui.status = match v.get("version").and_then(Value::as_str) {
                Some(version) => crate::i18n::fmt(tl!("Capishot {version} ready to install"), &[("version", version)]),
                None => String::new(),
            };
        }
        JobOutcome::Cancelled => app.ui.status = tl!("Update download cancelled").into(),
        // The status line and a notice say why.
        JobOutcome::Failed(err) => crate::notices::error(app, format!("{label}: {err}")),
    }
}

/// Each frame: a finished download offers to restart (or, with automatic installs, installs when
/// the app closes).
pub fn tick(app: &mut PhotocraftApp) {
    let Some(r) = ready() else { return };
    if app.ui.notices.iter().any(|n| n.tag.as_deref() == Some(READY_TAG)) || pending() {
        return;
    }
    let auto = app.session.prefs().interface.auto_install_updates;
    if auto {
        set_pending(Some(r.plan.clone()));
    }
    let line = if auto { "It will be installed when you close Capishot." } else { "Capishot restarts to install it; open documents ask to be saved first." };
    let id = crate::notices::post(app, "Capishot {version} is ready to install", vec![line.into()], false, None);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.args = vec![("version".into(), r.plan.version.clone())];
        n.actions = vec![crate::notices::NoticeAction { label: "Restart and Update".into(), command: APPLY.into() }];
        n.tag = Some(READY_TAG.into());
        n.sticky = true;
    }
}

/// [`APPLY`]: quit (unsaved documents ask first) and install once the app has exited.
pub fn apply(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let Some(r) = ready() else { return Err(tl!("No update has been downloaded").into()) };
    set_pending(Some(r.plan.clone()));
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(READY_TAG));
    // The usual exit: autosave and recovery flush, unsaved documents prompt (Cancel keeps the
    // app open; the update then installs whenever it closes).
    crate::menus::invoke(app, ctx, "file.exit", json!({}))
}

/// At launch: report what the last update did when it failed or was cancelled.
pub fn report_last(app: &mut PhotocraftApp, portable: bool) {
    let Some(o) = take_outcome(&updates_dir(portable)) else { return };
    if o.ok {
        app.ui.status = crate::i18n::fmt(tl!("Updated to Capishot {version}"), &[("version", &o.version)]);
        return;
    }
    let id = crate::notices::post(app, "Capishot {version} wasn't installed", vec![o.message.clone()], true, None);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.args = vec![("version".into(), o.version.clone())];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("capishot-update-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn helper_arguments_round_trip_and_bad_ones_are_refused() {
        let plan = Plan {
            kind: Kind::Msi,
            source: PathBuf::from("C:/Users/a b/AppData/Local/Capishot/updates/capishot-0.13.9-windows-x64.msi"),
            exe: PathBuf::from("C:/Program Files/Capishot/capishot.exe"),
            version: "0.13.9".into(),
            updates_dir: PathBuf::from("C:/Users/a b/AppData/Local/Capishot/updates"),
        };
        let args = helper_args(&plan, 4242);
        assert_eq!(parse_helper_args(&args), Some(Ok((plan, 4242))));
        assert_eq!(parse_helper_args(&["--safe-gpu".into()]), None, "not a helper run");
        assert!(matches!(parse_helper_args(&[HELPER_FLAG.into(), "exe".into(), "x".into()]), Some(Err(_))));
        assert!(matches!(parse_helper_args(&[HELPER_FLAG.into(), "zip".into(), "x".into(), "--pid".into(), "1".into()]), Some(Err(_))), "missing values");
    }

    #[test]
    fn the_portable_swap_replaces_the_app_and_keeps_the_user_data() {
        let root = tmp("swap");
        let (new, app) = (root.join("new"), root.join("app"));
        for (p, c) in [("capishot.exe", "new exe"), ("capishot-cli.exe", "new cli"), ("licenses/NOTICE", "new notice"), ("CapishotData/prefs.json", "theirs")] {
            let f = new.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        for (p, c) in
            [("capishot.exe", "old exe"), ("portable.txt", "portable"), ("CapishotData/prefs.json", "mine"), ("CapishotData/recovery/a.pcraft", "doc")]
        {
            let f = app.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, c).unwrap();
        }
        let n = swap_portable(&new, &app).unwrap();
        assert_eq!(n, 3);
        let read = |p: &str| std::fs::read_to_string(app.join(p)).unwrap();
        assert_eq!(read("capishot.exe"), "new exe");
        assert_eq!(read("licenses/NOTICE"), "new notice");
        assert_eq!(read("portable.txt"), "portable");
        assert_eq!(read("CapishotData/prefs.json"), "mine", "the user's data stays");
        assert_eq!(read("CapishotData/recovery/a.pcraft"), "doc");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn linux_helper_kinds_round_trip() {
        for kind in [Kind::AppImage, Kind::Tar] {
            let plan = Plan {
                kind,
                source: PathBuf::from("/home/a b/.cache/capishot/updates/capishot-0.13.9-linux-x86_64.AppImage"),
                exe: PathBuf::from("/home/a b/Apps/Capishot.AppImage"),
                version: "0.13.9".into(),
                updates_dir: PathBuf::from("/home/a b/.cache/capishot/updates"),
            };
            assert_eq!(parse_helper_args(&helper_args(&plan, 7)), Some(Ok((plan, 7))));
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_appimage_is_replaced_in_place_and_stays_executable() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("appimage");
        let (new, app) = (root.join("new.AppImage"), root.join("Capishot.AppImage"));
        std::fs::write(&new, "new").unwrap();
        std::fs::write(&app, "old").unwrap();
        std::fs::write(root.join("prefs.json"), "mine").unwrap();
        replace_appimage(&new, &app).unwrap();
        assert_eq!(std::fs::read_to_string(&app).unwrap(), "new");
        assert_eq!(std::fs::metadata(&app).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_to_string(root.join("prefs.json")).unwrap(), "mine");
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 3, "no temporary file left");
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_tar_gz_extracts_swaps_and_unsafe_ones_are_refused() {
        let root = tmp("tar");
        let top = root.join("src/capishot-0.13.9-linux-x86_64");
        std::fs::create_dir_all(top.join("licenses")).unwrap();
        std::fs::write(top.join("capishot"), "new exe").unwrap();
        std::fs::write(top.join("licenses/NOTICE"), "n").unwrap();
        let tgz = root.join("u.tar.gz");
        assert!(std::process::Command::new("tar").arg("-czf").arg(&tgz).arg("-C").arg(root.join("src")).arg(".").status().unwrap().success());
        extract_tar(&tgz, &root.join("out")).unwrap();
        let folder = app_folder(&root.join("out")).unwrap();
        assert_eq!(folder, root.join("out/capishot-0.13.9-linux-x86_64"));
        let app = root.join("app");
        std::fs::create_dir_all(app.join(PORTABLE_DATA)).unwrap();
        std::fs::write(app.join("capishot"), "old exe").unwrap();
        std::fs::write(app.join(PORTABLE_DATA).join("prefs.json"), "mine").unwrap();
        swap_portable(&folder, &app).unwrap();
        assert_eq!(std::fs::read_to_string(app.join("capishot")).unwrap(), "new exe");
        assert_eq!(std::fs::read_to_string(app.join(PORTABLE_DATA).join("prefs.json")).unwrap(), "mine");
        // A link (it could point anywhere) and a parent path are refused.
        let bad = root.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", bad.join("capishot")).unwrap();
        let bad_tgz = root.join("bad.tar.gz");
        assert!(std::process::Command::new("tar").arg("-czf").arg(&bad_tgz).arg("-C").arg(&bad).arg(".").status().unwrap().success());
        assert!(extract_tar(&bad_tgz, &root.join("out2")).is_err());
        let dotdot = root.join("dotdot.tar.gz");
        assert!(
            std::process::Command::new("tar").arg("-czf").arg(&dotdot).arg("-C").arg(&top).arg("--transform=s,^,../,").arg("capishot").status().unwrap().success()
        );
        assert!(extract_tar(&dotdot, &root.join("out3")).is_err());
        assert!(extract_tar(&root.join("missing.tar.gz"), &root.join("out4")).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_zip_extracts_and_its_app_folder_is_found_and_unsafe_paths_are_refused() {
        let root = tmp("zip");
        let mut z = photocraft_format::zip::ZipWriter::new();
        z.add("capishot-0.13.9-windows-x64-portable/capishot.exe", b"exe").unwrap();
        z.add("capishot-0.13.9-windows-x64-portable/portable.txt", b"p").unwrap();
        std::fs::write(root.join("u.zip"), z.finish().unwrap()).unwrap();
        extract_zip(&root.join("u.zip"), &root.join("out")).unwrap();
        assert_eq!(app_folder(&root.join("out")), Some(root.join("out/capishot-0.13.9-windows-x64-portable")));
        let mut bad = photocraft_format::zip::ZipWriter::new();
        bad.add("../evil.exe", b"x").unwrap();
        std::fs::write(root.join("bad.zip"), bad.finish().unwrap()).unwrap();
        assert!(extract_zip(&root.join("bad.zip"), &root.join("out2")).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_download_that_does_not_match_its_checksum_is_refused() {
        // A local server answering with the wrong bytes.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in listener.incoming().take(4) {
                use std::io::{Read, Write};
                let mut s = s.unwrap();
                let mut buf = [0u8; 2048];
                let _ = s.read(&mut buf);
                let body = b"not the published file";
                let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = s.write_all(body);
            }
        });
        let root = tmp("sha");
        let http = craft_update::http::Http::new("Capishot-test").unwrap();
        let stop = craft_update::http::Stop::default();
        let r = http.download(&format!("http://127.0.0.1:{port}/c.msi"), &root.join("c.msi"), 0, &"a".repeat(64), &stop, &mut |_| {});
        assert!(r.is_err(), "{r:?}");
        assert!(!root.join("c.msi").exists(), "nothing kept");
        let _ = std::fs::remove_dir_all(root);
    }

    /// The status line stops saying "Downloading" when the download ends, however it ends.
    #[test]
    fn the_status_line_says_how_the_download_ended() {
        use photocraft_engine::jobs::JobOutcome;
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let label = "Downloading Capishot 9.9.9";
        app.ui.status = label.into();
        on_download_end(&mut app, label, JobOutcome::Done(json!({"ready": true, "version": "9.9.9"})));
        assert_eq!(app.ui.status, "Capishot 9.9.9 ready to install");
        app.ui.status = label.into();
        on_download_end(&mut app, label, JobOutcome::Cancelled);
        assert_eq!(app.ui.status, "Update download cancelled");
        app.ui.status = label.into();
        on_download_end(&mut app, label, JobOutcome::Failed("checksum mismatch".into()));
        assert_eq!(app.ui.status, "Downloading Capishot 9.9.9: checksum mismatch");
        assert!(app.ui.status_error);
        assert!(app.ui.notices.iter().any(|n| n.title.contains("checksum mismatch")));
    }

    #[test]
    fn the_outcome_is_read_once() {
        let root = tmp("outcome");
        write_outcome(&root, &Outcome { version: "0.13.9".into(), ok: false, code: Some(1602), message: "cancelled".into() });
        let o = take_outcome(&root).unwrap();
        assert_eq!((o.code, o.ok), (Some(1602), false));
        assert!(take_outcome(&root).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
