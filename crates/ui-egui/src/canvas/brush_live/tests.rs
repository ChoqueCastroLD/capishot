//! A Brush stroke through the real canvas with its dabs rendered by a job, as in the app.

use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;

/// A 1600x1200 document at 50% with a soft 600 px Brush (a dab of about a megapixel), strokes
/// rendered off the frame.
fn harness() -> Harness<'static, PhotocraftApp> {
    super::set_off_frame(true);
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1600, "height": 1200, "background": "white"})).unwrap();
    app.sync_views();
    app.ui.tool = Tool::Brush;
    app.session.tools.brush.size = 600.0;
    app.session.tools.brush.hardness = 0.0;
    let mut h = Harness::builder().with_size(vec2(1000.0, 800.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            // (What the app's frame does before its panels.)
            crate::canvas::land_stroke_in_frame(app, &ctx);
            egui::CentralPanel::default().show(ui, |ui| crate::canvas::document_area(app, ui));
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    let v = &mut h.state_mut().ui.views[0];
    v.zoom = 0.5;
    v.center = [800.0, 600.0];
    v.fit_pending = false;
    h.run_steps(2);
    h
}

fn screen(h: &Harness<'static, PhotocraftApp>, x: f32, y: f32) -> Pos2 {
    let app = h.state();
    let v = &app.ui.views[0];
    let xf = ViewXform {
        rect: crate::rulers::content_rect(app, app.last_canvas_rect),
        zoom: v.zoom,
        center: v.center,
        flip: app.ui.view.flip_horizontal,
        rotation: v.rotation,
        aspect: app.ui.view.display_aspect(),
    };
    xf.to_screen(x, y)
}

fn button(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32, pressed: bool) {
    let pos = screen(h, x, y);
    h.event(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

fn move_to(h: &mut Harness<'static, PhotocraftApp>, x: f32, y: f32) {
    let p = screen(h, x, y);
    h.event(egui::Event::PointerMoved(p));
}

/// The active document's pixels.
fn pixels(app: &PhotocraftApp) -> Vec<[f32; 4]> {
    let doc = &app.session.active().expect("doc").doc;
    photocraft_compose::flatten(doc).px
}

fn strokes(app: &PhotocraftApp) -> usize {
    app.session.journal.iter().filter(|(id, _)| id == "paint.stroke").count()
}

/// Frames until the released stroke is committed.
fn land(h: &mut Harness<'static, PhotocraftApp>) {
    for _ in 0..2000 {
        if h.state().landing_stroke.is_none() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        h.run_steps(1);
    }
    panic!("the released stroke never landed");
}

/// A drag across the page, a move a frame; the release comes with a last move in its frame.
fn drag(h: &mut Harness<'static, PhotocraftApp>) {
    move_to(h, 300.0, 400.0);
    h.run_steps(1);
    button(h, 300.0, 400.0, true);
    h.run_steps(1);
    for i in 1..=6 {
        move_to(h, 300.0 + 150.0 * i as f32, 400.0 + 40.0 * i as f32);
        h.run_steps(1);
    }
    move_to(h, 1400.0, 800.0);
    button(h, 1400.0, 800.0, false);
    h.run_steps(1);
}

#[test]
fn the_dabs_are_rendered_off_the_frame_and_the_stroke_is_committed_whole() {
    let mut h = harness();
    let (before, blank) = (photocraft_engine::paint::dabs_here(), pixels(h.state()));
    drag(&mut h);
    land(&mut h);
    assert_eq!(photocraft_engine::paint::dabs_here(), before, "no frame rendered a dab on the window's thread");
    let app = h.state_mut();
    assert_eq!(strokes(app), 1, "the stroke is committed once");
    assert!(app.committed_stroke.is_some(), "the commit took the pixels the job rendered");
    let painted = pixels(app);
    assert_ne!(painted, blank, "the stroke painted");
    // The same stroke rendered by its command alone gives the same pixels.
    let (_, params) = app.session.journal.iter().rev().find(|(id, _)| id == "paint.stroke").cloned().expect("the stroke");
    app.run("edit.undo", json!({})).unwrap();
    assert_eq!(pixels(app), blank);
    app.run("paint.stroke", params).unwrap();
    assert!(photocraft_engine::paint::dabs_here() > before, "the command rendered it here");
    assert!(pixels(app) == painted, "every dab landed, in the order it was drawn");
}

#[test]
fn a_stroke_still_landing_at_release_is_shown_until_it_is_committed() {
    let mut h = harness();
    drag(&mut h);
    let app = h.state_mut();
    assert!(app.landing_stroke.is_some(), "the release didn't wait for the last megapixel dabs");
    assert_eq!(strokes(app), 0);
    let idx = app.session.active_index().expect("doc");
    let (shown, key) = crate::canvas::display_doc(app, idx);
    assert_ne!(key, 0, "the canvas keeps showing the stroke");
    assert!(!std::sync::Arc::ptr_eq(&shown, &app.session.active().expect("doc").doc));
    land(&mut h);
    assert_eq!(strokes(h.state()), 1);
}

#[test]
fn a_command_after_the_release_runs_after_the_stroke() {
    let mut h = harness();
    drag(&mut h);
    let app = h.state_mut();
    assert!(app.landing_stroke.is_some());
    app.run("layer.new.layer", json!({"name": "after"})).unwrap();
    assert!(app.landing_stroke.is_none());
    let ids: Vec<&str> = app.session.journal.iter().map(|(id, _)| id.as_str()).filter(|id| matches!(*id, "paint.stroke" | "layer.new.layer")).collect();
    assert_eq!(ids, ["paint.stroke", "layer.new.layer"], "the stroke was committed first");
}

#[test]
fn a_press_after_the_release_starts_on_the_committed_stroke() {
    let mut h = harness();
    drag(&mut h);
    assert!(h.state().landing_stroke.is_some());
    button(&mut h, 500.0, 900.0, true);
    h.run_steps(1);
    assert_eq!(strokes(h.state()), 1, "the first stroke was committed before the second began");
    button(&mut h, 500.0, 900.0, false);
    h.run_steps(1);
    land(&mut h);
    assert_eq!(strokes(h.state()), 2);
}
