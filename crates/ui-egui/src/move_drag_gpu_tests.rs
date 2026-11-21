//! Move drags: a big Smart Object on a credits page (the owner's "creditos villana embarazada"
//! report, 0.13.28), and a layer at the bottom of a 16-layer stack. Every frame of a drag must show
//! the whole canvas at the drag's offset as of that frame's pointer event (not the one before),
//! never parts of it at older offsets (vertical bands where the slow-GPU frame budget left pieces
//! of the refresh for frames that never came while the pointer kept moving), and stay cheap: the
//! moving layer drawn in place, the layers above it from maps made once for the drag
//! (`gpu_canvas::above`). Drives the real app offscreen with real pointer events, compares every
//! presented frame with the CPU composite of the document moved to that frame's offset, and prints
//! the per-frame cost. Skips without a GPU adapter.

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

/// What is dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Page {
    /// The big Smart Object on top of the credits page.
    Credits,
    /// A layer at the bottom of a 16-layer stack (just above the Background), the layers above
    /// cached for the drag.
    Bottom,
    /// The same with an Overlay layer above (not affine: composited whole every frame).
    BottomOverlay,
    /// [`Page::Bottom`] with the cache of the layers above turned off (the measure to compare).
    BottomUncached,
    /// The same 16 layers with the dragged one on top.
    Top,
    /// [`Page::Bottom`] dragged at once, before the drag cache is ready.
    BottomCold,
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
    app.run("select.rect", json!({"x": 0, "y": 700, "width": 1920, "height": 380})).expect("select");
    app.run("edit.fill", json!({"color": "#d8c8e8"})).expect("fill");
    app.run("select.deselect", json!({})).expect("deselect");
    for i in 0..6 {
        let x = 80 + i * 300;
        app.run("shape.create", json!({"rect": [x, 760, x + 220, 1000], "fill": "#6a7fb8"})).expect("a rectangle");
    }
    for (i, text) in ["typer: naxu", "facebook.com/shiliellano", "capitulo x", "gracias por leer", "creditos"].iter().enumerate() {
        let l = (app.run("type.create", json!({"x": 120, "y": 140 + i as u32 * 120, "text": text, "size": 64, "color": "#202020"})).expect("type"))["layer"].as_u64().expect("a type layer");
        app.run("layer.layerStyle.stroke", json!({"layer": l, "size": 6, "position": "outside", "color": "#ffffff"})).expect("stroke");
    }
    app.run("layer.new.layer", json!({"name": "mirai project icon"})).expect("a layer");
    app.run("select.rect", json!({"x": 1650, "y": 60, "width": 200, "height": 200})).expect("select");
    app.run("edit.fill", json!({"color": "#3070c0"})).expect("fill");
    app.run("select.deselect", json!({})).expect("deselect");
    app.run("layer.smartObjects.convertToSmartObject", json!({})).expect("a smart object");
    stripes_layer(app, "Gemini_Generated_Image");
    app.run("layer.smartObjects.convertToSmartObject", json!({})).expect("a smart object");
    app.run("edit.transform", json!({"scaleX": 104, "scaleY": 123})).expect("scaled past the canvas");
    let big = app.session.active().expect("doc").active_layer.expect("the big Smart Object");
    // An empty group on top, as on the page (Auto-Select picks the big Smart Object under it).
    app.run("layer.new.group", json!({"name": "Group 1"})).ok();
    app.run("layer.select", json!({"layer": big.0})).expect("select");
    let st = app.session.active().expect("doc");
    assert!(matches!(st.doc.layer(big).map(|l| &l.content), Some(photocraft_doc::LayerContent::Smart(_))), "a Smart Object");
    let b = st.doc.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds()).expect("pixels");
    eprintln!("the big Smart Object's pixels: {b:?}");
    big
}

/// A page of 16 layers with the dragged one at the bottom: the Background, the dragged stripes,
/// then five type layers with a Stroke, four rectangles, a Multiply and a Screen layer, a group
/// with a half-transparent layer in it, and a layer with a drop shadow. With `overlay`, the
/// Screen layer blends in Overlay instead. Returns the dragged layer's id.
fn bottom_page(app: &mut PhotocraftApp, overlay: bool) -> LayerId {
    stack_page(app, overlay, false)
}

