use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use photocraft_ai_lama::service::{Done, FakeInpainter, Hooks, Inpainter, ModelState, Request};
use photocraft_engine::ai_inpaint_cmds::ID;
use serde_json::json;

use super::*;
use crate::state::Tool;

/// Manga LaMa whose state the test sets; records each request's consent.
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
    fn consents(&self) -> Vec<bool> {
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
        if self.stale.load(std::sync::atomic::Ordering::Relaxed) && !allowed {
            return done(Err(photocraft_ai_lama::LamaError::Unavailable("Manga LaMa isn't downloaded yet".into())));
        }
        if matches!(self.model_state(), ModelState::NeedsDownload { .. }) && !allowed {
            return done(Err(photocraft_ai_lama::LamaError::Unavailable("not downloaded".into())));
        }
        FakeInpainter::default().run(req, hooks, done);
    }
}

fn app_with(lama: &Arc<Lama>) -> PhotocraftApp {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48, "background": "white"})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 10, "width": 12, "height": 14})).unwrap();
    s.set_inpainter(Some(lama.clone()));
    PhotocraftApp::new(s, crate::Services::default())
}

fn layers(app: &PhotocraftApp) -> usize {
    app.session.active().unwrap().doc.layer_count()
}

fn prompt(app: &PhotocraftApp) -> Option<&crate::state::Dialog> {
    app.ui.dialogs.iter().find(|d| owns(&d.fields))
}

#[test]
fn the_menu_item_asks_then_confirm_downloads_and_cleans_and_remembers() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    let before = layers(&app);
    let r = crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    let d = prompt(&app).expect("the consent prompt opens").clone();
    assert_eq!(r["dialog"], d.id);
    assert_eq!(d.fields["__label"], "Download Manga LaMa?");
    assert_eq!(ok_label(&d.fields), Some("Download and Clean"));
    assert!(lama.consents().is_empty(), "nothing ran before the answer");
    assert_eq!(layers(&app), before);
    crate::dialogs::confirm(&mut app, d.id).unwrap();
    assert!(prompt(&app).is_none());
    assert_eq!(layers(&app), before + 1, "the cleaned layer");
    assert_eq!(lama.consents(), vec![true]);
    assert!(consented(&app, catalog::model(catalog::LAMA_ID).unwrap()));
    // Remembered: the next run doesn't ask (the model is still "not cached" here).
    app.session.execute("select.rect", json!({"x": 2, "y": 2, "width": 8, "height": 8})).unwrap();
    crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    assert!(prompt(&app).is_none());
    assert_eq!(layers(&app), before + 2);
    assert_eq!(lama.consents(), vec![true, true]);
}

#[test]
fn cancel_runs_nothing_and_remembers_nothing() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    let before = layers(&app);
    crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    let id = prompt(&app).unwrap().id;
    crate::dialogs::cancel(&mut app, id).unwrap();
    assert!(app.ui.dialogs.is_empty());
    assert_eq!(layers(&app), before);
    assert!(lama.consents().is_empty());
    assert!(!consented(&app, catalog::model(catalog::LAMA_ID).unwrap()));
    // It asks again next time.
    crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    assert!(prompt(&app).is_some());
    // A second request while it is open doesn't stack another prompt.
    crate::menus::invoke(&mut app, &ctx, ID, json!({})).unwrap();
    assert_eq!(app.ui.dialogs.iter().filter(|d| owns(&d.fields)).count(), 1);
}

#[test]
fn a_cached_model_runs_without_asking() {
    let lama = Arc::new(Lama::default());
    let mut app = app_with(&lama);
    let before = layers(&app);
    run(&mut app, ID, json!({})).unwrap();
    assert!(app.ui.dialogs.is_empty());
    assert_eq!(layers(&app), before + 1);
    assert_eq!(lama.consents(), vec![false]);
}

#[test]
fn the_palette_and_a_shortcut_ask_too() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    // The palette runs its pick through menus::invoke with `{}`; shortcuts dispatch the same way.
    crate::shortcut_dispatch::dispatch(&mut app, &ctx, ID);
    assert!(prompt(&app).is_some(), "status: {}", app.ui.status);
    assert!(!app.ui.status_error, "no error is shown: {}", app.ui.status);
    assert!(lama.consents().is_empty());
}

