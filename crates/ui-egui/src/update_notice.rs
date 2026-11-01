//! "Update available" on the desktop. At launch and every [`RECHECK_SECS`] the manifest is fetched
//! off the UI thread into [`UpdateCheck::manifest`]; when it names a newer version than this
//! build, a notice offers it. Where the app can install updates itself (Windows,
//! [`UpdateCheck::installs`]) the notice has Update (download in the background and install on
//! restart, [`crate::win_update`]), Later and Skip This Version; elsewhere a link to download it.
//!
//! The manifest: `{"version": "0.13.0", "url_zip": "https://…", "url_msi": "https://…",
//! "sha256": {"zip": "…", "msi": "…"}, "size": {"zip": n, "msi": n}?, "min_version": "…"?,
//! "notes": "…"?, "notes_url": "https://…"?}`. The portable build takes the zip, an installed one
//! the MSI. Below `min_version` the update can't be skipped.

use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// Opens the newer version's download page (the notice's button).
pub const COMMAND: &str = "help.downloadUpdate";
/// Where the Windows app looks for a newer version.
pub const MANIFEST_URL: &str = "https://capichan.com/capishot/desktop/latest.json";
/// Larger responses are not a manifest.
pub const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
/// The notice's tag.
pub const TAG: &str = "update";
/// Skips the offered version (it isn't offered again).
pub const SKIP: &str = "help.skipUpdate";
/// Hides the notice until the next check.
pub const LATER: &str = "help.updateLater";
/// Opens the release notes.
pub const NOTES: &str = "help.updateNotes";
/// How often (seconds) a running app checks again.
pub const RECHECK_SECS: f64 = 6.0 * 3600.0;
/// `prefs.dialogs` key holding the version whose notice was dismissed: it isn't offered again.
const DISMISSED: &str = "ui.updateDismissed";

/// The shell's half: it fills `manifest` once (from any thread); the next frame reads it.
#[derive(Clone, Debug, Default)]
pub struct UpdateCheck {
    pub manifest: Arc<Mutex<Option<String>>>,
    /// This copy runs in portable mode, so the zip is the download to offer.
    pub portable: bool,
    /// The manifest's address, for the checks after the launch's (none: the launch's only).
    pub url: Option<String>,
    /// This app can download and install updates itself (Windows).
    pub installs: bool,
    /// The last check's time (egui input time) and whether it was asked for (Help › Check for
    /// Updates: the result is reported even when there is nothing new).
    pub state: Arc<Mutex<CheckState>>,
}

/// When the manifest was last asked for, and whether the user asked.
#[derive(Clone, Debug, Default)]
pub struct CheckState {
    pub last: Option<f64>,
    pub manual: bool,
}

#[derive(Deserialize)]
struct Manifest {
    version: String,
    #[serde(default)]
    url_zip: Option<String>,
    #[serde(default)]
    url_msi: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    notes: Option<String>,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    min_version: Option<String>,
    #[serde(default)]
    sha256: Option<std::collections::HashMap<String, String>>,
    #[serde(default)]
    size: Option<std::collections::HashMap<String, u64>>,
}

/// A newer version and where to get it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub version: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The file's published SHA-256 (hex), when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    /// The file's size in bytes, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes_url: Option<String>,
    /// This build is older than the manifest's minimum: the update can't be skipped.
    #[serde(default)]
    pub required: bool,
}

/// The update `body` offers over `current`, if any: a newer version with an `https://` link.
pub fn parse(body: &str, current: &str, portable: bool) -> Option<Update> {
    let m: Manifest = serde_json::from_str(body).ok()?;
    let version = m.version.trim().to_string();
    if !is_newer(&version, current) {
        return None;
    }
    let (first, second) = if portable { (("zip", m.url_zip), ("msi", m.url_msi)) } else { (("msi", m.url_msi), ("zip", m.url_zip)) };
    let (kind, url) = [first, second, ("", m.url)]
        .into_iter()
        .filter_map(|(k, u)| u.map(|u| (k, u.trim().to_string())))
        .find(|(_, u)| u.starts_with("https://") || (u.starts_with("http://") && is_loopback(u)))?;
    let notes = m.notes.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let sha256 = m
        .sha256
        .as_ref()
        .and_then(|s| s.get(kind))
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()));
    let size = m.size.as_ref().and_then(|s| s.get(kind)).copied();
    let notes_url = m.notes_url.map(|u| u.trim().to_string()).filter(|u| u.starts_with("https://"));
    let required = m.min_version.as_deref().is_some_and(|min| is_newer(min, current));
    Some(Update { version, url, notes, sha256, size, notes_url, required })
}

