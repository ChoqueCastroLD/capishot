//! A Move drag's preview draws the moving layers from their resident pre-move pages at the
//! offset (`Compositor::set_translated`): the result must equal compositing the moved copy, and
//! the moved copy's tiles (all new) must not be uploaded.

use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerMask, Size};
use photocraft_geom::Rect;
use photocraft_gpu::{Translated, surface_fingerprint};

fn gpu() -> Option<(wgpu::Device, wgpu::Queue, photocraft_gpu::Compositor)> {
    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()?;
    let comp = photocraft_gpu::Compositor::try_new_with_format(&device, photocraft_gpu::Compositor::preferred_acc_format(&adapter)).ok()?;
    Some((device, queue, comp))
}

/// A white document with an opaque-ish painted layer (several tiles, ragged edges) under a
/// half-transparent one, the lower one with a linked mask.
fn doc() -> Document {
    let mut d = Document::with_background("t", Size::new(700, 520), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let fmt = d.pixel_format();
    let mut a = Layer::raster("a", fmt);
    let s = a.surface_mut().expect("raster");
    s.fill_rect(Rect::new(40, 30, 420, 350), &photocraft_raster::from_rgba(&fmt, [0.9, 0.2, 0.1, 1.0]));
    s.fill_rect(Rect::new(300, 200, 610, 470), &photocraft_raster::from_rgba(&fmt, [0.1, 0.5, 0.9, 0.7]));
    let mut m = LayerMask::reveal_all();
    m.surface.fill_rect(Rect::new(100, 0, 260, 520), &[0.25]);
    a.mask = Some(m);
    let mut b = Layer::raster("b", fmt);
    b.blend = BlendMode::Multiply;
    b.opacity = 0.6;
    b.surface_mut().expect("raster").fill_rect(Rect::new(0, 250, 700, 300), &photocraft_raster::from_rgba(&fmt, [0.2, 0.8, 0.2, 1.0]));
    d.layers.push(a);
    d.layers.push(b);
    d
}

fn moved(d: &Document, dx: i32, dy: i32) -> Document {
    let mut out = d.clone();
    let l = &mut out.layers[1];
    let s = l.surface_mut().expect("raster");
    *s = s.translated(dx, dy, s.content_bounds());
    if let Some(m) = l.mask.as_mut() {
        m.surface = m.surface.translated(dx, dy, m.surface.content_bounds());
    }
    out
}

fn translations(before: &Document, after: &Document, offset: (i32, i32)) -> Vec<Translated> {
    let (b, a) = (&before.layers[1], &after.layers[1]);
    let (Some(bm), Some(am)) = (b.mask.as_ref(), a.mask.as_ref()) else { return Vec::new() };
    vec![
        Translated {
            layer: b.id,
            mask: false,
            shown: surface_fingerprint(a.surface().expect("raster")),
            source: b.surface().expect("raster").clone(),
            offset,
        },
        Translated { layer: b.id, mask: true, shown: surface_fingerprint(&am.surface), source: bm.surface.clone(), offset },
    ]
}

#[test]
fn translated_layers_draw_like_the_moved_copy_without_uploading_it() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let base = doc();
    for offset in [(37, -23), (-256, 512), (5, 0), (-400, -300)] {
        let shown = moved(&base, offset.0, offset.1);
        // The reference: the moved copy as it is.
        let mut plain = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut plain, &device, &queue, &shown, shown.bounds()).expect("reference");
        // The pre-move document resident, then the moved one drawn from it.
        comp.set_translated(Vec::new());
        photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("before");
        comp.set_translated(translations(&base, &shown, offset));
        let (got, stats) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &shown, shown.bounds()).expect("translated");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "offset {offset:?}: max diff {:.2}/255", worst * 255.0);
        let moved_tiles = shown.layers[1].surface().map_or(0, |s| s.tile_count());
        assert!(stats.tiles_uploaded < moved_tiles / 2, "offset {offset:?}: {} tiles uploaded of {moved_tiles}", stats.tiles_uploaded);
        // A list that doesn't describe the surfaces drawn is ignored: still the moved copy.
        let mut stale = translations(&base, &shown, offset);
        for t in &mut stale {
            t.shown ^= 1;
        }
        comp.set_translated(stale);
        let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &shown, shown.bounds()).expect("stale");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "stale list, offset {offset:?}: max diff {:.2}/255", worst * 255.0);
    }
}

#[test]
fn fingerprints_follow_tile_identity() {
    let d = doc();
    let s = d.layers[1].surface().expect("raster");
    assert_eq!(surface_fingerprint(s), surface_fingerprint(&s.clone()), "a clone shares its tiles");
    let t = s.translated(3, 0, s.content_bounds());
    assert_ne!(surface_fingerprint(s), surface_fingerprint(&t));
    let back = t.translated(-3, 0, t.content_bounds());
    assert_ne!(surface_fingerprint(s), surface_fingerprint(&back), "same pixels in new tiles is another surface");
}
