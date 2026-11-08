//! The question File › Save As asks before writing a PSD that would lose the document's layers:
//! Photoshop has none in Indexed Color, Bitmap and Multichannel, so such a file is written
//! flattened. A raw opened from a PNG of few colours is Indexed, and its layers were lost with only
//! a notice in the corner. The answer converts the document (to RGB, or Grayscale from Bitmap) and
//! saves it with its layers, or saves it flattened as before. Like the flat copy prompt: the save is
//! parked in [`Prompt`] while the modal is up, and only the save dialog asks.

use photocraft_doc::ColorMode;

use crate::{FormatChoices, PhotocraftApp};

/// A save waiting on the answer.
pub struct Prompt {
    /// The document that requested the save, even if tabs change while this is open.
    doc: photocraft_doc::DocId,
    pub path: String,
    choices: FormatChoices,
    /// File › Save back to the document's own file, recorded as one.
    plain_save: bool,
    /// The document's mode, for the message.
    mode: ColorMode,
}

/// Whether saving the active document to `path` (a PSD or PSB) would lose its layers.
pub fn wants_prompt(app: &PhotocraftApp, path: &str) -> bool {
    let psd = photocraft_engine::file_cmds::extension(path).is_some_and(|e| e == "psd" || e == "psb");
    psd && app.session.active().is_some_and(|st| photocraft_io::psd_drops_layers(&st.doc))
}

/// Parks the save behind the prompt.
pub fn park(app: &mut PhotocraftApp, path: String, choices: FormatChoices, plain_save: bool) -> Result<(), String> {
    let doc = app.active_doc_id()?;
    let mode = app.session.active().map_or(ColorMode::Rgb, |st| st.doc.mode);
    app.psd_mode = Some(Prompt { doc, path, choices, plain_save, mode });
    Ok(())
}

/// The mode a document converts to so a PSD keeps its layers: Bitmap only converts to Grayscale.
fn target(mode: ColorMode) -> (&'static str, &'static str) {
    if mode == ColorMode::Bitmap { ("image.mode.grayscale", tl!("Grayscale")) } else { ("image.mode.rgb", tl!("RGB Color")) }
}

fn mode_name(mode: ColorMode) -> &'static str {
    match mode {
        ColorMode::Indexed => tl!("Indexed Color"),
        ColorMode::Bitmap => tl!("Bitmap"),
        _ => tl!("Multichannel"),
    }
}

