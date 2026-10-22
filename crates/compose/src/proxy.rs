//! Downsampled proxy documents: every pixel surface scaled down by an integer factor `k`, so a
//! composite of the proxy approximates the document's composite at 1/k resolution for ~1/k² of
//! the work. Used for interactive previews and for thumbnails of large documents (navigator,
//! channel thumbnails, histograms, file previews), where a full-resolution composite of a 200 MP
//! document would cost seconds and gigabytes.

use photocraft_doc::{Document, Layer, LayerContent, Size};
use photocraft_geom::Rect;
use photocraft_raster::Surface;

/// Nearest-neighbour downsample of a surface by integer factor `k` (document coordinates / k).
pub fn downsample(s: &Surface, k: u32) -> Surface {
    let mut out = Surface::with_default(s.format(), &s.default_pixel());
    let b = s.content_bounds();
    if b.is_empty() || k <= 1 {
        return if k <= 1 { s.clone() } else { out };
    }
    // Surfaces span at most ±2^30 px; larger factors reduce everything to one pixel anyway.
    let k = k.min(1 << 20) as i32;
    let bpp = s.format().bytes_per_pixel();
    let (x0, x1) = (b.x0.div_euclid(k), b.x1.div_euclid(k) + i32::from(b.x1.rem_euclid(k) != 0));
    let (y0, y1) = (b.y0.div_euclid(k), b.y1.div_euclid(k) + i32::from(b.y1.rem_euclid(k) != 0));
    let w = (x1 - x0).max(0) as usize;
    let mut row = vec![0u8; w * bpp];
    for oy in y0..y1 {
        let src = s.to_interleaved(Rect::new(x0 * k, oy * k, x1 * k, oy * k + 1));
        for (i, px) in row.chunks_exact_mut(bpp).enumerate() {
            px.copy_from_slice(&src[i * k as usize * bpp..(i * k as usize + 1) * bpp]);
        }
        out.write_interleaved(Rect::new(x0, oy, x1, oy + 1), &row);
    }
    out
}

/// Scale what a layer measures in document pixels outside its surfaces by `1 / k`: its effects'
/// sizes, distances and pattern scales, its effects' reference point and its vector mask, so a
/// proxy draws them at the proxy's scale.
fn shrink_vectors(l: &mut Layer, k: f32) {
    use photocraft_doc::effects::{Effect, FxPaint};
    let paint = |p: &mut FxPaint| {
        if let FxPaint::Pattern { scale, .. } = p {
            *scale /= k;
        }
    };
    for e in &mut l.effects.items {
        match e {
            Effect::DropShadow(s) | Effect::InnerShadow(s) => {
                s.distance /= k;
                s.size /= k;
            }
            Effect::OuterGlow(g) | Effect::InnerGlow(g) => {
                g.size /= k;
                paint(&mut g.paint);
            }
            Effect::Stroke(st) => {
                st.size /= k;
                paint(&mut st.paint);
            }
            Effect::PatternOverlay { scale, phase, .. } => {
                *scale /= k;
                *phase = (phase.0 / k, phase.1 / k);
            }
            Effect::Satin(s) => {
                s.distance /= k;
                s.size /= k;
            }
            Effect::BevelEmboss(b) => {
                b.size /= k;
                b.soften /= k;
                if let Some(t) = &mut b.texture {
                    t.scale /= k;
                    t.phase = (t.phase.0 / k, t.phase.1 / k);
                }
            }
            Effect::ColorOverlay { .. } | Effect::GradientOverlay { .. } => {}
        }
    }
    let kd = f64::from(k);
    if let Some((x, y)) = &mut l.effects.reference {
        *x /= kd;
        *y /= kd;
    }
    if let Some(v) = &mut l.vector_mask {
        v.path = v.path.transform(&photocraft_geom::Affine::scale(1.0 / kd));
        v.feather /= k;
    }
}