/// [`bottom_page`], the dragged layer at the bottom (above the Background) or on `top`.
fn stack_page(app: &mut PhotocraftApp, overlay: bool, top: bool) -> LayerId {
    app.run("file.new", json!({"width": W, "height": H, "background": "white", "name": "pila"})).expect("a document");
    let mut moving = if top { None } else { Some(stripes_layer(app, "photo")) };
    for (i, text) in ["typer: naxu", "facebook.com/shiliellano", "capitulo x", "gracias por leer", "creditos"].iter().enumerate() {
        let l = (app.run("type.create", json!({"x": 120 + i as u32 * 40, "y": 140 + i as u32 * 120, "text": text, "size": 64, "color": "#202020"})).expect("type"))["layer"]
            .as_u64()
            .expect("a type layer");
        app.run("layer.layerStyle.stroke", json!({"layer": l, "size": 6, "position": "outside", "color": "#ffffff"})).expect("stroke");
    }
    for i in 0..4 {
        let x = 80 + i * 450;
        app.run("shape.create", json!({"rect": [x, 760, x + 220, 1000], "fill": "#6a7fb8"})).expect("a rectangle");
    }
    let tint = |app: &mut PhotocraftApp, name: &str, color: &str, rect: [i32; 4], blend: photocraft_color::BlendMode, opacity: f32| {
        let id = LayerId(app.run("layer.new.layer", json!({"name": name})).expect("a layer")["layer"].as_u64().expect("id"));
        app.run("select.rect", json!({"x": rect[0], "y": rect[1], "width": rect[2] - rect[0], "height": rect[3] - rect[1]})).expect("select");
        app.run("edit.fill", json!({"color": color})).expect("fill");
        app.run("select.deselect", json!({})).expect("deselect");
        app.session
            .edit("blend", |doc, _| {
                let l = doc.layer_mut(id).expect("layer");
                l.blend = blend;
                l.opacity = opacity;
                Ok(())
            })
            .expect("blend");
        id
    };
    tint(app, "multiply", "#e0a040", [300, 0, 1300, 700], photocraft_color::BlendMode::Multiply, 0.7);
    let light = if overlay { photocraft_color::BlendMode::Overlay } else { photocraft_color::BlendMode::Screen };
    tint(app, "light", "#3050c0", [900, 300, 1900, 1080], light, 0.6);
    let inner = tint(app, "veil", "#ffffff", [0, 500, 1920, 650], photocraft_color::BlendMode::Normal, 0.5);
    app.run("layer.select", json!({"layer": inner.0})).expect("select");
    app.run("layer.groupLayers", json!({"name": "Group 1"})).expect("group");
    let shadow = tint(app, "badge", "#c03030", [1500, 80, 1800, 300], photocraft_color::BlendMode::Normal, 1.0);
    app.run("layer.layerStyle.dropShadow", json!({"layer": shadow.0})).ok();
    if moving.is_none() {
        let groups = app.session.active().expect("doc").doc.layers.len();
        app.run("layer.select", json!({"layer": app.session.active().expect("doc").doc.layers[groups - 1].id.0})).expect("select");
        moving = Some(stripes_layer(app, "photo"));
    }
    let n = app.session.active().expect("doc").doc.walk().len();
    assert_eq!(n, 16, "16 layers");
    moving.expect("the dragged layer")
}

