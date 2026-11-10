//! File › Save of a document whose file is flat (a JPEG, PNG, WebP…), on the desktop: as in
//! Photoshop it can go straight back to that file, with its name and extension, flattened if it
//! has to be. The question ([`show`]) offers that, Save As or Cancel, and can be remembered;
//! Window › Save ([`panel`]) changes what was remembered and the quality of a JPEG or lossy WebP.
//! Written through the desktop's atomic write: a failed save leaves the original as it was.

use photocraft_engine::prefs::{OriginalQuality, SaveFlatFile};
use serde_json::{Value, json};

use crate::{FormatChoices, PhotocraftApp};

pub const PANEL: &str = "window.panel.saveOptions";

/// A File › Save waiting on the answer.
pub struct Prompt {
    doc: photocraft_doc::DocId,
    path: String,
    remember: bool,
}

/// The document's own file when File › Save would otherwise ask for a name: flat, writable by
/// its format, and still there.
fn own_flat_file(app: &PhotocraftApp) -> Option<String> {
    // An action being recorded keeps its plain Save, which writes the file it is played on.
    if cfg!(target_arch = "wasm32") || app.session.actions.recording.is_some() || app.session.is_enabled("layer.smartObjects.saveContents") {
        return None;
    }
    let st = app.session.active()?;
    let path = st.path.clone()?;
    let writable = photocraft_engine::file_cmds::extension(&path).is_some_and(|e| photocraft_codecs::from_extension(&e).is_some_and(|f| f.caps().write));
    (writable && !photocraft_engine::file_cmds::saves_in_place(&path) && !photocraft_engine::file_cmds::holds_at(&st.doc, &path) && std::path::Path::new(&path).is_file()).then_some(path)
}

/// File › Save of a flat file: the remembered answer, or the question. `None` saves as before.
pub fn intercept(app: &mut PhotocraftApp, choices: FormatChoices) -> Option<Result<Value, String>> {
    if choices.tga_bits.is_some() || choices.dds.is_some() || choices.jpeg.is_some() {
        return None;
    }
    let path = own_flat_file(app)?;
    match app.session.prefs().file_handling.save_flat_file {
        SaveFlatFile::SaveAs => None,
        SaveFlatFile::Original => Some(over_original(app, path)),
        SaveFlatFile::Ask => {
            let doc = match app.active_doc_id() {
                Ok(doc) => doc,
                Err(e) => return Some(Err(e)),
            };
            app.save_original = Some(Prompt { doc, path: path.clone(), remember: false });
            Some(Ok(json!({"path": path, "warnings": [], "confirmationPending": true})))
        }
    }
}

