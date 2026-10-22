use std::sync::Arc;

use egui_kittest::kittest::Queryable;
use photocraft_ai::catalog::{LAMA_ID, MIGAN_ID, model};
use photocraft_ai::models::{FakeStore, ModelStore};
use serde_json::json;

use super::*;

fn app_with(store: &Arc<FakeStore>) -> PhotocraftApp {
    let mut s = photocraft_engine::Session::new();
    s.execute("file.new", json!({"width": 64, "height": 48})).unwrap();
    s.set_ai_models(Some(store.clone()));
    PhotocraftApp::new(s, crate::Services::default())
}

fn harness(store: &Arc<FakeStore>) -> egui_kittest::Harness<'static, PhotocraftApp> {
    let store = store.clone();
    let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(64).build_eframe(move |cc| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        app_with(&store)
    });
    h.run_steps(4);
    h
}

fn status(store: &FakeStore, id: &str) -> Status {
    store.status(model(id).unwrap())
}

#[test]
fn the_window_menu_item_opens_and_closes_it() {
    let store = Arc::new(FakeStore::default());
    let mut app = app_with(&store);
    let ctx = egui::Context::default();
    let item = crate::menus::menu_items(&app).into_iter().find(|i| i.id == COMMAND).expect("in the menus");
    assert_eq!(item.path.first().map(String::as_str), Some("Window"));
    assert!(crate::menus::is_enabled(&app, COMMAND));
    crate::menus::invoke(&mut app, &ctx, COMMAND, json!({"model": MIGAN_ID})).unwrap();
    assert!(app.ui.ai_models.open);
    assert_eq!(app.ui.ai_models.highlight.as_deref(), Some(model(MIGAN_ID).unwrap().file));
    assert_eq!(inspect(&app)["open"], true);
    crate::menus::invoke(&mut app, &ctx, COMMAND, json!({"open": false})).unwrap();
    assert!(!app.ui.ai_models.open);
}

#[test]
fn it_lists_every_model_with_tools_device_licence_and_status() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let store = Arc::new(FakeStore::default());
    store.put(model(MIGAN_ID).unwrap());
    let mut h = harness(&store);
    open(h.state_mut(), None);
    h.run_steps(4);
    for f in photocraft_ai::models::files() {
        assert!(h.query_all_by_label_contains(&photocraft_ai::models::file_title(f)).next().is_some(), "{}", f.title);
    }
    for want in [
        "Cleaning",
        "Upscaling",
        "Used by: Clean with AI, Remove Tool",
        "Used by: Upscale with AI",
        "Runs on: GPU or CPU (best on a GPU)",
        "Runs on: GPU or CPU (lite: fine on either)",
        "MIT · dreMaz (AnimeMangaInpainting)",
        "Downloaded",
        "Not downloaded",
        "Models on this device: 13 MB",
        "Download recommended for this device",
        "Download all",
    ] {
        assert!(h.query_all_by_label_contains(want).next().is_some(), "{want}");
    }
}

#[test]
fn download_cancel_and_delete_go_through_the_commands() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let store = Arc::new(FakeStore::default());
    let mut h = harness(&store);
    open(h.state_mut(), None);
    h.run_steps(4);
    // The first row is Manga LaMa.
    h.query_all_by_label("Download").next().expect("a Download button").click();
    h.run_steps(4);
    assert!(matches!(status(&store, LAMA_ID), Status::Downloading { .. }));
    assert!(h.query_by_label_contains("Downloading 0%").is_some());
    // The status bar shows the queue while the window is closed.
    h.state_mut().ui.ai_models.open = false;
    h.run_steps(4);
    assert!(h.query_by_label_contains("↓ Manga LaMa 0%").is_some());
    h.get_by_label_contains("↓ Manga LaMa").click();
    h.run_steps(4);
    assert!(h.state().ui.ai_models.open, "the indicator opens the window");
    h.query_all_by_label("Cancel").next().expect("Cancel").click();
    h.run_steps(4);
    assert_eq!(status(&store, LAMA_ID), Status::NotDownloaded);
    // Download again and let it finish: Delete removes it.
    h.query_all_by_label("Download").next().unwrap().click();
    h.run_steps(2);
    store.step();
    h.run_steps(4);
    assert_eq!(status(&store, LAMA_ID), Status::Downloaded);
    h.query_all_by_label("Delete").next().expect("Delete").click();
    h.run_steps(4);
    assert_eq!(status(&store, LAMA_ID), Status::NotDownloaded);
}

