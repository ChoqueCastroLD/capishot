//! The Remove with AI tool ("Quitar con IA"): paint over what to remove and Manga LaMa fills it
//! (`edit.removeAi`); with a selection, a tap fills the selection. Nothing to choose: always
//! Manga LaMa at its best, on the GPU or the CPU (on a device with 2 GB of memory or less, the
//! lighter MI-GAN, said once: `remove_ai_cmds::model`). The first use downloads the model: a modal says
//! so, the download starts by itself (the AI Model Manager's queue, so it resumes), and when it is
//! done the stroke that asked for it is applied. The existing Remove tool is a separate tool.

use photocraft_ai::catalog;
use photocraft_ai::models::Status;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use photocraft_doc::DocId;

/// Strokes no larger than this (document px) with a selection are a tap: fill the selection.
const TAP: f64 = 6.0;

/// A run waiting for the model (`edit.removeAi` params), while the download modal shows.
#[derive(Clone, Debug, Default)]
pub struct Pending {
    pub params: Option<Value>,
    /// Strokes ended while the document's run was going: they run in order when it ends.
    queued: std::collections::VecDeque<(DocId, Value)>,
}

/// The model the tool runs on this device: Manga LaMa, or MI-GAN with little memory.
pub(crate) fn model(app: &PhotocraftApp) -> Option<&'static catalog::ModelInfo> {
    photocraft_engine::remove_ai_cmds::model(&app.session)
}

/// The model's state on this device (`None`: no model store, a build without downloads).
fn status(app: &PhotocraftApp) -> Option<Status> {
    Some(app.session.ai_models()?.status(model(app)?))
}

/// The preference that records the lighter model was announced on this device.
const LITE_TOLD: &str = "removeAi.liteModelTold";

/// The first run on a device with little memory says, once, that a lighter model is used.
fn tell_lite_once(app: &mut PhotocraftApp) {
    if !photocraft_engine::remove_ai_cmds::lite(&app.session) || app.session.prefs().dialogs.get(LITE_TOLD).and_then(Value::as_bool) == Some(true) {
        return;
    }
    crate::notices::post(app, "Remove with AI uses a lighter model on this device", vec!["This device has little memory, so Remove with AI uses MI-GAN instead of Manga LaMa.".to_owned()], false, None);
    app.session.prefs.edit(|prefs| {
        prefs.dialogs.insert(LITE_TOLD.into(), Value::Bool(true));
    });
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

/// The options that say where a clean's result goes, shared by the tools that clean.
pub(crate) fn output_options() -> [(String, &'static str); 3] {
    [("current".to_string(), tl!("Current Layer")), ("new".to_string(), tl!("New Layer Each Time")), ("single".to_string(), tl!("One Cleaning Layer"))]
}

/// The dropdown of where a clean's result goes.
pub(crate) fn output_dropdown(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::dropdown(ui, "clean-output", tl!("Result:"), &mut app.ui.tool_options.clean_output, &output_options(), 170.0);
}

/// Runs `params`, or, the first time, downloads the model first (the modal) and runs them then.
pub fn request(app: &mut PhotocraftApp, mut params: Value) {
    params["output"] = json!(app.ui.tool_options.clean_output);
    params["layerName"] = json!(tl!("AI Clean"));
    tell_lite_once(app);
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

fn active_doc(app: &PhotocraftApp) -> Option<DocId> {
    app.session.active().map(|st| st.doc.id)
}

/// The document whose run of the tool is going now, when it is the active one.
fn running_here(app: &PhotocraftApp) -> Option<DocId> {
    let active = active_doc(app)?;
    let running = app.session.jobs().iter().any(|j| j.command == photocraft_engine::remove_ai_cmds::ID && j.document == Some(active));
    running.then_some(active)
}

/// A run of the tool ended: the next stroke waiting on its document runs, on the layer it was
/// drawn on. A failure or a cancel drops them.
pub(crate) fn run_ended(app: &mut PhotocraftApp, doc: Option<DocId>, finished: bool) {
    if !finished {
        app.remove_ai.queued.retain(|(d, _)| Some(*d) != doc);
        return;
    }
    let Some(doc) = doc.filter(|d| active_doc(app) == Some(*d)) else { return };
    if let Some(at) = app.remove_ai.queued.iter().position(|(d, _)| *d == doc)
        && let Some((_, params)) = app.remove_ai.queued.remove(at)
    {
        run(app, params);
    }
}

fn run(app: &mut PhotocraftApp, mut params: Value) {
    if let Some(doc) = running_here(app) {
        if params.get("strokes").is_some()
            && let Some(layer) = app.session.active().and_then(|st| st.active_layer)
        {
            params["layer"] = json!(layer.0);
        }
        app.remove_ai.queued.push_back((doc, params));
        return;
    }
    app.ui.status.clear();
    if let Err(e) = app.run(photocraft_engine::remove_ai_cmds::ID, params) {
        app.ui.status = crate::tools::retouch_ui::remove_refusal(e);
        app.ui.status_error = true;
    }
}

fn start_download(app: &mut PhotocraftApp) {
    let (Some(store), Some(m)) = (app.session.ai_models(), model(app)) else { return };
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
    if let (Some(store), Some(m)) = (app.session.ai_models(), model(app)) {
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

/// The options bar: the brush size (the brush chip before this), Sample All Layers, and Paint
/// mask, then run (with its eraser, Run and Clear mask: `remove_mask_ui`).
pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::widgets::checkbox(ui, &mut app.ui.tool_options.sample_all.remove_ai, tl!("Sample All Layers"));
    crate::widgets::vline(ui, 22.0);
    output_dropdown(app, ui);
    crate::widgets::vline(ui, 22.0);
    crate::remove_mask_ui::options_bar(app, ui, crate::state::Tool::RemoveAi);
}

#[cfg(test)]
#[path = "remove_ai_ui_tests.rs"]
mod tests;
