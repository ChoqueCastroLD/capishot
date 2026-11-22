//! The Layers panel's frame on a page like a scanlator's (a 3 MP page, its cleaning layer and a
//! few type layers) while type is resized and a layer is moved, as the ChromeOS install whose
//! `dock.layers` frames took 270 ms did. `cargo test -p photocraft-ui-egui --lib -- --ignored
//! layers_panel_frame_timing --nocapture`. And on pages of 80 and 200 layers: the panel's work
//! is the rows in view, not the document's.

use egui::vec2;
use egui_kittest::Harness;
use serde_json::json;

use super::*;

fn page() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1200, "height": 2500, "background": "white"})).unwrap();
    app.run("layer.new.layer", json!({})).unwrap();
    app.run("select.rect", json!({"x": 100, "y": 100, "width": 300, "height": 200})).unwrap();
    app.run("edit.fill", json!({"color": "#808080"})).unwrap();
    app.run("select.deselect", json!({})).unwrap();
    for i in 0..14 {
        let text = format!("¿Y ahora qué hacemos con todo esto que nos dejaron aquí, número {i}? No lo sé, pero algo habrá que hacer");
        app.run("type.create", json!({"text": text, "x": 100 + 150 * (i % 3), "y": 300 + 300 * (i / 3), "size": 24})).unwrap();
        let _ = app.run("layer.layerStyle.stroke", json!({"size": 3}));
    }
    app
}

/// Self time per span (ms): the mean over the frames, and the worst frame.
#[test]
#[ignore = "timing"]
fn layers_panel_frame_timing() {
    let app = page();
    let mut ready = false;
    let mut h = Harness::builder().with_size(vec2(320.0, 600.0)).with_max_steps(1).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !ready {
                PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::ProMedium);
                ready = true;
                return;
            }
            let _s = crate::perf_span::span("dock.layers");
            super::layers::layers(app, ui);
        },
        app,
    );
    h.step();
    h.step();
    let mut sums: Vec<(&str, f64, f64)> = Vec::new();
    let n = 40;
    for i in 0..n {
        // Alternately: TypeR resize of the active type layer, a move of the cleaning layer.
        let app = h.state_mut();
        if i % 2 == 0 {
            app.run("typer.resize", json!({"delta": if i % 4 == 0 { 1 } else { -1 }})).unwrap();
        } else {
            let id = app.session.active().unwrap().doc.layers[1].id.0;
            app.run("layer.translate", json!({"layer": id, "dx": 3, "dy": 0})).unwrap();
        }
        crate::perf_span::begin_frame();
        let t = std::time::Instant::now();
        h.step();
        let total = t.elapsed().as_secs_f64() * 1e3;
        for (name, ms) in crate::perf_span::frame().into_iter().chain([("frame", total)]) {
            match sums.iter_mut().find(|w| w.0 == name) {
                Some(w) => {
                    w.1 += ms;
                    w.2 = w.2.max(ms);
                }
                None => sums.push((name, ms, ms)),
            }
        }
    }
    sums.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (name, sum, worst) in sums {
        eprintln!("{name:24} mean {:7.2} ms  worst {worst:7.2} ms", sum / f64::from(n));
    }
}

/// A page of `n` layers: every third a type layer with a Stroke and an Outer Glow (three rows
/// under it in the panel), the others pixel layers.
fn many(n: usize) -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
    app.run("file.new", json!({"width": 1200, "height": 1800, "background": "white"})).unwrap();
    while app.session.active().unwrap().doc.walk().len() < n {
        let i = app.session.active().unwrap().doc.walk().len();
        if i.is_multiple_of(3) {
            let l = app.run("type.create", json!({"text": format!("GLOBO {i}"), "x": 40 + (i % 7) * 150, "y": 60 + (i % 23) * 75, "size": 20})).unwrap()["layer"].as_u64().unwrap();
            app.run("layer.layerStyle.stroke", json!({"layer": l, "size": 3})).unwrap();
            app.run("layer.layerStyle.outerGlow", json!({"layer": l})).unwrap();
        } else {
            app.run("layer.new.layer", json!({"name": format!("capa {i}")})).unwrap();
            app.run("select.rect", json!({"x": (i % 9) * 120, "y": (i % 17) * 100, "width": 90, "height": 60})).unwrap();
            app.run("edit.fill", json!({"color": "#406080"})).unwrap();
            app.run("select.deselect", json!({})).unwrap();
        }
    }
    app
}

