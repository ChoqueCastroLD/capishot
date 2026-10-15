use std::sync::{Arc, Mutex};

use photocraft_ai::profile::{DeviceProfile, GpuClass};
use photocraft_ai::service::FakeUpscaler;
use photocraft_ai_lama::service::FakeInpainter;
use egui_kittest::kittest::Queryable;
use serde_json::json;

use super::*;

fn app(profile: DeviceProfile) -> (PhotocraftApp, Arc<FakeUpscaler>) {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 12, "height": 14})).unwrap();
    let up = Arc::new(FakeUpscaler { profile: Mutex::new(profile), ..Default::default() });
    s.set_ai_upscaler(Some(up.clone()));
    s.set_inpainter(Some(Arc::new(FakeInpainter::default())));
    s.set_ai_models(Some(Arc::new(photocraft_ai::models::FakeStore::default())));
    (PhotocraftApp::new(s, crate::Services::default()), up)
}

fn no_gpu() -> DeviceProfile {
    DeviceProfile { gpu: GpuClass::None, cores: Some(8), ..Default::default() }
}

fn gpu() -> DeviceProfile {
    DeviceProfile { gpu: GpuClass::Discrete, memory_gb: Some(8.0), cores: Some(8), ..Default::default() }
}

fn harness(profile: DeviceProfile) -> egui_kittest::Harness<'static, PhotocraftApp> {
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app(profile).0
    });
    h.run_steps(4);
    h
}

#[test]
fn settings_are_remembered_and_fill_ui_runs_without_overriding_them() {
    let (mut app, _) = app(gpu());
    let mut f = Map::new();
    fill_fields(&app, Kind::Inpaint, &mut f);
    assert_eq!(f["quality"], "auto");
    assert!(params_of(Kind::Inpaint, &f).is_empty(), "defaults aren't stored or sent");
    f.insert("quality".into(), json!("fast"));
    f.insert("threads".into(), json!(3));
    f.insert("keepLoaded".into(), json!(false));
    save(&mut app, Kind::Inpaint, &f);
    assert_eq!(app.session.prefs().dialogs["ai.inpaint"], json!({"quality": "fast", "threads": 3, "keepLoaded": false}));
    // Every UI entry point gets them; what the run sets itself wins.
    let p = with_saved(&app, ai_inpaint_cmds::ID, &json!({}));
    assert_eq!((p["quality"].as_str(), p["threads"].as_u64()), (Some("fast"), Some(3)));
    let p = with_saved(&app, "paint.remove", &json!({"ai": true, "quality": "best"}));
    assert_eq!(p["quality"], "best");
    assert_eq!(with_saved(&app, "paint.remove", &json!({"ai": false})), json!({"ai": false}), "patch mode has no AI settings");
    assert_eq!(with_saved(&app, ai_cmds::COMMAND, &json!({"scale": 2})), json!({"scale": 2}), "upscale has its own settings");
    let mut g = Map::new();
    fill_fields(&app, Kind::Inpaint, &mut g);
    assert_eq!((g["quality"].as_str(), g["keepLoaded"].as_bool()), (Some("fast"), Some(false)));
    // A corrupt preference is ignored.
    app.session.edit_prefs(|p| {
        p.dialogs.insert("ai.upscale".into(), json!({"quality": 7, "tile": "big", "device": "cpu"}));
    });
    assert_eq!(Value::Object(saved(&app, Kind::Upscale)), json!({"device": "cpu"}));
}

#[test]
fn the_explanation_says_what_runs_and_why_with_its_size() {
    let (app, _) = app(no_gpu());
    let (m, run) = ai_inpaint_cmds::model_for(&app.session, &json!({}), ai_inpaint_cmds::ID).unwrap();
    let line = explanation(m, &run);
    assert!(line.contains("No GPU") && line.contains("MI-GAN") && line.contains("13 MB"), "{line}");
    let (m, run) = ai_inpaint_cmds::model_for(&app.session, &json!({"quality": "best"}), ai_inpaint_cmds::ID).unwrap();
    let line = explanation(m, &run);
    assert!(line.contains("Manga LaMa") && line.contains("CPU"), "{line}");
    let (app, _) = self::app(gpu());
    let (m, run) = ai_inpaint_cmds::model_for(&app.session, &json!({}), ai_inpaint_cmds::ID).unwrap();
    assert!(explanation(m, &run).starts_with("Your GPU runs the full model: Manga LaMa"));
    let (m, run) = ai_inpaint_cmds::model_for(&app.session, &json!({"model": "migan-512"}), ai_inpaint_cmds::ID).unwrap();
    assert!(explanation(m, &run).starts_with("Model chosen in Advanced: MI-GAN"));
    let p = ai_cmds::plan(&app.session, &json!({"quality": "fast", "scale": 2})).unwrap();
    let line = explanation(p.model, &p.run);
    assert!(line.contains("Real-ESRGAN Compact 2x") && line.contains("1.2 MB"), "{line}");
}

