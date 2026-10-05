use egui::vec2;
use egui_kittest::Harness;
use photocraft_engine::paint::{Control, GrayTile, MaskMode, Pattern, PatternStyle, TipShape};

use super::*;

fn app() -> PhotocraftApp {
    PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
}

fn last_journal(app: &PhotocraftApp) -> Option<(String, Value)> {
    app.session.journal.last().cloned()
}

#[test]
fn sections_with_boxes_have_flags() {
    let mut b = BrushSettings::default();
    for (i, (_, has_box)) in SECTIONS.iter().enumerate() {
        assert_eq!(section_flag(&mut b, i).is_some(), *has_box, "{i}");
    }
    *section_flag(&mut b, 3).unwrap() = true;
    assert!(b.texture.enabled);
}

/// What each section's controls edit, set to non-default values.
fn edit_section(i: usize, b: &mut BrushSettings) {
    let dynamic = |jitter: f32, control: Control, minimum: f32| paint::Dynamic { jitter, control, fade_steps: 40, minimum };
    match i {
        0 => {
            b.size = 87.0;
            b.flip_x = true;
            b.flip_y = true;
            b.angle = -35.0;
            b.roundness = 0.4;
            b.hardness = 0.25;
            b.spacing = 0.6;
        }
        1 => {
            let sd = &mut b.shape_dynamics;
            sd.enabled = true;
            sd.size = dynamic(0.3, Control::Fade, 0.2);
            sd.angle = dynamic(0.1, Control::Direction, 0.0);
            sd.roundness = dynamic(0.5, Control::PenTilt, 0.25);
            sd.flip_x_jitter = true;
            sd.flip_y_jitter = true;
        }
        2 => {
            let sc = &mut b.scattering;
            sc.enabled = true;
            sc.scatter = dynamic(2.5, Control::PenPressure, 0.0);
            sc.both_axes = true;
            sc.count = 4;
            sc.count_jitter = dynamic(0.4, Control::StylusWheel, 0.0);
        }
        3 => {
            let tx = &mut b.texture;
            tx.enabled = true;
            tx.pattern = Pattern::Procedural { style: PatternStyle::Canvas, size: 200, seed: 1 };
            tx.invert = true;
            tx.scale = 1.5;
            tx.brightness = -0.2;
            tx.contrast = 0.6;
            tx.each_tip = true;
            tx.mode = MaskMode::HardMix;
            tx.depth = 0.7;
            tx.depth_jitter = dynamic(0.3, Control::Rotation, 0.1);
        }
        4 => {
            let d = &mut b.dual_brush;
            d.enabled = true;
            d.mode = MaskMode::ColorBurn;
            d.flip = true;
            d.tip = TipShape::Sampled(GrayTile::from_fn(8, 6, |x, y| ((x + y) % 2) as f32));
            d.size = 33.0;
            d.hardness = 0.5;
            d.spacing = 0.8;
            d.scatter = 1.2;
            d.both_axes = true;
            d.count = 3;
        }
        5 => {
            let c = &mut b.color_dynamics;
            c.enabled = true;
            c.per_tip = false;
            c.fg_bg = dynamic(0.6, Control::Fade, 0.0);
            c.hue_jitter = 0.1;
            c.saturation_jitter = 0.2;
            c.brightness_jitter = 0.3;
            c.purity = -0.4;
        }
        6 => {
            b.transfer.enabled = true;
            b.transfer.opacity = dynamic(0.5, Control::PenPressure, 0.1);
            b.transfer.flow = dynamic(0.25, Control::Fade, 0.3);
        }
        7 => {
            let p = &mut b.pose;
            p.enabled = true;
            p.tilt_x = 45.0;
            p.tilt_y = -27.0;
            p.override_tilt = true;
            p.rotation = 120.0;
            p.override_rotation = true;
            p.pressure = 0.4;
            p.override_pressure = true;
        }
        8 => b.noise = true,
        9 => b.wet_edges = true,
        10 => {
            b.build_up = true;
            b.build_up_rate = 55.0;
        }
        11 => {
            let s = &mut b.smoothing;
            s.amount = 0.45;
            s.pulled_string = true;
            s.catch_up = false;
            s.catch_up_on_end = false;
            s.adjust_for_zoom = false;
        }
        _ => b.protect_texture = true,
    }
}

#[test]
fn every_section_round_trips_through_set_brush() {
    for (i, (name, _)) in SECTIONS.iter().enumerate() {
        let mut app = app();
        let before = app.session.tools.brush.clone();
        let mut after = before.clone();
        edit_section(i, &mut after);
        assert_ne!(before, after, "section {i} edits nothing");
        commit(&mut app, &before, &after);
        assert_eq!(app.session.tools.brush, after, "section {i} ({name})");
        // Through the command, with only the changed fields.
        let (id, p) = last_journal(&app).expect("journaled");
        assert_eq!(id, "tools.setBrush");
        let patch = &p["brush"];
        assert!(patch.as_object().is_some_and(|o| !o.is_empty() && o.len() <= 7), "section {i}: {patch}");
        // And the engine's view (`brush.get`) reads back the same settings.
        let got = app.session.execute("brush.get", json!({})).unwrap();
        assert_eq!(serde_json::from_value::<BrushSettings>(got).unwrap(), after, "section {i}");
        // Replaying the journaled call onto a fresh session gives the same brush (drivable).
        let mut s = photocraft_engine::Session::new();
        s.execute(&id, p).unwrap();
        assert_eq!(s.tools.brush, after, "replay of section {i}");
    }
}

