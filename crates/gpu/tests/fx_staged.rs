//! A large styled layer's effect maps built ahead of a render (`Compositor::prepare`, a page
//! filling in or a drag cache made ahead) are built in parts over calls: its shape and distance
//! fields in bands, the maps once they are all there. One call built the whole layer (on the
//! owner's PC, 0.13.43: gpu.fx 120 to 146 ms in one frame on a 200-layer page's crop, its undo
//! and its opening). The maps match those a render builds in one go.

use photocraft_color::{Color, ColorMode, SampleType};
use photocraft_doc::{BlendMode, Contour, Document, Effect, FxCommon, FxPaint, Glow, GlowSource, GlowTechnique, Layer, Size, StrokeFx, StrokePosition};
use photocraft_geom::Rect;

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, wgpu::TextureFormat)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let format = photocraft_gpu::Compositor::preferred_acc_format(&adapter);
    Some((device, queue, format))
}

fn worst(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter().zip(b).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max)
}

/// A 1600x1200 page with blocks over most of it on a layer with a Stroke, an Outer Glow and a
/// Drop Shadow.
fn page() -> Document {
    let mut d = Document::with_background("page", Size::new(1600, 1200), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let mut l = Layer::raster("art", d.pixel_format());
    let red = FxPaint::Color(Color::rgb(0.9, 0.1, 0.1));
    l.effects.items.push(Effect::Stroke(StrokeFx { common: FxCommon::new(BlendMode::Normal, 1.0), size: 5.0, position: StrokePosition::Outside, paint: red, overprint: false }));
    l.effects.items.push(Effect::OuterGlow(Glow {
        common: FxCommon::new(BlendMode::Normal, 0.8),
        paint: FxPaint::Color(Color::rgb(0.1, 0.3, 0.9)),
        technique: GlowTechnique::Softer,
        spread: 0.1,
        size: 12.0,
        contour: Contour::Linear,
        anti_alias: false,
        range: 0.5,
        jitter: 0.0,
        noise: 0.0,
        source: GlowSource::Edge,
    }));
    l.effects.items.push(Effect::default_drop_shadow());
    let fmt = d.pixel_format();
    let s = l.surface_mut().expect("raster");
    for i in 0..60 {
        let (x, y) = (60 + (i % 10) * 150, 60 + (i / 10) * 180);
        s.fill_rect(Rect::new(x, y, x + 90 + (i % 3) * 10, y + 120), &photocraft_raster::from_rgba(&fmt, [0.1, 0.1, 0.1, 1.0]));
    }
    d.layers.push(l);
    d
}

#[test]
fn a_large_styled_layers_maps_are_prepared_in_parts() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let doc = page();
    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    if let Err(e) = comp.supports(&doc) {
        eprintln!("skipping: {}", e.0);
        return;
    }
    // No time left in any call: each does one part of the work.
    let (b0, mut calls, mut first_built, mut slowest) = (photocraft_gpu::fx_builds(), 0, None, 0.0f64);
    loop {
        let t = std::time::Instant::now();
        let ready = match comp.prepare(&device, &queue, &doc, doc.bounds(), 0, &mut || false) {
            Ok(r) => r,
            // GL without float render targets draws layer effects on the CPU.
            Err(e) if calls == 0 => {
                eprintln!("skipping: {}", e.0);
                return;
            }
            Err(e) => panic!("prepare: {}", e.0),
        };
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        eprintln!("call {calls}: {ms:.1} ms");
        slowest = slowest.max(ms);
        calls += 1;
        if first_built.is_none() && photocraft_gpu::fx_builds() > b0 {
            first_built = Some(calls);
        }
        if ready || calls > 1000 {
            break;
        }
    }
    eprintln!("prepared in {calls} calls, the maps built in call {first_built:?}, slowest call {slowest:.1} ms");
    assert!(first_built.is_some_and(|c| c > 4), "the layer's maps were built in call {first_built:?} (in parts: after several)");
    assert_eq!(photocraft_gpu::fx_builds() - b0, 1, "built once");
    let b1 = photocraft_gpu::fx_builds();
    let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).expect("render");
    assert_eq!(photocraft_gpu::fx_builds(), b1, "the render built no maps");
    let mut fresh = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    let (want, _) = photocraft_gpu::render_to_vec_stats(&mut fresh, &device, &queue, &doc, doc.bounds()).expect("reference");
    let off = worst(&want, &got);
    assert!(off <= 1.0 / 255.0, "{:.2}/255 off the maps built in one go", off * 255.0);
}