/// A local address (a test server): plain http is allowed there.
fn is_loopback(url: &str) -> bool {
    url.starts_with("http://127.0.0.1") || url.starts_with("http://localhost") || url.starts_with("http://[::1]")
}

/// `1.2.3` or `1.2.3-rc.1` (an optional leading `v`, build metadata ignored).
fn version_parts(v: &str) -> Option<([u64; 3], Option<&str>)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut nums = [0u64; 3];
    for (n, part) in core.split('.').enumerate() {
        *nums.get_mut(n)? = part.parse().ok()?;
    }
    Some((nums, pre))
}

/// Semantic-version order of pre-release tags: numeric identifiers numerically, others as text,
/// a shorter tag first when it is a prefix of the other.
fn cmp_pre(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.split('.'), b.split('.'));
    loop {
        match (x.next(), y.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(p), Some(q)) => {
                let o = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                    (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                    (Err(_), Err(_)) => p.cmp(q),
                };
                if o.is_ne() {
                    return o;
                }
            }
        }
    }
}

/// Is `candidate` a later version than `current`? Unreadable versions never are.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (Some((a, ap)), Some((b, bp))) = (version_parts(candidate), version_parts(current)) else { return false };
    match a.cmp(&b) {
        std::cmp::Ordering::Equal => match (ap, bp) {
            (None, Some(_)) => true,
            (Some(p), Some(q)) => cmp_pre(p, q).is_gt(),
            _ => false,
        },
        o => o.is_gt(),
    }
}

/// Once per frame: when a manifest has arrived, offer the update it names; every
/// [`RECHECK_SECS`] fetch it again.
pub fn poll(app: &mut PhotocraftApp, now: f64) {
    let Some(check) = app.services.update_check.clone() else { return };
    {
        let mut st = check.state.lock().unwrap_or_else(PoisonError::into_inner);
        match st.last {
            None => st.last = Some(now),
            Some(t) if now - t > RECHECK_SECS => {
                st.last = Some(now);
                drop(st);
                refetch(&check);
            }
            _ => {}
        }
    }
    let Some(body) = check.manifest.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
    let manual = std::mem::take(&mut check.state.lock().unwrap_or_else(PoisonError::into_inner).manual);
    match parse(&body, photocraft_engine::build_info::VERSION, check.portable) {
        Some(update) => {
            if manual {
                app.session.prefs.edit(|p| {
                    p.dialogs.remove(DISMISSED);
                });
            }
            offer(app, update);
        }
        None if manual => {
            app.ui.status = crate::i18n::fmt(tl!("Capishot {version} is up to date"), &[("version", photocraft_engine::build_info::VERSION)]);
            app.ui.status_error = false;
        }
        None => {}
    }
}

/// Fetch the manifest again, off the UI thread.
pub fn refetch(check: &UpdateCheck) {
    let Some(url) = check.url.clone() else { return };
    let slot = check.manifest.clone();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = std::thread::Builder::new().name("capishot-update-check".into()).spawn(move || {
            let got = craft_update::http::Http::new(&format!("Capishot/{}", photocraft_engine::build_info::VERSION))
                .and_then(|h| h.get_bytes(&url, MAX_MANIFEST_BYTES as usize));
            match got {
                Ok(Some(bytes)) => *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(String::from_utf8_lossy(&bytes).into_owned()),
                Ok(None) => log::info!("update check: {url}: nothing published"),
                Err(e) => log::info!("update check: {url}: {e}"),
            }
        });
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (url, slot);
}

/// Help › Check for Updates: fetch now and report the result, even when nothing is new.
pub fn check_now(app: &mut PhotocraftApp) -> Result<Value, String> {
    let Some(check) = app.services.update_check.clone() else { return Err(tl!("Updates are checked in the Windows app only").into()) };
    check.state.lock().unwrap_or_else(PoisonError::into_inner).manual = true;
    refetch(&check);
    app.ui.status = tl!("Checking for updates…").into();
    app.ui.status_error = false;
    Ok(json!({"checking": true}))
}

