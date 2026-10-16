//! Using an AI tool never downloads: a tool whose model isn't on this device says so.
//!
//! Clean with AI, the Remove tool's AI mode and Upscale with AI run their models only once they
//! are downloaded, and downloading is the AI Model Manager's job (Window › AI Models…,
//! `ai_models_ui`). Every UI entry point (menu item, command palette, shortcut, the Remove tool's
//! button and brush, the upscale dialog) runs these commands through [`invoke`]: when the model
//! the run would use isn't downloaded, an error dialog opens instead ("Manga LaMa isn't
//! downloaded", why, [Open Model Manager] [Cancel]), and OK opens the Manager on that model. A
//! run whose model turned out missing anyway (the browser's cache was cleared, or still being
//! checked when it started) ends in the same dialog ([`on_job_failed`]).
//!
//! Agents keep the engine's `allowDownload` / `download` params (and the `ai.models.*`
//! commands); `engine.execute` skips all this and gets the engine's own error. The consent
//! preferences of the former download prompt (`prefs.dialogs["aiModelDownload.*"]`) are no longer
//! read; [`forget_old_consents`] drops them.

use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_ai::models::Status;
use photocraft_engine::{ai_cmds, ai_inpaint_cmds, ai_models_cmds, genfill_cmds};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::ai_options_ui::Kind;
use crate::state::DialogKind;

const MARK: &str = "__aiModelMissing";
/// The former consent prompt's `prefs.dialogs` key prefix.
const OLD_CONSENT_PREFIX: &str = "aiModelDownload.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    Missing,
    Checking,
    Unavailable,
}

/// The model a command run needs, the param that carries an agent's download consent, and the
/// model's state.
struct Target {
    model: &'static ModelInfo,
    key: &'static str,
    state: State,
}

fn target(app: &PhotocraftApp, id: &str, params: &Value) -> Option<Target> {
    match id {
        ai_inpaint_cmds::ID => inpaint(app, params),
        "paint.remove" if params.get("ai").and_then(Value::as_bool) == Some(true) => inpaint(app, params),
        ai_cmds::COMMAND => {
            use photocraft_ai::service::ModelState as M;
            let plan = ai_cmds::plan(&app.session, params).ok()?;
            let state = match plan.state {
                M::Ready => State::Ready,
                M::NeedsDownload => State::Missing,
                M::Checking => State::Checking,
                M::Unavailable(_) => State::Unavailable,
            };
            Some(Target { model: plan.model, key: "download", state })
        }
        _ => None,
    }
}

/// The inpainting model these params run (Manga LaMa, or the lite MI-GAN that Quality / Device
/// choose on this machine), so the dialog names the model the run really needs.
fn inpaint(app: &PhotocraftApp, params: &Value) -> Option<Target> {
    use photocraft_ai_lama::service::ModelState as M;
    let (model, state) = ai_inpaint_cmds::model_state(&app.session, params)?;
    let state = match state {
        M::Ready => State::Ready,
        M::NeedsDownload { .. } => State::Missing,
        M::Checking => State::Checking,
        M::Unavailable(_) => State::Unavailable,
    };
    Some(Target { model, key: "allowDownload", state })
}

/// Generative Fill: its tier's files are several; the dialog names the first missing one (the
/// Manager's Generative Fill group downloads them all).
fn genfill(app: &mut PhotocraftApp, params: &Value) -> Option<Result<Value, String>> {
    use photocraft_ai_sd::service::ModelState as M;
    if params.get("allowDownload").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    let first_missing = |app: &PhotocraftApp, tier| -> Option<&'static ModelInfo> {
        let store = app.session.ai_models()?;
        photocraft_ai_sd::files::tier(tier)?.files.iter().copied().find(|m| !store.status(m).is_downloaded())
    };
    let (tier, state) = genfill_cmds::model_state(&app.session, params)?;
    if let M::NeedsDownload { .. } = state
        && let Some(m) = first_missing(app, tier)
    {
        return Some(Ok(open(app, genfill_cmds::ID, m)));
    }
    app.ai_model_last = Some(genfill_cmds::ID.to_string());
    Some(match app.run(genfill_cmds::ID, params.clone()) {
        Err(e) if is_missing_error(&e) => match first_missing(app, tier) {
            Some(m) => {
                app.ui.status.clear();
                app.ui.status_error = false;
                Ok(open(app, genfill_cmds::ID, m))
            }
            None => Err(e),
        },
        r => r,
    })
}

/// The engine's (or the AI worker's) "model not downloaded" error.
fn is_missing_error(e: &str) -> bool {
    e.contains(ai_models_cmds::NOT_DOWNLOADED)
}

/// Runs `id` like [`PhotocraftApp::run`], with the missing-model dialog in front.
pub fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Result<Value, String> {
    match invoke(app, id, &params) {
        Some(r) => r,
        None => {
            let p = crate::ai_options_ui::with_saved(app, id, &params);
            app.run(id, p)
        }
    }
}