fn shrink_layer(l: &mut Layer, k: u32, fmt: photocraft_color::PixelFormat, shrink: &mut dyn FnMut(&Surface) -> Surface) {
    shrink_vectors(l, k as f32);
    if let Some(m) = &mut l.mask {
        m.surface = shrink(&m.surface);
    }
    if let Some(fc) = &mut l.fill_cache {
        fc.surface = shrink(&fc.surface);
    }
    match &mut l.content {
        LayerContent::Raster(s) => *s = shrink(s),
        LayerContent::Group(g) => {
            if let Some(ab) = &mut g.artboard {
                let divisor = i64::from(k);
                // Pixel x samples source x*k: both half-open edges round up, including negatives.
                let edge = |v: i32| {
                    let v = i64::from(v);
                    (v.div_euclid(divisor) + i64::from(v.rem_euclid(divisor) != 0)) as i32
                };
                ab.rect = Rect::new(edge(ab.rect.x0), edge(ab.rect.y0), edge(ab.rect.x1), edge(ab.rect.y1));
            }
            for c in &mut g.children {
                shrink_layer(c, k, fmt, shrink);
            }
        }
        LayerContent::Text(t) => t.cache = t.cache.as_ref().map(&mut *shrink),
        LayerContent::Shape(sh) => sh.cache = sh.cache.as_ref().map(&mut *shrink),
        LayerContent::Smart(so) => so.cache = so.cache.as_ref().map(&mut *shrink),
        // Deep data does not scale: the proxy flattens it once and downsamples that.
        LayerContent::Deep(d) => {
            // A model the deep flattener cannot target (CMYK, Lab): keep the deep layer as is.
            // The proxy is an optimisation; the full-resolution composite handles it.
            if let Ok(flat) = crate::deep::flat_surface(d, fmt) {
                l.content = LayerContent::Raster(shrink(&flat));
            }
        }
        LayerContent::Adjustment(_) | LayerContent::Fill(_) => {}
    }
}

/// A copy of `doc` scaled down by `k` (same layer ids, so adjustments can be swapped in). The
/// selection scales with it, so a filter previewed on the proxy stays inside the (feathered)
/// selection exactly as the full-size result does.
pub fn proxy_document(doc: &Document, k: u32) -> Document {
    proxy_document_with(doc, k, &mut |s| downsample(s, k))
}

/// [`proxy_document`] with each surface scaled down by `shrink` (which must scale by `k`, as
/// [`downsample`] or [`crate::mip::MipCache::surface`] do).
pub fn proxy_document_with(doc: &Document, k: u32, shrink: &mut dyn FnMut(&Surface) -> Surface) -> Document {
    let mut p = doc.clone();
    if k <= 1 {
        return p;
    }
    p.size = Size::new(doc.size.width.div_ceil(k), doc.size.height.div_ceil(k));
    let fmt = p.pixel_format();
    for l in &mut p.layers {
        shrink_layer(l, k, fmt, shrink);
    }
    // Spot channels are printed under Multichannel composites.
    for c in &mut p.channels {
        c.surface = shrink(&c.surface);
    }
    p.selection = p.selection.as_ref().map(shrink);
    p
}

/// Whether a proxy composite approximates `doc`'s composite at its scale (zoomed-out canvas
/// views): effects and vector masks are scaled with it ([`proxy_document_with`]); a stroked shape
/// layer's vector stroke is drawn from its path in document pixels at render time, so it is not.
pub fn lod_faithful(doc: &Document) -> bool {
    doc.walk().iter().all(|(_, _, l)| !matches!(&l.content, LayerContent::Shape(sh) if sh.stroke.is_some()))
}

