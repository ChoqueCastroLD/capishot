//! A frame's work is proportional to what changed, not to the document's layer count: the app
//! asks for no frames while idle, the Layers panel lays out only the rows in view, thumbnails are
//! rebuilt only for layers whose pixels changed, and while a tool is dragged on the canvas the
//! chrome around it is repainted from its last shapes instead of being built again
//! (`chrome_freeze`), without changing what the gesture does.

use egui::{Event, Modifiers, PointerButton, Pos2, ViewportId};
use egui_kittest::Harness;
use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerId};
use photocraft_geom::{Rect, Size};
use photocraft_ui_egui::canvas::ViewXform;
use photocraft_ui_egui::state::Tool;
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::json;

type H = Harness<'static, PhotocraftApp>;

/// A 600×400 document with `n` painted layers (a group every 10).
fn doc(n: usize) -> (Document, LayerId) {
    let mut d = Document::with_background("work", Size::new(600, 400), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
    let fmt = d.pixel_format();
    let mut group = Vec::new();
    let mut mid = None;
    for i in 0..n {
        let mut l = Layer::raster(format!("L{i}"), fmt);
        let x = (i * 37 % 500) as i32;
        l.surface_mut().expect("raster").fill_rect(Rect::new(x, 50, x + 80, 300), &[0.1 * (i % 10) as f32, 0.3, 0.5, 1.0]);
        if i == n / 2 {
            mid = Some(l.id);
        }
        group.push(l);
        if group.len() == 9 {
            d.insert_above(None, Layer::group(format!("G{i}"), std::mem::take(&mut group)));
        }
    }
    for l in group {
        d.insert_above(None, l);
    }
    (d, mid.expect("a middle layer"))
}

fn app(n: usize) -> (H, LayerId) {
    let (d, mid) = doc(n);
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_pixels_per_point(1.0).with_max_steps(200).with_step_dt(1.0 / 60.0).build_eframe(
        move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.session.open_document(d, None);
            app.sync_views();
            app
        },
    );
    h.run_steps(8);
    (h, mid)
}

fn screen(h: &H, x: f64, y: f64) -> Pos2 {
    let app = h.state();
    let v = &app.ui.views[0];
    let rect = photocraft_ui_egui::rulers::content_rect(app, app.last_canvas_rect);
    ViewXform { rect, zoom: v.zoom, center: v.center, flip: false, rotation: v.rotation, aspect: 1.0 }.to_screen(x as f32, y as f32)
}

fn repaint_asked(h: &H) -> bool {
    h.output().viewport_output.get(&ViewportId::ROOT).is_some_and(|v| v.repaint_delay < std::time::Duration::from_secs(1))
}

/// Run until the app asks for no more frames (the CPU canvas builds its tiles on worker threads,
/// polling until they land; on a busy machine that can take a while), at most 20 s.
fn settle(h: &mut H) {
    let t = std::time::Instant::now();
    while t.elapsed().as_secs() < 20 {
        let _ = h.try_run_realtime();
        h.step();
        if !repaint_asked(h) {
            break;
        }
    }
}

fn counter(h: &H, name: &str) -> u64 {
    h.state().perf_counters.snapshot().iter().find(|c| c.0 == name).map_or(0, |c| c.1)
}

#[test]
fn an_idle_app_asks_for_no_frames() {
    let (mut h, _) = app(60);
    // Real time: the CPU canvas builds its tiles on worker threads, polling until they land (on a
    // busy machine that can take a while).
    settle(&mut h);
    for _ in 0..20 {
        h.step();
        assert!(!repaint_asked(&h), "idle frame asked for another: {:?}", h.ctx.repaint_causes());
    }
    // A pointer resting on the canvas (Move tool, rollover highlight on) costs no frames either.
    h.state_mut().ui.tool = Tool::Move;
    h.event(Event::PointerMoved(screen(&h, 300.0, 200.0)));
    let _ = h.try_run_realtime();
    for _ in 0..5 {
        h.step();
        assert!(!repaint_asked(&h), "resting pointer asked for frames: {:?}", h.ctx.repaint_causes());
    }
}

#[test]
fn the_layers_panel_lays_out_only_the_rows_in_view() {
    let (mut h, _) = app(200);
    let _ = h.try_run();
    let (drawn, skipped) = (counter(&h, "rows_drawn"), counter(&h, "rows_skipped"));
    h.step();
    let rows = counter(&h, "rows_drawn") - drawn;
    assert!(rows < 60, "a frame laid out {rows} of ~220 rows");
    assert!(counter(&h, "rows_skipped") - skipped > 100, "rows out of view only keep their place");
}

