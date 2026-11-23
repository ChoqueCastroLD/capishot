//! The steps after which the published 0.13.35 ended on a 200-layer page in CPU mode: Text
//! Selection's detect, a clean of every piece with AI on a new layer (`select.cleanPieces`), an
//! undo (of the whole clean: it is one history step), and two clicks of the Bubble Text Remover.
//! The clean asked for each tile from inside the answer to the one before, a model run deep in
//! the stack per tile, on the window's thread (1 MB on Windows): here the app goes through the
//! steps, drawing frames between them, on the CPU canvas and on the GPU one, and no request runs
//! inside another.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use photocraft_ai_lama::service::FakeInpainter;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::move_repeat_tests::{Device, gpu_drawing, harness, styled_text};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

const W: i32 = 1600;
const H: i32 = 2400;

/// Finds whatever is dark.
struct Dark;
impl photocraft_ai_textmask::TextMasker for Dark {
    fn model_state(&self) -> photocraft_ai_textmask::ModelState {
        photocraft_ai_textmask::ModelState::Ready
    }
    fn device(&self) -> String {
        "test".into()
    }
    fn detect(&self, page: photocraft_ai_textmask::Page<'_>, _: &dyn Fn() -> bool) -> Result<photocraft_ai_textmask::Detection, photocraft_ai_textmask::Error> {
        let prob: Vec<f32> = page.rgb.as_chunks::<3>().0.iter().map(|c| if c[0] < 128 { 0.995 } else { 0.0 }).collect();
        Ok(photocraft_ai_textmask::regions::find_regions(&prob, page.width, page.height))
    }
}

/// The two speech bubbles of the page: boxes with a black outline and three lines of lettering.
const BUBBLES: [Rect; 2] = [Rect { x0: 100, y0: 90, x1: 500, y1: 300 }, Rect { x0: 1100, y0: 90, x1: 1500, y1: 300 }];

/// A white page of 200 layers: the art (two bubbles and lettering spread over the page, far
/// enough apart to be a tile each) on the Background, 20 styled type layers and small patches of
/// paint on layers of their own.
fn page(app: &mut PhotocraftApp) {
    app.run("file.new", json!({"width": W, "height": H, "background": "white"})).expect("doc");
    let background = app.session.active().expect("doc").active_layer.expect("the Background");
    app.session
        .edit("art", |doc, _| {
            let l = doc.layer_mut(background).ok_or(photocraft_engine::EngineError::NoDocument)?.surface_mut().ok_or(photocraft_engine::EngineError::NoDocument)?;
            let fmt = l.format();
            let black = photocraft_raster::from_rgba(&fmt, [0.0, 0.0, 0.0, 1.0]);
            let white = photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 1.0, 1.0]);
            for b in BUBBLES {
                l.fill_rect(b, &black);
                l.fill_rect(Rect::new(b.x0 + 5, b.y0 + 5, b.x1 - 5, b.y1 - 5), &white);
                for row in 0..3 {
                    for word in 0..4 {
                        let (x, y) = (b.x0 + 60 + word * 72, b.y0 + 50 + row * 42);
                        l.fill_rect(Rect::new(x, y, x + 48, y + 22), &black);
                    }
                }
            }
            for j in 0..7 {
                for i in 0..6 {
                    let (x, y) = (130 + i * 260, 520 + j * 260);
                    l.fill_rect(Rect::new(x, y, x + 30, y + 12), &black);
                }
            }
            Ok(())
        })
        .expect("art");
    for i in 0..20u32 {
        styled_text(app, 80 + (i % 4) * 380, 400 + (i / 4) * 420, &format!("Texto {i}"), 28);
    }
    for i in 0..179i32 {
        app.run("layer.new.layerSilent", json!({})).expect("a layer");
        app.session
            .edit("patch", |doc, a| {
                let l = a.and_then(|id| doc.layer_mut(id)).and_then(|l| l.surface_mut()).ok_or(photocraft_engine::EngineError::NoDocument)?;
                let fmt = l.format();
                let (x, y) = (40 + (i * 97) % (W - 120), 340 + (i * 53) % (H - 400));
                l.fill_rect(Rect::new(x, y, x + 60, y + 6), &photocraft_raster::from_rgba(&fmt, [0.2, 0.4, 0.8, 0.5]));
                Ok(())
            })
            .expect("patch");
    }
    assert_eq!(app.session.active().expect("doc").doc.walk().len(), 200);
    app.run("layer.select", json!({"layer": background.0})).expect("the Background active");
}

fn frames(h: &mut Harness, n: usize) {
    for _ in 0..n {
        h.step();
        // The worker pool's tiles land a frame or two later.
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

/// A click of the Bubble Text Remover in `b`, as the measurement that ended the app made it.
fn click(app: &mut PhotocraftApp, b: Rect) -> Result<Value, String> {
    app.run("edit.removeBubbleText", json!({"x": (b.x0 + b.x1) / 2, "y": b.y0 + 20, "sampleAll": true, "newLayer": true}))
}

fn the_steps(device: Device) {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let fake = Arc::new(FakeInpainter::default());
    {
        let app = h.state_mut();
        app.background_jobs = false;
        page(app);
        app.session.set_text_masker(Some(Arc::new(Dark)));
        app.session.set_inpainter(Some(fake.clone()));
        app.sync_views();
        app.ui.tool = crate::state::Tool::Move;
    }
    frames(&mut h, 30);
    if !gpu_drawing(&mut h, device) {
        return;
    }
    let found = h.state_mut().run("select.detectText", json!({})).expect("detect");
    let pieces = found["pieces"].as_array().map_or(0, Vec::len);
    assert!(pieces >= 42, "the lettering is found: {pieces} pieces");
    frames(&mut h, 10);
    let layers = h.state().session.active().expect("doc").doc.walk().len();
    h.state_mut().run("select.cleanPieces", json!({"steps": [{"pieces": "all", "method": "ai", "output": "new"}]})).expect("clean");
    let tiles = fake.sizes.lock().expect("sizes").len();
    assert!(tiles >= 30, "a clean of many tiles: {tiles}");
    assert_eq!(fake.deepest.load(Ordering::SeqCst), 1, "a tile's request ran inside another's answer");
    frames(&mut h, 10);
    assert_eq!(h.state().session.active().expect("doc").doc.walk().len(), layers + 1, "the clean is on a new layer");
    // One undo takes the whole clean back, its layer with it: the clean is one history step.
    h.state_mut().run("edit.undo", json!({})).expect("undo");
    frames(&mut h, 10);
    assert_eq!(h.state().session.active().expect("doc").doc.walk().len(), layers, "one undo takes the clean away");
    // Each click paints over its bubble's lettering on a new layer.
    for (i, b) in BUBBLES.into_iter().enumerate() {
        let r = click(h.state_mut(), b).unwrap_or_else(|e| panic!("click {}: {e}", i + 1));
        assert!(r["pixels"].as_u64().is_some_and(|p| p > 1000), "click {}: the lettering is painted over: {r}", i + 1);
        frames(&mut h, 10);
    }
    assert_eq!(h.state().session.active().expect("doc").doc.walk().len(), layers + 2, "a layer per click");
    eprintln!("{device:?}: {pieces} pieces, {tiles} tiles, last refresh {}", h.state().perf.last_refresh);
}

#[test]
fn detect_clean_undo_and_two_bubble_clicks_on_200_layers_on_the_cpu_canvas() {
    the_steps(Device::Cpu);
}

#[test]
fn detect_clean_undo_and_two_bubble_clicks_on_200_layers_on_the_gpu_canvas() {
    the_steps(Device::Gpu);
}