/// Whether a proxy composite is a faithful reduction of `doc`: layer effects (whose sizes are in
/// document pixels) and vector masks (paths in document coordinates) would not scale with it.
pub fn proxy_faithful(doc: &Document) -> bool {
    doc.walk().iter().all(|(_, _, l)| !crate::effects::has_effects(l) && l.vector_mask.as_ref().is_none_or(|v| !v.enabled))
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{Color, ColorMode, PixelFormat, SampleType};

    #[test]
    fn downsample_picks_every_kth_pixel() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(0, 0, 8, 8), &[1.0, 0.0, 0.0, 1.0]);
        s.write_pixel(4, 4, &[0.0, 0.0, 1.0, 1.0]);
        let d = downsample(&s, 4);
        assert_eq!(d.pixel(0, 0), vec![1.0, 0.0, 0.0, 1.0]);
        assert_eq!(d.pixel(1, 1), vec![0.0, 0.0, 1.0, 1.0]);
        assert_eq!(d.pixel(2, 2)[3], 0.0);
    }

    #[test]
    fn downsample_handles_negative_coordinates() {
        let mut s = Surface::new(PixelFormat::RGBA8);
        s.fill_rect(Rect::new(-8, -8, 0, 0), &[0.0, 1.0, 0.0, 1.0]);
        let d = downsample(&s, 4);
        assert_eq!(d.pixel(-1, -1), vec![0.0, 1.0, 0.0, 1.0]);
        assert_eq!(d.pixel(0, 0)[3], 0.0);
    }

    #[test]
    fn proxy_keeps_structure_and_ids() {
        let doc = Document::with_background("d", Size::new(4000, 3000), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let p = proxy_document(&doc, 4);
        assert_eq!((p.size.width, p.size.height), (1000, 750));
        assert_eq!(p.layers[0].id, doc.layers[0].id);
        assert_eq!(p.layers[0].surface().unwrap().pixel(999, 749), vec![1.0; 4]);
        assert!(proxy_faithful(&doc));
    }

    #[test]
    fn proxy_scales_effects_and_vector_masks() {
        use photocraft_doc::effects::{Effect, FxCommon, Shadow};
        let mut doc = Document::with_background("d", Size::new(400, 400), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let shadow = Shadow {
            common: FxCommon::new(photocraft_color::blend::BlendMode::Multiply, 0.75),
            color: Color::BLACK,
            angle: 120.0,
            use_global_light: false,
            distance: 20.0,
            spread: 0.1,
            size: 8.0,
            contour: Default::default(),
            anti_alias: false,
            noise: 0.0,
            knocks_out: true,
        };
        doc.layers[0].effects.items.push(Effect::DropShadow(shadow));
        doc.layers[0].effects.reference = Some((40.0, 80.0));
        doc.layers[0].vector_mask =
            Some(photocraft_doc::VectorMask::new(photocraft_doc::vector::Path::new(vec![photocraft_doc::vector::Subpath::polygon(&[
                (0.0, 0.0),
                (200.0, 0.0),
                (200.0, 100.0),
            ])])));
        let p = proxy_document(&doc, 4);
        let Effect::DropShadow(s) = &p.layers[0].effects.items[0] else { panic!() };
        assert_eq!((s.distance, s.size, s.spread), (5.0, 2.0, 0.1));
        assert_eq!(p.layers[0].effects.reference, Some((10.0, 20.0)));
        let knot = &p.layers[0].vector_mask.as_ref().unwrap().path.subpaths[0].knots[2];
        assert_eq!((knot.anchor.x, knot.anchor.y), (50.0, 25.0));
        assert!(lod_faithful(&doc));
    }

    #[test]
    fn proxy_scales_the_selection() {
        let mut doc = Document::with_background("d", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let mut sel = Surface::new(PixelFormat::GRAY8);
        sel.fill_rect(Rect::new(0, 0, 32, 64), &[1.0]);
        doc.selection = Some(sel);
        let p = proxy_document(&doc, 4);
        let s = p.selection.as_ref().expect("selection kept");
        assert_eq!((s.pixel(7, 15)[0], s.pixel(8, 0)[0]), (1.0, 0.0));
        assert!(proxy_document(&Document::new("n", Size::new(8, 8), ColorMode::Rgb, SampleType::U8), 4).selection.is_none());
    }
}
