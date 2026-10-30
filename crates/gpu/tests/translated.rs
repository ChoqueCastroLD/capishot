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
        Translated { layer: b.id, mask: false, shown: surface_fingerprint(a.surface().expect("raster")), source: b.surface().expect("raster").clone(), offset },
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

/// A floating selection's preview (`photocraft_engine::float_cmds::CutParts::split`): the piece is
/// a new layer over the cut-out one, drawn from its resident pixels where it was cut at each
/// offset. The result equals compositing the moved copy, and once the piece is resident moving it
/// uploads no tile.
#[test]
fn a_piece_layer_drawn_at_each_offset_uploads_nothing_once_resident() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let base = doc();
    let fmt = base.pixel_format();
    let mut piece = photocraft_raster::Surface::new(fmt);
    piece.fill_rect(Rect::new(120, 90, 390, 330), &photocraft_raster::from_rgba(&fmt, [0.2, 0.7, 0.3, 1.0]));
    piece.fill_rect(Rect::new(200, 150, 260, 410), &photocraft_raster::from_rgba(&fmt, [0.9, 0.9, 0.2, 0.5]));
    let id = photocraft_doc::LayerId::fresh();
    let below = base.layers[1].id;
    let at = |dx: i32, dy: i32| {
        let mut d = base.clone();
        let mut l = Layer::new("piece", photocraft_doc::LayerContent::Raster(piece.translated(dx, dy, piece.content_bounds())));
        l.id = id;
        d.insert_above(Some(below), l);
        d
    };
    for (i, offset) in [(0, 0), (9, 4), (130, -60), (-77, 201), (300, 0)].into_iter().enumerate() {
        let shown = at(offset.0, offset.1);
        let mut plain = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut plain, &device, &queue, &shown, shown.bounds()).expect("reference");
        let moved = shown.layer(id).and_then(|l| l.surface()).expect("piece");
        comp.set_translated(vec![Translated { layer: id, mask: false, shown: surface_fingerprint(moved), source: piece.clone(), offset }]);
        let (got, stats) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &shown, shown.bounds()).expect("translated");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "offset {offset:?}: max diff {:.2}/255", worst * 255.0);
        if i > 0 {
            assert_eq!(stats.tiles_uploaded, 0, "offset {offset:?}: the piece and every other layer are resident");
        }
    }
}

/// A Move drag drawn from the unmoved layer (`Translated::in_place`): the document keeps its
/// pixels where they are, the plan places the layer's bounds at the offset, and every frame of
/// the drag equals compositing the moved copy, with nothing uploaded once resident and the
/// lower-stack cache never serving the moving layer's old place.
#[test]
fn in_place_moves_draw_like_the_moved_copy() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let mut base = doc();
    // More stack above, so the lower-stack cache has a checkpoint above the moving layer.
    let fmt = base.pixel_format();
    let mut top = Layer::raster("top", fmt);
    top.surface_mut().expect("raster").fill_rect(Rect::new(500, 20, 690, 200), &photocraft_raster::from_rgba(&fmt, [0.3, 0.3, 0.9, 0.5]));
    base.layers.push(top);
    let l = &base.layers[1];
    let (id, content, mask) = (l.id, l.surface().expect("raster").clone(), l.mask.as_ref().expect("mask").surface.clone());
    photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("resident");
    for (i, offset) in [(0, 0), (37, -23), (40, -20), (-256, 512), (5, 0), (-400, -300)].into_iter().enumerate() {
        let shown = moved(&base, offset.0, offset.1);
        let mut plain = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut plain, &device, &queue, &shown, shown.bounds()).expect("reference");
        comp.set_translated(vec![Translated::in_place(id, false, &content, offset), Translated::in_place(id, true, &mask, offset)]);
        let (got, stats) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("in place");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "offset {offset:?}: max diff {:.2}/255", worst * 255.0);
        if i > 0 {
            assert_eq!(stats.tiles_uploaded, 0, "offset {offset:?}: everything is resident");
        }
        // A damage rect of the move (old and new place) alone matches too.
        let r = Rect::new(30, 20, 430, 360).union(&Rect::new(30, 20, 430, 360).translate(offset.0, offset.1)).intersect(&base.bounds());
        let (part, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, r).expect("damage");
        let w = base.size.width as usize;
        for y in 0..r.height() as usize {
            for x in 0..r.width() as usize {
                let (a, b) = (part[y * r.width() as usize + x], want[(r.y0 as usize + y) * w + r.x0 as usize + x]);
                assert!((0..4).all(|k| (a[k] - b[k]).abs() <= 1.0 / 255.0), "offset {offset:?} damage at {x},{y}");
            }
        }
    }
    // A layer with effects is built from where it is: refused, not drawn wrong.
    let mut fx = base.clone();
    fx.layers[1].effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    comp.set_translated(vec![Translated::in_place(id, false, &content, (10, 10))]);
    assert!(photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &fx, fx.bounds()).is_err());
    assert!(!photocraft_gpu::plan::movable_in_place(&fx.layers[1]));
    assert!(photocraft_gpu::plan::movable_in_place(&base.layers[1]));
}
