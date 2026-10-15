//! Image › Upscale with AI… (`image.upscaleAi`): the dialog in front of the engine command.
//!
//! It shows what will happen before anything runs (from [`ai_cmds::plan`]): the detected page
//! type and the MangaJaNai / IllustrationJaNai model that goes with it, the new size, where the
//! model runs, and, the first time, the download it needs (size, licence, author). OK then reads
//! "Download and Upscale": clicking it is the user's consent, and the command gets
//! `download: true`. The upscale runs as a background job (progress in the status bar, Esc
//! cancels) and lands as one undo step or a new document.

use photocraft_ai::catalog::Content;
use photocraft_ai::service::ModelState;
use photocraft_engine::ai_cmds::{self, COMMAND, Output};
use serde_json::{Map, Value, json};

use crate::PhotocraftApp;
use crate::ai_options_ui::Kind;
use crate::state::DialogKind;

const MARK: &str = "__aiUpscale";
const NEEDS_DOWNLOAD: &str = "__needsDownload";

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
    if !owns(f) {
        return None;
    }
    Some(if f.get(NEEDS_DOWNLOAD).and_then(Value::as_bool) == Some(true) { tl!("Download and Upscale") } else { tl!("Upscale") })
}

pub fn body(app: &mut PhotocraftApp, ui: &mut egui::Ui, f: &mut Map<String, Value>) {
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
    let plan = ai_cmds::plan(&app.session, &params_of(f));
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
                tl!("Runs on this device (your GPU when the browser offers WebGPU); nothing is uploaded.").to_string()
            } else {
                crate::i18n::fmt(tl!("Runs on this device ({device}); nothing is uploaded."), &[("device", &device)])
            };
            ui.label(egui::RichText::new(runs).color(tokens.text_dim));
            match &p.state {
                ModelState::Ready => {}
                ModelState::NeedsDownload | ModelState::Checking => {
                    needs_download = true;
                    let mb = format!("{:.1}", p.model.bytes as f64 / 1e6);
                    ui.add_space(6.0);
                    ui.label(crate::i18n::fmt(
                        tl!("First use downloads the model: {mb} MB. Licence {license} ({author}). It is kept for next time."),
                        &[("mb", &mb), ("license", p.model.license), ("author", p.model.author)],
                    ));
                    if p.state == ModelState::Checking {
                        ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
                    }
                }
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
    crate::ai_options_ui::advanced(app, ui, Kind::Upscale, f);
}

pub fn confirm(app: &mut PhotocraftApp, f: &Map<String, Value>) -> Result<Value, String> {
    crate::ai_options_ui::save(app, Kind::Upscale, f);
    let mut p = params_of(f);
    // OK said "Download and Upscale" next to the size and licence: that is the consent.
    if f.get(NEEDS_DOWNLOAD).and_then(Value::as_bool) == Some(true) {
        p["download"] = json!(true);
        if let Ok(plan) = ai_cmds::plan(&app.session, &p) {
            crate::ai_download_prompt::remember(app, plan.model);
        }
    }
    app.run(COMMAND, p)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use photocraft_ai::service::FakeUpscaler;

    use super::*;

    fn app() -> PhotocraftApp {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 8, "height": 6})).unwrap();
        let fake = Arc::new(FakeUpscaler::default());
        fake.download_needed.store(true, std::sync::atomic::Ordering::Relaxed);
        s.set_ai_upscaler(Some(fake));
        PhotocraftApp::new(s, crate::Services::default())
    }

    #[test]
    fn the_menu_item_opens_the_dialog_and_ok_downloads_with_consent() {
        let mut app = app();
        let r = invoke(&mut app, COMMAND, &json!({})).unwrap().unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let d = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().clone();
        assert!(owns(&d.fields));
        let mut f = d.fields.clone();
        f.insert("scale".into(), json!("4"));
        f.insert(NEEDS_DOWNLOAD.into(), json!(true));
        assert_eq!(ok_label(&f), Some("Download and Upscale"));
        assert_eq!(params_of(&f)["scale"], 4);
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
    fn the_dialog_renders() {
        let mut app = app();
        let r = invoke(&mut app, COMMAND, &json!({})).unwrap().unwrap();
        let id = r["dialog"].as_u64().unwrap();
        let mut f = app.ui.dialogs.iter().find(|d| d.id == id).unwrap().fields.clone();
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        ctx.run_ui(Default::default(), |ui| body(&mut app, ui, &mut f)).textures_delta.clear();
        assert_eq!(f.get(NEEDS_DOWNLOAD), Some(&json!(true)));
    }
}