/// Writes the parked save, and records it as a plain Save when it was one.
fn write(app: &mut PhotocraftApp, p: Prompt) {
    let path = p.path.clone();
    let written = app.with_document(p.doc, |app| app.save_to(p.path, p.choices));
    match written {
        Ok(_) if p.plain_save => app.journal_as_plain_save(&path),
        Ok(_) => {}
        Err(e) => {
            app.ui.status = crate::i18n::fmt(tl!("Save failed: {error}"), &[("error", &e)]);
            app.ui.status_error = true;
        }
    }
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(p) = &app.psd_mode else { return };
    let (mut convert, mut flat, mut cancel) = (false, false, false);
    let (_, to) = target(p.mode);
    let message = crate::i18n::fmt(
        tl!("This document is in {mode} mode, and a PSD in that mode can't keep layers: it would be saved flattened. Convert it to {to} to save it with its layers."),
        &[("mode", mode_name(p.mode)), ("to", to)],
    );
    let convert_label = crate::i18n::fmt(tl!("Convert to {to} and Save"), &[("to", to)]);
    let modal = egui::Modal::new(egui::Id::unique("psd-mode")).show(ctx, |ui| {
        ui.set_max_width(440.0);
        ui.label(egui::RichText::new(tl!("Keep the layers?")).font(crate::theme::semibold(15.0)));
        ui.add_space(4.0);
        crate::widgets::hairline(ui);
        ui.add_space(8.0);
        ui.label(message);
        ui.add_space(14.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            convert = crate::widgets::primary_button(ui, &convert_label, 0.0).clicked();
            flat = crate::widgets::secondary_button(ui, tl!("Save Flattened"), 0.0).clicked();
            cancel = crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked();
        });
    });
    cancel |= modal.should_close();
    // A focused button takes Enter as its click without consuming the key.
    if convert || (!cancel && !flat && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))) {
        let Some(p) = app.psd_mode.take() else { return };
        let (command, _) = target(p.mode);
        let converted = app.with_document(p.doc, |app| app.run(command, serde_json::json!({})));
        match converted {
            Ok(_) => write(app, p),
            Err(e) => {
                app.ui.status = crate::i18n::fmt(tl!("Save failed: {error}"), &[("error", &e)]);
                app.ui.status_error = true;
            }
        }
    } else if flat {
        if let Some(p) = app.psd_mode.take() {
            write(app, p);
        }
    } else if cancel {
        app.psd_mode = None;
        app.ui.status = tl!("Save cancelled").into();
        app.ui.status_error = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileDialogAnswer, Services};
    use egui_kittest::kittest::Queryable;
    use serde_json::json;
    use std::{cell::RefCell, rc::Rc};

    type Writes = Rc<RefCell<Vec<String>>>;

    /// An Indexed document with a second layer; the save dialog answers `to`.
    fn harness(to: &str) -> (egui_kittest::Harness<'static, PhotocraftApp>, Writes) {
        let writes: Writes = Rc::default();
        let written = writes.clone();
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        app.run("file.new", json!({"width": 8, "height": 8, "name": "025"})).unwrap();
        app.run("image.mode.indexedColor", json!({})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.services.file_dialog = Some(crate::file_dialog::fake(vec![Some(FileDialogAnswer::SaveTo(to.into()))]).0);
        app.services.export = Some(Box::new(|_, _, _| Ok((vec![1], Vec::new()))));
        app.services.write = Some(Box::new(move |path, _| {
            written.borrow_mut().push(path.to_string());
            Ok(())
        }));
        let h = egui_kittest::Harness::builder().with_size(egui::vec2(800.0, 600.0)).build_ui_state(
            |ui, app| {
                show(app, ui.ctx());
                crate::flat_copy_ui::show(app, ui.ctx());
                app.poll_file_dialog(ui.ctx(), None);
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        (h, writes)
    }

    fn mode(h: &egui_kittest::Harness<'static, PhotocraftApp>) -> ColorMode {
        h.state().session.active().unwrap().doc.mode
    }

    #[test]
    fn an_indexed_psd_asks_and_converting_keeps_the_layers() {
        let (mut h, writes) = harness("025.psd");
        assert_eq!(mode(&h), ColorMode::Indexed);
        h.state_mut().save_as(None).unwrap();
        h.run_steps(3);
        assert!(h.state().psd_mode.is_some(), "it asks");
        assert!(writes.borrow().is_empty(), "nothing is written before the answer");
        h.get_by_label("Convert to RGB Color and Save").click();
        h.run_steps(3);
        assert!(h.state().psd_mode.is_none());
        assert_eq!(mode(&h), ColorMode::Rgb);
        assert!(h.state().session.active().unwrap().doc.layers.len() >= 2, "with its layers");
        assert_eq!(*writes.borrow(), ["025.psd"]);
    }

    #[test]
    fn saving_flattened_keeps_the_mode() {
        let (mut h, writes) = harness("025.psd");
        h.state_mut().save_as(None).unwrap();
        h.run_steps(3);
        h.get_by_label("Save Flattened").click();
        h.run_steps(3);
        assert_eq!(mode(&h), ColorMode::Indexed);
        assert_eq!(*writes.borrow(), ["025.psd"]);
    }

    #[test]
    fn cancel_writes_nothing() {
        let (mut h, writes) = harness("025.psd");
        h.state_mut().save_as(None).unwrap();
        h.run_steps(3);
        h.get_by_label("Cancel").click();
        h.run_steps(3);
        assert!(h.state().psd_mode.is_none() && writes.borrow().is_empty());
        assert_eq!(mode(&h), ColorMode::Indexed);
    }

    #[test]
    fn an_rgb_psd_does_not_ask() {
        let (mut h, writes) = harness("025.psd");
        h.state_mut().run("image.mode.rgb", json!({})).unwrap();
        h.state_mut().save_as(None).unwrap();
        h.run_steps(3);
        assert!(h.state().psd_mode.is_none());
        assert_eq!(*writes.borrow(), ["025.psd"]);
    }
}
