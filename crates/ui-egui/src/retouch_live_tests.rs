//! Every brush-based retouching tool shows its stroke live on the canvas (TOOL-213-7), with the
//! options-bar settings its commit uses, and the options bars' Photoshop settings reach the
//! commands (Mode, Sample Size, Erase to History, Eraser Mode).

use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::live_doc;
use crate::canvas::{ToolEvent, tool_event};
use crate::state::Tool;

fn textured() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 160, "height": 80, "depth": 32})).unwrap();
    // Some texture to retouch.
    app.session
        .edit("setup", |doc, active| {
            let b = doc.bounds();
            let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
            let fmt = surf.format();
            let mut data = Vec::new();
            for y in b.y0..b.y1 {
                for x in b.x0..b.x1 {
                    let v = 0.3 + 0.4 * (((x / 4 + y / 4) % 2) as f32);
                    data.extend(photocraft_raster::from_rgba(&fmt, [v, v * 0.8, 0.5, 1.0]));
                }
            }
            surf.write_region(b, &data);
            Ok(())
        })
        .unwrap();
    app.run("tools.setBrush", json!({"brush": {"size": 16, "hardness": 0.8}})).unwrap();
    app.sync_views();
    app
}

fn drag(app: &mut PhotocraftApp, from: [f64; 2], to: [f64; 2], steps: usize, before_up: impl FnOnce(&mut PhotocraftApp)) {
    tool_event(app, ToolEvent::Down { x: from[0], y: from[1], pressure: 1.0 }, egui::Modifiers::NONE);
    for i in 1..=steps {
        let t = i as f64 / steps as f64;
        tool_event(app, ToolEvent::Move { x: from[0] + (to[0] - from[0]) * t, y: from[1] + (to[1] - from[1]) * t, pressure: 1.0 }, egui::Modifiers::NONE);
    }
    before_up(app);
    tool_event(app, ToolEvent::Up { x: to[0], y: to[1] }, egui::Modifiers::NONE);
}

fn layer_px(doc: &photocraft_doc::Document, x: i32, y: i32) -> [f32; 4] {
    doc.layers[0].surface().unwrap().rgba(x, y)
}

