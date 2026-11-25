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

/// A Smart Object larger than the canvas, inside a group, moved in place (a Move drag of a placed
/// image, 0.13.28): drawn from its rendered pixels at the offset, every frame equals compositing
/// the moved copy (the rendered pixels shifted, as the drag's copy shows them), pixels from past
/// the canvas edge coming into view, uploaded once.
#[test]
fn a_smart_object_in_a_group_moves_in_place_like_the_moved_copy() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let mut base = doc();
    let fmt = base.pixel_format();
    let mut cache = photocraft_raster::Surface::new(fmt);
    let mut x = -80;
    let mut i = 0;
    while x < 790 {
        let w = 9 + (i * 7919) % 29;
        cache.fill_rect(Rect::new(x, -60, x + w, 590), &photocraft_raster::from_rgba(&fmt, [(i % 7) as f32 / 7.0, 0.4, 1.0 - (i % 5) as f32 / 5.0, 1.0]));
        x += w + 4 + (i * 104_729) % 11;
        i += 1;
    }
    let source = photocraft_doc::SmartSource::Linked { path: "placed.png".into() };
    let smart = Layer::new("placed", photocraft_doc::LayerContent::Smart(photocraft_doc::SmartObject::new(source, photocraft_geom::Affine::IDENTITY, Some(cache.clone()))));
    let id = smart.id;
    let group = Layer::group("Group 1", vec![smart]);
    assert!(photocraft_gpu::plan::movable_in_place(&group), "a plain group");
    assert!(photocraft_gpu::plan::movable_in_place(&group.children().expect("group")[0]), "a Smart Object with rendered pixels");
    base.layers.push(group);
    photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("resident");
    // The first offset brings pixels from past the canvas edge into view (their pages upload
    // once); the next ones show nothing that isn't resident by then.
    for (i, offset) in [(37, -23), (30, -20), (5, 0), (-12, 7)].into_iter().enumerate() {
        let mut shown = base.clone();
        let l = shown.layer_mut(id).expect("smart");
        if let photocraft_doc::LayerContent::Smart(sm) = &mut l.content {
            sm.cache = Some(cache.translated(offset.0, offset.1, cache.content_bounds()));
        }
        let mut plain = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut plain, &device, &queue, &shown, shown.bounds()).expect("reference");
        comp.set_translated(vec![Translated::in_place(id, false, &cache, offset)]);
        let (got, stats) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("in place");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "offset {offset:?}: max diff {:.2}/255", worst * 255.0);
        if i > 0 && offset.0 >= 0 {
            assert_eq!(stats.tiles_uploaded, 0, "offset {offset:?}: everything is resident");
        }
    }
}

/// A styled layer dragged (drawn translated, at a new offset each frame) keeps its effect maps
/// without its shape being computed again to check that it moved unchanged: the check took 20
/// to 30 ms in every frame of a Move drag of a styled layer on an 8K strip. The frames still
/// equal compositing the moved copy.
#[test]
fn a_styled_layer_drawn_translated_keeps_its_maps_without_computing_its_shape_again() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let mut base = doc();
    base.layers[1].effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    comp.set_translated(Vec::new());
    photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &base, base.bounds()).expect("before");
    let mut computed = Vec::new();
    for offset in [(4, 2), (9, 5), (15, 8), (22, 11), (-12, -6)] {
        let shown = moved(&base, offset.0, offset.1);
        let px = photocraft_gpu::fx_shape_px();
        comp.set_translated(translations(&base, &shown, offset));
        let (got, _) = photocraft_gpu::render_to_vec_stats(&mut comp, &device, &queue, &shown, shown.bounds()).expect("translated");
        computed.push(photocraft_gpu::fx_shape_px() - px);
        let mut plain = photocraft_gpu::Compositor::try_new_with_format(&device, wgpu::TextureFormat::Rgba32Float).expect("compositor");
        let (want, _) = photocraft_gpu::render_to_vec_stats(&mut plain, &device, &queue, &shown, shown.bounds()).expect("reference");
        let worst = want.iter().zip(&got).map(|(w, g)| (0..4).map(|k| (w[k] - g[k]).abs()).fold(0.0f32, f32::max)).fold(0.0f32, f32::max);
        assert!(worst <= 1.0 / 255.0, "offset {offset:?}: max diff {:.2}/255", worst * 255.0);
    }
    eprintln!("effect shape pixels computed per frame of the drag: {computed:?}");
    assert!(computed[1..].iter().all(|px| *px == 0), "the shape was computed again as the layer moved: {computed:?}");
}

/// The same drag zoomed out (level 3, maps at that level): the reduced maps move with the layer
/// wherever it lands, so no frame after the first (which starts from the unmoved layer) builds
/// them anew.
#[test]
fn a_styled_layer_dragged_zoomed_out_keeps_its_reduced_maps() {
    let Some((device, queue, mut comp)) = gpu() else { return };
    let mut base = doc();
    base.layers[1].effects.items.push(photocraft_doc::Effect::default_drop_shadow());
    comp.set_translated(Vec::new());
    photocraft_gpu::render_to_vec_lod(&mut comp, &device, &queue, &base, base.bounds(), 3).expect("before");
    let mut built = Vec::new();
    for offset in [(4, 2), (9, 5), (15, 8), (22, 11), (-12, -6)] {
        let shown = moved(&base, offset.0, offset.1);
        comp.set_translated(translations(&base, &shown, offset));
        let (_, st) = photocraft_gpu::render_to_vec_lod(&mut comp, &device, &queue, &shown, shown.bounds(), 3).expect("translated");
        built.push(st.fx_shapes);
    }
    eprintln!("effect shapes built per frame of the drag at level 3: {built:?}");
    assert!(built[1..].iter().all(|n| *n == 0), "the reduced maps were built again as the layer moved: {built:?}");
}
