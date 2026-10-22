//! Image › Upscale with AI… (`image.upscaleAi`): the dialog in front of the engine command.
//!
//! It shows what will happen before anything runs (from [`ai_cmds::plan`]): the detected page
//! type and the MangaJaNai / IllustrationJaNai model that goes with it, the new size, where the
//! model runs, and whether that model is downloaded. Downloading is the AI Model Manager's job
//! (Window › AI Models…): with the model missing the dialog says so, offers the Manager, and OK
//! opens the missing-model dialog instead of running (`ai_model_gate`). The upscale runs as a
//! background job (progress in the status bar, Esc cancels) and lands as one undo step or a new
//! document.
//!
//! Detecting the page type flattens the document, so it runs once per document revision, not
//! every frame (the dialog keeps the answer in its fields).

use photocraft_ai::catalog::Content;
use photocraft_ai::service::ModelState;
use photocraft_engine::ai_cmds::{self, COMMAND, Output};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::ai_options_ui::Kind;
use crate::state::DialogKind;

const MARK: &str = "__aiUpscale";
const NEEDS_DOWNLOAD: &str = "__needsDownload";
/// The page type detected for `DETECT_KEY` (document id and revision): "grayscale" or "color".
const DETECTED: &str = "__detected";
const DETECT_KEY: &str = "__detectKey";

pub fn owns(fields: &Map<String, Value>) -> bool {
    fields.contains_key(MARK)
}

/// The menu item without params opens the dialog; with params the command runs as is.
pub fn invoke(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != COMMAND || !params.as_object().is_none_or(|o| o.is_empty()) {
        return None;
    }
    let label = photocraft_engine::commands::find(COMMAND).map_or("Upscale with AI…", |c| c.label);
    let mut f = Map::new();
    f.insert(MARK.into(), json!(true));
    f.insert("__command".into(), json!(COMMAND));
    f.insert("__label".into(), json!(label));
    for (k, v) in [("scale", json!("2")), ("content", json!("auto")), ("autoLevels", json!(true)), ("output", json!("auto"))] {
        f.insert(k.into(), v);
    }
    // Quality, Device and Advanced (model, tiles, threads…) as last chosen.
    crate::ai_options_ui::fill_fields(app, Kind::Upscale, &mut f);
    Some(Ok(json!({"dialog": app.ui.open_dialog(DialogKind::Command, f)})))
}

/// The command params the dialog's fields stand for.
pub fn params_of(f: &Map<String, Value>) -> Value {
    let s = |k: &str, d: &str| f.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
    let scale: u64 = if s("scale", "2") == "4" { 4 } else { 2 };
    let mut p = json!({
        "scale": scale,
        "content": s("content", "auto"),
        "model": s("model", "auto"),
        "autoLevels": f.get("autoLevels").and_then(Value::as_bool).unwrap_or(true),
        "output": s("output", "auto"),
    });
    // A manual model brings its own scale.
    if p["model"] != "auto" {
        p.as_object_mut().map(|o| o.remove("scale"));
    }
    if let Some(o) = p.as_object_mut() {
        for (k, v) in crate::ai_options_ui::params_of(Kind::Upscale, f) {
            o.entry(k).or_insert(v);
        }
    }
    p
}

pub fn ok_label(f: &Map<String, Value>) -> Option<&'static str> {
    owns(f).then(|| tl!("Upscale"))
}

/// The plan for the dialog's fields, detecting the page type at most once per document revision.
fn cached_plan(app: &PhotocraftApp, f: &mut Map<String, Value>) -> photocraft_engine::Result<ai_cmds::Plan> {
    let mut p = params_of(f);
    let auto = p.get("content").and_then(Value::as_str) == Some("auto") && p.get("model").and_then(Value::as_str) == Some("auto");
    let key = app.session.active().map(|d| format!("{}:{}", d.doc.id.0, d.revision));
    if !auto || key.is_none() {
        return ai_cmds::plan(&app.session, &p);
    }
    if f.get(DETECT_KEY).and_then(Value::as_str) != key.as_deref() {
        let plan = ai_cmds::plan(&app.session, &p)?;
        f.insert(DETECT_KEY.into(), json!(key));
        f.insert(DETECTED.into(), json!(if plan.grayscale { "grayscale" } else { "color" }));
        return Ok(plan);
    }
    p["content"] = f.get(DETECTED).cloned().unwrap_or_else(|| json!("auto"));
    let mut plan = ai_cmds::plan(&app.session, &p)?;
    plan.detected = true;
    Ok(plan)
}

