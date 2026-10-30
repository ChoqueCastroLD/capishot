//! Crash rescue in the browser: the "Recover N documents" dialog and File › Recover Documents….
//!
//! The browser shell keeps recovery checkpoints of unsaved work (autosave, `prefs_ui`) and, on
//! load, offers the ones a session left behind when it didn't close cleanly
//! ([`PhotocraftApp::offer_recovery`]); File › Recover Documents… asks it to offer every
//! checkpoint no open tab owns ([`PhotocraftApp::take_recovery_request`]). The dialog lists each
//! document with its thumbnail, name and when it was saved; the answer goes back to the shell
//! ([`PhotocraftApp::take_recovery_answer`]), which loads the chosen ones
//! ([`PhotocraftApp::recover_documents`]) or deletes them.

use std::collections::HashSet;

use egui::{Color32, Key, RichText, vec2};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// The File menu command that lists the checkpoints again.
pub const COMMAND: &str = "file.recoverDocuments";

/// The id `ui.inspect` lists the dialog under.
pub const CONTROL_ID: &str = "recovery";

/// One recoverable document, as the shell found it in storage.
#[derive(Clone, Debug)]
pub struct RecoveryOfferEntry {
    /// The checkpoint's key in the shell's storage.
    pub key: String,
    pub name: String,
    /// When the checkpoint was written, in milliseconds since the Unix epoch.
    pub saved_at_ms: f64,
    /// A small preview: width, height and straight RGBA8 pixels.
    pub thumbnail: Option<(u32, u32, Vec<u8>)>,
}

/// What the user chose, by checkpoint key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryAnswer {
    /// Open these documents (and keep their checkpoints until they are saved or closed).
    Recover(Vec<String>),
    /// Delete these checkpoints.
    Discard(Vec<String>),
    /// Keep everything for later (File › Recover Documents…).
    Later,
}

/// The open dialog.
pub(crate) struct RecoveryDialog {
    entries: Vec<RecoveryOfferEntry>,
    /// Keys left unticked.
    skipped: HashSet<String>,
    /// Offered on load after an unclean exit (else from the menu).
    crashed: bool,
    textures: Vec<Option<egui::TextureHandle>>,
    /// Where the buttons were drawn last frame, for `ui.inspect`.
    buttons: Vec<(&'static str, egui::Rect)>,
}

/// Recovery state the app keeps for the shell.
#[derive(Default)]
pub(crate) struct RecoveryState {
    pub(crate) dialog: Option<RecoveryDialog>,
    pub(crate) answer: Option<RecoveryAnswer>,
    pub(crate) requested: bool,
}

impl PhotocraftApp {
    /// Shows the recovery dialog with `entries` (newest first). `crashed`: offered on load after
    /// a session that didn't close cleanly; otherwise from File › Recover Documents…, where an
    /// empty list says there is nothing to recover.
    pub fn offer_recovery(&mut self, entries: Vec<RecoveryOfferEntry>, crashed: bool) {
        if crashed && entries.is_empty() {
            return;
        }
        let textures = entries.iter().map(|_| None).collect();
        self.recovery.dialog = Some(RecoveryDialog { entries, skipped: HashSet::new(), crashed, textures, buttons: Vec::new() });
    }

    /// Answer the open recovery dialog with every document it lists (the page reloaded on purpose
    /// after its graphics context was lost).
    pub fn accept_recovery_offer(&mut self) {
        if let Some(d) = self.recovery.dialog.take() {
            self.recovery.answer = Some(RecoveryAnswer::Recover(d.entries.iter().map(|e| e.key.clone()).collect()));
        }
    }

    /// The answer to the recovery dialog, once.
    pub fn take_recovery_answer(&mut self) -> Option<RecoveryAnswer> {
        self.recovery.answer.take()
    }

    /// Whether File › Recover Documents… was chosen since the last call.
    pub fn take_recovery_request(&mut self) -> bool {
        std::mem::take(&mut self.recovery.requested)
    }

    /// Whether the recovery dialog is open.
    pub fn recovery_dialog_open(&self) -> bool {
        self.recovery.dialog.is_some()
    }

