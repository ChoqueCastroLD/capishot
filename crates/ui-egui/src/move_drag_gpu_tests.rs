//! A Move drag of a big Smart Object on a credits page (the owner's "creditos villana embarazada"
//! report, 0.13.28): every frame of the drag must show the whole canvas at the drag's current
//! offset, never parts of it at older offsets (vertical bands where the slow-GPU frame budget left
//! pieces of the refresh for frames that never came while the pointer kept moving), and stay
//! cheap. Drives the real app offscreen with real pointer events, compares every presented frame
//! with the CPU composite of the document moved to that frame's offset, and prints the per-frame
//! cost. Skips without a GPU adapter.

use egui::{Modifiers, PointerButton};
use photocraft_doc::LayerId;
use photocraft_geom::Rect;
use serde_json::json;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

const W: u32 = 1920;
const H: u32 = 1080;

/// How the drag is run.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Device {
    /// The GPU canvas as it comes.
    Gpu,
    /// The GPU canvas on a GPU so slow that one piece of a big refresh takes a whole frame's work.
    SlowGpu,
    /// The GPU canvas with the memory of a 4 GB device.
    LowMemoryGpu,
    /// The CPU canvas (no GPU canvas: software or blocklisted drivers).
    Cpu,
}

fn harness(device: Device) -> Option<Harness> {
    std::panic::catch_unwind(move || {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(2300.0, 1400.0))
            .with_pixels_per_point(1.0)
            .with_max_steps(64)
            .wgpu_setup(crate::gpu_canvas::wgpu_setup())
            .build_eframe(move |cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
                if device != Device::Cpu
                    && let Some(rs) = cc.wgpu_render_state.as_ref()
                {
                    app.set_wgpu(rs.clone());
                }
                app
            })
    })
    .ok()
}

/// The credits page: a painted layer, rectangle shapes, type with a Stroke, an empty group, a
/// small Smart Object and, on top, a Smart Object of vertical colour stripes scaled past every
/// edge of the canvas (a placed image larger than the page). Returns the big one's id.
fn credits_page(app: &mut PhotocraftApp) -> LayerId {
    app.run("file.new", json!({"width": W, "height": H, "background": "white", "name": "creditos"})).expect("a document");
    app.run("layer.new.layer", json!({"name": "Layer 2"})).expect("a layer");
    app.run("edit.fill", json!({"color": "#d8c8e8", "rect": [0, 700, 1920, 1080]})).ok();
    for i in 0..6 {
        let x = 80 + i * 300;
        app.run("shape.create", json!({"rect": [x, 760, x + 220, 1000], "fill": "#6a7fb8"})).expect("a rectangle");
    }
    for (i, text) in ["typer: naxu", "facebook.com/shiliellano", "capitulo x", "gracias por leer", "creditos"].iter().enumerate() {
        let l = (app.run("type.create", json!({"x": 120, "y": 140 + i as u32 * 120, "text": text, "size": 64, "color": "#202020"})).expect("type"))["layer"].as_u64().expect("a type layer");
        app.run("layer.layerStyle.stroke", json!({"layer": l, "size": 6, "position": "outside", "color": "#ffffff"})).expect("stroke");
    }
    app.run("layer.new.group", json!({"name": "Group 1"})).ok();
    app.run("layer.new.layer", json!({"name": "mirai project icon"})).expect("a layer");
    app.run("edit.fill", json!({"color": "#3070c0", "rect": [1650, 60, 1850, 260]})).ok();
    app.run("layer.smartObjects.convertToSmartObject", json!({})).expect("a smart object");
    app.run("layer.new.layer", json!({"name": "Gemini_Generated_Image"})).expect("a layer");
    app.session
        .edit("stripes", |doc, active| {
            let l = doc.layer_mut(active.expect("active")).expect("layer").surface_mut().expect("raster");
            let fmt = l.format();
            for i in 0..(W as i32 / 24 + 1) {
                let t = i as f32 * 0.137;
                let c = [0.5 + 0.45 * (t * 6.0).sin(), 0.5 + 0.45 * (t * 4.0 + 2.0).sin(), 0.5 + 0.45 * (t * 3.0 + 4.0).sin(), 1.0];
                // A transparent gap beside every stripe: the layers below show through.
                l.fill_rect(Rect::new(i * 24, 0, i * 24 + 16, H as i32), &photocraft_raster::from_rgba(&fmt, c));
            }
            Ok(())
        })
        .expect("stripes");
    app.run("layer.smartObjects.convertToSmartObject", json!({})).expect("a smart object");
    app.run("edit.transform", json!({"scaleX": 104, "scaleY": 123})).expect("scaled past the canvas");
    let st = app.session.active().expect("doc");
    let big = st.active_layer.expect("the big Smart Object");
    assert!(matches!(st.doc.layer(big).map(|l| &l.content), Some(photocraft_doc::LayerContent::Smart(_))), "a Smart Object");
    let b = st.doc.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds()).expect("pixels");
    eprintln!("the big Smart Object's pixels: {b:?}");
    big
}

