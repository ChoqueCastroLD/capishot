//! Effect maps at the level of detail a render draws at (`Compositor::map_level`): a zoomed-out
//! view of an 8K page of 24 styled layers builds each layer's maps once, at the view's level,
//! within the effect budget, where the full-size maps were more than it holds and every render
//! built some again; the reduced maps differ from the full-size composite only by resampling.
//! Tests here count effect work process-wide: they take turns ([`serial`]).

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{BlendMode, Contour, Document, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Layer, Size, StrokeFx, StrokePosition};
use photocraft_geom::Rect;
use photocraft_gpu::{Compositor, level_bounds, render_to_vec_lod};

fn serial() -> std::sync::MutexGuard<'static, ()> {
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let format = Compositor::preferred_acc_format(&adapter);
    Some((device, queue, format))
}

fn stroke(size: f32) -> Effect {
    Effect::Stroke(StrokeFx { common: FxCommon::new(BlendMode::Normal, 1.0), size, position: StrokePosition::Outside, paint: FxPaint::Color(Color::WHITE), overprint: false })
}

fn glow(size: f32) -> Effect {
    Effect::OuterGlow(Glow {
        common: FxCommon::new(BlendMode::Screen, 0.75),
        paint: FxPaint::Color(Color::rgb(1.0, 1.0, 0.75)),
        technique: GlowTechnique::Softer,
        spread: 0.0,
        size,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    })
}

fn block(d: &Document, name: &str, r: Rect, rgba: [f32; 4], effects: Vec<Effect>) -> Layer {
    let fmt = d.pixel_format();
    let mut l = Layer::raster(name, fmt);
    l.surface_mut().expect("raster").fill_rect(r, &photocraft_raster::from_rgba(&fmt, rgba));
    l.effects.items = effects;
    l
}