    /// Opens recovered documents, one at a time on a worker (their checkpoints stay theirs: the
    /// next autosave updates them, saving or closing the document removes them).
    pub fn recover_documents(&mut self, documents: Vec<crate::Recoverable>) {
        crate::prefs_ui::queue_recovery(self, documents);
    }
}

/// Whether File › Recover Documents… can run: the shell keeps checkpoints it can list.
pub(crate) fn available(app: &PhotocraftApp) -> bool {
    app.services.recovery_menu
}

/// File › Recover Documents…: the shell lists the checkpoints and offers them.
pub(crate) fn request(app: &mut PhotocraftApp) {
    app.recovery.requested = true;
}

/// "2 min ago", "3 hours ago", "4 days ago": how long ago a checkpoint was written.
pub(crate) fn ago(saved_at_ms: f64, now_ms: f64) -> String {
    let minutes = ((now_ms - saved_at_ms) / 60_000.0).max(0.0) as u64;
    let lang = crate::i18n::current();
    match minutes {
        0 => tl!("just now").to_string(),
        1..=59 => crate::i18n::trn(lang, minutes, "{n} min ago", "{n} min ago"),
        60..=1439 => crate::i18n::trn(lang, minutes / 60, "{n} hour ago", "{n} hours ago"),
        _ => crate::i18n::trn(lang, minutes / 1440, "{n} day ago", "{n} days ago"),
    }
}

/// Draws the dialog while it is open.
pub(crate) fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(d) = app.recovery.dialog.as_mut() else { return };
    let t = Tokens::get(ctx);
    let now = wall_clock_ms(&app.services);
    let n = d.entries.len();
    let title = if n == 0 {
        tl!("Recover Documents").to_string()
    } else {
        crate::i18n::trn(crate::i18n::current(), n as u64, "Recover {n} document", "Recover {n} documents")
    };
    let message = match (n, d.crashed) {
        (0, _) => tl!("There are no documents to recover."),
        (_, true) => tl!("Capishot didn't close properly last time. These documents had unsaved changes:"),
        (_, false) => tl!("These documents had unsaved changes that were never saved:"),
    };
    let mut answer = None;
    let mut buttons = Vec::new();
    let modal = egui::Modal::new(egui::Id::unique("recovery-dialog")).show(ctx, |ui| {
        let width = (ctx.content_rect().width() - 48.0).clamp(240.0, 460.0);
        ui.set_width(width);
        ui.label(RichText::new(&title).font(crate::theme::semibold(15.0)));
        ui.add_space(4.0);
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.label(message);
        ui.add_space(8.0);
        let thumb = 56.0;
        egui::ScrollArea::vertical().max_height((ctx.content_rect().height() * 0.5).max(thumb + 8.0)).show(ui, |ui| {
            for (i, e) in d.entries.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mut on = !d.skipped.contains(&e.key);
                    if ui.checkbox(&mut on, "").changed() {
                        if on {
                            d.skipped.remove(&e.key);
                        } else {
                            d.skipped.insert(e.key.clone());
                        }
                    }
                    let (rect, _) = ui.allocate_exact_size(vec2(thumb, thumb), egui::Sense::hover());
                    ui.painter().rect_filled(rect, t.radius_sm, t.canvas);
                    if d.textures.get(i).is_some_and(Option::is_none)
                        && let Some((w, h, px)) = &e.thumbnail
                        && px.len() == (*w as usize) * (*h as usize) * 4
                    {
                        let image = egui::ColorImage::from_rgba_unmultiplied([*w as usize, *h as usize], px);
                        d.textures[i] = Some(ctx.load_texture(format!("recovery-thumb-{}", e.key), image, egui::TextureOptions::LINEAR));
                    }
                    if let Some(Some(tex)) = d.textures.get(i) {
                        let s = tex.size_vec2();
                        let k = (thumb / s.x.max(1.0)).min(thumb / s.y.max(1.0));
                        let r = egui::Rect::from_center_size(rect.center(), s * k);
                        ui.painter().image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                    }
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(&e.name).strong()).truncate());
                        ui.label(RichText::new(crate::i18n::fmt(tl!("Saved {when}"), &[("when", &ago(e.saved_at_ms, now))])).color(t.text_dim).size(12.0));
                    });
                });
                ui.add_space(4.0);
            }
        });
        ui.add_space(12.0);
        let chosen = d.entries.iter().filter(|e| !d.skipped.contains(&e.key)).count();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if n == 0 {
                let r = crate::widgets::primary_button(ui, tl!("Close"), 84.0);
                buttons.push(("close", r.rect));
                if r.clicked() {
                    answer = Some(RecoveryAnswer::Later);
                }
                return;
            }
            // Right to left: the default answer at the edge, as `dialog_buttons` puts it on Windows.
            let r = ui.add_enabled_ui(chosen > 0, |ui| crate::widgets::primary_button(ui, tl!("Recover"), 96.0)).inner;
            buttons.push(("recover", r.rect));
            if r.clicked() {
                answer = Some(RecoveryAnswer::Recover(d.entries.iter().filter(|e| !d.skipped.contains(&e.key)).map(|e| e.key.clone()).collect()));
            }
            let r = ui.add_enabled_ui(chosen > 0, |ui| crate::widgets::secondary_button(ui, tl!("Discard"), 84.0)).inner;
            buttons.push(("discard", r.rect));
            if r.clicked() {
                answer = Some(RecoveryAnswer::Discard(d.entries.iter().filter(|e| !d.skipped.contains(&e.key)).map(|e| e.key.clone()).collect()));
            }
            let r = crate::widgets::secondary_button(ui, tl!("Later"), 84.0);
            buttons.push(("later", r.rect));
            if r.clicked() {
                answer = Some(RecoveryAnswer::Later);
            }
        });
    });
    d.buttons = buttons;
    // Esc (or a click outside) keeps everything for later, never discards.
    if modal.should_close() && answer.is_none() {
        answer = Some(RecoveryAnswer::Later);
    }
    if answer.is_none() && n > 0 && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter)) {
        answer = Some(RecoveryAnswer::Recover(d.entries.iter().filter(|e| !d.skipped.contains(&e.key)).map(|e| e.key.clone()).collect()));
    }
    if let Some(answer) = answer {
        app.recovery.dialog = None;
        if !matches!(answer, RecoveryAnswer::Later) || n > 0 {
            app.recovery.answer = Some(answer);
        }
    }
}

