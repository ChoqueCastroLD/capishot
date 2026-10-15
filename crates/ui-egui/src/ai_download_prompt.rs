//! Consent before an AI model download, like Photoshop's "download the model?" prompt.
//!
//! `edit.inpaintAi` and the Remove tool's AI mode (Manga LaMa) and `image.upscaleAi`
//! (MangaJaNai / IllustrationJaNai) need their weights once; the engine refuses to fetch them
//! without the consent param (`allowDownload` / `download`). Every UI entry point (menu item,
//! command palette, shortcut, the Remove tool's button and brush) runs these commands through
//! [`invoke`]: a model that isn't on this device yet opens "Download Manga LaMa?" with its size,
//! licence and author; confirming runs the same command again with the consent param, and the
//! job's progress (download, then inference) shows in the status bar. The answer is remembered per
//! model (`prefs.dialogs["aiModelDownload.<model id>"]`), so later runs don't ask. While the
//! browser's cache is still being checked the run waits ([`tick`]) instead of failing.
//! `engine.execute` (agents) skips all this and gets the engine's own error.

use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_engine::{ai_cmds, ai_inpaint_cmds};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::state::DialogKind;

const MARK: &str = "__aiDownload";
/// `prefs.dialogs` key prefix: `aiModelDownload.<model id>` = true once the user agreed.
pub const PREF_PREFIX: &str = "aiModelDownload.";
/// How long a run waits for the browser's cache to answer before asking anyway.
const CHECK_WAIT_MS: f64 = 15_000.0;

/// A run waiting for the model's cache check.
#[derive(Clone, Debug)]
pub struct Waiting {
    pub command: String,
    pub params: Value,
    pub since_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Ready,
    NeedsDownload,
    Checking,
    Unavailable,
}

/// The model a command run needs, the param that carries consent, and the model's state.
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
                M::NeedsDownload => State::NeedsDownload,
                M::Checking => State::Checking,
                M::Unavailable(_) => State::Unavailable,
            };
            Some(Target { model: plan.model, key: "download", state })
        }
        _ => None,
    }
}

/// The inpainting model these params run (Manga LaMa, or the lite MI-GAN that Quality / Device
/// choose on this machine), so the prompt names the model that will really download.
fn inpaint(app: &PhotocraftApp, params: &Value) -> Option<Target> {
    use photocraft_ai_lama::service::ModelState as M;
    let (model, state) = ai_inpaint_cmds::model_state(&app.session, params)?;
    let state = match state {
        M::Ready => State::Ready,
        M::NeedsDownload { .. } => State::NeedsDownload,
        M::Checking => State::Checking,
        M::Unavailable(_) => State::Unavailable,
    };
    Some(Target { model, key: "allowDownload", state })
}

fn pref_key(model: &ModelInfo) -> String {
    format!("{PREF_PREFIX}{}", model.id)
}

/// The user already agreed to download `model`.
pub fn consented(app: &PhotocraftApp, model: &ModelInfo) -> bool {
    app.session.prefs().dialogs.get(&pref_key(model)).and_then(Value::as_bool) == Some(true)
}

/// Remembers the user's consent to download `model` (and saves the preferences).
pub fn remember(app: &mut PhotocraftApp, model: &ModelInfo) {
    if consented(app, model) {
        return;
    }
    let key = pref_key(model);
    app.session.edit_prefs(|p| {
        p.dialogs.insert(key, json!(true));
    });
    if let Err(e) = crate::prefs_ui::save_preferences(app) {
        log::warn!("couldn't save the AI model download consent: {e}");
    }
}

fn with_consent(params: &Value, key: &str) -> Value {
    let mut p = if params.is_object() { params.clone() } else { json!({}) };
    if let Some(o) = p.as_object_mut() {
        o.insert(key.into(), json!(true));
    }
    p
}

/// The engine's "needs a download" refusal (its text names the consent param).
fn is_download_refusal(e: &str) -> bool {
    e.contains("needs a one-time download") || e.contains("isn't downloaded yet")
}

/// Runs `id` like [`PhotocraftApp::run`], asking first when it needs a model download.
pub fn run(app: &mut PhotocraftApp, id: &str, params: Value) -> Result<Value, String> {
    match invoke(app, id, &params) {
        Some(r) => r,
        None => {
            let p = crate::ai_options_ui::with_saved(app, id, &params);
            app.run(id, p)
        }
    }
}