/// A new layer of uneven colour stripes over the whole canvas, a transparent gap beside each (the
/// layers below show through). Returns its id.
fn stripes_layer(app: &mut PhotocraftApp, name: &str) -> LayerId {
    app.run("layer.new.layer", json!({"name": name})).expect("a layer");
    app.session
        .edit("stripes", |doc, active| {
            let l = doc.layer_mut(active.expect("active")).expect("layer").surface_mut().expect("raster");
            let fmt = l.format();
            // No period a drag offset could line up with.
            let (mut x, mut i) = (0, 0);
            while x < W as i32 {
                let t = i as f32 * 0.37;
                let c = [0.5 + 0.45 * (t * 6.0).sin(), 0.5 + 0.45 * (t * 4.0 + 2.0).sin(), 0.5 + 0.45 * (t * 3.0 + 4.0).sin(), 1.0];
                let w = 9 + (i * 7919) % 29;
                l.fill_rect(Rect::new(x, 0, x + w, H as i32), &photocraft_raster::from_rgba(&fmt, c));
                x += w + 4 + (i * 104_729) % 11;
                i += 1;
            }
            Ok(())
        })
        .expect("stripes");
    app.session.active().expect("doc").active_layer.expect("the stripes")
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
    let img = h.render().expect("render");
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
    /// GPU drain after each drag frame.
    gpu_ms: Vec<f64>,
    /// Drag frames drawn from a moved copy rather than in place.
    copied: usize,
    /// Drag frames drawn from the drag cache (`gpu_canvas::above`), and the first one's index.
    cached: usize,
    cached_first: usize,
    worst: f64,
    worst_older: f64,
}