/// Milliseconds since the Unix epoch: the shell's clock (the browser's `Date.now`), else the system's.
fn wall_clock_ms(services: &crate::Services) -> f64 {
    if let Some(clock) = services.clock {
        return clock();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
    }
    #[cfg(target_arch = "wasm32")]
    0.0
}

/// The dialog as `ui.inspect` lists it: the documents and where its buttons are (window points).
pub fn inspect(app: &PhotocraftApp) -> Option<Value> {
    let d = app.recovery.dialog.as_ref()?;
    let buttons: serde_json::Map<String, Value> =
        d.buttons.iter().map(|(name, r)| ((*name).to_string(), json!([r.min.x, r.min.y, r.max.x, r.max.y]))).collect();
    Some(json!({
        "id": CONTROL_ID,
        "kind": "recovery",
        "title": "Recover Documents",
        "fields": {
            "crashed": d.crashed,
            "documents": d.entries.iter().map(|e| json!({"key": e.key, "name": e.name, "savedAt": e.saved_at_ms, "thumbnail": e.thumbnail.is_some()})).collect::<Vec<_>>(),
            "buttons": buttons,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(key: &str, name: &str) -> RecoveryOfferEntry {
        RecoveryOfferEntry { key: key.into(), name: name.into(), saved_at_ms: 0.0, thumbnail: Some((2, 2, vec![255; 16])) }
    }

    #[test]
    fn ago_reads_minutes_hours_and_days() {
        assert_eq!(ago(0.0, 30_000.0), "just now");
        assert_eq!(ago(0.0, 60_000.0), "1 min ago");
        assert_eq!(ago(0.0, 5.0 * 3_600_000.0), "5 hours ago");
        assert_eq!(ago(0.0, 3.0 * 86_400_000.0), "3 days ago");
    }

    /// The dialog answers with the ticked documents; Esc keeps them all for later.
    #[test]
    fn dialog_answers_recover_discard_and_later() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let ctx = egui::Context::default();
        PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Pro);
        let frame = |app: &mut PhotocraftApp, events: Vec<egui::Event>| {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(1400.0, 860.0))), events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| show(app, ui.ctx()));
            out.textures_delta.clear();
        };
        app.offer_recovery(vec![entry("a", "Page 1"), entry("b", "Page 2")], true);
        frame(&mut app, Vec::new());
        let v = inspect(&app).unwrap();
        assert_eq!(v["fields"]["documents"].as_array().unwrap().len(), 2);
        assert!(v["fields"]["buttons"]["recover"].is_array(), "{v}");
        frame(&mut app, vec![egui::Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        assert_eq!(app.take_recovery_answer(), Some(RecoveryAnswer::Recover(vec!["a".into(), "b".into()])));
        assert!(!app.recovery_dialog_open());
        app.offer_recovery(vec![entry("a", "Page 1")], true);
        frame(&mut app, Vec::new());
        frame(&mut app, vec![egui::Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        assert_eq!(app.take_recovery_answer(), Some(RecoveryAnswer::Later));
        // Nothing to offer after a crash: no dialog. From the menu: "no documents", closed by Close.
        app.offer_recovery(Vec::new(), true);
        assert!(!app.recovery_dialog_open());
        app.offer_recovery(Vec::new(), false);
        assert!(app.recovery_dialog_open());
    }
}
