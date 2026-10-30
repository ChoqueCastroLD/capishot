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

/// Stops waiting: the run is dropped and the download pauses (the progress card's Cancel).
pub(crate) fn cancel(app: &mut PhotocraftApp) {
    app.remove_ai.params = None;
    if let (Some(store), Some(m)) = (app.session.ai_models(), lama()) {
        store.pause(Some(m));
    }
}

/// Every frame while a run waits for the model: the run once the model is here, a queued or
/// paused download started again, Esc to stop waiting. The progress card (`ai_progress`) shows
/// the download.
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
    // A queued or paused download (another file was downloading, or the page closed) starts again.
    if matches!(st, Some(Status::Paused { .. } | Status::NotDownloaded)) {
        start_download(app);
    }
    if app.ui.dialogs.is_empty() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
        cancel(app);
    }
}

/// The first-use download's progress: its share and speed (bytes a second) when known, and why
/// it failed.
pub fn download_progress(app: &PhotocraftApp) -> (Option<f32>, Option<f64>, Option<String>) {
    match status(app) {
        Some(Status::Downloading { done, total, bytes_per_sec }) if total > 0 => {
            (Some(done as f32 / total as f32), (bytes_per_sec > 1.0).then_some(bytes_per_sec), None)
        }
        Some(Status::Paused { done, total }) if total > 0 => (Some(done as f32 / total as f32), None, None),
        Some(Status::Failed(why)) => (None, None, Some(why)),
        _ => (None, None, None),
    }
}

/// The failed download starts again (the card's Retry).
pub fn retry_download(app: &mut PhotocraftApp) {
    start_download(app);
}

/// The options bar: the brush size (the brush chip before this) and Sample All Layers, nothing
/// else.
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::checkbox(ui, &mut app.ui.tool_options.sample_all.remove_ai, tl!("Sample All Layers"));
}

#[cfg(test)]
#[path = "remove_ai_ui_tests.rs"]
mod tests;