/// The gate for menu, palette, shortcut and tool runs; `None` for other commands (or a run that
/// carries an agent's download consent).
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == genfill_cmds::ID {
        return genfill(app, params);
    }
    Kind::of(id, params)?;
    forget_old_consents(app);
    // Quality, Device and the Advanced settings the user chose (AI Options…, the upscale dialog).
    let with_settings = crate::ai_options_ui::with_saved(app, id, params);
    let params = &with_settings;
    let t = target(app, id, params)?;
    if params.get(t.key).and_then(Value::as_bool) == Some(true) {
        return None;
    }
    Some(match t.state {
        State::Missing => Ok(open(app, id, t.model)),
        // Checking: the engine runs it, and a missing model fails the run (`on_job_failed`).
        State::Ready | State::Checking | State::Unavailable => {
            app.ai_model_last = Some(id.to_string());
            match app.run(id, params.clone()) {
                Err(e) if is_missing_error(&e) => {
                    // The dialog replaces the engine's error in the status bar.
                    app.ui.status.clear();
                    app.ui.status_error = false;
                    Ok(open(app, id, t.model))
                }
                r => r,
            }
        }
    })
}

/// A background run that found its model missing after all: the dialog instead of the error.
/// True when handled.
pub fn on_job_failed(app: &mut PhotocraftApp, command: &str, error: &str) -> bool {
    if !is_missing_error(error) {
        return false;
    }
    let Some(id) = app.ai_model_last.take().filter(|id| id == command) else { return false };
    // The model named in the error, else the one the command's default params pick.
    let model = catalog::MODELS
        .iter()
        .filter(|m| error.contains(m.title))
        .max_by_key(|m| m.title.len())
        .or_else(|| target(app, &id, &crate::ai_options_ui::with_saved(app, &id, &json!({}))).map(|t| t.model));
    let Some(model) = model else { return false };
    open(app, &id, model);
    true
}

/// Drops the consent preferences of the former download prompt (once, if any are left).
pub fn forget_old_consents(app: &mut PhotocraftApp) {
    if !app.session.prefs().dialogs.keys().any(|k| k.starts_with(OLD_CONSENT_PREFIX)) {
        return;
    }
    app.session.edit_prefs(|p| p.dialogs.retain(|k, _| !k.starts_with(OLD_CONSENT_PREFIX)));
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        log::warn!("couldn't save the preferences: {e}");
    }
}

/// The tool's name in the UI language ("Clean with AI", "Remove Tool", "Upscale with AI").
pub fn tool_name(id: &str) -> String {
    match id {
        "paint.remove" => tl!("Remove Tool").to_string(),
        ai_cmds::COMMAND => tl!("Upscale with AI…").trim_end_matches('…').trim_end_matches("...").to_string(),
        genfill_cmds::ID => tl!("Generative Fill").to_string(),
        _ => tl!("Clean with AI").to_string(),
    }
}

/// Opens the dialog (once: a second request while it is open just names it).
fn open(app: &mut PhotocraftApp, id: &str, model: &'static ModelInfo) -> Value {
    if let Some(d) = app.ui.dialogs.iter().find(|d| owns(&d.fields)) {
        return json!({"dialog": d.id, "modelMissing": d.fields.get("model")});
    }
    let title = crate::i18n::fmt(tl!("{model} isn't downloaded"), &[("model", model.title)]);
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(id));
    f.insert("__label".into(), json!(title));
    f.insert("model".into(), json!(model.id));
    let dialog = app.ui.open_dialog(DialogKind::Command, f);
    json!({"dialog": dialog, "modelMissing": model.id})
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

fn model_of(f: &Map<String, Value>) -> Option<&'static ModelInfo> {
    f.get("model").and_then(Value::as_str).and_then(catalog::model)
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    owns(f).then(|| tl!("Open Model Manager"))
}

pub fn body(app: &PhotocraftApp, ui: &mut egui::Ui, f: &Map<String, Value>) {
    let Some(m) = model_of(f) else { return };
    let dim = crate::theme::Tokens::get(ui.ctx()).text_dim;
    let tool = tool_name(f.get("__command").and_then(Value::as_str).unwrap_or_default());
    let mb = crate::ai_models_ui::mb(m.bytes);
    ui.set_max_width(420.0);
    ui.label(crate::i18n::fmt(tl!("{tool} needs {model} ({mb} MB), which isn't on this device yet."), &[("tool", &tool), ("model", m.title), ("mb", &mb)]));
    ui.add_space(6.0);
    let status = app.session.ai_models().map(|s| s.status(m));
    if let Some(Status::Downloading { done, total, .. }) = status {
        let pct = format!("{:.0}", done as f64 * 100.0 / total.max(1) as f64);
        ui.label(crate::i18n::fmt(tl!("It is downloading now ({pct}%); try again when it is done."), &[("pct", &pct)]));
    } else if status == Some(Status::Queued) {
        ui.label(tl!("It is waiting in the download queue; try again when it is done."));
    } else {
        ui.label(tl!("Download it once in the AI Model Manager. Using a tool never downloads anything."));
    }
    ui.add_space(6.0);
    let stored = if cfg!(target_arch = "wasm32") {
        tl!("Models are kept in this browser and run on this device: your images are not uploaded.")
    } else {
        tl!("Models are kept on this computer and run on this device: your images are not uploaded.")
    };
    ui.label(egui::RichText::new(stored).color(dim));
}

/// OK: open the AI Model Manager on that model.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let model = model_of(f);
    crate::ai_models_ui::open(app, model);
    Ok(json!({"aiModels": true, "model": model.map(|m| m.id)}))
}

#[cfg(test)]
#[path = "ai_model_gate_tests.rs"]
mod tests;