/// Draws the dialog; true when it should close (the user opened the AI Model Manager).
pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) -> bool {
    let mut dismiss = false;
    let tokens = crate::theme::Tokens::for_kind(app.ui.theme);
    let mut scale = f.get("scale").and_then(Value::as_str).unwrap_or("2").to_string();
    let mut content = f.get("content").and_then(Value::as_str).unwrap_or("auto").to_string();
    let model = f.get("model").and_then(Value::as_str).unwrap_or("auto").to_string();
    let mut levels = f.get("autoLevels").and_then(Value::as_bool).unwrap_or(true);
    let mut output = f.get("output").and_then(Value::as_str).unwrap_or("auto").to_string();
    egui::Grid::new("ai-upscale-form").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.label(tl!("Scale:"));
        ui.add_enabled_ui(model == "auto", |ui| {
            crate::widgets::dropdown(ui, "ai-upscale-scale", &mut scale, &[("2".to_string(), "2×"), ("4".to_string(), "4×")], 200.0);
        });
        ui.end_row();
        ui.label(tl!("Image type:"));
        ui.add_enabled_ui(model == "auto", |ui| {
            let opts = [("auto".to_string(), tl!("Detect")), ("grayscale".to_string(), tl!("Black-and-white manga")), ("color".to_string(), tl!("Colour"))];
            crate::widgets::dropdown(ui, "ai-upscale-content", &mut content, &opts, 200.0);
        });
        ui.end_row();
        crate::ai_options_ui::simple_rows(ui, f);
        ui.label("");
        ui.checkbox(&mut levels, tl!("Auto levels (black-and-white pages)"));
        ui.end_row();
        ui.label(tl!("Result:"));
        let opts = [
            ("auto".to_string(), tl!("Automatic")),
            ("replace".to_string(), tl!("Replace this image (flattens)")),
            ("newDocument".to_string(), tl!("New document")),
        ];
        crate::widgets::dropdown(ui, "ai-upscale-output", &mut output, &opts, 260.0);
        ui.end_row();
    });
    f.insert("scale".into(), json!(scale));
    f.insert("content".into(), json!(content));
    f.insert("autoLevels".into(), json!(levels));
    f.insert("output".into(), json!(output));

    ui.add_space(10.0);
    let plan = cached_plan(app, f);
    let mut needs_download = false;
    match plan {
        Ok(p) => {
            let kind = match (p.grayscale, p.detected) {
                (true, true) => tl!("Detected a black-and-white page"),
                (false, true) => tl!("Detected a colour image"),
                (true, false) => tl!("Black-and-white page"),
                (false, false) => tl!("Colour image"),
            };
            ui.label(format!("{kind}: {}", p.model.title));
            ui.label(crate::ai_options_ui::explanation(p.model, &p.run));
            ui.label(crate::i18n::fmt(tl!("New size: {w} × {h} px"), &[("w", &p.new_width().to_string()), ("h", &p.new_height().to_string())]));
            if p.output == Output::Replace && output == "auto" {
                ui.label(egui::RichText::new(tl!("The image becomes one layer at the new size (Undo restores it).")).color(tokens.text_dim));
            }
            let device = app.session.ai_upscaler().map(|u| u.device()).unwrap_or_default();
            let runs = if device.is_empty() {
                // Where it runs is in the line above; say only that nothing leaves the device.
                tl!("Nothing is uploaded: it runs on this device.").to_string()
            } else {
                crate::i18n::fmt(tl!("Runs on this device ({device}); nothing is uploaded."), &[("device", &device)])
            };
            ui.label(egui::RichText::new(runs).color(tokens.text_dim));
            match &p.state {
                ModelState::Ready => {}
                ModelState::NeedsDownload => {
                    needs_download = true;
                    ui.add_space(6.0);
                    let mb = crate::ai_models_ui::mb(p.model.bytes);
                    ui.colored_label(
                        tokens.warning,
                        crate::i18n::fmt(
                            tl!("{model} ({mb} MB) isn't downloaded. Download it in the AI Model Manager first."),
                            &[("model", p.model.title), ("mb", &mb)],
                        ),
                    );
                    if crate::widgets::secondary_button(ui, tl!("AI Models…"), 0.0).clicked() {
                        crate::ai_options_ui::save(app, Kind::Upscale, f);
                        crate::ai_models_ui::open(app, Some(p.model));
                        dismiss = true;
                    }
                }
                // The browser's cache answers asynchronously.
                ModelState::Checking => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
                ModelState::Unavailable(why) => {
                    ui.colored_label(tokens.warning, why.as_str());
                }
            }
            if p.model.content == Content::Grayscale && !p.grayscale {
                ui.colored_label(tokens.warning, tl!("MangaJaNai is made for black-and-white pages; colour is lost."));
            }
        }
        Err(e) => {
            ui.colored_label(tokens.warning, e.to_string());
        }
    }
    f.insert(NEEDS_DOWNLOAD.into(), json!(needs_download));
    dismiss |= crate::ai_options_ui::advanced(app, ui, Kind::Upscale, f);
    dismiss
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    crate::ai_options_ui::save(app, Kind::Upscale, f);
    // A model that isn't downloaded opens the missing-model dialog instead of running.
    crate::ai_model_gate::run(app, COMMAND, params_of(f))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use photocraft_ai::service::FakeUpscaler;

    use super::*;

    fn app() -> (PhotocraftApp, Arc<FakeUpscaler>) {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 8, "height": 6})).unwrap();
        let fake = Arc::new(FakeUpscaler::default());
        fake.download_needed.store(true, Ordering::Relaxed);
        s.set_ai_upscaler(Some(fake.clone()));
        (PhotocraftApp::new(s, crate::Services::default()), fake)
    }

    #[test]
    fn the_menu_item_opens_the_dialog_and_ok_needs_the_model_downloaded() {
        let (mut app, fake) = app();
        let r = invoke(&mut app, COMMAND, &json!({})).unwrap().unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let d = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().clone();
        assert!(owns(&d.fields));
        let mut f = d.fields.clone();
        f.insert("scale".into(), json!("4"));
        assert_eq!(ok_label(&f), Some("Upscale"));
        assert_eq!(params_of(&f)["scale"], 4);
        // Not downloaded: OK opens the missing-model dialog, nothing runs or downloads.
        let out = confirm(&mut app, &f).unwrap();
        assert!(out["modelMissing"].is_string(), "{out}");
        assert!(app.ui.dialogs.iter().any(|d| crate::ai_model_gate::owns(&d.fields)));
        assert_eq!(app.session.active().unwrap().doc.size.width, 8);
        assert!(fake.download_needed.load(Ordering::Relaxed), "nothing was downloaded");
        // Downloaded: it upscales.
        fake.download_needed.store(false, Ordering::Relaxed);
        let out = confirm(&mut app, &f).unwrap();
        assert_eq!(out["width"], 32);
        // With params the command runs without a dialog.
        assert!(invoke(&mut app, COMMAND, &json!({"scale": 2})).is_none());
    }

    #[test]
    fn a_manual_model_sets_the_scale() {
        let mut f = Map::new();
        f.insert(MARK.into(), json!(true));
        f.insert("scale".into(), json!("2"));
        f.insert("model".into(), json!("mangajanai-v1-4x-1500p"));
        let p = params_of(&f);
        assert!(p.get("scale").is_none());
        assert_eq!(ok_label(&f), Some("Upscale"));
    }

    #[test]
    fn the_dialog_renders_and_detects_the_page_once_per_revision() {
        let (mut app, _) = app();
        let r = invoke(&mut app, COMMAND, &json!({})).unwrap().unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let mut f = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().fields.clone();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        ctx.run_ui(Default::default(), |ui| {
            body(&mut app, ui, &mut f);
        })
        .textures_delta
        .clear();
        assert_eq!(f.get(NEEDS_DOWNLOAD), Some(&json!(true)));
        let key = f.get(DETECT_KEY).cloned().expect("detected once");
        let detected = f.get(DETECTED).cloned().expect("the page type");
        // The next frames reuse it; an edit detects again.
        let plan = cached_plan(&app, &mut f).unwrap();
        assert!(plan.detected);
        assert_eq!(json!(if plan.grayscale { "grayscale" } else { "color" }), detected);
        app.session.execute("edit.fill", json!({"color": [1.0, 0.0, 0.0]})).ok();
        app.session.execute("select.all", json!({})).unwrap();
        let _ = cached_plan(&app, &mut f).unwrap();
        assert_ne!(f.get(DETECT_KEY), Some(&key));
    }
}
