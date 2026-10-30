use super::PhotocraftApp;
use serde_json::Value;
use std::hash::{DefaultHasher, Hash, Hasher};

// Sampling misses changed channels even in a 1x1 image: include the complete OS image.
fn clip_signature(w: u32, h: u32, px: &[u8]) -> u64 {
    let mut sig = DefaultHasher::new();
    w.hash(&mut sig);
    h.hash(&mut sig);
    px.hash(&mut sig);
    sig.finish()
}

/// The commands that paste the clipboard image, and so read the OS clipboard first: an image
/// copied in another app (a screenshot, a browser image) pastes like one copied here.
pub(crate) fn is_os_paste(id: &str) -> bool {
    matches!(id, "edit.paste" | "edit.pasteSpecial.pasteInPlace" | "edit.pasteSpecial.pasteInto" | "edit.pasteSpecial.pasteOutside" | "file.newFromClipboard")
}

impl PhotocraftApp {
    /// Mirror the session clipboard onto the OS clipboard (RGBA8).
    pub(super) fn export_os_clipboard(&mut self) {
        self.clip_unmirrored = true;
        if !self.session.prefs().general.export_clipboard {
            return;
        }
        let (Some(set), Some(clip)) = (self.services.clipboard_set_image.as_mut(), self.session.clipboard.as_ref()) else { return };
        let b = clip.bounds;
        if b.is_empty() {
            return;
        }
        let mut px = vec![[0u8; 4]; b.width() as usize * b.height() as usize];
        clip.surface.read_rgba8_into(b, &mut px);
        let bytes: Vec<u8> = px.into_iter().flatten().collect();
        if set(b.width(), b.height(), &bytes).is_ok() {
            self.os_clip_sig = Some(clip_signature(b.width(), b.height(), &bytes));
            self.clip_unmirrored = false;
        }
    }

    /// Before an explicit paste on a platform that reads the OS clipboard asynchronously: start
    /// the read and park `id` until it finishes (true). Automation never waits: an agent has no
    /// user gesture to grant the read with.
    pub(crate) fn wait_for_os_clipboard(&mut self, id: &str, params: &Value) -> bool {
        if self.automation_input {
            return false;
        }
        let Some(refresh) = self.services.clipboard_refresh.as_mut() else { return false };
        if !refresh() {
            return false;
        }
        // A second paste while the first still waits replaces it: one paste per read.
        self.clip_waiting = Some((id.to_string(), params.clone()));
        self.ui.status = tl!("Reading the clipboard…").into();
        self.ui.status_error = false;
        true
    }

    /// The asynchronous clipboard read [`Self::wait_for_os_clipboard`] started has finished: run
    /// the paste that waited for it (None when none did).
    pub fn resume_clipboard_paste(&mut self, ctx: &egui::Context) -> Option<Result<Value, String>> {
        let (id, params) = self.clip_waiting.take()?;
        if self.ui.status == tl!("Reading the clipboard…") {
            self.ui.status.clear();
        }
        // Enabled on the strength of the clipboard service; the command itself reports what's wrong.
        let r = crate::menus::invoke(self, ctx, &id, params);
        if let Err(e) = &r {
            self.ui.status = e.clone();
            self.ui.status_error = true;
        }
        Some(r)
    }

    /// If the OS clipboard holds an image that isn't the one we put there, make it the session
    /// clipboard (so ⌘V pastes screenshots and images copied in other apps, like Photoshop).
    /// Returns `Ok(true)` when a new external image was imported. Once the OS clipboard no longer holds
    /// the image we mirrored or imported (text or a file was copied since), that image is stale:
    /// it is dropped rather than pasted. Unexported internal copies survive unavailable or empty
    /// system clipboards.
    pub(crate) fn import_os_clipboard(&mut self) -> Result<bool, String> {
        let Some(get) = self.services.clipboard_get_image.as_mut() else { return Ok(false) };
        // A read failure says nothing about the clipboard contents. Keep our editable copy and
        // its signature rather than treating the failure as an empty image.
        let image = match get() {
            Ok(image) => image,
            Err(error) => {
                // An unexported copy remains usable without an OS clipboard (for example, in
                // a Wayland sandbox). Mirrored/imported copies still require a successful read
                // before pasting, because their system contents may have changed.
                if self.clip_unmirrored && !self.clip_external && self.session.clipboard.is_some() {
                    return Ok(false);
                }
                if self.session.clipboard.is_none() && self.services.clipboard_unreadable.as_ref().is_some_and(|unreadable| unreadable()) {
                    // Let paste explain the unavailable backend without treating a saved copy as empty.
                    return Ok(false);
                }
                self.ui.status = error.clone();
                self.ui.status_error = true;
                return Err(error);
            }
        };
        let image = image.filter(|(w, h, bytes)| *w > 0 && *h > 0 && bytes.len() == *w as usize * *h as usize * 4);
        let Some((w, h, bytes)) = image else {
            if self.os_clip_sig.take().is_some() && !self.clip_unmirrored {
                self.session.clipboard = None;
                self.clip_external = false;
                self.clip_unmirrored = true;
            }
            return Ok(false);
        };
        let sig = clip_signature(w, h, &bytes);
        if self.os_clip_sig == Some(sig) && self.session.clipboard.is_some() {
            return Ok(false);
        }
        let r = photocraft_geom::Rect::new(0, 0, w as i32, h as i32);
        let mut surface = photocraft_raster::Surface::from_interleaved(photocraft_color::PixelFormat::RGBA8, r, &bytes);
        surface.prune();
        self.session.clipboard = Some(photocraft_engine::edit_cmds::Clip { surface, bounds: r, layers: None, resolution: None, source: None });
        self.os_clip_sig = Some(sig);
        self.clip_external = true;
        self.clip_unmirrored = false;
        Ok(true)
    }
}
