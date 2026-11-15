//! Switching between named views on a large page with three languages (Capikoma request 17): the
//! first frame after `document.views.apply` + `Host::show`, on the GPU canvas (a software adapter
//! on a server). Skips without a GPU adapter.

use std::time::Instant;

use egui::{Pos2, Rect, vec2};
use serde_json::json;

use super::*;

struct Pane {
    editor: CapishotEditor,
}

impl eframe::App for Pane {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let r = ui.max_rect();
        let _ = self.editor.ui_in_rect(ui, Rect::from_min_size(Pos2::ZERO, r.size()));
    }
}

const LANGS: [&str; 3] = ["lang_es", "lang_en", "lang_pt"];

/// A page like the owner's: noisy `_base` and `_fondo`, a `clean` patch layer, and three language
/// groups of `texts` type layers each with a 4 px outline, the same places in every language.
fn page(editor: &mut CapishotEditor, w: u32, h: u32, texts: usize) -> DocumentId {
    let host = &mut editor.host;
    host.execute("file.new", json!({"width": w, "height": h, "background": "white", "name": "page"})).expect("a document");
    let doc = host.active_document().expect("active");
    let id = |v: Value| v["layer"].as_u64().expect("a layer id");
    let noisy = |host: &mut Host, name: &str, color: &str| {
        let l = id(host.execute("layer.new.layer", json!({"name": name})).expect("a layer"));
        host.execute("edit.fill", json!({"color": color})).expect("fill");
        host.execute("filter.noise.addNoise", json!({"amount": 40, "seed": 7})).expect("noise");
        l
    };
    let base = noisy(host, "base", "#d0c8b8");
    let fondo = noisy(host, "fondo", "#a0a8b0");
    let clean = id(host.execute("layer.new.layer", json!({"name": "patches"})).expect("a layer"));
    host.execute("edit.fill", json!({"color": "#ffffff", "rect": [200, 200, 900, 600]})).ok();
    let mut groups: Vec<(String, Vec<u64>)> = vec![("_base".into(), vec![base]), ("_fondo".into(), vec![fondo]), ("clean".into(), vec![clean])];
    for lang in LANGS {
        let mut ids = Vec::new();
        for i in 0..texts {
            let (x, y) = (150 + (i % 3) as u32 * (w / 3), 300 + (i / 3) as u32 * (h / (texts as u32 / 3 + 2)));
            let l = id(host.execute("type.create", json!({"x": x, "y": y, "text": format!("{lang} text number {i}"), "size": 56, "color": "#000000"})).expect("type"));
            host.execute("layer.layerStyle.stroke", json!({"layer": l, "size": 4, "position": "outside", "color": "#ffffff"})).expect("stroke");
            ids.push(l);
        }
        groups.push((lang.into(), ids));
    }
    for (name, ids) in &groups {
        host.execute("layer.selectMany", json!({"layers": ids})).expect("select");
        host.execute("layer.groupLayers", json!({"name": name})).expect("group");
    }
    for lang in LANGS {
        let hidden: Vec<&str> = LANGS.iter().copied().filter(|l| *l != lang).collect();
        host.define_view(doc, lang, json!({"visible": [lang], "hidden": hidden})).expect("a view");
    }
    host.apply_view(doc, "lang_es").expect("apply");
    doc
}

fn time_switches(prewarm: bool) {
    let built = std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder().with_size(vec2(1280.0, 900.0)).with_pixels_per_point(1.0).with_max_steps(64).wgpu().build_eframe(|cc| {
            let rs = cc.wgpu_render_state.as_ref().expect("a render state");
            Pane { editor: CapishotEditor::new(Some(rs.clone())) }
        })
    });
    let Ok(mut h) = built else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let (w, ht, texts) = (
        std::env::var("SWITCH_W").ok().and_then(|v| v.parse().ok()).unwrap_or(2900),
        std::env::var("SWITCH_H").ok().and_then(|v| v.parse().ok()).unwrap_or(4000),
        std::env::var("SWITCH_TEXTS").ok().and_then(|v| v.parse().ok()).unwrap_or(17),
    );
    let t = Instant::now();
    let doc = page(&mut h.state_mut().editor, w, ht, texts);
    eprintln!("NUM build {w}x{ht} {texts} texts per language: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);
    for _ in 0..40 {
        h.step();
    }
    assert!(h.state().editor.view.gpu_active(), "the GPU canvas");
    let frame = |h: &mut egui_kittest::Harness<'_, Pane>| {
        let t = Instant::now();
        h.step();
        t.elapsed().as_secs_f64() * 1000.0
    };
    let mut idle: Vec<f64> = (0..10).map(|_| frame(&mut h)).collect();
    idle.sort_by(f64::total_cmp);
    eprintln!("NUM idle p50 {:.1} ms", idle[idle.len() / 2]);
    if prewarm {
        for lang in ["lang_en", "lang_pt"] {
            assert!(h.state_mut().editor.host.prewarm_view(doc, lang).expect("prewarm"));
        }
        for _ in 0..20 {
            h.step();
        }
    }
    for lang in ["lang_en", "lang_pt", "lang_es", "lang_en", "lang_pt", "lang_es"] {
        let t = Instant::now();
        let editor = &mut h.state_mut().editor;
        editor.host.apply_view(doc, lang).expect("apply");
        editor.host.show(doc, false).expect("show");
        let apply = t.elapsed().as_secs_f64() * 1000.0;
        photocraft_ui_egui::perf_span::begin_frame();
        let first = frame(&mut h);
        let perf = &h.state().editor.view.app().perf;
        eprintln!("NUM   fallback {:?} kind {} px {} comp {:.1} up {:.1} adapter {:?}", perf.gpu_fallback, perf.last_refresh, perf.last_refresh_px, perf.composite_ms, perf.upload_ms, perf.gpu_info.fallback);
        let spans: Vec<String> = photocraft_ui_egui::perf_span::frame().iter().take(8).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
        let rest: Vec<f64> = (0..5).map(|_| frame(&mut h)).collect();
        eprintln!("NUM switch {lang}: apply {apply:.1} first {first:.1} next max {:.1} ms; spans {}", rest.iter().copied().fold(0.0, f64::max), spans.join(", "));
    }
}