fn panel(app: PhotocraftApp) -> Harness<'static, PhotocraftApp> {
    let mut ready = false;
    let mut h = Harness::builder().with_size(vec2(320.0, 600.0)).with_max_steps(1).build_ui_state(
        move |ui, app: &mut PhotocraftApp| {
            if !ready {
                PhotocraftApp::setup_context(ui.ctx(), theme::ThemeKind::ProMedium);
                ready = true;
                return;
            }
            let _s = crate::perf_span::span("dock.layers");
            super::layers::layers(app, ui);
        },
        app,
    );
    h.step();
    h.step();
    h
}

fn visited(app: &PhotocraftApp) -> u64 {
    app.perf_counters.rows_drawn + app.perf_counters.rows_skipped
}

/// With 80 or 200 layers, a frame of the panel visits the rows in view (and none it only gives a
/// place), at the top, scrolled to the middle and at the bottom; there the rows in view are the
/// ones shown. On a92ad8384 every row of the document was visited each frame.
#[test]
fn the_layers_panel_visits_only_the_rows_in_view() {
    for n in [80, 200] {
        let mut h = panel(many(n));
        for (scroll, last) in [(0.0, None), (-2400.0, None), (-1.0e6, Some("Background"))] {
            h.hover_at(egui::pos2(160.0, 300.0));
            h.event(egui::Event::MouseWheel {
                source: egui::MouseWheelSource::Unknown,
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, scroll),
                phase: egui::TouchPhase::Move,
                modifiers: Default::default(),
            });
            h.step();
            h.step();
            let (v0, d0) = (visited(h.state()), h.state().perf_counters.rows_drawn);
            h.step();
            let (seen, drawn) = (visited(h.state()) - v0, h.state().perf_counters.rows_drawn - d0);
            assert!(drawn > 4 && seen <= drawn + 1, "{n} layers, scrolled {scroll}: {seen} rows visited, {drawn} laid out");
            if let Some(name) = last {
                use egui_kittest::kittest::Queryable;
                assert!(h.query_by_label(name).is_some(), "{n} layers: the bottom row is shown at the bottom");
            }
        }
    }
}

/// Median, p99 and max of the panel's frame on 80 and 200 layers while a type layer is edited
/// and a layer moved, one per frame. `--ignored layers_panel_many_layers_timing --nocapture`.
#[test]
#[ignore = "timing"]
fn layers_panel_many_layers_timing() {
    for n in [80, 200] {
        let mut h = panel(many(n));
        let text = h.state().session.active().unwrap().doc.walk().iter().rev().find(|(_, _, l)| matches!(l.content, photocraft_doc::LayerContent::Text(_))).map(|(_, _, l)| l.id.0).unwrap();
        let mut ms = Vec::new();
        for i in 0..60 {
            let app = h.state_mut();
            if i % 2 == 0 {
                app.run("type.edit", json!({"layer": text, "replace": {"start": 0, "end": 0, "text": "A"}})).unwrap();
            } else {
                let id = app.session.active().unwrap().doc.layers[1].id.0;
                app.run("layer.translate", json!({"layer": id, "dx": 1, "dy": 0})).unwrap();
            }
            let t = std::time::Instant::now();
            h.step();
            ms.push(t.elapsed().as_secs_f64() * 1e3);
        }
        ms.sort_by(f64::total_cmp);
        let at = |p: f64| ms[((ms.len() as f64 - 1.0) * p).round() as usize];
        eprintln!("LAYERS {n} layers: panel frame median {:.2} ms, p99 {:.2} ms, max {:.2} ms", at(0.5), at(0.99), at(1.0));
    }
}