#[test]
fn patches_are_minimal_and_skip_unchanged_bitmaps() {
    let tip = TipShape::Sampled(GrayTile::from_fn(400, 300, |x, y| ((x * y) % 7) as f32 / 7.0));
    let old = BrushSettings {
        tip: tip.clone(),
        texture: paint::Texture { pattern: Pattern::Tile(GrayTile::from_fn(64, 64, |x, _| x as f32 / 64.0)), ..Default::default() },
        ..Default::default()
    };
    let new = BrushSettings { size: 44.0, ..old.clone() };
    assert_eq!(brush_patch(&old, &new), json!({"size": 44.0}));
    let new = BrushSettings { shape_dynamics: paint::ShapeDynamics { enabled: true, ..Default::default() }, ..old.clone() };
    assert_eq!(brush_patch(&old, &new), json!({"shapeDynamics": {"enabled": true}}));
    assert_eq!(brush_patch(&old, &old), json!({}));
    // A changed tip is sent whole.
    let new = BrushSettings { tip: TipShape::Round, ..old.clone() };
    assert_eq!(brush_patch(&old, &new), json!({"tip": "round"}));
    // Applying any of these through the engine gives back `new`.
    let mut app = app();
    app.session.tools.brush = old.clone();
    let new = BrushSettings { size: 9.0, roundness: 0.5, ..old.clone() };
    commit(&mut app, &old, &new);
    assert_eq!(app.session.tools.brush, new);
    // No change, no command.
    let n = app.session.journal.len();
    commit(&mut app, &new, &new);
    assert_eq!(app.session.journal.len(), n);
}

#[test]
fn bad_patches_fail_without_touching_the_brush() {
    let mut app = app();
    let before = app.session.tools.brush.clone();
    for bad in [
        json!({"brush": {"size": "huge"}}),
        json!({"brush": {"shapeDynamics": {"size": {"control": "telepathy"}}}}),
        json!({"brush": {"tip": {"sampled": {"width": 0}}}}),
    ] {
        assert!(app.run("tools.setBrush", bad).is_err());
        assert_eq!(app.session.tools.brush, before);
    }
}

fn harness(tab: usize, section: usize) -> Harness<'static, PhotocraftApp> {
    let mut app = app();
    app.ui.panels.brush_settings = true;
    app.ui.brush_tab = tab;
    app.ui.brush_section = section;
    let mut h = Harness::builder().with_size(vec2(1200.0, 900.0)).build_ui_state(
        |ui, app: &mut PhotocraftApp| {
            let ctx = ui.ctx().clone();
            if !ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                return;
            }
            window(app, &ctx);
        },
        app,
    );
    PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
    h.run_steps(4);
    h
}

#[test]
fn preview_strip_re_renders_only_when_the_brush_changes() {
    let mut h = harness(0, 0);
    let ctx = h.ctx.clone();
    let n0 = brush_preview::render_count(&ctx);
    assert!(n0 >= 1, "the strip rendered");
    h.run_steps(6);
    assert_eq!(brush_preview::render_count(&ctx), n0, "idle frames re-rendered a preview");
    // Change the brush through the engine: the strip renders once more, then stays cached.
    h.state_mut().run("tools.setBrush", json!({"brush": {"scattering": {"enabled": true, "scatter": {"jitter": 2.0}}}})).unwrap();
    h.run_steps(5);
    assert_eq!(brush_preview::render_count(&ctx), n0 + 1);
    // Colour and the jitter seed of strokes aren't preview inputs: no re-render.
    h.state_mut().session.tools.brush.color = [1.0, 0.0, 0.0, 1.0];
    h.run_steps(3);
    assert_eq!(brush_preview::render_count(&ctx), n0 + 1);
}

#[test]
fn every_section_draws_without_issuing_commands() {
    for tab in 0..2 {
        for section in 0..SECTIONS.len() {
            let mut h = harness(tab, section);
            // A brush with every section on and a sampled tip.
            let mut b = BrushSettings::default();
            for i in 0..SECTIONS.len() {
                edit_section(i, &mut b);
            }
            b.tip = TipShape::Sampled(GrayTile::from_fn(50, 40, |x, y| ((x ^ y) & 1) as f32));
            h.state_mut().session.tools.brush = b.clone();
            let n = h.state().session.journal.len();
            h.run_steps(4);
            // Merely showing the panel must not rewrite the brush (no rounding drift, no commands).
            assert_eq!(h.state().session.journal.len(), n, "tab {tab} section {section}");
            assert_eq!(h.state().session.tools.brush, b, "tab {tab} section {section}");
        }
    }
}

#[test]
fn brushes_panel_groups_collapse_and_filter() {
    let mut h = harness(1, 0);
    let groups = grouped_presets(&h.state().session.tools.presets);
    assert!(groups.len() >= 2, "{groups:?}");
    let ctx = h.ctx.clone();
    let shown = |h: &mut Harness<'static, PhotocraftApp>| {
        h.run_steps(2);
        brush_preview::with_cache(&ctx, |c| c.len())
    };
    let all = shown(&mut h);
    // Collapse every group: no more previews are needed, and the old ones are kept (cached).
    h.state_mut().ui.brushes_panel.collapsed = groups.iter().map(|(g, _)| g.clone()).collect();
    let n = brush_preview::render_count(&ctx);
    assert_eq!(shown(&mut h), all);
    assert_eq!(brush_preview::render_count(&ctx), n);
    // A filter shows matching presets even in collapsed groups.
    let first = h.state().session.tools.presets[0].name.clone();
    h.state_mut().ui.brushes_panel.filter = first.to_uppercase();
    h.run_steps(2);
    assert!(brush_preview::render_count(&ctx) >= n);
}