#[test]
fn the_remove_brush_in_ai_mode_asks_and_reruns_the_same_stroke() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    app.session.execute("select.deselect", json!({})).unwrap();
    app.ui.tool_options.remove_ai = true;
    let past = app.session.active().unwrap().history.past_len();
    let pts = [[20.0, 20.0, 1.0], [30.0, 20.0, 1.0]];
    assert!(crate::retouch_ui::finish_stroke(&mut app, Tool::Remove, &pts, egui::Modifiers::NONE));
    let d = prompt(&app).expect("the stroke asks first").clone();
    assert!(!app.ui.status_error, "{}", app.ui.status);
    assert_eq!(d.fields["__command"], "paint.remove");
    assert_eq!(d.fields["params"]["ai"], true);
    assert!(d.fields["params"]["points"].is_array());
    assert!(d.fields["params"].get("allowDownload").is_none());
    assert_eq!(app.session.active().unwrap().history.past_len(), past);
    crate::dialogs::confirm(&mut app, d.id).unwrap();
    assert_eq!(lama.consents(), vec![true]);
    assert_eq!(app.session.active().unwrap().history.past_len(), past + 1, "the stroke ran");
}

#[test]
fn a_cache_check_in_progress_waits_then_runs_or_asks() {
    let lama = Arc::new(Lama::default());
    lama.set(ModelState::Checking);
    let mut app = app_with(&lama);
    let ctx = egui::Context::default();
    let before = layers(&app);
    let r = run(&mut app, ID, json!({})).unwrap();
    assert_eq!(r["pending"], "modelCheck");
    assert!(app.ui.dialogs.is_empty() && !app.ui.status_error);
    tick(&mut app, &ctx);
    assert!(app.ai_download_wait.is_some(), "still waiting");
    // The cache answers "cached": it runs without asking.
    lama.set(ModelState::Ready);
    tick(&mut app, &ctx);
    assert!(app.ai_download_wait.is_none());
    assert_eq!(layers(&app), before + 1);
    assert!(app.ui.dialogs.is_empty());
    // The cache answers "not cached": it asks.
    lama.set(ModelState::Checking);
    run(&mut app, ID, json!({})).unwrap();
    lama.set(ModelState::NeedsDownload { bytes: 1 << 20 });
    tick(&mut app, &ctx);
    assert!(prompt(&app).is_some());
    // No answer for too long: it asks anyway (consent covers either outcome).
    let id = prompt(&app).unwrap().id;
    crate::dialogs::cancel(&mut app, id).unwrap();
    lama.set(ModelState::Checking);
    run(&mut app, ID, json!({})).unwrap();
    app.ai_download_wait.as_mut().unwrap().since_ms -= CHECK_WAIT_MS + 1.0;
    tick(&mut app, &ctx);
    assert!(prompt(&app).is_some());
}

#[test]
fn a_stale_cached_answer_asks_instead_of_showing_the_error() {
    let lama = Arc::new(Lama::default());
    lama.stale.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut app = app_with(&lama);
    let before = layers(&app);
    run(&mut app, ID, json!({})).unwrap();
    let id = prompt(&app).expect("asks").id;
    assert!(!app.ui.status_error, "{}", app.ui.status);
    crate::dialogs::confirm(&mut app, id).unwrap();
    assert_eq!(lama.consents(), vec![false, true]);
    assert_eq!(layers(&app), before + 1);
    // As a background job's failure, with consent given: it runs again at once.
    lama.consents.lock().unwrap().clear();
    app.ai_download_last = Some((ID.into(), json!({})));
    assert!(on_job_failed(&mut app, ID, "Clean with AI: Manga LaMa isn't downloaded yet"));
    assert_eq!(lama.consents(), vec![true]);
    assert!(!on_job_failed(&mut app, ID, "device lost"));
}

