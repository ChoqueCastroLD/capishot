use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use photocraft_ai::models::FakeStore;
use photocraft_ai_lama::service::{Done, FakeInpainter, Hooks, Inpainter, ModelState, Request};
use photocraft_engine::ai_inpaint_cmds::ID;
use serde_json::json;

use super::*;
use crate::state::Tool;

/// Manga LaMa whose state the test sets; records each request's download consent.
#[derive(Default)]
struct Lama {
    state: Mutex<Option<ModelState>>,
    consents: Mutex<Vec<bool>>,
    /// Says "cached" but the weights are gone (the browser's cache was cleared).
    stale: std::sync::atomic::AtomicBool,
}

impl Lama {
    fn needs() -> Arc<Self> {
        let l = Arc::new(Lama::default());
        l.set(ModelState::NeedsDownload { bytes: 101_904_726 });
        l
    }
    fn set(&self, s: ModelState) {
        *self.state.lock().unwrap() = Some(s);
    }
    fn runs(&self) -> Vec<bool> {
        self.consents.lock().unwrap().clone()
    }
}

impl Inpainter for Lama {
    fn model_state(&self) -> ModelState {
        self.state.lock().unwrap().clone().unwrap_or(ModelState::Ready)
    }
    fn model_state_of(&self, _model: &str) -> ModelState {
        self.model_state()
    }
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        let allowed = req.allow_download;
        self.consents.lock().unwrap().push(allowed);
        let missing = self.stale.load(std::sync::atomic::Ordering::Relaxed) || matches!(self.model_state(), ModelState::NeedsDownload { .. });
        if missing && !allowed {
            return done(Err(photocraft_ai_lama::LamaError::Unavailable("Manga LaMa isn't downloaded".into())));
        }
        FakeInpainter::default().run(req, hooks, done);
    }
}

fn app_with(lama: &Arc<Lama>) -> PhotocraftApp {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 12, "height": 14})).unwrap();
    s.set_inpainter(Some(lama.clone()));
    s.set_ai_models(Some(Arc::new(FakeStore::default())));
    PhotocraftApp::new(s, crate::Services::default())
}

fn layers(app: &PhotocraftApp) -> usize {
    app.session.active().unwrap().doc.layer_count()
}

fn dialog(app: &PhotocraftApp) -> Option<&crate::state::Dialog> {
    app.ui.dialogs.iter().find(|d| owns(&d.fields))
}

#[test]
fn a_missing_model_opens_the_error_dialog_and_ok_opens_the_manager() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    let before = layers(&app);
    let r = crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    let d = dialog(&app).expect("the missing-model dialog opens").clone();
    assert_eq!(r["dialog"], d.id);
    assert_eq!(d.fields["__label"], "Manga LaMa isn't downloaded");
    assert_eq!(ok_label(&d.fields), Some("Open Model Manager"));
    assert!(lama.runs().is_empty(), "nothing ran, nothing downloaded");
    let out = crate::dialogs::confirm(&mut app, d.id).unwrap();
    assert_eq!(out["aiModels"], true);
    assert!(app.ui.ai_models.open);
    assert_eq!(app.ui.ai_models.highlight.as_deref(), Some("manga-lama-f16.safetensors"));
    assert_eq!(layers(&app), before);
    assert!(app.session.prefs().dialogs.keys().all(|k| !k.starts_with("aiModelDownload.")));
}

#[test]
fn cancel_does_nothing() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let before = layers(&app);
    run(&mut app, ID, json!({})).unwrap();
    let id = dialog(&app).unwrap().id;
    crate::dialogs::cancel(&mut app, id).unwrap();
    assert!(app.ui.dialogs.is_empty() && !app.ui.ai_models.open);
    assert_eq!(layers(&app), before);
    assert!(lama.runs().is_empty());
}

#[test]
fn a_downloaded_model_runs_without_a_dialog() {
    let lama = Arc::new(Lama::default());
    let mut app = app_with(&lama);
    let before = layers(&app);
    run(&mut app, ID, json!({})).unwrap();
    assert!(app.ui.dialogs.is_empty());
    assert_eq!(layers(&app), before + 1);
    assert_eq!(lama.runs(), vec![false], "never with download consent");
}