/// The consent gate for menu, palette and shortcut runs; `None` for other commands (or a run
/// that already carries consent).
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    // Quality, Device and the Advanced settings the user chose (AI Options…, the upscale dialog).
    let with_settings = crate::ai_options_ui::with_saved(app, id, params);
    let params = &with_settings;
    let t = target(app, id, params)?;
    if params.get(t.key).and_then(Value::as_bool) == Some(true) {
        return None;
    }
    Some(match t.state {
        State::Ready | State::Unavailable => {
            // A stale "cached" answer shows up later, as the job's failure (`on_job_failed`).
            app.ai_download_last = Some((id.to_string(), params.clone()));
            match app.run(id, params.clone()) {
                Err(e) if is_download_refusal(&e) => {
                    // The prompt replaces the engine's refusal in the status bar.
                    app.ui.status.clear();
                    app.ui.status_error = false;
                    Ok(open(app, id, params, t.model))
                }
                r => r,
            }
        }
        _ if consented(app, t.model) => app.run(id, with_consent(params, t.key)),
        State::NeedsDownload => Ok(open(app, id, params, t.model)),
        State::Checking => {
            app.ai_download_wait = Some(Waiting { command: id.to_string(), params: params.clone(), since_ms: crate::gpu_canvas::now_ms() });
            Ok(json!({"pending": "modelCheck"}))
        }
    })
}

/// A background run that found the model missing after all (the browser's cache was cleared
/// since it was checked): ask, or with consent run it again. True when handled.
pub fn on_job_failed(app: &mut PhotocraftApp, command: &str, error: &str) -> bool {
    if !is_download_refusal(error) {
        return false;
    }
    let Some((id, params)) = app.ai_download_last.take().filter(|(id, _)| id == command) else { return false };
    let Some(t) = target(app, &id, &params) else { return false };
    let r = if consented(app, t.model) { app.run(&id, with_consent(&params, t.key)) } else { Ok(open(app, &id, &params, t.model)) };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    true
}

/// Per frame: a run waiting for the browser's cache check goes on (or asks) once it answers.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    let Some(w) = app.ai_download_wait.take() else { return };
    let r = match target(app, &w.command, &w.params) {
        Some(t) if t.state == State::Checking => {
            if crate::gpu_canvas::now_ms() - w.since_ms < CHECK_WAIT_MS {
                app.ai_download_wait = Some(w);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return;
            }
            // Still no answer: ask (consent covers a model that turns out to be cached).
            if consented(app, t.model) { app.run(&w.command, with_consent(&w.params, t.key)) } else { Ok(open(app, &w.command, &w.params, t.model)) }
        }
        _ => run(app, &w.command, w.params),
    };
    if let Err(e) = r {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// Opens the prompt (once: a second request while it is open just waits for it).
fn open(app: &mut PhotocraftApp, id: &str, params: &Value, model: &'static ModelInfo) -> Value {
    if let Some(d) = app.ui.dialogs.iter().find(|d| owns(&d.fields)) {
        return json!({"dialog": d.id, "downloadConsentPending": true});
    }
    let title = crate::i18n::fmt(tl!("Download {model}?"), &[("model", model.title)]);
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(id));
    f.insert("__label".into(), json!(title));
    f.insert("model".into(), json!(model.id));
    f.insert("params".into(), params.clone());
    let dialog = app.ui.open_dialog(DialogKind::Command, f);
    json!({"dialog": dialog, "downloadConsentPending": true})
}

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

fn model_of(f: &Map<String, Value>) -> Option<&'static ModelInfo> {
    f.get("model").and_then(Value::as_str).and_then(catalog::model)
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    if !owns(f) {
        return None;
    }
    Some(if f.get("__command").and_then(Value::as_str) == Some(ai_cmds::COMMAND) { tl!("Download and Upscale") } else { tl!("Download and Clean") })
}

pub fn body(ui: &mut egui::Ui, f: &Map<String, Value>) {
    let Some(m) = model_of(f) else { return };
    let dim = crate::theme::Tokens::get(ui.ctx()).text_dim;
    let mb = format!("{:.0}", m.bytes as f64 / 1e6);
    ui.label(crate::i18n::fmt(tl!("{model} needs a one-time download of {mb} MB before it can run."), &[("model", m.title), ("mb", &mb)]));
    ui.add_space(6.0);
    ui.label(crate::i18n::fmt(tl!("Licence {license}, by {author}."), &[("license", m.license), ("author", m.author)]));
    ui.add_space(6.0);
    let stored = if cfg!(target_arch = "wasm32") {
        tl!("It is stored in this browser, only once. It runs on this device: your images are not uploaded.")
    } else {
        tl!("It is stored on this computer, only once. It runs on this device: your images are not uploaded.")
    };
    ui.label(egui::RichText::new(stored).color(dim));
}

/// OK: remember the consent and run the command again with it.
pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    let cmd = f.get("__command").and_then(Value::as_str).ok_or("dialog has no command")?.to_string();
    let params = f.get("params").cloned().unwrap_or_else(|| json!({}));
    let key = if cmd == ai_cmds::COMMAND { "download" } else { "allowDownload" };
    if let Some(m) = model_of(f) {
        remember(app, m);
    }
    app.run(&cmd, with_consent(&params, key))
}

#[cfg(test)]
#[path = "ai_download_prompt_tests.rs"]
mod tests;
