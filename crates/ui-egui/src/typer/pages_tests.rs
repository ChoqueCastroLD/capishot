//! Page files: picking keeps the contents in memory under a cap, the current page's file opens or
//! comes to the front as the cursor changes page, and auto-close asks before closing unsaved work.

use photocraft_doc::{ColorMode, Document, SampleType, Size};
use serde_json::json;

use super::*;

const SCRIPT: &str = "Página 1\nHola.\nPág. 2\nAdiós.\nPage 3\nFin.";

/// An app whose importer makes a 4×4 document named after the file.
fn app() -> PhotocraftApp {
    let services = crate::Services {
        import: Some(Box::new(|name: &str, _b: &[u8], _depth: usize| Ok((Document::new(name, Size::new(4, 4), ColorMode::Rgb, SampleType::U8), Vec::new())))),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    app.run("typer.script.set", json!({"text": SCRIPT})).unwrap();
    app
}

fn names(app: &PhotocraftApp) -> Vec<String> {
    app.session.documents().iter().map(|d| d.doc.name.clone()).collect()
}

fn active(app: &PhotocraftApp) -> String {
    app.session.active().map(|d| d.doc.name.clone()).unwrap_or_default()
}

fn files(n: &[&str]) -> Vec<(String, Vec<u8>)> {
    n.iter().map(|s| (s.to_string(), vec![0u8; 10])).collect()
}

#[test]
fn page_files_open_as_the_cursor_changes_page() {
    let mut app = app();
    let ctx = egui::Context::default();
    let picked = ["ch5_p03.psd", "ch5_p01.psd", "notes.txt", "ch5_p02.psd"];
    set_files(&mut app, picked.iter().map(|s| s.to_string()).collect(), files(&picked)).unwrap();
    assert_eq!(app.session.typer.state.images, ["ch5_p01.psd", "ch5_p02.psd", "ch5_p03.psd"], "pages only, natural order");
    assert_eq!(app.ui.typer.page_files.total(), 30);
    sync(&mut app, &ctx);
    assert_eq!(names(&app), ["ch5_p01.psd"], "the current page's file opens");
    sync(&mut app, &ctx);
    assert_eq!(names(&app).len(), 1, "only once");
    // Spanish and English markers both move the page.
    app.run("typer.page.next", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(active(&app), "ch5_p02.psd");
    app.run("typer.page.next", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(names(&app), ["ch5_p01.psd", "ch5_p02.psd", "ch5_p03.psd"]);
    // Back to page 1: its open document comes to the front instead of opening again.
    app.run("typer.line.set", json!({"index": 1})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(names(&app).len(), 3);
    assert_eq!(active(&app), "ch5_p01.psd");
    assert_eq!(app.session.typer.state.last_opened_image.as_deref(), Some("ch5_p01.psd"));
    // Desync: nothing opens any more.
    desync(&mut app);
    assert!(app.session.typer.state.images.is_empty() && app.ui.typer.page_files.total() == 0);
    app.run("typer.page.next", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(names(&app).len(), 3);
}

#[test]
fn auto_close_asks_before_closing_unsaved_pages() {
    let mut app = app();
    let ctx = egui::Context::default();
    app.run("typer.settings.set", json!({"key": "autoClosePSD", "value": true})).unwrap();
    let picked = ["01.png", "02.png", "03.png"];
    set_files(&mut app, picked.iter().map(|s| s.to_string()).collect(), files(&picked)).unwrap();
    sync(&mut app, &ctx);
    app.run("typer.page.next", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(names(&app), ["02.png"], "a clean previous page closes without asking");
    // Unsaved changes on page 2: closing it asks, and nothing is closed meanwhile.
    app.run("edit.fill", json!({"color": [1.0, 0.0, 0.0]})).ok();
    if !app.session.active().is_some_and(|d| d.is_dirty()) {
        app.session.active_mut().unwrap().revision += 1;
    }
    app.run("typer.page.next", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(names(&app), ["02.png", "03.png"]);
    assert!(app.discard.is_some(), "the Save / Don't Save / Cancel prompt is up");
    // While it is up, the sync waits.
    app.run("typer.page.previous", json!({})).unwrap();
    sync(&mut app, &ctx);
    assert_eq!(app.session.typer.state.last_opened_image.as_deref(), Some("03.png"));
}

#[test]
fn contents_past_the_cap_are_not_kept_and_missing_files_are_reported() {
    let mut app = app();
    let ctx = egui::Context::default();
    let picked = ["p1.jpg", "p2.jpg", "p3.jpg"];
    set_files_capped(&mut app, picked.iter().map(|s| s.to_string()).collect(), files(&picked), 25).unwrap();
    assert_eq!(app.ui.typer.page_files.total(), 20, "the first pages fit");
    assert!(app.ui.typer.message.contains("(1)"), "{}", app.ui.typer.message);
    assert!(app.ui.typer.page_files.available("p2.jpg") && !app.ui.typer.page_files.available("p3.jpg"));
    app.run("typer.page.next", json!({})).unwrap();
    app.run("typer.page.next", json!({})).unwrap();
    app.ui.typer.message.clear();
    sync(&mut app, &ctx);
    assert!(names(&app).is_empty());
    assert!(app.ui.typer.message.starts_with("p3.jpg: "), "{}", app.ui.typer.message);
    app.ui.typer.message.clear();
    sync(&mut app, &ctx);
    assert!(app.ui.typer.message.is_empty(), "reported once, not every frame");
    // Nothing that is a page: an error, the old files stay.
    assert!(set_files(&mut app, vec!["a.txt".into()], Vec::new()).is_err());
    assert_eq!(app.session.typer.state.images.len(), 3);
    assert!(is_page_file("X.PSD") && is_page_file("a.tiff") && !is_page_file("psd"));
}