fn screen(h: &Harness, x: f32, y: f32) -> egui::Pos2 {
    ViewXform::active(h.state()).expect("a view").to_screen(x, y)
}

/// The CPU composite of the document with `big` moved by `(dx, dy)`, as 8-bit straight RGBA.
fn expected(before: &photocraft_doc::Document, big: LayerId, dx: i32, dy: i32) -> Vec<[u8; 4]> {
    let moved = photocraft_engine::layer_multi_cmds::moved(before, &[big], dx, dy).expect("moved");
    let buf = photocraft_compose::render(&moved, moved.bounds());
    let mut out = vec![[0u8; 4]; buf.px.len()];
    photocraft_compose::quantize_rgba8(&mut out, &buf.px);
    out
}

/// A presented frame: RGBA8, physical pixels.
struct Frame {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

fn render(h: &mut Harness) -> Frame {
    let img = render(&mut h);
    Frame { w: img.width(), h: img.height(), px: img.into_raw() }
}

/// Fraction of sampled document pixels of the presented frame `img` that differ from `want` by
/// more than a few levels, away from `pointer` (the ΔX/ΔY readout beside it).
fn mismatch(h: &Harness, img: &Frame, want: &[[u8; 4]], pointer: egui::Pos2) -> f64 {
    let (mut n, mut bad) = (0u64, 0u64);
    for y in (6..H as i32 - 6).step_by(7) {
        for x in (6..W as i32 - 6).step_by(5) {
            let p = screen(h, x as f32 + 0.5, y as f32 + 0.5);
            if (p.x - pointer.x).abs() < 160.0 && (p.y - pointer.y).abs() < 100.0 {
                continue;
            }
            let (sx, sy) = (p.x.floor() as u32, p.y.floor() as u32);
            let Some(got) = img.px.get((sy as usize * img.w as usize + sx as usize) * 4..).filter(|_| sx < img.w && sy < img.h) else { continue };
            let w = want[y as usize * W as usize + x as usize];
            n += 1;
            if (0..3).any(|k| got[k].abs_diff(w[k]) > 12) {
                bad += 1;
            }
        }
    }
    bad as f64 / n.max(1) as f64
}

struct Frames {
    step_ms: Vec<f64>,
    worst: f64,
    worst_older: f64,
}

/// Drag the big Smart Object across the page over `frames` frames and check every presented frame.
fn drag(device: Device, frames: i32) -> Option<Frames> {
    let _gpu = photocraft_testkit::gpu_lock();
    if device == Device::LowMemoryGpu {
        crate::gpu_canvas::set_physical_memory(4 << 30);
    }
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let big = credits_page(h.state_mut());
    {
        let app = h.state_mut();
        app.sync_views();
        app.ui.tool = crate::state::Tool::Move;
        app.ui.tool_options.move_auto_select = true;
        app.ui.tool_options.move_show_transform = true;
        app.ui.extras.snap = false;
        app.ui.view.show.smart_guides = false;
        app.ui.extras.rulers = false;
        app.run("layer.select", json!({"layer": big.0})).expect("select");
    }
    let ctx = h.ctx.clone();
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": 1.0, "center": [W / 2, H / 2]}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(20);
    if device != Device::Cpu && !h.state().perf.gpu {
        eprintln!("skipping: the canvas fell back to the CPU here");
        return None;
    }
    let before = h.state().session.active().expect("doc").doc.clone();
    // Settled: the page as it is.
    let img = render(&mut h);
    let base = mismatch(&h, &img, &expected(&before, big, 0, 0), egui::pos2(-1e4, -1e4));
    assert!(base < 0.01, "{device:?}: the settled page differs from the CPU composite in {:.1}% of samples", base * 100.0);

    if device == Device::SlowGpu {
        crate::gpu_canvas::set_frame_work_ms(Some(0.0));
    }
    let start = [W as f32 / 2.0 + 3.0, H as f32 / 2.0 + 2.0];
    let p0 = screen(&h, start[0], start[1]);
    h.event(egui::Event::PointerMoved(p0));
    h.step();
    h.event(egui::Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let mut out = Frames { step_ms: Vec::new(), worst: 0.0, worst_older: 1.0 };
    let mut last = (0, 0);
    for i in 1..=frames {
        let (dx, dy) = (-(i * 23), -(i * 9));
        let p = screen(&h, start[0] + dx as f32, start[1] + dy as f32);
        // Pointer positions are document positions here (100%, no snapping).
        let p = egui::pos2(p.x.round() + (p0.x - p0.x.round()), p.y.round() + (p0.y - p0.y.round()));
        h.event(egui::Event::PointerMoved(p));
        let t = std::time::Instant::now();
        h.step();
        out.step_ms.push(t.elapsed().as_secs_f64() * 1000.0);
        let img = render(&mut h);
        let now = mismatch(&h, &img, &expected(&before, big, dx, dy), p);
        let older = mismatch(&h, &img, &expected(&before, big, last.0, last.1), p);
        eprintln!("{device:?} frame {i} at ({dx}, {dy}): {:.2}% off its offset, {:.2}% off the previous one", now * 100.0, older * 100.0);
        out.worst = out.worst.max(now);
        out.worst_older = out.worst_older.min(older);
        last = (dx, dy);
    }
    h.event(egui::Event::PointerButton { pos: screen(&h, start[0] + last.0 as f32, start[1] + last.1 as f32), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
    crate::gpu_canvas::set_frame_work_ms(None);
    let after = h.state().session.active().expect("doc").doc.clone();
    let moved = after.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds());
    let was = before.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds());
    assert_eq!(moved, was.map(|r| r.translate(last.0, last.1)), "{device:?}: the release moved the layer by the last offset");
    let mut sorted = out.step_ms.clone();
    sorted.sort_by(f64::total_cmp);
    eprintln!(
        "{device:?}: {} drag frames, frame work median {:.1} ms, max {:.1} ms; worst frame {:.2}% off its offset",
        sorted.len(),
        sorted[sorted.len() / 2],
        sorted.last().copied().unwrap_or(0.0),
        out.worst * 100.0
    );
    Some(out)
}

fn check(device: Device) {
    let Some(f) = drag(device, 10) else { return };
    assert!(f.worst < 0.01, "{device:?}: a drag frame showed {:.1}% of the page away from the drag's offset (parts drawn at older offsets)", f.worst * 100.0);
    assert!(f.worst_older > 0.05, "{device:?}: the frames really moved the layer");
}

#[test]
fn a_big_smart_object_drag_shows_every_frame_at_its_offset() {
    check(Device::Gpu);
}

#[test]
fn a_big_smart_object_drag_on_a_slow_gpu_never_shows_older_offsets() {
    check(Device::SlowGpu);
}

#[test]
fn a_big_smart_object_drag_with_4_gb_of_memory_shows_every_frame_at_its_offset() {
    check(Device::LowMemoryGpu);
}

#[test]
fn a_big_smart_object_drag_on_the_cpu_canvas_shows_every_frame_at_its_offset() {
    check(Device::Cpu);
}
