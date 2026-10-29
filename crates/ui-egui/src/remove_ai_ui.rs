//! The Remove with AI tool ("Quitar con IA"): paint over what to remove and Manga LaMa fills it
//! (`edit.removeAi`); with a selection, a tap fills the selection. Nothing to choose: always
//! Manga LaMa at its best, on the GPU or the CPU. The first use downloads the model: a modal says
//! so, the download starts by itself (the AI Model Manager's queue, so it resumes), and when it is
//! done the stroke that asked for it is applied. The existing Remove tool is a separate tool.

use photocraft_ai::catalog;
use photocraft_ai::models::Status;
use serde_json::{Value, json};

use crate::PhotocraftApp;

/// Strokes no larger than this (document px) with a selection are a tap: fill the selection.
const TAP: f64 = 6.0;

/// A run waiting for the model (`edit.removeAi` params), while the download modal shows.
#[derive(Clone, Debug, Default)]
pub struct Pending {
    pub params: Option<Value>,
}

fn lama() -> Option<&'static catalog::ModelInfo> {
    catalog::model(catalog::LAMA_ID)
}

/// The model's state on this device (`None`: no model store, a build without downloads).
fn status(app: &PhotocraftApp) -> Option<Status> {
    Some(app.session.ai_models()?.status(lama()?))
}

/// A finished stroke of the tool (document px; pressure in [2]).
pub fn finish_stroke(app: &mut PhotocraftApp, points: &[[f64; 3]]) {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
    }
    let tap = points.is_empty() || (x1 - x0).max(y1 - y0) <= TAP;
    let has_selection = app.session.active().is_some_and(|st| st.doc.selection.as_ref().is_some_and(|s| !s.content_bounds().is_empty()));
    let mut p = json!({"sampleAllLayers": app.ui.tool_options.sample_all.remove_ai});
    if !(tap && has_selection) {
        p["strokes"] = json!([points]);
        p["size"] = json!(app.session.tools.brush.size);
        p["target"] = crate::canvas::paint_target(app);
    }
    request(app, p);
}

/// Runs `params`, or, the first time, downloads the model first (the modal) and runs them then.
pub fn request(app: &mut PhotocraftApp, params: Value) {
    match status(app) {
        Some(Status::Downloaded) | None => run(app, params),
        Some(Status::Unavailable(why)) => {
            app.ui.status = why;
            app.ui.status_error = true;
        }
        Some(_) => {
            app.remove_ai.params = Some(params);
            start_download(app);
        }
    }
}

fn run(app: &mut PhotocraftApp, params: Value) {
    app.ui.status.clear();
    if let Err(e) = app.run(photocraft_engine::remove_ai_cmds::ID, params) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

fn start_download(app: &mut PhotocraftApp) {
    let (Some(store), Some(m)) = (app.session.ai_models(), lama()) else { return };
    if matches!(store.status(m), Status::NotDownloaded | Status::Paused { .. } | Status::Failed(_) | Status::UpdateAvailable)
        && let Err(e) = store.download(m)
    {
        app.ui.status = e.to_string();
        app.ui.status_error = true;
    }
}

/// The download modal is open.
pub fn is_open(app: &PhotocraftApp) -> bool {
    app.remove_ai.params.is_some()
}

fn cancel(app: &mut PhotocraftApp) {
    app.remove_ai.params = None;
    if let (Some(store), Some(m)) = (app.session.ai_models(), lama()) {
        store.pause(Some(m));
    }
}

/// Every frame: the first-use download modal, and the waiting run once the model is here.
pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !is_open(app) {
        return;
    }
    let st = status(app);
    if matches!(st, Some(Status::Downloaded) | None) {
        if let Some(mut p) = app.remove_ai.params.take() {
            // The inpainter may not have seen the new file yet: it reads it from the store.
            p["allowDownload"] = json!(true);
            run(app, p);
        }
        return;
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(200));
    let t = crate::theme::Tokens::get(ctx);
    let screen = crate::safe_area::rect(ctx);
    let width = (screen.width() - 32.0).min(420.0);
    let bytes = lama().map_or(0, |m| m.bytes);
    let mb = format!("{} MB", (bytes as f64 / 1e6).round());
    let mut cancelled = false;
    let mut retry = false;
    let modal = egui::Modal::new(egui::Id::new("remove-ai-download")).show(ctx, |ui| {
        ui.set_width(width);
        ui.label(egui::RichText::new(tl!("Remove with AI")).font(crate::theme::semibold(15.0)));
        ui.add_space(6.0);
        ui.label(crate::i18n::fmt(tl!("Remove with AI needs to download the Manga LaMa model ({size}). Only once."), &[("size", &mb)]));
        ui.add_space(8.0);
        let (fraction, line) = match &st {
            Some(Status::Downloading { done, total, .. }) if *total > 0 => {
                let f = *done as f32 / *total as f32;
                (f, crate::i18n::fmt(tl!("Downloading… {pct}%"), &[("pct", &format!("{:.0}", f * 100.0))]))
            }
            Some(Status::Paused { done, total }) if *total > 0 => (*done as f32 / *total as f32, tl!("Starting the download…").to_string()),
            Some(Status::Failed(why)) => (0.0, crate::i18n::fmt(tl!("The download failed: {reason}"), &[("reason", why)])),
            _ => (0.0, tl!("Starting the download…").to_string()),
        };
        let (r, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 8.0), egui::Sense::hover());
        crate::jobs_ui::bar(ui, r, Some(fraction), &t);
        ui.add_space(4.0);
        ui.label(egui::RichText::new(line).color(if matches!(st, Some(Status::Failed(_))) { t.danger } else { t.text_dim }).size(12.5));
        ui.add_space(10.0);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if crate::widgets::secondary_button(ui, tl!("Cancel"), 84.0).clicked() {
                cancelled = true;
            }
            if matches!(st, Some(Status::Failed(_))) && crate::widgets::primary_button(ui, tl!("Retry"), 84.0).clicked() {
                retry = true;
            }
        });
    });
    // A queued or paused download (another file was downloading, or the page closed) starts again.
    if matches!(st, Some(Status::Paused { .. } | Status::NotDownloaded)) || retry {
        start_download(app);
    }
    let esc = ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if cancelled || esc || modal.should_close() {
        cancel(app);
    }
}

/// The options bar: the brush size (the brush chip before this) and Sample All Layers, nothing
/// else.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::checkbox(ui, &mut app.ui.tool_options.sample_all.remove_ai, tl!("Sample All Layers"));
}

#[cfg(test)]
#[path = "remove_ai_ui_tests.rs"]
mod tests;