#[test]
fn the_palette_and_a_shortcut_show_the_dialog_too() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    crate::shortcut_dispatch::dispatch(&mut app, &ctx, ID);
    assert!(dialog(&app).is_some(), "status: {}", app.ui.status);
    assert!(!app.ui.status_error, "no error is shown: {}", app.ui.status);
    assert!(lama.runs().is_empty());
}

#[test]
fn the_remove_brush_in_ai_mode_shows_the_dialog_and_changes_nothing() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    app.session.execute("select.deselect", json!({})).unwrap();
    app.ui.tool_options.remove_ai = true;
    let past = app.session.active().unwrap().history.past_len();
    let pts = [[20.0, 20.0, 1.0], [30.0, 20.0, 1.0]];
    assert!(crate::retouch_ui::finish_stroke(&mut app, Tool::Remove, &pts, egui::Modifiers::NONE));
    let d = dialog(&app).expect("the dialog").clone();
    assert!(!app.ui.status_error, "{}", app.ui.status);
    assert_eq!(d.fields["__command"], "paint.remove");
    assert_eq!(app.session.active().unwrap().history.past_len(), past);
    assert!(lama.runs().is_empty());
}

#[test]
fn a_cache_check_in_progress_runs_and_a_missing_model_ends_in_the_dialog() {
    let lama = Arc::new(Lama::default());
    lama.set(ModelState::Checking);
    let mut app = app_with(&lama);
    let before = layers(&app);
    // Checking, but cached: it runs at once (no waiting loop).
    lama.stale.store(false, std::sync::atomic::Ordering::Relaxed);
    run(&mut app, ID, json!({})).unwrap();
    assert_eq!(layers(&app), before + 1);
    // Checking, and missing: the run's error becomes the dialog.
    lama.stale.store(true, std::sync::atomic::Ordering::Relaxed);
    run(&mut app, ID, json!({})).unwrap();
    assert!(dialog(&app).is_some(), "status: {}", app.ui.status);
    assert!(!app.ui.status_error, "{}", app.ui.status);
}

#[test]
fn a_background_run_that_finds_its_model_missing_shows_the_dialog() {
    let lama = Arc::new(Lama::default());
    let mut app = app_with(&lama);
    app.ai_model_last = Some(ID.into());
    assert!(on_job_failed(&mut app, ID, "Clean with AI: Manga LaMa: Manga LaMa isn't downloaded"));
    let d = dialog(&app).expect("the dialog").clone();
    assert_eq!(d.fields["model"], "manga-lama");
    assert!(!on_job_failed(&mut app, ID, "device lost"));
    app.ai_model_last = Some(ID.into());
    assert!(!on_job_failed(&mut app, "image.upscaleAi", "MangaJaNai isn't downloaded"), "another command's failure");
}

/// Without a GPU the automatic choice is the lite MI-GAN: the dialog names it, not Manga LaMa.
#[test]
fn the_dialog_names_the_model_the_device_profile_picks() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let up = photocraft_ai::service::FakeUpscaler { profile: Mutex::new(DeviceProfile { gpu: GpuClass::None, ..Default::default() }), ..Default::default() };
    app.session.set_ai_upscaler(Some(Arc::new(up)));
    invoke(&mut app, ID, &json!({})).unwrap().unwrap();
    assert_eq!(dialog(&app).unwrap().fields.get("model").and_then(Value::as_str), Some(catalog::MIGAN_ID));
    app.ui.dialogs.clear();
    invoke(&mut app, ID, &json!({"quality": "best"})).unwrap().unwrap();
    assert_eq!(dialog(&app).unwrap().fields.get("model").and_then(Value::as_str), Some(catalog::LAMA_ID));
}

#[test]
fn agents_keep_the_engine_error_and_their_download_param() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let e = app.run(ID, json!({})).unwrap_err();
    assert!(e.contains("isn't downloaded") && e.contains("allowDownload"), "{e}");
    assert!(app.ui.dialogs.is_empty());
    // A UI run carrying the agent's consent goes straight to the engine.
    assert!(invoke(&mut app, ID, &json!({"allowDownload": true})).is_none());
}