/// Show the notice for `update`, unless the user dismissed this version's before.
pub fn offer(app: &mut PhotocraftApp, update: Update) {
    let dismissed = app.session.prefs().dialogs.get(DISMISSED).and_then(Value::as_str) == Some(update.version.as_str());
    app.ui.update = Some(update.clone());
    if dismissed {
        return;
    }
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(TAG));
    let installs = app.services.update_check.as_ref().is_some_and(|c| c.installs) && update.sha256.is_some();
    let mut lines = vec![if installs { "You have {current}." } else { "You have {current}. Nothing is installed automatically." }.to_owned()];
    if update.required {
        lines.push("This version is required: yours is no longer supported.".into());
    }
    lines.extend(update.notes.clone());
    let id = crate::notices::post(app, "Capishot {version} is available", lines, false, None);
    let action = |label: &str, command: &str| crate::notices::NoticeAction { label: label.into(), command: command.into() };
    let mut actions = if installs { vec![action("Update", crate::win_update::DOWNLOAD), action("Later", LATER)] } else { vec![action("Download", COMMAND)] };
    if installs && !update.required {
        actions.push(action("Skip This Version", SKIP));
    }
    if update.notes_url.is_some() {
        actions.push(action("Release Notes", NOTES));
    }
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.args = vec![("version".into(), update.version.clone()), ("current".into(), photocraft_engine::build_info::VERSION.into())];
        n.actions = actions;
        n.tag = Some(TAG.into());
        n.sticky = true;
    }
    // Install Updates Automatically: download it at once (it installs when the app closes).
    if installs && app.session.prefs().interface.auto_install_updates && crate::win_update::ready().is_none() {
        let _ = crate::win_update::download(app);
    }
}

/// [`COMMAND`]: open the download link and stop offering this version.
pub fn download(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let Some(update) = app.ui.update.clone() else { return Err(tl!("No update is available").into()) };
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(TAG));
    let version = update.version.clone();
    app.session.prefs.edit(|p| {
        p.dialogs.insert(DISMISSED.into(), Value::String(version));
    });
    let opened = crate::links::open(app, ctx, &update.url);
    Ok(json!({"url": opened["url"], "version": update.version}))
}

/// [`SKIP`]: this version is not offered again.
pub fn skip(app: &mut PhotocraftApp) -> Result<Value, String> {
    let version = app.ui.update.as_ref().map(|u| u.version.clone()).ok_or_else(|| tl!("No update is available").to_string())?;
    app.session.prefs.edit(|p| {
        p.dialogs.insert(DISMISSED.into(), Value::String(version.clone()));
    });
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(TAG));
    Ok(json!({"skipped": version}))
}

/// [`LATER`]: hide the notice; the next check offers it again.
pub fn later(app: &mut PhotocraftApp) -> Result<Value, String> {
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(TAG));
    Ok(json!({"later": true}))
}

