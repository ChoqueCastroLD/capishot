//! The Navigator's image follows edits by their area, even an edit made of several revisions, and
//! a whole new image (a history jump) is drawn a band per frame without blocking one.

use serde_json::json;

use crate::PhotocraftApp;

fn app() -> (PhotocraftApp, egui::Context) {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1200, "height": 1800, "background": "white"})).unwrap();
    for i in 0..6 {
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("select.rect", json!({"x": 100 * i, "y": 150 * i, "width": 300, "height": 300})).unwrap();
        app.run("edit.fill", json!({"color": "#336699"})).unwrap();
        app.run("select.deselect", json!({})).unwrap();
    }
    app.sync_views();
    let ctx = egui::Context::default();
    (app, ctx)
}

#[test]
fn an_edit_of_several_revisions_updates_only_its_area() {
    let (mut app, ctx) = app();
    super::navigator_texture(&mut app, &ctx, 0).unwrap();
    let full = app.perf_counters.navigator_full;
    let rev = app.session.active().unwrap().revision;
    // Three commands (three revisions) before the Navigator draws again, as a Move commits.
    app.run("select.rect", json!({"x": 10, "y": 10, "width": 40, "height": 40})).unwrap();
    app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    app.run("layer.translate", json!({"dx": 5, "dy": 0})).unwrap();
    app.sync_views();
    assert!(app.session.active().unwrap().revision >= rev + 3);
    let n = app.perf_counters.navigator;
    super::navigator_texture(&mut app, &ctx, 0).unwrap();
    assert_eq!(app.perf_counters.navigator, n + 1, "updated");
    assert_eq!(app.perf_counters.navigator_full, full, "by the edits' area, not a whole new image");
}

#[test]
fn a_history_jump_redraws_in_bands_within_the_frame_budget() {
    let (mut app, ctx) = app();
    super::navigator_texture(&mut app, &ctx, 0).unwrap();
    let full = app.perf_counters.navigator_full;
    // An edit whose area isn't known (several revisions in one step): a whole new image.
    app.run("select.rect", json!({"x": 10, "y": 10, "width": 40, "height": 40})).unwrap();
    app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    app.sync_views();
    let id = app.session.active().unwrap().doc.id;
    app.navigator_damage.get_mut(&id).unwrap().area = None;
    let mut frames = 0;
    loop {
        let t0 = std::time::Instant::now();
        super::navigator_texture(&mut app, &ctx, 0).unwrap();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        frames += 1;
        // A debug build is slower than the budget's aim; a band never takes a whole page's time.
        assert!(ms < 400.0, "frame {frames} took {ms:.0} ms");
        if app.navigator_bands.is_empty() {
            break;
        }
        assert!(frames < 400, "the image finishes");
    }
    assert_eq!(app.perf_counters.navigator_full, full + 1, "one whole new image");
    assert!(frames > 1, "drawn over several frames");
}