/// Writes the active document over its own file.
fn over_original(app: &mut PhotocraftApp, path: String) -> Result<Value, String> {
    let doc = app.active_doc_id()?;
    let maximum = app.session.prefs().file_handling.original_quality == OriginalQuality::Maximum;
    // Another app changed the file: its own question first. Replace writes at the same quality.
    let jpeg = maximum.then_some(crate::jpeg_options_ui::JpegChoice { quality: Some(crate::jpeg_options_ui::MAX_QUALITY), format: Default::default() });
    if let Some(r) = crate::disk_change_prompt::intercept(app, doc, &path, true, FormatChoices { jpeg, ..Default::default() }) {
        return r;
    }
    app.save_over_original(path, maximum)
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let flattens = app.save_original.as_ref().is_some_and(|p| app.save_loses_content(&p.path));
    let Some(p) = &mut app.save_original else { return };
    let (mut original, mut save_as, mut cancel) = (false, false, false);
    let mut remember = p.remember;
    let name = crate::file_open::display_name(&p.path);
    let title = crate::i18n::fmt(tl!("Save “{name}”"), &[("name", name.as_str())]);
    let modal = egui::Modal::new(egui::Id::unique("save-original")).show(ctx, |ui| {
        ui.set_max_width(460.0);
        ui.label(egui::RichText::new(&title).font(crate::theme::semibold(15.0)));
        ui.add_space(4.0);
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.label(tl!("Process and save writes the image over the original file, with the same name and extension."));
        if flattens {
            ui.add_space(6.0);
            ui.label(tl!("The file can't hold layers: it gets the flattened image. They stay editable here until you close the document."));
        }
        ui.add_space(8.0);
        crate::widgets::checkbox(ui, &mut remember, tl!("Remember my choice"));
        ui.add_space(12.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            original = crate::widgets::primary_button(ui, tl!("Process and Save to Original"), 0.0).clicked();
            save_as = crate::widgets::secondary_button(ui, tl!("Save As…"), 0.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    cancel |= modal.should_close();
    p.remember = remember;
    let enter = !cancel && !save_as && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if !(original || save_as || cancel || enter) {
        return;
    }
    let Some(p) = app.save_original.take() else { return };
    if cancel {
        app.ui.status = tl!("Save cancelled").into();
        app.ui.status_error = false;
        return;
    }
    let to_original = original || enter;
    if p.remember {
        let answer = if to_original { SaveFlatFile::Original } else { SaveFlatFile::SaveAs };
        app.session.edit_prefs(|prefs| prefs.file_handling.save_flat_file = answer);
    }
    let written = app.with_document(p.doc, |app| if to_original { over_original(app, p.path) } else { app.save_dialog_flow(None, FormatChoices::default(), true) });
    if let Err(e) = written {
        app.ui.status = crate::i18n::fmt(tl!("Save failed: {error}"), &[("error", &e)]);
        app.ui.status_error = true;
    }
}

/// Window › Save.
pub fn panel(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.shell.save_options {
        return;
    }
    let fh = &app.session.prefs().file_handling;
    let (mut on_save, mut quality) = (fh.save_flat_file.name().to_string(), fh.original_quality.name().to_string());
    let (was_save, was_quality) = (on_save.clone(), quality.clone());
    let mut close = false;
    crate::analysis_ui::panel_window(app, ctx, "save-options", tl!("Save"), egui::vec2(-460.0, 120.0), 330.0, |ui| {
        close = crate::analysis_ui::title_row(ui, tl!("Save"));
        ui.label(tl!("Ctrl+S on a JPEG, PNG or other flat file:"));
        let options = [("ask".to_string(), tl!("Ask")), ("original".to_string(), tl!("Save to the original")), ("saveAs".to_string(), tl!("Save As…"))];
        crate::widgets::dropdown(ui, "save-flat-file", tl!("Save"), &mut on_save, &options, 300.0);
        ui.add_space(8.0);
        ui.label(tl!("JPEG and lossy WebP quality:"));
        let options = [("maximum".to_string(), tl!("Maximum (larger file)")), ("file".to_string(), tl!("The file's own quality"))];
        crate::widgets::dropdown(ui, "save-original-quality", tl!("Quality:"), &mut quality, &options, 300.0);
        ui.add_space(6.0);
        ui.label(egui::RichText::new(tl!("PNG, TIFF and lossless WebP lose nothing. A JPEG is compressed again on each save: Maximum keeps the loss invisible.")).small());
    });
    if on_save != was_save || quality != was_quality {
        let (on_save, quality) = (SaveFlatFile::parse(&on_save).unwrap_or_default(), OriginalQuality::parse(&quality).unwrap_or_default());
        app.session.edit_prefs(|prefs| {
            prefs.file_handling.save_flat_file = on_save;
            prefs.file_handling.original_quality = quality;
        });
    }
    if close {
        app.ui.shell.save_options = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Services;
    use egui_kittest::kittest::Queryable;
    use std::{cell::RefCell, rc::Rc};

    type Writes = Rc<RefCell<Vec<(String, Option<u8>)>>>;

    /// A document bound to a real flat file on disk.
    fn harness(ext: &str, layered: bool) -> (egui_kittest::Harness<'static, PhotocraftApp>, Writes, String, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("raw.{ext}")).to_string_lossy().into_owned();
        std::fs::write(&path, b"x").unwrap();
        let writes: Writes = Rc::default();
        let (w1, q) = (writes.clone(), Rc::new(RefCell::new(None)));
        let q1 = q.clone();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.run("file.new", json!({"width": 4, "height": 4, "name": "raw"})).unwrap();
        app.session.active_mut().unwrap().path = Some(path.clone());
        if layered {
            app.run("layer.new.layer", json!({})).unwrap();
        }
        app.services.export = Some(Box::new(move |_, _, s| {
            *q1.borrow_mut() = s.jpeg_quality;
            Ok((vec![1], Vec::new()))
        }));
        app.services.write = Some(Box::new(move |p, _| {
            w1.borrow_mut().push((p.to_string(), *q.borrow()));
            Ok(())
        }));
        app.services.file_dialog = Some(crate::file_dialog::fake(vec![None]).0);
        let h = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(|ui, app| show(app, ui.ctx()), app);
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        (h, writes, path, dir)
    }

    fn save(h: &mut egui_kittest::Harness<'static, PhotocraftApp>) {
        let ctx = h.ctx.clone();
        crate::menus::invoke(h.state_mut(), &ctx, "file.save", json!({})).unwrap();
        h.run_steps(3);
    }

    #[test]
    fn it_asks_and_writes_over_the_original_at_maximum_quality() {
        let (mut h, writes, path, _d) = harness("jpg", false);
        save(&mut h);
        assert!(h.state().save_original.is_some() && writes.borrow().is_empty());
        h.get_by_label("Process and Save to Original").click();
        h.run_steps(3);
        assert_eq!(*writes.borrow(), [(path, Some(100))]);
        assert!(h.state().session.active().unwrap().revision == h.state().session.active().unwrap().saved_revision, "saved");
    }

    #[test]
    fn a_remembered_answer_no_longer_asks() {
        let (mut h, writes, _, _d) = harness("png", true);
        save(&mut h);
        h.get_by_label("Remember my choice").click();
        h.run_steps(2);
        h.get_by_label("Process and Save to Original").click();
        h.run_steps(3);
        assert_eq!(h.state().session.prefs().file_handling.save_flat_file, SaveFlatFile::Original);
        assert_eq!(writes.borrow().len(), 1);
        save(&mut h);
        assert!(h.state().save_original.is_none(), "with layers too");
        assert_eq!(writes.borrow().len(), 2);
    }

    #[test]
    fn the_files_own_quality_can_be_kept() {
        let (mut h, writes, path, _d) = harness("jpg", false);
        h.state_mut().session.edit_prefs(|p| {
            p.file_handling.save_flat_file = SaveFlatFile::Original;
            p.file_handling.original_quality = OriginalQuality::File;
        });
        save(&mut h);
        assert_eq!(*writes.borrow(), [(path, None)]);
    }

    #[test]
    fn cancel_writes_nothing() {
        let (mut h, writes, _, _d) = harness("jpg", false);
        save(&mut h);
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert!(h.state().save_original.is_none() && writes.borrow().is_empty());
    }

    #[test]
    fn a_psd_is_not_asked_about() {
        let (mut h, writes, _, _d) = harness("psd", true);
        save(&mut h);
        assert!(h.state().save_original.is_none());
        assert_eq!(writes.borrow().len(), 1);
    }

    #[test]
    fn a_file_that_is_gone_goes_to_save_as() {
        let (mut h, writes, path, _d) = harness("jpg", false);
        std::fs::remove_file(&path).unwrap();
        save(&mut h);
        assert!(h.state().save_original.is_none() && writes.borrow().is_empty());
    }
}