/// [`NOTES`]: open the release notes.
pub fn notes(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<Value, String> {
    let url = app.ui.update.as_ref().and_then(|u| u.notes_url.clone()).ok_or_else(|| tl!("No release notes").to_string())?;
    Ok(crate::links::open(app, ctx, &url))
}

/// Dismissing the notice (its ×) also stops offering that version.
pub fn dismissed(app: &mut PhotocraftApp, tag: Option<&str>) {
    if tag != Some(TAG) {
        return;
    }
    if let Some(version) = app.ui.update.as_ref().map(|u| u.version.clone()) {
        app.session.prefs.edit(|p| {
            p.dialogs.insert(DISMISSED.into(), Value::String(version));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use photocraft_engine::Session;

    const BODY: &str = r#"{"version": "9.1.0", "url_zip": "https://example.com/c.zip", "url_msi": "https://example.com/c.msi",
        "sha256": {"zip": "ab", "msi": "cd"}, "notes": "Faster brushes"}"#;

    #[test]
    fn versions_compare_by_semver() {
        assert!(is_newer("0.12.1", "0.12.0"));
        assert!(is_newer("0.13", "0.12.9"));
        assert!(is_newer("v1.0.0", "0.99.0"));
        assert!(is_newer("0.12.0", "0.12.0-rc.2"));
        assert!(is_newer("0.12.0-rc.10", "0.12.0-rc.2"));
        assert!(!is_newer("0.12.0", "0.12.0"));
        assert!(!is_newer("0.11.9", "0.12.0"));
        assert!(!is_newer("0.12.0-rc.1", "0.12.0"));
        assert!(!is_newer("soon", "0.12.0"));
        assert!(!is_newer("1.2.3.4", "0.1.0"));
    }

    #[test]
    fn the_manifest_links_the_installer_or_the_zip() {
        let installed = parse(BODY, "0.12.0", false).unwrap();
        assert_eq!((installed.version.as_str(), installed.url.as_str()), ("9.1.0", "https://example.com/c.msi"));
        assert_eq!(installed.notes.as_deref(), Some("Faster brushes"));
        assert_eq!(parse(BODY, "0.12.0", true).unwrap().url, "https://example.com/c.zip");
        // No MSI yet: the zip is offered to everyone.
        let zip_only = r#"{"version": "9.1.0", "url_zip": "https://example.com/c.zip", "url_msi": null}"#;
        assert_eq!(parse(zip_only, "0.12.0", false).unwrap().url, "https://example.com/c.zip");
        // Not newer, not https, or not a manifest: nothing.
        assert_eq!(parse(BODY, "9.1.0", false), None);
        assert_eq!(parse(r#"{"version": "9.1.0", "url_zip": "http://example.com/c.zip"}"#, "0.12.0", true), None);
        assert_eq!(parse("<html>404</html>", "0.12.0", false), None);
    }

    #[test]
    fn a_newer_version_is_offered_once_with_a_download_button() {
        let check = UpdateCheck::default();
        let mut app = PhotocraftApp::new(Session::new(), Services { update_check: Some(check.clone()), ..Default::default() });
        app.ui.notices.clear();
        poll(&mut app, 0.0);
        assert!(app.ui.notices.is_empty(), "nothing before the manifest arrives");
        *check.manifest.lock().unwrap() = Some(BODY.into());
        poll(&mut app, 0.0);
        let n = app.ui.notices.iter().find(|n| n.tag.as_deref() == Some(TAG)).expect("update notice");
        assert_eq!(n.text(&n.title), "Capishot 9.1.0 is available");
        assert_eq!(n.actions[0].command, COMMAND);
        assert!(n.lines.iter().any(|l| l == "Faster brushes"));
        assert!(n.sticky, "it waits for the user rather than auto-hiding");
        // The button opens the link and the notice goes; the same version isn't offered again.
        let ctx = egui::Context::default();
        let v = crate::menus::invoke(&mut app, &ctx, COMMAND, json!({})).unwrap();
        assert_eq!(v["url"], "https://example.com/c.msi");
        assert!(app.ui.notices.iter().all(|n| n.tag.as_deref() != Some(TAG)));
        *check.manifest.lock().unwrap() = Some(BODY.into());
        poll(&mut app, 0.0);
        assert!(app.ui.notices.iter().all(|n| n.tag.as_deref() != Some(TAG)), "dismissed version stays quiet");
        assert!(app.ui.update.is_some(), "the command still knows the link");
    }

    #[test]
    fn the_manifest_gives_checksum_size_notes_and_minimum() {
        let body = r#"{"version": "0.14.0", "url_msi": "https://example.com/c.msi", "url_zip": "https://example.com/c.zip",
            "sha256": {"msi": "ABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABABAB", "zip": "short"},
            "size": {"msi": 1234}, "min_version": "0.13.5", "notes_url": "https://example.com/notes"}"#;
        let u = parse(body, "0.13.2", false).unwrap();
        assert_eq!(u.sha256.as_deref(), Some("abababababababababababababababababababababababababababababababab"));
        assert_eq!((u.size, u.notes_url.as_deref(), u.required), (Some(1234), Some("https://example.com/notes"), true));
        let z = parse(body, "0.13.6", true).unwrap();
        assert_eq!((z.sha256, z.required), (None, false), "a bad checksum is none; above the minimum it isn't required");
        // A local test server may be plain http.
        assert!(parse(r#"{"version": "0.14.0", "url_msi": "http://127.0.0.1:8000/c.msi"}"#, "0.13.0", false).is_some());
    }

    #[test]
    fn where_the_app_installs_updates_the_notice_has_update_later_and_skip() {
        let check = UpdateCheck { installs: true, ..Default::default() };
        let mut app = PhotocraftApp::new(Session::new(), Services { update_check: Some(check.clone()), ..Default::default() });
        let body = |min: &str| {
            format!(r#"{{"version": "9.0.0", "url_msi": "https://example.com/c.msi", "sha256": {{"msi": "{}"}}, "min_version": "{min}"}}"#, "c".repeat(64))
        };
        *check.manifest.lock().unwrap() = Some(body("0.0.1"));
        poll(&mut app, 0.0);
        let n = app.ui.notices.iter().find(|n| n.tag.as_deref() == Some(TAG)).unwrap();
        let cmds: Vec<&str> = n.actions.iter().map(|a| a.command.as_str()).collect();
        assert_eq!(cmds, [crate::win_update::DOWNLOAD, LATER, SKIP]);
        // Later hides it; Skip stops offering the version.
        let ctx = egui::Context::default();
        crate::menus::invoke(&mut app, &ctx, LATER, json!({})).unwrap();
        assert!(app.ui.notices.iter().all(|n| n.tag.as_deref() != Some(TAG)));
        // A required update has no Skip.
        *check.manifest.lock().unwrap() = Some(body("99.0.0"));
        poll(&mut app, 1.0);
        let n = app.ui.notices.iter().find(|n| n.tag.as_deref() == Some(TAG)).unwrap();
        assert!(n.actions.iter().all(|a| a.command != SKIP), "required: no skipping");
        // Six hours later it checks again (no url here: nothing is fetched, the time moves on).
        poll(&mut app, RECHECK_SECS + 10.0);
        assert!(check.state.lock().unwrap().last.unwrap() > RECHECK_SECS);
    }
}