/// The 8K bench page's styled layers (`an_8k_page_on_the_gpu_canvas`): 8 shapes of 1400x900
/// with a Drop Shadow and an 8 px Stroke, 16 lines of type (1300x150 blocks) with an Outer Glow
/// and a 6 px Stroke, over a white 7680x4320 page.
fn page_8k() -> Document {
    let (w, h) = (7680, 4320);
    let mut d = Document::with_background("8k", Size::new(w as u32, h as u32), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    for i in 0..8 {
        let (x, y) = (200 + (i % 4) * 1800, 300 + (i / 4) * (h / 2));
        let l = block(&d, &format!("shape {i}"), Rect::new(x, y, x + 1400, y + 900), [0.5, 0.56, 0.69, 1.0], vec![Effect::default_drop_shadow(), stroke(8.0)]);
        d.layers.push(l);
    }
    for i in 0..16 {
        let (x, y) = (300 + (i % 4) * 1800, 200 + (i / 4) * (h / 4));
        let l = block(&d, &format!("type {i}"), Rect::new(x, y, x + 1300, y + 150), [1.0, 1.0, 1.0, 1.0], vec![glow(5.0), stroke(6.0)]);
        d.layers.push(l);
    }
    d
}

#[test]
fn a_zoomed_out_8k_pages_maps_fit_the_effect_cache_and_are_built_once() {
    let _turn = serial();
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page_8k();
    let mut comp = Compositor::try_new_with_format(&device, format).expect("compositor");
    // The bench machine's budget: `gpu_canvas::memory_budget`'s floor, 512 MB (96 MB of maps).
    comp.set_memory_budget(512 << 20);
    // The page at fit in a 1000 px view (level 3), and as the canvas refines it a full-size
    // piece of its top left: three frames of both.
    let piece = Rect::new(0, 0, 1024, 1024);
    let mut built = Vec::new();
    for _ in 0..3 {
        let mut frame = 0;
        for (region, level) in [(doc.bounds(), 3), (piece, 0)] {
            match render_to_vec_lod(&mut comp, &device, &queue, &doc, region, level) {
                Ok((_, st)) => frame += st.fx_shapes,
                // GL without float render targets draws layer effects on the CPU.
                Err(e) => {
                    eprintln!("skipping: {}", e.0);
                    return;
                }
            }
        }
        built.push(frame);
    }
    eprintln!("effect shapes computed per frame: {built:?}; cache {:.1} MB", comp.fx_cache_bytes() as f64 / f64::from(1 << 20));
    assert!(built[0] >= 24, "the first frame builds every styled layer's maps");
    assert_eq!(built[1..], [0, 0], "the frames after built maps again");
}

/// `doc`'s CPU composite box-filtered down to `level` (premultiplied), as the canvas mips hold it.
fn reduced_cpu(doc: &Document, level: u32) -> Vec<[f32; 4]> {
    let full = photocraft_compose::flatten(doc);
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    let s = 1usize << level;
    let lb = level_bounds(doc.bounds(), level);
    let (lw, lh) = (lb.width() as usize, lb.height() as usize);
    let mut out = vec![[0.0f32; 4]; lw * lh];
    for y in 0..lh {
        for x in 0..lw {
            let mut acc = [0.0f32; 4];
            for dy in 0..s {
                for dx in 0..s {
                    let p = full.px[(y * s + dy).min(h - 1) * w + (x * s + dx).min(w - 1)];
                    for k in 0..3 {
                        acc[k] += p[k] * p[3];
                    }
                    acc[3] += p[3];
                }
            }
            out[y * lw + x] = acc.map(|v| v / (s * s) as f32);
        }
    }
    out
}

#[test]
fn reduced_effect_maps_differ_from_the_full_size_composite_only_by_resampling() {
    let _turn = serial();
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    // Balloons and type: a dark block with a shadow and a stroke, a light one with a glow and a
    // stroke, an inner shadow on a third.
    let mut d = Document::with_background("page", Size::new(1200, 800), ColorMode::Rgb, SampleType::U8, Color::rgb(0.9, 0.9, 0.85));
    let a = block(&d, "a", Rect::new(80, 90, 520, 400), [0.2, 0.25, 0.5, 1.0], vec![Effect::default_drop_shadow(), stroke(8.0)]);
    let b = block(&d, "b", Rect::new(600, 120, 1100, 260), [0.95, 0.95, 0.95, 1.0], vec![glow(12.0), stroke(6.0)]);
    let mut inner = Effect::default_drop_shadow();
    if let Effect::DropShadow(s) = inner {
        inner = Effect::InnerShadow(s);
    }
    let c = block(&d, "c", Rect::new(300, 480, 1000, 740), [0.8, 0.4, 0.2, 1.0], vec![inner, stroke(3.0)]);
    d.layers.extend([a, b, c]);
    let mut comp = Compositor::try_new_with_format(&device, format).expect("compositor");
    for level in 1..=3 {
        let got = match render_to_vec_lod(&mut comp, &device, &queue, &d, d.bounds(), level) {
            Ok((v, _)) => v,
            Err(e) => {
                eprintln!("skipping: {}", e.0);
                return;
            }
        };
        let want = reduced_cpu(&d, level);
        let (mut worst, mut sum) = (0.0f32, 0.0f32);
        for (g, w) in got.iter().zip(&want) {
            let g = [g[0] * g[3], g[1] * g[3], g[2] * g[3], g[3]];
            for k in 0..4 {
                worst = worst.max((g[k] - w[k]).abs());
                sum += (g[k] - w[k]).abs();
            }
        }
        let mean = sum / (want.len() * 4) as f32;
        eprintln!("level {level}: worst {:.1}/255, mean {:.2}/255", worst * 255.0, mean * 255.0);
        // Maps built from the shape the pixel stands for (the full-size ones read at its centre
        // were up to 32/255 off, 0.23/255 on average, at level 3; shadows offset by whole
        // reduced pixels 77/255).
        assert!(mean <= 1.0 / 255.0, "level {level}: mean {:.2}/255", mean * 255.0);
        assert!(worst <= 24.0 / 255.0, "level {level}: worst {:.1}/255", worst * 255.0);
    }
}

#[test]
fn a_preparation_once_the_frames_work_is_spent_builds_no_maps() {
    let _turn = serial();
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page_8k();
    let mut comp = Compositor::try_new_with_format(&device, format).expect("compositor");
    // The view of the page's top half at level 1, as a page fills in.
    let view = Rect::new(0, 0, 7680, 2160);
    let px = photocraft_gpu::fx_shape_px();
    match comp.prepare(&device, &queue, &doc, view, 1, &mut || false) {
        Ok(ready) => assert!(!ready, "nothing was built, yet the view was ready"),
        Err(e) => {
            eprintln!("skipping: {}", e.0);
            return;
        }
    }
    let spent = photocraft_gpu::fx_shape_px() - px;
    eprintln!("shape pixels computed with no time left: {spent}");
    assert_eq!(spent, 0, "a preparation with no time left built a layer's maps");
    // With time, the next calls build them, and the render then builds none.
    let mut calls = 1;
    while !comp.prepare(&device, &queue, &doc, view, 1, &mut || true).expect("prepare") {
        calls += 1;
        assert!(calls < 200, "the preparation never ends");
    }
    let (_, st) = render_to_vec_lod(&mut comp, &device, &queue, &doc, view, 1).expect("render");
    assert_eq!(st.fx_shapes, 0, "the render built maps the preparation left");
}

#[test]
fn the_effect_cache_lets_go_of_maps_outside_the_view_first() {
    let _turn = serial();
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    // Three areas of two styled balloons: the view's (A), then two drawn after it off the view
    // (B, C: a margin drawn ahead, a drag cache piece). Each a chunk (a render of one draws no
    // other's layers).
    let mut doc = Document::with_background("page", Size::new(3072, 1024), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let areas: Vec<Rect> = (0..3).map(|a| Rect::new(a * 1024, 0, a * 1024 + 1024, 1024)).collect();
    for (a, area) in areas.iter().enumerate() {
        for b in 0..2 {
            let (x, y) = (area.x0 + 60 + b * 450, 200);
            let l = block(&doc, &format!("balloon {a}{b}"), Rect::new(x, y, x + 400, y + 600), [0.3, 0.4, 0.8, 1.0], vec![Effect::default_drop_shadow(), stroke(8.0)]);
            doc.layers.push(l);
        }
    }
    let mut comp = Compositor::try_new_with_format(&device, format).expect("compositor");
    let shapes = |comp: &mut Compositor, r: Rect| render_to_vec_lod(comp, &device, &queue, &doc, r, 0).map(|(_, st)| st.fx_shapes);
    let Ok(built) = shapes(&mut comp, areas[0]) else {
        eprintln!("skipping: no effect maps on this adapter");
        return;
    };
    assert_eq!(built, 2);
    // Room for the view's maps whole and another area's, not for three areas' (a set's maps
    // without the distance fields and CPU shape they are built from are about half of it).
    let set = comp.fx_cache_bytes() as u64;
    comp.set_memory_budget(set * 13 / 10 * 16 / 3);
    comp.set_focus(Some(areas[0]));
    for r in [areas[1], areas[2]] {
        shapes(&mut comp, r).expect("off the view");
    }
    let again = shapes(&mut comp, areas[0]).expect("the view again");
    eprintln!("the view's maps built again: {again} (a set {:.1} MB, cache {:.1} MB)", set as f64 / f64::from(1 << 20), comp.fx_cache_bytes() as f64 / f64::from(1 << 20));
    assert_eq!(again, 0, "the view's maps were let go for maps drawn off it");
}