/// A brush stroke over 40 frames: the chrome is repainted from its last shapes on nearly every
/// frame of the drag (no Layers rows laid out, no thumbnails built), the stroke commits as one
/// history step, and the frame after the release builds the chrome again with the new thumbnail.
#[test]
fn a_canvas_drag_repaints_the_chrome_without_building_it() {
    let (mut h, mid) = app(80);
    h.state_mut().run("layer.select", json!({"layer": mid.0})).expect("select");
    h.state_mut().ui.tool = Tool::Brush;
    let _ = h.try_run();
    let steps = h.state().session.active().expect("doc").history.entries().len();
    let p0 = screen(&h, 100.0, 100.0);
    h.event(Event::PointerMoved(p0));
    h.step();
    h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let (reused, rows, thumbs) = (counter(&h, "chrome_reused"), counter(&h, "rows_drawn"), counter(&h, "thumbs"));
    for i in 1..=40 {
        h.event(Event::PointerMoved(screen(&h, 100.0 + f64::from(i) * 8.0, 100.0 + f64::from(i % 7) * 10.0)));
        h.step();
    }
    let reused = counter(&h, "chrome_reused") - reused;
    assert!(reused >= 36, "chrome repainted from shapes on {reused} of 40 drag frames");
    assert!(counter(&h, "rows_drawn") - rows <= 4 * 60, "Layers rows laid out only on the refresh frames");
    assert_eq!(counter(&h, "thumbs"), thumbs, "no thumbnail rebuilt while drawing");
    let p1 = screen(&h, 420.0, 100.0);
    h.event(Event::PointerButton { pos: p1, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    let built = counter(&h, "chrome_reused");
    h.run_steps(3);
    assert_eq!(counter(&h, "chrome_reused"), built, "after the release the chrome is built again");
    assert_eq!(h.state().session.active().expect("doc").history.entries().len(), steps + 1, "one stroke committed");
    assert!(counter(&h, "thumbs") > thumbs, "the painted layer's thumbnail follows the commit");
}

/// Anything but pointer moves during the drag (a key) and the pointer leaving the canvas build
/// the chrome for real.
#[test]
fn input_besides_moves_or_leaving_the_canvas_builds_the_chrome() {
    let (mut h, mid) = app(30);
    h.state_mut().run("layer.select", json!({"layer": mid.0})).expect("select");
    h.state_mut().ui.tool = Tool::Brush;
    let _ = h.try_run();
    let p0 = screen(&h, 100.0, 100.0);
    h.event(Event::PointerMoved(p0));
    h.step();
    h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    h.event(Event::PointerMoved(screen(&h, 120.0, 100.0)));
    h.step();
    h.event(Event::PointerMoved(screen(&h, 130.0, 100.0)));
    h.step();
    let before = counter(&h, "chrome_reused");
    // One frame with a move and a key (`Harness::event` would run them in separate frames).
    let p = screen(&h, 140.0, 100.0);
    h.input_mut().events.push(Event::PointerMoved(p));
    h.input_mut().events.push(Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(counter(&h, "chrome_reused"), before, "a key press builds the chrome");
    h.event(Event::PointerMoved(egui::pos2(1390.0, 400.0)));
    h.step();
    assert_eq!(counter(&h, "chrome_reused"), before, "the pointer over the dock builds the chrome");
    h.event(Event::PointerButton { pos: egui::pos2(1390.0, 400.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

/// The chrome repainted from its shapes looks exactly like the chrome built for real (on the GPU
/// canvas; skipped without a GPU adapter).
#[test]
fn the_repainted_chrome_matches_the_built_one() {
    let (d, mid) = doc(40);
    let built = std::panic::catch_unwind(move || {
        Harness::builder().with_size(egui::vec2(1200.0, 760.0)).with_pixels_per_point(1.0).with_max_steps(200).wgpu().build_eframe(move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
            }
            app.session.open_document(d, None);
            app.sync_views();
            app
        })
    });
    let Ok(mut h) = built else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    h.run_steps(8);
    h.state_mut().run("layer.select", json!({"layer": mid.0})).expect("select");
    h.state_mut().ui.tool = Tool::Brush;
    let _ = h.try_run();
    let p0 = screen(&h, 100.0, 100.0);
    h.event(Event::PointerMoved(p0));
    h.step();
    h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=4 {
        h.event(Event::PointerMoved(screen(&h, 100.0 + f64::from(i) * 5.0, 100.0)));
        h.step();
    }
    let reused = counter(&h, "chrome_reused");
    h.event(Event::PointerMoved(screen(&h, 130.0, 100.0)));
    h.step();
    assert_eq!(counter(&h, "chrome_reused"), reused + 1, "this frame repaints the chrome from its shapes");
    let frozen = h.render().expect("render");
    // Same pointer position, plus a key nothing is bound to: the chrome is built for real.
    h.input_mut().events.push(Event::Key { key: egui::Key::F35, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    h.input_mut().events.push(Event::Key { key: egui::Key::F35, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    h.step();
    assert_eq!(counter(&h, "chrome_reused"), reused + 1, "built");
    let real = h.render().expect("render");
    let canvas = h.state().last_canvas_rect;
    let mut diff = 0;
    let mut bbox = egui::Rect::NOTHING;
    for (x, y, p) in real.enumerate_pixels() {
        if canvas.expand(2.0).contains(egui::pos2(x as f32 + 0.5, y as f32 + 0.5)) {
            continue;
        }
        if frozen.get_pixel(x, y) != p {
            diff += 1;
            bbox.extend_with(egui::pos2(x as f32, y as f32));
        }
    }
    if diff > 0
        && let Ok(dir) = std::env::var("FRAME_WORK_DUMP")
    {
        let _ = frozen.save(format!("{dir}/frozen.png"));
        let _ = real.save(format!("{dir}/real.png"));
        eprintln!("diff bbox {bbox:?} canvas {canvas:?}");
    }
    assert_eq!(diff, 0, "chrome pixels differ between the repainted and the built frame");
    h.event(Event::PointerButton { pos: screen(&h, 130.0, 100.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(2);
}

/// Jumping to another part of a long Layers panel builds at most
/// `THUMBS_PER_FRAME` thumbnails a frame; the next frames finish the rest.
#[test]
fn thumbnails_are_built_a_few_per_frame() {
    let (mut h, _) = app(200);
    let _ = h.try_run();
    // Scroll far down the rows in one wheel step.
    let row = h.state().last_canvas_rect.right() + 150.0;
    h.event(Event::PointerMoved(egui::pos2(row, 600.0)));
    h.step();
    let (thumbs, deferred) = (counter(&h, "thumbs"), counter(&h, "thumbs_deferred"));
    h.input_mut().events.push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: egui::vec2(0.0, -4000.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
    });
    let mut per_frame = Vec::new();
    for _ in 0..12 {
        let before = counter(&h, "thumbs");
        h.step();
        per_frame.push(counter(&h, "thumbs") - before);
    }
    assert!(per_frame.iter().all(|n| *n <= u64::from(photocraft_ui_egui::THUMBS_PER_FRAME)), "{per_frame:?}");
    assert!(counter(&h, "thumbs") > thumbs, "the rows scrolled into view got thumbnails");
    assert!(counter(&h, "thumbs_deferred") > deferred, "some waited for a later frame: {per_frame:?}");
    let _ = h.try_run();
    let settled = counter(&h, "thumbs");
    h.step();
    assert_eq!(counter(&h, "thumbs"), settled, "then nothing more to build");
}

/// A Move drag's preview copies only the moving layers each frame (the last frame's document is
/// reused), and the release lands the layer where the pointer went, as one step.
#[test]
fn a_move_drag_reuses_its_preview_document() {
    let (mut h, mid) = app(60);
    h.state_mut().run("layer.select", json!({"layer": mid.0})).expect("select");
    h.state_mut().ui.tool = Tool::Move;
    h.state_mut().ui.tool_options.move_auto_select = false;
    h.state_mut().ui.extras.snap = false;
    settle(&mut h);
    let bounds =
        |h: &H| photocraft_engine::layer_multi_cmds::layer_bounds(h.state().session.active().expect("doc").doc.layer(mid).expect("layer")).expect("bounds");
    let before = bounds(&h);
    let zoom = f64::from(h.state().ui.views[0].zoom);
    let from = [f64::from(before.x0 + 10), 120.0];
    let p0 = screen(&h, from[0], from[1]);
    h.event(Event::PointerMoved(p0));
    h.step();
    h.event(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let (reused, split) = (counter(&h, "move_preview_reused"), counter(&h, "move_split"));
    for i in 1..=20 {
        h.event(Event::PointerMoved(screen(&h, from[0] + f64::from(i) * 3.0 / zoom.max(0.01), from[1])));
        h.step();
    }
    // The CPU canvas draws the drag from its split composites (`move_cpu`), the preview then
    // copying nothing; else the preview reuses its document.
    let (reused, split) = (counter(&h, "move_preview_reused") - reused, counter(&h, "move_split") - split);
    assert!(split >= 15, "split composites on {split} of 20 frames (preview reused on {reused})");
    let p1 = screen(&h, from[0] + 60.0 / zoom.max(0.01), from[1]);
    h.event(Event::PointerButton { pos: p1, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(3);
    let after = bounds(&h);
    assert_eq!(after.y0, before.y0);
    assert!(after.x0 > before.x0, "moved right: {before:?} -> {after:?}");
}

/// A press that starts on the chrome (menu bar, toolbar) with a jittering pointer until the
/// release is still a click: the chrome is built every frame (never repainted from shapes), so
/// egui keeps the pressed widget. Regression: frozen chrome between press and release ate menu
/// clicks on the web.
#[test]
fn a_jittery_click_on_the_chrome_still_clicks() {
    use egui_kittest::kittest::Queryable;
    let (mut h, _) = app(40);
    h.state_mut().ui.tool = Tool::Brush;
    let _ = h.try_run();
    // Down, a few one-pixel moves (each its own frame), up: a real hand on a mouse.
    let human_click = |h: &mut H, at: Pos2| {
        h.event(Event::PointerMoved(at));
        h.step();
        h.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        for (dx, dy) in [(1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (-1.0, 1.0)] {
            h.event(Event::PointerMoved(at + egui::vec2(dx, dy)));
            h.step();
        }
        let up = at + egui::vec2(-1.0, 1.0);
        h.event(Event::PointerButton { pos: up, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        h.run_steps(4);
    };
    let reused = counter(&h, "chrome_reused");
    let file = h.get_by_label("File").rect().center();
    human_click(&mut h, file);
    assert!(h.query_all_by_label_contains("Open").next().is_some(), "the File menu opened");
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    // A toolbar button: the Move tool, the topmost square click target left of the canvas.
    assert_ne!(h.state().ui.tool, Tool::Move);
    let left = h.state().last_canvas_rect.left();
    let first = h.ctx.viewport(|v| {
        v.prev_pass
            .widgets
            .layers()
            .flat_map(|(_, w)| w.iter())
            .filter(|w| w.rect.right() <= left && (w.rect.width() - w.rect.height()).abs() < 0.5 && w.rect.width() >= 24.0 && w.sense.senses_click())
            .map(|w| w.rect)
            .filter(|r| r.top() > 60.0)
            .min_by(|a, b| a.top().total_cmp(&b.top()))
    });
    let first = first.expect("toolbar buttons");
    human_click(&mut h, first.center());
    let picked = h.state().ui.tool == Tool::Move;
    assert!(picked, "a jittery click on the toolbar picked the Move tool");
    assert_eq!(counter(&h, "chrome_reused"), reused, "chrome presses never reuse frozen chrome");
    // A toolbar flyout (right-click on the slot) hangs over the canvas: a jittery click on its
    // second item, inside the canvas rect, still picks that tool.
    h.event(Event::PointerMoved(first.center()));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: first.center(), button: PointerButton::Secondary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(2);
    let item = egui::pos2(first.right() + 6.0 + 90.0, first.top() + 6.0 + 26.0 + 13.0);
    assert!(h.state().last_canvas_rect.contains(item), "the flyout item lies over the canvas");
    human_click(&mut h, item);
    assert_ne!(h.state().ui.tool, Tool::Move, "the flyout's second tool was picked");
    assert_eq!(counter(&h, "chrome_reused"), reused, "a press on the flyout never freezes the chrome");
    // No document open (the home screen; the canvas rect is whatever the last document left):
    // the menus still click.
    h.state_mut().run("file.close", json!({"discard": true})).expect("close");
    let _ = h.try_run();
    let file = h.get_by_label("File").rect().center();
    human_click(&mut h, file);
    assert!(h.query_all_by_label_contains("Open").next().is_some(), "the File menu opened without a document");
    assert_eq!(counter(&h, "chrome_reused"), reused);
}

/// The Crop tool's first press on a big stack with pixels past the canvas doesn't composite them
/// on the UI thread (they land a few frames later; ~0.5 s on a 100-layer page before), and the
/// app settles afterwards (no frames asked for once they landed).
#[test]
fn crop_press_composites_past_the_canvas_off_the_ui_thread() {
    let (mut d, _) = doc(60);
    for l in d.layers.iter_mut().filter(|l| !l.is_group()).skip(1) {
        if let Some(s) = l.surface_mut() {
            s.fill_rect(Rect::new(560, 0, 1400, 400), &[0.9, 0.2, 0.1, 1.0]);
        }
    }
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_pixels_per_point(1.0).with_max_steps(400).with_step_dt(1.0 / 60.0).build_eframe(
        move |cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
            app.session.open_document(d, None);
            app.sync_views();
            app
        },
    );
    h.run_steps(8);
    h.state_mut().ui.tool = Tool::Crop;
    let _ = h.try_run();
    let p = screen(&h, 300.0, 200.0);
    h.event(Event::PointerMoved(p));
    h.step();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    assert!(h.state().perf.spans.contains_key("crop-beyond-canvas"), "the press started the composite");
    assert_eq!(counter(&h, "crop_beyond_async"), 1, "off the UI thread");
    // It lands, and then nothing more is asked for.
    let _ = h.try_run_realtime();
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    let _ = h.try_run_realtime();
    h.step();
    assert!(!repaint_asked(&h), "settled: {:?}", h.ctx.repaint_causes());
}
