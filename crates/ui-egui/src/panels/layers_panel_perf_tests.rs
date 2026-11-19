//! The Layers panel's frame on a page like a scanlator's (a 3 MP page, its cleaning layer and a
//! few type layers) while type is resized and a layer is moved, as the ChromeOS install whose
//! `dock.layers` frames took 270 ms did. `cargo test -p photocraft-ui-egui --lib -- --ignored
//! layers_panel_frame_timing --nocapture`.

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