#[test]
fn every_brush_retouch_tool_previews_its_stroke_live() {
    type Setup = Box<dyn Fn(&mut PhotocraftApp)>;
    let setups: Vec<(Tool, Setup)> = vec![
        (Tool::CloneStamp, Box::new(|a| crate::retouch_ui::set_source(a, 20.0, 20.0))),
        (Tool::Healing, Box::new(|a| crate::retouch_ui::set_source(a, 20.0, 20.0))),
        (Tool::HistoryBrush, Box::new(|_| {})),
        (Tool::PatternStamp, Box::new(|_| {})),
        (Tool::PatternStamp, Box::new(|a| a.ui.tool_options.pattern_stamp_impressionist = true)),
        (Tool::Dodge, Box::new(|_| {})),
        (Tool::Burn, Box::new(|_| {})),
        (Tool::Sponge, Box::new(|_| {})),
        (Tool::Blur, Box::new(|_| {})),
        (Tool::Sharpen, Box::new(|_| {})),
        (Tool::Smudge, Box::new(|_| {})),
    ];
    for (i, (tool, setup)) in setups.iter().enumerate() {
        let mut app = textured();
        if *tool == Tool::HistoryBrush {
            app.run("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        app.ui.tool = *tool;
        setup(&mut app);
        let rev = app.session.active().unwrap().revision;
        let mut shown = None;
        drag(&mut app, [60.0, 40.0], [130.0, 44.0], 20, |a| shown = live_doc(a));
        let shown = shown.unwrap_or_else(|| panic!("#{i} {tool:?}: no live stroke while dragging"));
        let st = app.session.active().unwrap();
        assert!(st.revision > rev, "#{i} {tool:?}: committed");
        // What was shown is what was committed (the last dab lands on release; healing's live
        // solve is incremental, so it is close rather than equal).
        let tol = if *tool == Tool::Healing { 0.08 } else { 2e-3 };
        let mut changed = false;
        for y in 30..54 {
            for x in 50..118 {
                let (a, b) = (layer_px(&shown, x, y), layer_px(&st.doc, x, y));
                changed |= (0..3).any(|c| (a[c] - layer_px(&st.history.state(0).unwrap(), x, y)[c]).abs() > 0.01);
                for c in 0..4 {
                    assert!((a[c] - b[c]).abs() <= tol, "#{i} {tool:?} at ({x}, {y}): shown {a:?}, committed {b:?}");
                }
            }
        }
        assert!(changed, "#{i} {tool:?}: the live stroke showed a change");
    }
}

#[test]
fn spot_healing_shows_a_dark_footprint_and_heals_on_release() {
    let mut app = textured();
    app.ui.tool = Tool::SpotHealing;
    app.ui.tool_options.spot_mode = "replace".into();
    let rev = app.session.active().unwrap().revision;
    let mut live = true;
    drag(&mut app, [60.0, 40.0], [90.0, 40.0], 8, |a| live = live_doc(a).is_some());
    assert!(!live, "Spot Healing heals the whole stroke on release, like Photoshop");
    assert!(app.session.active().unwrap().revision > rev, "Replace mode reaches the command: {}", app.ui.status);
    assert!(!app.ui.status_error, "{}", app.ui.status);
}

#[test]
fn stamp_and_healing_modes_reach_their_commands() {
    let mut app = textured();
    app.ui.tool = Tool::CloneStamp;
    crate::retouch_ui::set_source(&mut app, 20.0, 20.0);
    crate::paint_mouse::sync_tool_brush(&mut app);
    app.run("tools.setBrush", json!({"brush": {"mode": "Darken"}})).unwrap();
    let p = crate::retouch_ui::clone_params(&app).unwrap();
    assert_eq!(p["mode"], json!("Darken"));
    app.ui.tool = Tool::Healing;
    crate::paint_mouse::sync_tool_brush(&mut app);
    app.run("tools.setBrush", json!({"brush": {"mode": "Luminosity"}})).unwrap();
    assert_eq!(crate::retouch_ui::clone_params(&app).unwrap()["mode"], json!("Luminosity"));
    drag(&mut app, [60.0, 40.0], [100.0, 40.0], 6, |_| {});
    assert!(!app.ui.status_error, "{}", app.ui.status);
    app.ui.tool = Tool::PatternStamp;
    crate::paint_mouse::sync_tool_brush(&mut app);
    app.run("tools.setBrush", json!({"brush": {"mode": "Multiply", "opacity": 0.5, "flow": 0.7}})).unwrap();
    let p = crate::retouch_ui::pattern_stamp_params(&app, &[[10.0, 10.0, 1.0]]);
    assert_eq!((p["mode"].clone(), p["opacity"].as_f64().map(f64::round), p["flow"].as_f64().map(f64::round)), (json!("Multiply"), Some(50.0), Some(70.0)));
    // Impressionist's seed follows the stroke's first point, for the preview and the commit.
    app.ui.tool_options.pattern_stamp_impressionist = true;
    let a = crate::retouch_ui::pattern_stamp_params(&app, &[[10.0, 10.0, 1.0]]);
    assert_eq!(a["seed"], crate::retouch_ui::pattern_stamp_params(&app, &[[10.0, 10.0, 0.5]])["seed"]);
    assert_ne!(a["seed"], crate::retouch_ui::pattern_stamp_params(&app, &[[11.0, 10.0, 1.0]])["seed"]);
}

#[test]
fn the_eraser_erases_to_history_live_and_in_pencil_mode_hard() {
    let mut app = textured();
    // The History Brush's default state: the document as opened (white from File › New).
    let original = layer_px(&app.session.active().unwrap().history.state(0).unwrap(), 80, 40);
    app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    app.ui.tool = Tool::Eraser;
    app.ui.tool_options.erase_to_history = true;
    let mut shown = None;
    drag(&mut app, [60.0, 40.0], [100.0, 40.0], 10, |a| shown = live_doc(a));
    let shown = shown.expect("Erase to History shows live");
    let doc = &app.session.active().unwrap().doc;
    for got in [layer_px(&shown, 80, 40), layer_px(doc, 80, 40)] {
        assert!((0..3).all(|c| (got[c] - original[c]).abs() < 0.01), "restored the opening state: {got:?} want {original:?}");
    }
    assert_eq!(layer_px(doc, 80, 70), [1.0, 0.0, 0.0, 1.0], "outside the stroke");
    // Pencil mode: an aliased eraser on a layer, every pixel fully erased or untouched.
    let mut app = textured();
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("edit.fill", json!({"color": "#00ff00"})).unwrap();
    app.run("tools.setBrush", json!({"brush": {"size": 15, "hardness": 0.0}})).unwrap();
    app.ui.tool = Tool::Eraser;
    app.ui.tool_options.eraser_mode = "pencil".into();
    drag(&mut app, [60.0, 40.0], [100.0, 40.0], 10, |_| {});
    let st = app.session.active().unwrap();
    let top = st.doc.layers.last().unwrap().surface().unwrap();
    let alphas: Vec<f32> = (25..56).map(|y| top.rgba(80, y)[3]).collect();
    assert!(alphas.iter().all(|a| *a == 0.0 || *a == 1.0), "aliased: {alphas:?}");
    assert!(alphas.contains(&0.0), "erased");
}

#[test]
fn magic_wand_and_bucket_send_their_options() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 40, "height": 40, "background": "white"})).unwrap();
    app.run("select.rect", json!({"x": 10, "y": 10, "width": 10, "height": 10})).unwrap();
    app.run("edit.fill", json!({"color": "#ff0000"})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
    app.sync_views();
    // A 31 × 31 average at the red square's corner is mostly white: the white is selected.
    app.ui.tool = Tool::MagicWand;
    app.ui.tool_options.wand_sample_size = 31;
    app.ui.tool_options.anti_alias = false;
    app.ui.tool_options.tolerance = 50.0;
    tool_event(&mut app, ToolEvent::Down { x: 10.5, y: 10.5, pressure: 1.0 }, egui::Modifiers::NONE);
    tool_event(&mut app, ToolEvent::Up { x: 10.5, y: 10.5 }, egui::Modifiers::NONE);
    let sel = app.session.active().unwrap().doc.selection.clone().expect("selected");
    assert_eq!((sel.sample_channel(2, 2, 0), sel.sample_channel(15, 15, 0)), (1.0, 0.0));
    app.run("select.deselect", json!({})).unwrap();
    app.ui.tool_options.tolerance = 32.0;
    // Paint Bucket in Multiply: grey over red keeps red's hue, darker.
    app.ui.tool = Tool::PaintBucket;
    app.ui.tool_options.bucket_blend_mode = photocraft_color::BlendMode::Multiply;
    app.run("tools.setColors", json!({"foreground": [0.5, 0.5, 0.5, 1.0]})).unwrap();
    tool_event(&mut app, ToolEvent::Down { x: 15.0, y: 15.0, pressure: 1.0 }, egui::Modifiers::NONE);
    tool_event(&mut app, ToolEvent::Up { x: 15.0, y: 15.0 }, egui::Modifiers::NONE);
    let px = app.session.active().unwrap().doc.layers[0].surface().unwrap().rgba(15, 15);
    assert!((px[0] - 0.5).abs() < 0.02 && px[1] < 0.01, "multiplied: {px:?}");
}