#[test]
fn upscale_with_params_shows_the_dialog_for_its_model() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 8, "height": 6})).unwrap();
    let fake = Arc::new(photocraft_ai::service::FakeUpscaler::default());
    fake.download_needed.store(true, std::sync::atomic::Ordering::Relaxed);
    s.set_ai_upscaler(Some(fake.clone()));
    let mut app = PhotocraftApp::new(s, crate::Services::default());
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, ai_cmds::COMMAND, json!({"scale": 2})).unwrap();
    let d = dialog(&app).expect("the dialog").clone();
    let model = model_of(&d.fields).unwrap();
    assert!(d.fields["__label"].as_str().unwrap().contains(model.title));
    assert!(fake.download_needed.load(std::sync::atomic::Ordering::Relaxed), "nothing downloaded");
    assert_eq!(app.session.active().unwrap().doc.size.width, 8);
}

#[test]
fn old_download_consents_are_dropped() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    app.session.edit_prefs(|p| {
        p.dialogs.insert("aiModelDownload.manga-lama".into(), json!(true));
        p.dialogs.insert("ai.inpaint".into(), json!({"quality": "fast"}));
    });
    run(&mut app, ID, json!({})).unwrap();
    let d = &app.session.prefs().dialogs;
    assert!(!d.contains_key("aiModelDownload.manga-lama"));
    assert!(d.contains_key("ai.inpaint"), "settings stay");
    assert!(lama.runs().is_empty(), "an old consent doesn't download");
}

fn harness(lama: &Arc<Lama>) -> egui_kittest::Harness<'static, PhotocraftApp> {
    let lama = lama.clone();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app_with(&lama)
    });
    h.run_steps(4);
    h
}

#[test]
fn the_remove_tool_button_shows_the_dialog_and_ok_opens_the_manager() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let lama = Lama::needs();
    let mut h = harness(&lama);
    h.state_mut().ui.tool = Tool::Remove;
    h.run_steps(4);
    let before = layers(h.state());
    h.get_by_label("Clean Selection with AI").click();
    h.run_steps(4);
    assert!(dialog(h.state()).is_some(), "status: {}", h.state().ui.status);
    assert!(h.query_by_label_contains("Clean with AI needs Manga LaMa (102 MB), which isn't on this device yet.").is_some());
    assert!(h.query_by_label_contains("Using a tool never downloads anything").is_some());
    assert!(h.query_by_label_contains("Download and").is_none(), "no download button");
    h.get_by_label("Open Model Manager").click();
    h.run_steps(6);
    assert!(dialog(h.state()).is_none());
    assert!(h.state().ui.ai_models.open);
    assert!(h.query_by_label_contains("Manga LaMa").is_some(), "the Manager lists it");
    assert_eq!(layers(h.state()), before);
    assert!(lama.runs().is_empty());
}

#[test]
fn the_edit_menu_item_shows_the_dialog_and_cancel_does_nothing() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let lama = Lama::needs();
    let mut h = harness(&lama);
    let before = layers(h.state());
    h.get_by_label("Edit").click();
    h.run_steps(4);
    h.get_by_label("Clean with AI").click();
    h.run_steps(4);
    assert!(dialog(h.state()).is_some(), "status: {}", h.state().ui.status);
    h.get_by_label("Cancel").click();
    h.run_steps(4);
    assert!(h.state().ui.dialogs.is_empty());
    assert_eq!(layers(h.state()), before);
    assert!(lama.runs().is_empty());
}

/// Every string this module adds.
pub(crate) const STRINGS: &[&str] = &[
    "{model} isn't downloaded",
    "Open Model Manager",
    "{tool} needs {model} ({mb} MB), which isn't on this device yet.",
    "It is downloading now ({pct}%); try again when it is done.",
    "It is waiting in the download queue; try again when it is done.",
    "Download it once in the AI Model Manager. Using a tool never downloads anything.",
    "Models are kept in this browser and run on this device: your images are not uploaded.",
    "Models are kept on this computer and run on this device: your images are not uploaded.",
];

#[test]
fn strings_are_translated_in_every_catalog() {
    for language in crate::i18n::LANGUAGES.iter().filter(|l| l.code != "en") {
        let lang = crate::i18n::Lang::from_code(language.code).unwrap();
        for key in STRINGS {
            assert_ne!(crate::i18n::tr(lang, key), *key, "{}: {key}", language.code);
        }
    }
}
