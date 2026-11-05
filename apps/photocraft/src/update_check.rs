//! The launch-time update check: one small GET of the release manifest on a background thread,
//! handed to `photocraft_ui_egui::update_notice`, which compares versions and shows the notice
//! (and checks again every few hours). It sends nothing but the request itself (no identifiers,
//! no usage data). On Windows the notice's Update downloads and installs it
//! (`photocraft_ui_egui::win_update`).
//!
//! On by default in the Windows app and in the official Linux packages (AppImage, tar.gz, .deb),
//! which say how they were installed in a [`CHANNEL_FILE`] beside the binary: the AppImage and
//! the tar.gz install updates themselves, the .deb only links the download page. Other Linux
//! builds (from source, distributions) don't check. `CAPISHOT_UPDATE_URL` points it at another
//! manifest on any platform (`off` turns it off).

use std::path::Path;

use photocraft_ui_egui::update_notice::{Channel, MANIFEST_URL, MAX_MANIFEST_BYTES, UpdateCheck};

/// The file the Linux packages put beside `capishot`: `appimage`, `tar` or `deb`.
pub const CHANNEL_FILE: &str = "capishot.channel";

/// How this Linux copy was installed, from the [`CHANNEL_FILE`] in `exe_dir`; an AppImage only
/// when it runs as one (`appimage`, the runtime's `$APPIMAGE`, is set).
fn linux_channel(exe_dir: Option<&Path>, appimage: bool) -> Option<Channel> {
    let text = std::fs::read_to_string(exe_dir?.join(CHANNEL_FILE)).ok()?;
    match text.trim() {
        "appimage" if appimage => Some(Channel::AppImage),
        "tar" => Some(Channel::Tar),
        "deb" => Some(Channel::Deb),
        _ => None,
    }
}

/// How this official copy was installed, as the usage statistics name it (`None`: not an official
/// package: a build from source, a distribution's, macOS).
pub fn channel_name(portable: bool) -> Option<&'static str> {
    if cfg!(windows) {
        return Some(if portable { "zip" } else { "msi" });
    }
    if !cfg!(target_os = "linux") {
        return None;
    }
    let exe = std::env::current_exe().ok();
    match linux_channel(exe.as_deref().and_then(Path::parent), std::env::var_os("APPIMAGE").is_some_and(|v| !v.is_empty()))? {
        Channel::AppImage => Some("appimage"),
        Channel::Tar => Some("tar"),
        Channel::Deb => Some("deb"),
        // Windows' own, handled above.
        Channel::Msi | Channel::Zip => None,
    }
}

/// The manifest to fetch, from `CAPISHOT_UPDATE_URL`'s value (`None`: unset), if any; `known`:
/// this copy's download is one the manifest lists.
fn manifest_url(var: Option<String>, known: bool) -> Option<String> {
    match var.as_deref().map(str::trim) {
        Some("" | "off" | "0") => None,
        Some(url) => Some(url.to_string()),
        None => known.then(|| MANIFEST_URL.to_string()),
    }
}

/// Start the check; `None` when it is off. A failed request (offline, no manifest yet) is only
/// logged.
pub fn start(portable: bool) -> Option<UpdateCheck> {
    let channel = if cfg!(target_os = "linux") {
        let exe = std::env::current_exe().ok();
        linux_channel(exe.as_deref().and_then(Path::parent), std::env::var_os("APPIMAGE").is_some_and(|v| !v.is_empty()))
    } else {
        None
    };
    let url = manifest_url(std::env::var("CAPISHOT_UPDATE_URL").ok(), cfg!(windows) || channel.is_some())?;
    // Re-checked every few hours by the app; Windows copies, AppImages and tar.gz folders
    // download and install updates themselves (`win_update`).
    let installs = cfg!(windows) || matches!(channel, Some(Channel::AppImage | Channel::Tar));
    let check = UpdateCheck { portable, channel, url: Some(url.clone()), installs, ..Default::default() };
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
        assert_eq!(manifest_url(None, true).as_deref(), Some(MANIFEST_URL));
        assert_eq!(manifest_url(None, false), None);
        assert_eq!(manifest_url(Some("off".into()), true), None);
        assert_eq!(manifest_url(Some(" ".into()), true), None);
        assert_eq!(manifest_url(Some("http://127.0.0.1:9/latest.json".into()), false).as_deref(), Some("http://127.0.0.1:9/latest.json"));
    }

    #[test]
    fn the_linux_packages_say_how_they_were_installed() {
        let dir = std::env::temp_dir().join(format!("capishot-channel-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(linux_channel(Some(&dir), true), None, "no file: a build from source");
        for (text, appimage, want) in [
            ("appimage\n", true, Some(Channel::AppImage)),
            ("appimage", false, None),
            ("tar", false, Some(Channel::Tar)),
            ("deb", false, Some(Channel::Deb)),
            ("rpm", false, None),
        ] {
            std::fs::write(dir.join(CHANNEL_FILE), text).unwrap();
            assert_eq!(linux_channel(Some(&dir), appimage), want, "{text:?}");
        }
        assert_eq!(linux_channel(None, false), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