#[test]
fn download_all_queues_everything_one_at_a_time_and_delete_all_asks_first() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let store = Arc::new(FakeStore::default());
    let mut h = harness(&store);
    open(h.state_mut(), None);
    h.run_steps(4);
    h.get_by_label_contains("Download all").click();
    h.run_steps(4);
    // "Download all" is every tool's model; Generative Fill (2 GB) has its own button.
    let files: Vec<_> =
        photocraft_ai::models::files().into_iter().filter(|m| photocraft_ai::models::group(m) != photocraft_ai::models::Group::Generative).collect();
    let downloading = files.iter().filter(|m| matches!(store.status(m), Status::Downloading { .. })).count();
    let queued = files.iter().filter(|m| store.status(m) == Status::Queued).count();
    assert_eq!((downloading, queued), (1, files.len() - 1), "one at a time");
    assert!(h.query_by_label_contains("more queued").is_some());
    while store.step() {}
    h.run_steps(4);
    assert!(files.iter().all(|m| store.status(m) == Status::Downloaded));
    h.get_by_label("Delete all").click();
    h.run_steps(4);
    assert!(h.query_by_label_contains("Delete every downloaded model?").is_some());
    assert!(files.iter().all(|m| store.status(m) == Status::Downloaded), "nothing deleted before confirming");
    h.query_all_by_label("Delete all").next().unwrap().click();
    h.run_steps(4);
    assert!(files.iter().all(|m| store.status(m) == Status::NotDownloaded));
}

#[test]
fn recommended_downloads_follow_the_device() {
    let store = Arc::new(FakeStore::default());
    let mut app = app_with(&store);
    app.run(ai_models_cmds::DOWNLOAD, json!({"recommended": true})).unwrap();
    while store.step() {}
    // No upscaler, so no known GPU: the lite models.
    assert_eq!(status(&store, MIGAN_ID), Status::Downloaded);
    assert_eq!(status(&store, LAMA_ID), Status::NotDownloaded);
}

#[test]
fn the_window_says_why_without_downloads() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    let store = Arc::new(FakeStore::default());
    store.inner.lock().unwrap().unavailable = Some("AI models are not hosted on this site yet".into());
    let mut h = harness(&store);
    open(h.state_mut(), None);
    h.run_steps(4);
    assert!(h.query_by_label_contains("not hosted on this site").is_some());
}

#[test]
fn texts() {
    let _lang = crate::i18n::language_scope(crate::i18n::Lang::EN);
    assert_eq!(mb(1_247_200), "1.2");
    assert_eq!(size(101_904_726), "102 MB");
    assert_eq!(size(1_400_000_000), "1.4 GB");
    assert_eq!(duration(12.2), "12 s");
    assert_eq!(duration(400.0), "7 min");
    let s = status_text(&Status::Downloading { done: 42, total: 100, bytes_per_sec: 3e6 });
    assert!(s.starts_with("Downloading 42%") && s.contains("3.0 MB/s") && s.contains("left"), "{s}");
    assert_eq!(status_text(&Status::Failed("HTTP 404".into())), "Failed: HTTP 404");
    assert_eq!(tools_text(model(LAMA_ID).unwrap()), "Clean with AI, Remove Tool");
}

/// Every string this module adds.
const STRINGS: &[&str] = &[
    "AI Models",
    "AI Models…",
    "GPU or CPU (best on a GPU)",
    "GPU or CPU (lite: fine on either)",
    "Not downloaded",
    "Queued",
    "Downloading {pct}%",
    "{time} left",
    "Downloaded",
    "Update available",
    "Failed: {reason}",
    "Checking…",
    "Models on this device: {size}",
    "site storage {used} of {quota}",
    "Download recommended for this device ({size})",
    "No usable GPU here: the lite models (MI-GAN, Real-ESRGAN Compact).",
    "A GPU is available: the full models (Manga LaMa, MangaJaNai for your open pages, IllustrationJaNai).",
    "Download all ({size})",
    "Delete every downloaded model?",
    "Delete all",
    "Keep",
    "Cancel all",
    "Cleaning",
    "Upscaling",
    "Downloading {model} {pct}%",
    "Starting the next download…",
    "Retry",
    "Download",
    "Update",
    "Used by: {tools}",
    "Runs on: {where}",
    "Licence:",
    "AI model downloads: click to open the AI Model Manager",
    "{model} ({mb} MB) isn't downloaded. Download it in the AI Model Manager first.",
];

#[test]
fn strings_are_translated_in_every_catalog() {
    for language in crate::i18n::LANGUAGES.iter().filter(|l| l.code != "en") {
        let lang = crate::i18n::Lang::from_code(language.code).unwrap();
        for key in STRINGS {
            assert_ne!(crate::i18n::tr(lang, key), *key, "{}: {key}", language.code);
        }
        assert_ne!(crate::i18n::trn(lang, 2, "{n} more queued", "{n} more queued"), "2 more queued", "{}", language.code);
    }
}