#[test]
fn the_options_dialog_shows_simple_choices_and_advanced_on_demand() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let mut h = harness(no_gpu());
    // The Remove tool's options bar opens it.
    h.state_mut().ui.tool = crate::state::Tool::Remove;
    h.run_steps(4);
    h.get_by_label("AI Options…").click();
    h.run_steps(4);
    assert!(h.state().ui.dialogs.iter().any(|d| owns(&d.fields)));
    for want in ["Quality:", "Device:", "the lite model MI-GAN (13 MB) on the CPU"] {
        assert!(h.query_by_label_contains(want).is_some(), "{want}");
    }
    assert!(h.query_by_label_contains("CPU threads:").is_none(), "Advanced is closed by default");
    h.get_by_label("Advanced…").click();
    h.run_steps(4);
    for want in ["Model:", "Max working side:", "Precision:", "CPU threads:", "Keep the model in memory between runs", "Downloaded models:", "AI Models…", "Hide Advanced"] {
        assert!(h.query_by_label_contains(want).is_some(), "{want}");
    }
    let d = h.state_mut().ui.dialogs.iter_mut().find(|d| owns(&d.fields)).unwrap();
    d.fields.insert("quality".into(), json!("best"));
    h.run_steps(2);
    assert!(h.query_by_label_contains("Best on the CPU: the full model Manga LaMa").is_some());
    h.get_by_label("OK").click();
    h.run_steps(4);
    let app = h.state();
    assert!(!app.ui.dialogs.iter().any(|d| owns(&d.fields)));
    assert_eq!(app.session.prefs().dialogs["ai.inpaint"]["quality"], "best");
    // Clean with AI and the Remove tool's brush now run Manga LaMa.
    let p = with_saved(app, ai_inpaint_cmds::ID, &json!({}));
    let (m, _) = ai_inpaint_cmds::model_for(&app.session, &p, ai_inpaint_cmds::ID).unwrap();
    assert_eq!(m.id, "manga-lama");
}

#[test]
fn advanced_opens_the_model_manager_and_closes_the_dialog() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let mut h = harness(no_gpu());
    let id = open_inpaint_options(h.state_mut());
    h.state_mut().ui.dialogs.iter_mut().find(|d| d.id == id).unwrap().fields.insert(ADVANCED.into(), json!(true));
    h.run_steps(4);
    h.get_by_label("AI Models…").click();
    h.run_steps(4);
    assert!(h.state().ui.ai_models.open, "the Manager opened");
    assert!(!h.state().ui.dialogs.iter().any(|d| owns(&d.fields)), "AI Options closed");
}

#[test]
fn new_strings_are_translated_in_every_catalog() {
    for language in crate::i18n::LANGUAGES.iter().filter(|l| l.code != "en") {
        let lang = crate::i18n::Lang::from_code(language.code).unwrap();
        for key in STRINGS {
            assert_ne!(crate::i18n::tr(lang, key), *key, "{}: {key}", language.code);
        }
    }
}

/// Every string this module adds.
const STRINGS: &[&str] = &[
    "Your GPU runs the full model: {model} ({mb} MB).",
    "Full model: {model} ({mb} MB), on your GPU when there is one, else on the CPU.",
    "No GPU available: the lite model {model} ({mb} MB) on the CPU. Choose Best for full quality (slower).",
    "Only a software GPU: the lite model {model} ({mb} MB) on the CPU. Choose Best for full quality (slower).",
    "Phone or tablet: the lite model {model} ({mb} MB), faster and lighter. Choose Best for full quality.",
    "Little memory or few cores: the lite model {model} ({mb} MB). Choose Best for full quality.",
    "Fast: the lite model {model} ({mb} MB); lower quality than the full model.",
    "Best on the CPU: the full model {model} ({mb} MB); this can take minutes per page.",
    "Best: the full model {model} ({mb} MB).",
    "CPU: the lite model {model} ({mb} MB). Choose Best for full quality (slower).",
    "Model chosen in Advanced: {model} ({mb} MB).",
    "Quality:",
    "Fast (lite)",
    "Best (full)",
    "Device:",
    "Hide Advanced",
    "Advanced…",
    "Tile size:",
    "Input pixels per tile; 0 = automatic. Larger tiles are faster but need more memory.",
    "Overlap:",
    "Max working side:",
    "Larger areas are scaled down to this size for the model; 0 = automatic (512, at most 512 for MI-GAN).",
    "Precision:",
    "f32 (weights downloaded as f16; int8 isn't available)",
    "CPU threads:",
    "0 = one per core.",
    "Keep the model in memory between runs",
    "Downloaded models: {mb} MB",
    "Downloaded models: unknown",
    "AI Models…",
    "Download and delete AI models",
    "AI Cleaning Options",
    "Quality and device for Clean with AI and the Remove tool's AI mode.",
    "AI Options…",
    "Use AI",
    "Nothing is uploaded: it runs on this device.",
    "Fill strokes with AI instead of nearby patches: better on screentone and line art. AI Options… chooses the quality and the device.",
    "Erase the selected text or SFX and redraw the background with AI, on a new layer. It uses a downloaded model (Window › AI Models…).",
];