/// Drag `page`'s layer across it over `frames` frames and check every presented frame.
fn drag(device: Device, page: Page, frames: i32) -> Option<Frames> {
    let _gpu = photocraft_testkit::gpu_lock();
    if device == Device::LowMemoryGpu {
        crate::gpu_canvas::set_physical_memory(4 << 30);
    }
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let big = match page {
        Page::Credits => credits_page(h.state_mut()),
        Page::Bottom | Page::BottomUncached | Page::BottomCold => bottom_page(h.state_mut(), false),
        Page::BottomOverlay => bottom_page(h.state_mut(), true),
        Page::Top => stack_page(h.state_mut(), false, true),
    };
    crate::gpu_canvas::set_above_cache(page != Page::BottomUncached);
    {
        let app = h.state_mut();
        app.sync_views();
        app.ui.tool = crate::state::Tool::Move;
        // The press would pick whatever is under the pointer (a stripe or a gap): the selected
        // layer moves, as Auto-Select does on the Smart Object itself.
        app.ui.tool_options.move_auto_select = false;
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
    // A GL adapter builds the GPU compositor's pipelines one a frame first (`warm_up`).
    let doc_id = h.state().session.active().expect("doc").doc.id;
    for _ in 0..600 {
        let Some(g) = h.state().gpu.clone() else { break };
        let doc = h.state().session.active().expect("doc").doc.clone();
        if g.supports(&doc) {
            if !h.state().perf.last_refresh.starts_with("gpu") {
                // Drawn on the CPU while it warmed up: drawn again on the GPU.
                g.evict(doc_id.0);
                h.run_steps(4);
            }
            break;
        }
        g.warm_up();
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.step();
    }
    if device != Device::Cpu && !h.state().perf.last_refresh.starts_with("gpu") {
        eprintln!("skipping: the GPU compositor isn't drawing here ({})", h.state().perf.last_refresh);
        return None;
    }
    if device != Device::Cpu && !h.state().perf.gpu {
        eprintln!("skipping: the canvas fell back to the CPU here");
        return None;
    }
    eprintln!("{device:?} settled: {} {} px, fallback {:?}", h.state().perf.last_refresh, h.state().perf.last_refresh_px, h.state().perf.gpu_fallback);
    // The pointer rests on the page with the Move tool a moment: the idle frames make the drag
    // cache (unless the drag starts at once).
    if page != Page::BottomCold
        && let Some(g) = h.state().gpu.clone()
    {
        let t = std::time::Instant::now();
        let mut frames = 0;
        while !g.drag_ready(doc_id.0) && frames < 400 {
            h.step();
            frames += 1;
        }
        eprintln!("{device:?} {page:?}: drag cache ready {} after {frames} idle frames, {:.0} ms, {:.1} MB", g.drag_ready(doc_id.0), t.elapsed().as_secs_f64() * 1000.0, g.above_bytes() as f64 / 1048576.0);
    } else if device == Device::Cpu {
        // The CPU canvas makes its split ahead in idle frames too.
        h.run_steps(40);
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
    let mut out = Frames { step_ms: Vec::new(), gpu_ms: Vec::new(), copied: 0, cached: 0, cached_first: 0, worst: 0.0, worst_older: 1.0 };
    let mut last = (0, 0);
    for i in 1..=frames {
        let (dx, dy) = (-(i * 23), -(i * 9));
        let p = screen(&h, start[0] + dx as f32, start[1] + dy as f32);
        // Pointer positions are document positions here (100%, no snapping).
        let p = egui::pos2(p.x.round() + (p0.x - p0.x.round()), p.y.round() + (p0.y - p0.y.round()));
        h.event(egui::Event::PointerMoved(p));
        // The frame that takes the pointer event shows the layer there (the canvas paints after
        // the Move tool's events), and so does the next one, with no event.
        for k in 0..2 {
            let cache_frames = crate::gpu_canvas::drag_frames();
            crate::perf_span::begin_frame();
            let t = std::time::Instant::now();
            h.step();
            let cpu = t.elapsed().as_secs_f64() * 1000.0;
            // The GPU side: what was queued this frame, drained (lavapipe runs it on the CPU).
            let t = std::time::Instant::now();
            if let Some(g) = h.state().gpu.as_ref() {
                g.finish();
            }
            let gpu = t.elapsed().as_secs_f64() * 1000.0;
            if k == 0 {
                let spans: Vec<String> = crate::perf_span::frame().iter().take(8).map(|(n, ms)| format!("{n} {ms:.1}")).collect();
                let attributed: f64 = crate::perf_span::frame().iter().map(|(_, ms)| ms).sum();
                eprintln!("PROFILE {device:?} {page:?} move {i}: cpu {cpu:.1} ms (spans {attributed:.1}: {}), gpu drain {gpu:.1} ms", spans.join(", "));
                out.gpu_ms.push(gpu);
                out.step_ms.push(cpu);
                if !crate::move_ui::drawn_in_place(h.state(), 0) {
                    out.copied += 1;
                }
                let (sprite, layered) = crate::gpu_canvas::drag_frames();
                if (sprite, layered) != cache_frames {
                    if out.cached == 0 {
                        out.cached_first = i as usize;
                    }
                    out.cached += 1;
                }
            }
            let img = render(&mut h);
            let now = mismatch(&h, &img, &expected(&before, big, dx, dy), p);
            eprintln!(
                "{device:?} {page:?} move {i}.{k} at ({dx}, {dy}): {:.1} ms, {:.2}% off ({} {} px)",
                out.step_ms.last().copied().unwrap_or(0.0),
                now * 100.0,
                h.state().perf.last_refresh,
                h.state().perf.last_refresh_px
            );
            out.worst = out.worst.max(now);
            if k == 0 {
                let older = mismatch(&h, &img, &expected(&before, big, last.0, last.1), p);
                out.worst_older = out.worst_older.min(older);
            }
        }
        last = (dx, dy);
    }
    h.event(egui::Event::PointerButton { pos: screen(&h, start[0] + last.0 as f32, start[1] + last.1 as f32), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.run_steps(4);
    crate::gpu_canvas::set_frame_work_ms(None);
    crate::gpu_canvas::set_above_cache(true);
    let after = h.state().session.active().expect("doc").doc.clone();
    let moved = after.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds());
    let was = before.layer(big).and_then(|l| l.surface()).map(|s| s.content_bounds());
    assert_eq!(moved, was.map(|r| r.translate(last.0, last.1)), "{device:?} {page:?}: the release moved the layer by the last offset");
    // The drag frames, the first one aside.
    let mut sorted: Vec<f64> = out.step_ms.iter().skip(1).copied().collect();
    sorted.sort_by(f64::total_cmp);
    eprintln!(
        "{device:?} {page:?}: {} drag frames ({} from a moved copy, {} with the layers above cached), CPU first {:.1} ms, then median {:.1} ms, max {:.1} ms; GPU drain first {:.1} ms, then median {:.1} ms; worst frame {:.2}% off its offset",
        out.step_ms.len(),
        out.copied,
        out.cached,
        out.step_ms.first().copied().unwrap_or(0.0),
        sorted[sorted.len() / 2],
        sorted.last().copied().unwrap_or(0.0),
        out.gpu_ms.first().copied().unwrap_or(0.0),
        median(&out.gpu_ms[1.min(out.gpu_ms.len())..]),
        out.worst * 100.0
    );
    Some(out)
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    v.get(v.len() / 2).copied().unwrap_or(0.0)
}

fn check(device: Device, page: Page) {
    let frames = if page == Page::BottomCold { 40 } else { 10 };
    let Some(f) = drag(device, page, frames) else { return };
    if device != Device::Cpu {
        // The moving layer is drawn from its resident pages at the offset: no moved copy of its
        // pixels (nor anything rendered again) per frame.
        assert_eq!(f.copied, 0, "{device:?} {page:?}: drag frames drawn from a moved copy");
        match page {
            Page::Bottom | Page::Credits | Page::Top => assert_eq!(f.cached, 10, "{device:?} {page:?}: every drag frame comes from the cache made ahead"),
            Page::BottomOverlay | Page::BottomUncached => assert_eq!(f.cached, 0, "{device:?} {page:?}: composited whole"),
            Page::BottomCold => assert!(
                f.cached > 0 && f.cached < frames as usize && f.cached_first > 1,
                "{device:?}: whole frames until the cache is ready ({} of {frames} from it, first at {})",
                f.cached,
                f.cached_first
            ),
        }
    }
    assert!(
        f.worst < 0.01,
        "{device:?} {page:?}: a drag frame showed {:.1}% of the page away from the drag's offset (parts at older offsets, or a frame behind the pointer)",
        f.worst * 100.0
    );
    assert!(f.worst_older > 0.05, "{device:?} {page:?}: the frames really moved the layer");
}

#[test]
fn a_big_smart_object_drag_shows_every_frame_at_its_offset() {
    check(Device::Gpu, Page::Credits);
}

#[test]
fn a_big_smart_object_drag_on_a_slow_gpu_never_shows_older_offsets() {
    check(Device::SlowGpu, Page::Credits);
}

#[test]
fn a_big_smart_object_drag_with_4_gb_of_memory_shows_every_frame_at_its_offset() {
    check(Device::LowMemoryGpu, Page::Credits);
}

#[test]
fn a_big_smart_object_drag_on_the_cpu_canvas_shows_every_frame_at_its_offset() {
    check(Device::Cpu, Page::Credits);
}

#[test]
fn a_bottom_layer_drag_lays_the_15_layers_above_from_maps() {
    check(Device::Gpu, Page::Bottom);
}

#[test]
fn a_bottom_layer_drag_with_4_gb_of_memory_lays_the_layers_above_from_maps() {
    check(Device::LowMemoryGpu, Page::Bottom);
}

#[test]
fn a_bottom_layer_drag_on_a_slow_gpu_lays_the_layers_above_from_maps() {
    check(Device::SlowGpu, Page::Bottom);
}

#[test]
fn a_bottom_layer_drag_on_the_cpu_canvas_shows_every_frame_at_its_offset() {
    check(Device::Cpu, Page::Bottom);
}

#[test]
fn a_bottom_layer_drag_under_an_overlay_layer_composites_the_layers_above_whole() {
    check(Device::Gpu, Page::BottomOverlay);
}

#[test]
fn a_top_layer_drag_shows_every_frame_at_its_offset() {
    check(Device::Gpu, Page::Top);
}

#[test]
fn a_bottom_layer_drag_before_the_cache_is_ready_draws_whole_frames_then_switches() {
    check(Device::Gpu, Page::BottomCold);
}

#[test]
fn a_top_layer_drag_with_4_gb_of_memory_shows_every_frame_at_its_offset() {
    check(Device::LowMemoryGpu, Page::Top);
}

/// The same drag with the cache turned off: the measure to compare with (and the frames still
/// right).
#[test]
fn a_bottom_layer_drag_without_the_cache_of_the_layers_above() {
    check(Device::Gpu, Page::BottomUncached);
}