/// Without a GPU the automatic choice is the lite MI-GAN: the prompt names it (and its size), not
/// Manga LaMa, and consent is remembered for it.
#[test]
fn the_prompt_names_the_lite_model_the_device_profile_picks() {
    use photocraft_ai::profile::{DeviceProfile, GpuClass};
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let up = photocraft_ai::service::FakeUpscaler { profile: Mutex::new(DeviceProfile { gpu: GpuClass::None, ..Default::default() }), ..Default::default() };
    app.session.set_ai_upscaler(Some(Arc::new(up)));
    invoke(&mut app, ID, &json!({})).unwrap().unwrap();
    let d = prompt(&app).expect("asks first");
    assert_eq!(d.fields.get("model").and_then(Value::as_str), Some(catalog::MIGAN_ID));
    // Best quality asks for Manga LaMa instead.
    app.ui.dialogs.clear();
    invoke(&mut app, ID, &json!({"quality": "best"})).unwrap().unwrap();
    assert_eq!(prompt(&app).unwrap().fields.get("model").and_then(Value::as_str), Some(catalog::LAMA_ID));
}

#[test]
fn engine_execute_keeps_the_technical_error() {
    let lama = Lama::needs();
    let mut app = app_with(&lama);
    let e = app.run(ID, json!({})).unwrap_err();
    assert!(e.contains("allowDownload"), "{e}");
    assert!(app.ui.dialogs.is_empty());
}

#[test]
fn upscale_with_params_asks_for_its_model() {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 8, "height": 6})).unwrap();
    let fake = Arc::new(photocraft_ai::service::FakeUpscaler::default());
    fake.download_needed.store(true, std::sync::atomic::Ordering::Relaxed);
    s.set_ai_upscaler(Some(fake));
    let mut app = PhotocraftApp::new(s, crate::Services::default());
    let ctx = egui::Context::default();
    crate::menus::invoke(&mut app, &ctx, ai_cmds::COMMAND, json!({"scale": 2})).unwrap();
    let d = prompt(&app).expect("asks").clone();
    assert_eq!(ok_label(&d.fields), Some("Download and Upscale"));
    let model = model_of(&d.fields).unwrap();
    assert!(d.fields["__label"].as_str().unwrap().contains(model.title));
    let out = crate::dialogs::confirm(&mut app, d.id).unwrap();
    assert_eq!(out["width"], 16, "{out}");
    assert!(consented(&app, model));
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
fn the_remove_tool_button_opens_the_prompt_and_ok_cleans() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let lama = Lama::needs();
    let mut h = harness(&lama);
    h.state_mut().ui.tool = Tool::Remove;
    h.run_steps(4);
    let before = layers(h.state());
    h.get_by_label("Clean Selection with AI").click();
    h.run_steps(4);
    assert!(prompt(h.state()).is_some(), "status: {}", h.state().ui.status);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    assert!(h.query_by_label_contains("needs a one-time download of 102 MB").is_some());
    assert!(h.query_by_label_contains("Licence MIT, by dreMaz").is_some());
    h.get_by_label("Download and Clean").click();
    h.run_steps(8);
    assert!(prompt(h.state()).is_none());
    assert_eq!(layers(h.state()), before + 1);
    assert_eq!(lama.consents(), vec![true]);
}

#[test]
fn the_edit_menu_item_opens_the_prompt_and_cancel_does_nothing() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let lama = Lama::needs();
    let mut h = harness(&lama);
    let before = layers(h.state());
    h.get_by_label("Edit").click();
    h.run_steps(4);
    h.get_by_label("Clean with AI").click();
    h.run_steps(4);
    assert!(prompt(h.state()).is_some(), "status: {}", h.state().ui.status);
    assert!(!h.state().ui.status_error, "{}", h.state().ui.status);
    h.get_by_label("Cancel").click();
    h.run_steps(4);
    assert!(h.state().ui.dialogs.is_empty());
    assert_eq!(layers(h.state()), before);
    assert!(lama.consents().is_empty());
}

#[test]
fn prompt_strings_are_translated_in_every_supported_catalog() {
    for language in crate::i18n::LANGUAGES.iter().filter(|l| l.code != "en") {
        let lang = crate::i18n::Lang::from_code(language.code).unwrap();
        for key in [
            "Download {model}?",
            "Download and Clean",
            "{model} needs a one-time download of {mb} MB before it can run.",
            "Licence {license}, by {author}.",
            "It is stored in this browser, only once. It runs on this device: your images are not uploaded.",
            "It is stored on this computer, only once. It runs on this device: your images are not uploaded.",
            "AI models",
            "Downloaded on demand the first time a feature needs them; not bundled with the app.",
        ] {
            assert_ne!(crate::i18n::tr(lang, key), key, "{}: {key}", language.code);
        }
    }
}
