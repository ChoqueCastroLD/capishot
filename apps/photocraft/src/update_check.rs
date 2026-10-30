//! The launch-time update check: one small GET of the release manifest on a background thread,
//! handed to `photocraft_ui_egui::update_notice`, which compares versions and shows the notice.
//! It sends nothing but the request itself (no identifiers, no usage data) and never downloads
//! or installs the update.
//!
//! On by default in the Windows app only (the manifest lists Windows downloads).
//! `CAPISHOT_UPDATE_URL` points it at another manifest on any platform (`off` turns it off).

use photocraft_ui_egui::update_notice::{MANIFEST_URL, MAX_MANIFEST_BYTES, UpdateCheck};

/// The manifest to fetch, from `CAPISHOT_UPDATE_URL`'s value (`None`: unset), if any.
fn manifest_url(var: Option<String>) -> Option<String> {
    match var.as_deref().map(str::trim) {
        Some("" | "off" | "0") => None,
        Some(url) => Some(url.to_string()),
        None => cfg!(windows).then(|| MANIFEST_URL.to_string()),
    }
}

/// Start the check; `None` when it is off. A failed request (offline, no manifest yet) is only
/// logged.
pub fn start(portable: bool) -> Option<UpdateCheck> {
    let url = manifest_url(std::env::var("CAPISHOT_UPDATE_URL").ok())?;
    let check = UpdateCheck { portable, ..Default::default() };
    let slot = check.manifest.clone();
    let spawned =
        std::thread::Builder::new().name("capishot-update-check".into()).spawn(move || match photocraft_fontfetch::get_small(&url, MAX_MANIFEST_BYTES) {
            Ok(bytes) => {
                *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(String::from_utf8_lossy(&bytes).into_owned());
            }
            Err(e) => log::info!("update check: {url}: {e}"),
        });
    if let Err(e) = spawned {
        log::warn!("update check not started: {e}");
    }
    Some(check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_overrides_or_turns_off_the_default() {
        assert_eq!(manifest_url(None), cfg!(windows).then(|| MANIFEST_URL.to_string()));
        assert_eq!(manifest_url(Some("off".into())), None);
        assert_eq!(manifest_url(Some(" ".into())), None);
        assert_eq!(manifest_url(Some("http://127.0.0.1:9/latest.json".into())).as_deref(), Some("http://127.0.0.1:9/latest.json"));
    }
}
