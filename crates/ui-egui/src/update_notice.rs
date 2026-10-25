//! "Update available" on the desktop. At launch the shell fetches a small JSON manifest on its
//! own thread and puts the response body in [`UpdateCheck::manifest`]; when it names a newer
//! version than this build, a notice offers the download link ([`COMMAND`] opens it in the
//! browser). The app never downloads or installs an update itself.
//!
//! The manifest: `{"version": "0.13.0", "url_zip": "https://…", "url_msi": "https://…",
//! "sha256": {…}, "notes": "…"}`. The portable build links the zip, an installed one the MSI.

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
const TAG: &str = "update";
/// `prefs.dialogs` key holding the version whose notice was dismissed: it isn't offered again.
const DISMISSED: &str = "ui.updateDismissed";

/// The shell's half: it fills `manifest` once (from any thread); the next frame reads it.
#[derive(Clone, Debug, Default)]
pub struct UpdateCheck {
    pub manifest: Arc<Mutex<Option<String>>>,
    /// This copy runs in portable mode, so the zip is the download to offer.
    pub portable: bool,
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
}

/// A newer version and where to get it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub version: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The update `body` offers over `current`, if any: a newer version with an `https://` link.
pub fn parse(body: &str, current: &str, portable: bool) -> Option<Update> {
    let m: Manifest = serde_json::from_str(body).ok()?;
    let version = m.version.trim().to_string();
    if !is_newer(&version, current) {
        return None;
    }
    let (first, second) = if portable { (m.url_zip, m.url_msi) } else { (m.url_msi, m.url_zip) };
    let url = [first, second, m.url].into_iter().flatten().map(|u| u.trim().to_string()).find(|u| u.starts_with("https://"))?;
    let notes = m.notes.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    Some(Update { version, url, notes })
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

/// Once per frame: when the shell's manifest has arrived, offer the update it names.
pub fn poll(app: &mut PhotocraftApp) {
    let Some(check) = &app.services.update_check else { return };
    let Some(body) = check.manifest.lock().unwrap_or_else(PoisonError::into_inner).take() else { return };
    let portable = check.portable;
    let Some(update) = parse(&body, photocraft_engine::build_info::VERSION, portable) else { return };
    offer(app, update);
}

/// Show the notice for `update`, unless the user dismissed this version's before.
pub fn offer(app: &mut PhotocraftApp, update: Update) {
    let dismissed = app.session.prefs().dialogs.get(DISMISSED).and_then(Value::as_str) == Some(update.version.as_str());
    app.ui.update = Some(update.clone());
    if dismissed {
        return;
    }
    app.ui.notices.retain(|n| n.tag.as_deref() != Some(TAG));
    let mut lines = vec!["You have {current}. Nothing is installed automatically.".to_owned()];
    lines.extend(update.notes.clone());
    let id = crate::notices::post(app, "Capishot {version} is available", lines, false, None);
    if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
        n.args = vec![("version".into(), update.version.clone()), ("current".into(), photocraft_engine::build_info::VERSION.into())];
        n.actions = vec![crate::notices::NoticeAction { label: "Download".into(), command: COMMAND.into() }];
        n.tag = Some(TAG.into());
        n.sticky = true;
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
        poll(&mut app);
        assert!(app.ui.notices.is_empty(), "nothing before the manifest arrives");
        *check.manifest.lock().unwrap() = Some(BODY.into());
        poll(&mut app);
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
        poll(&mut app);
        assert!(app.ui.notices.iter().all(|n| n.tag.as_deref() != Some(TAG)), "dismissed version stays quiet");
        assert!(app.ui.update.is_some(), "the command still knows the link");
    }
}