#[test]
#[ignore]
fn view_switch_frame_cost() {
    time_switches(false);
}

#[test]
#[ignore]
fn view_switch_frame_cost_prewarmed() {
    time_switches(true);
}

/// The colour at the middle of the canvas, as the GPU draws it.
fn middle(h: &mut egui_kittest::Harness<'_, Pane>) -> [u8; 3] {
    let shot = h.render().expect("render");
    let p = shot.get_pixel(shot.width() / 2, shot.height() / 2);
    [p[0], p[1], p[2]]
}

/// A page whose language groups each hold one layer of their own colour, with a view each.
fn coloured(editor: &mut CapishotEditor) -> (DocumentId, Vec<u64>) {
    let host = &mut editor.host;
    host.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).expect("a document");
    let doc = host.active_document().expect("active");
    let mut layers = Vec::new();
    for (lang, color) in LANGS.iter().zip(["#ff0000", "#00ff00", "#0000ff"]) {
        let l = host.execute("layer.new.layer", json!({"name": format!("{lang} text")})).expect("a layer")["layer"].as_u64().expect("an id");
        host.execute("edit.fill", json!({"color": color})).expect("fill");
        host.execute("layer.selectMany", json!({"layers": [l]})).expect("select");
        host.execute("layer.groupLayers", json!({"name": lang})).expect("group");
        layers.push(l);
    }
    for lang in LANGS {
        let hidden: Vec<&str> = LANGS.iter().copied().filter(|l| *l != lang).collect();
        host.define_view(doc, lang, json!({"visible": [lang], "hidden": hidden})).expect("a view");
    }
    host.apply_view(doc, "lang_es").expect("apply");
    (doc, layers)
}

fn pane_harness() -> Option<egui_kittest::Harness<'static, Pane>> {
    std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder().with_size(vec2(400.0, 300.0)).with_pixels_per_point(1.0).with_max_steps(64).wgpu().build_eframe(|cc| {
            let rs = cc.wgpu_render_state.as_ref().expect("a render state");
            Pane { editor: CapishotEditor::new(Some(rs.clone())) }
        })
    })
    .ok()
}

fn switch(h: &mut egui_kittest::Harness<'_, Pane>, doc: DocumentId, lang: &str) -> [u8; 3] {
    h.state_mut().editor.host.apply_view(doc, lang).expect("apply");
    h.step();
    middle(h)
}

/// Regression for Capikoma request 17: a view shown before comes back from its kept canvas, and
/// shows the right pixels; an edit in between drops the kept canvases instead of showing them.
/// Skips without a GPU adapter.
#[test]
fn a_view_shown_before_comes_back_kept_and_an_edit_drops_it() {
    let Some(mut h) = pane_harness() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let (doc, layers) = coloured(&mut h.state_mut().editor);
    for _ in 0..10 {
        h.step();
    }
    let gpu = |h: &egui_kittest::Harness<'_, Pane>| h.state().editor.view.app().gpu_canvas().expect("on the GPU").clone();
    assert_eq!(middle(&mut h), [255, 0, 0]);
    assert_eq!(switch(&mut h, doc, "lang_en"), [0, 255, 0]);
    assert_eq!(gpu(&h).view_sets(doc.0), 1, "the view before is kept");
    assert_eq!(switch(&mut h, doc, "lang_es"), [255, 0, 0]);
    assert_eq!(h.state().editor.view.app().perf.last_refresh, "view-set", "back from the kept canvas");
    assert_eq!(switch(&mut h, doc, "lang_en"), [0, 255, 0]);
    assert_eq!(h.state().editor.view.app().perf.last_refresh, "view-set");
    // An edit: the kept canvases show the document as it was, so they go.
    h.state_mut().editor.host.execute("layer.selectMany", json!({"layers": [layers[0]]})).expect("select");
    h.state_mut().editor.host.execute("edit.fill", json!({"color": "#ffff00"})).expect("fill");
    h.step();
    assert_eq!(gpu(&h).view_sets(doc.0), 0, "an edit drops the kept canvases");
    assert_eq!(switch(&mut h, doc, "lang_es"), [255, 255, 0], "the edit shows");
    assert_ne!(h.state().editor.view.app().perf.last_refresh, "view-set");
    // Prewarmed: the first switch to a view comes from its kept canvas too.
    assert!(h.state_mut().editor.host.prewarm_view(doc, "lang_pt").expect("prewarm"));
    for _ in 0..10 {
        h.step();
    }
    assert!(gpu(&h).view_sets(doc.0) >= 1, "the prewarmed view is kept");
    assert_eq!(switch(&mut h, doc, "lang_pt"), [0, 0, 255]);
    assert_eq!(h.state().editor.view.app().perf.last_refresh, "view-set", "the prewarmed view at once");
    // A tiny canvas budget keeps nothing beside the document's own canvas.
    gpu(&h).set_canvas_budget(gpu(&h).texture_info(doc.0).expect("a canvas").1);
    assert_eq!(gpu(&h).view_sets(doc.0), 0, "a smaller budget drops them");
    assert_eq!(switch(&mut h, doc, "lang_en"), [0, 255, 0]);
    assert_eq!(gpu(&h).view_sets(doc.0), 0, "no room to keep one");
}
