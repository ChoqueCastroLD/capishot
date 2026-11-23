//! Strokes through the real canvas with their dabs rendered by a job, as in the app: the Brush's,
//! the retouching tools' and the Mixer Brush's.

use egui::{Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::state::Tool;

/// A 1600x1200 document at 50% with a soft 600 px Brush (a dab of about a megapixel), strokes
/// rendered off the frame.
fn harness() -> Harness<'static, PhotocraftApp> {
    harness_with(Tool::Brush, 600.0, true)
}

/// A 1600x1200 document at 50% with `tool` in hand and a soft tip of `size` px, strokes rendered
/// off the frame (`off_frame`, as in the app) or in it. The page has paint for the retouching
/// tools to work on, and a clone source.
fn harness_with(tool: Tool, size: f32, off_frame: bool) -> Harness<'static, PhotocraftApp> {
    super::set_off_frame(off_frame);
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1600, "height": 1200, "background": "white"})).unwrap();
    if tool != Tool::Brush {
        for (i, color) in ["#c03020", "#2060c0", "#30a040", "#101010"].into_iter().enumerate() {
            let y = 250 + 200 * i;
            app.run("paint.stroke", json!({"points": [[100, y], [800, y + 150], [1500, y - 100]], "size": 90, "hardness": 0.6, "color": color})).unwrap();
        }
        app.ui.clone_source = Some([900.0, 300.0]);
    }
    app.sync_views();
    app.ui.tool = tool;
    app.session.tools.brush.size = size;
    app.session.tools.brush.hardness = 0.0;
    let mut h = Harness::builder().with_size(vec2(1000.0, 800.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            // (What the app's frame does before its panels.)
            crate::canvas::land_stroke_in_frame(app, &ctx);
            crate::jobs_ui::tick(app, &ctx);
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

/// The last command of `id` the app ran, with its params.
fn last(app: &PhotocraftApp, id: &str) -> Option<serde_json::Value> {
    app.session.journal.iter().rev().find(|(c, _)| c == id).map(|(_, p)| p.clone())
}

/// Frames until the released stroke is committed (and its command's job, if it is one, applied).
fn land(h: &mut Harness<'static, PhotocraftApp>) {
    for _ in 0..5000 {
        if h.state().landing_stroke.is_none() && !h.state().session.has_jobs() {
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

/// The tools whose live stroke draws what its command paints, with the command.
const OFFERED: [(Tool, &str); 8] = [
    (Tool::CloneStamp, "paint.cloneStamp"),
    (Tool::Dodge, "paint.dodge"),
    (Tool::Burn, "paint.burn"),
    (Tool::Sponge, "paint.sponge"),
    (Tool::Blur, "paint.blur"),
    (Tool::Sharpen, "paint.sharpen"),
    (Tool::Smudge, "paint.smudge"),
    (Tool::MixerBrush, "paint.mixerBrush"),
];

#[test]
fn retouch_strokes_are_rendered_off_the_frame_with_the_pixels_of_the_frame() {
    for (tool, cmd) in OFFERED {
        // Rendered in the frame, as before: what the stroke must be.
        let mut inline = harness_with(tool, 300.0, false);
        let here = photocraft_engine::paint::dabs_here();
        drag(&mut inline);
        assert!(inline.state().landing_stroke.is_none(), "{cmd}: rendered in the frame, the release commits");
        assert!(photocraft_engine::paint::dabs_here() > here, "{cmd}: its frames rendered the dabs");
        let want = pixels(inline.state());
        let params = last(inline.state(), cmd).unwrap_or_else(|| panic!("{cmd}: committed"));

        let mut h = harness_with(tool, 300.0, true);
        let (before, untouched) = (photocraft_engine::paint::dabs_here(), pixels(h.state()));
        assert_ne!(want, untouched, "{cmd}: the stroke changes pixels");
        drag(&mut h);
        land(&mut h);
        assert_eq!(photocraft_engine::paint::dabs_here(), before, "{cmd}: no frame rendered a dab on the window's thread");
        let app = h.state();
        assert_eq!(last(app, cmd), Some(params), "{cmd}: the same command, committed once the stroke landed");
        assert_eq!(app.session.journal.iter().filter(|(id, _)| id == cmd).count(), 1, "{cmd}");
        assert!(app.committed_stroke.is_some(), "{cmd}: the commit took the pixels the job rendered");
        assert!(pixels(app) == want, "{cmd}: every dab landed, in the order it was drawn");
    }
}

#[test]
fn a_command_or_a_press_after_a_retouch_release_comes_after_the_stroke() {
    let mut h = harness_with(Tool::Dodge, 600.0, true);
    drag(&mut h);
    let app = h.state_mut();
    assert!(app.landing_stroke.is_some(), "the release didn't wait for the last dabs");
    assert!(last(app, "paint.dodge").is_none());
    let idx = app.session.active_index().expect("doc");
    assert_ne!(crate::canvas::display_doc(app, idx).1, 0, "the canvas keeps showing the stroke");
    app.run("layer.new.layer", json!({"name": "after"})).unwrap();
    assert!(app.landing_stroke.is_none());
    let ids: Vec<&str> = app.session.journal.iter().map(|(id, _)| id.as_str()).filter(|id| matches!(*id, "paint.dodge" | "layer.new.layer")).collect();
    assert_eq!(ids, ["paint.dodge", "layer.new.layer"], "the stroke was committed first");
    assert!(app.retouch_from_live, "with the pixels the job rendered");

    // A press: the second stroke starts on the first, committed.
    let mut h = harness_with(Tool::Burn, 600.0, true);
    drag(&mut h);
    assert!(h.state().landing_stroke.is_some());
    button(&mut h, 500.0, 900.0, true);
    h.run_steps(1);
    assert!(last(h.state(), "paint.burn").is_some(), "the first stroke was committed before the second began");
    button(&mut h, 500.0, 900.0, false);
    h.run_steps(1);
    land(&mut h);
    assert_eq!(h.state().session.journal.iter().filter(|(id, _)| id == "paint.burn").count(), 2);
}

/// The Healing Brush blends the whole stroke on release: a background job in the app. The canvas
/// shows the stroke as it was drawn until the blend lands, what comes next waits for it, and the
/// result is the one of the command run in the frame.
#[test]
fn a_healing_stroke_is_blended_by_a_job_and_shown_until_it_lands() {
    let mut inline = harness_with(Tool::Healing, 200.0, false);
    drag(&mut inline);
    let (want, offset) = (pixels(inline.state()), inline.state().ui.clone_offset);
    assert!(offset.is_some(), "an aligned stroke leaves its offset");
    assert!(last(inline.state(), "paint.healingBrush").is_some());

    for then_a_command in [false, true] {
        let mut h = harness_with(Tool::Healing, 200.0, true);
        h.state_mut().background_jobs = true;
        let revision = h.state().session.active().expect("doc").revision;
        drag(&mut h);
        let app = h.state_mut();
        assert!(app.session.has_jobs() && app.landing_stroke.is_some(), "the release started the blend and didn't wait for it");
        let idx = app.session.active_index().expect("doc");
        assert_eq!(app.session.active().expect("doc").revision, revision);
        assert_ne!(crate::canvas::display_doc(app, idx).1, 0, "the canvas keeps showing the stroke as it was drawn");
        if then_a_command {
            app.run("layer.new.layer", json!({"name": "after"})).unwrap();
            assert_eq!(app.session.active().expect("doc").revision, revision + 2, "the blend landed, then the layer");
        }
        land(&mut h);
        let app = h.state_mut();
        let idx = app.session.active_index().expect("doc");
        assert_eq!(crate::canvas::display_doc(app, idx).1, 0, "the canvas shows the document");
        assert!(pixels(app) == want, "the blend of the job is the command's");
        assert_eq!(app.ui.clone_offset, offset, "the next aligned stroke samples as after the command");
        if !then_a_command {
            assert!(app.committed_stroke.is_some(), "the canvas repaints where the stroke and the blend drew, not the page");
        }
    }
}
