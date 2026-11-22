//! A styled layer whose pixels grow and shrink in place, as type does with each keystroke (it is
//! rendered anew: every tile a new one, one glyph more): its effect maps keep what the old and new
//! regions share and compute only the changed pixels and the part added. They match the maps a
//! compositor builds from scratch, and none is built again.

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

/// A page with a line of dark blocks on a layer with a Stroke, an Outer Glow and a Drop Shadow.
fn page() -> Document {
    let mut d = Document::with_background("page", Size::new(1400, 500), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let mut l = Layer::raster("line", d.pixel_format());
    // Coloured on the white page, so a stale map shows.
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
    d.layers.push(l);
    d
}

/// `doc` with its line drawn again on a new surface (as a type layer is rendered), `glyphs` blocks
/// long.
fn typed(doc: &Document, glyphs: i32) -> Document {
    let mut d = doc.clone();
    let fmt = d.pixel_format();
    let mut s = photocraft_raster::Surface::new(fmt);
    for g in 0..glyphs {
        let x = 120 + g * 46;
        s.fill_rect(Rect::new(x, 200 + (g % 3) * 4, x + 34, 262), &photocraft_raster::from_rgba(&fmt, [0.1, 0.1, 0.1, 1.0]));
    }
    *d.layers[1].surface_mut().expect("raster") = s;
    d
}

#[test]
fn a_line_that_grows_and_shrinks_keeps_its_effect_maps() {
    let Some((device, queue, format)) = gpu() else {
        eprintln!("skipping: no GPU adapter");
        return;
    };
    let mut comp = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
    let first = typed(&page(), 8);
    if let Err(e) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &first, first.bounds()) {
        // GL without float render targets draws layer effects on the CPU.
        eprintln!("skipping: {}", e.0);
        return;
    }
    // Typed on (the line grows to the right), then two glyphs deleted.
    for glyphs in [9, 10, 11, 9] {
        let doc = typed(&first, glyphs);
        let b0 = photocraft_gpu::fx_builds();
        let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &doc, doc.bounds()).expect("render");
        assert_eq!(photocraft_gpu::fx_builds() - b0, 0, "{glyphs} glyphs: the effect maps were built again whole");
        let mut fresh = photocraft_gpu::Compositor::try_new_with_format(&device, format).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut fresh, &device, &queue, &doc, doc.bounds()).expect("reference");
        let off = worst(&want, &got);
        assert!(off <= 1.0 / 255.0, "{glyphs} glyphs: {:.2}/255 off the maps built from scratch", off * 255.0);
    }
}
