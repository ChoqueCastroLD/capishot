use super::composite::composite_stack_at;
use super::composite::{blend_if_active, text_gamma};
use super::content::RENDER_TILE;
use super::content::channel_weights;
use super::ctx::{Ctx, RenderVectorMasks, prepare_effects, prepare_tones};
use super::{
    BlendMode, Buffer, Document, Layer, LayerContent, Rect, Surface, adjust, advanced, cmyk, effects, multichannel, native, native_lab, pattern, psblend,
};

/// Every layer the compositor draws, groups before their children: hidden layers and everything
/// inside hidden groups are left out.
pub(crate) fn visible_layers(layers: &[Layer]) -> Vec<&Layer> {
    fn rec<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>) {
        for l in layers.iter().filter(|l| l.visible) {
            out.push(l);
            if let Some(children) = l.children() {
                rec(children, out);
            }
        }
    }
    let mut out = Vec::new();
    rec(layers, &mut out);
    out
}

/// Composite the whole document over `rect`, in parallel 256² tiles on
/// native targets (single-threaded on wasm).
pub fn render(doc: &Document, rect: Rect) -> Buffer {
    render_tiled(doc, rect, RENDER_TILE)
}

/// Most pixels one [`render`] call will produce (2^30, a 16 GiB buffer; 32768² px). Larger rects
/// (a hostile canvas size is up to 300000 px a side) return an empty buffer instead of aborting
/// on allocation; callers wanting more render in pieces.
pub const MAX_RENDER_PIXELS: u64 = 1 << 30;

/// [`render`] with an explicit tile size (tests check tile independence).
pub fn render_tiled(doc: &Document, rect: Rect, tile: i32) -> Buffer {
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    let cx = Ctx::for_doc(doc, &patterns);
    render_tiled_with(doc, rect, tile, &cx)
}

pub(crate) fn render_tiled_with(doc: &Document, rect: Rect, tile: i32, cx: &Ctx) -> Buffer {
    let tile = tile.max(1);
    // Lab documents mix Normal blending in CIELAB, as Photoshop does (psblend::LAB_MIX); 32-bit
    // documents don't clip Add / Divide or Hue / Saturation / Color / Luminosity at 1 (psblend::HDR).
    let lab = doc.mode == photocraft_color::ColorMode::Lab;
    let hdr = doc.depth == photocraft_color::SampleType::F32;
    // CMYK layers are read through the document's own CMYK profile (thread-local scope).
    let cmyk = cmyk_space(doc);
    let cmyk = cmyk.as_ref();
    if rect.is_empty() {
        return Buffer::transparent(rect);
    }
    // Refuse rects too large to hold: an empty buffer, not an allocation failure.
    if u64::from(rect.width()) * u64::from(rect.height()) > MAX_RENDER_PIXELS {
        return Buffer::transparent(Rect::EMPTY);
    }
    // A CMYK stack with adjustment layers the inks carry exactly composites on its inks.
    if let Some(buf) = native::render_cmyk(doc, rect, tile) {
        return buf;
    }
    // A Lab stack with adjustment layers that work on L, a and b composites on its stored values.
    if let Some(buf) = native_lab::render_lab(doc, rect, tile) {
        return buf;
    }
    cx.set_fx_clip(rect);
    if i64::from(rect.width()) <= i64::from(tile) && i64::from(rect.height()) <= i64::from(tile) {
        return photocraft_color::convert::with_cmyk_space(cmyk, || {
            let mut buf = multichannel::backdrop(doc, rect);
            in_blend_space(lab, hdr, || composite_stack_at(&doc.layers, &mut buf, cx, advanced::Scope::ROOT));
            buf
        });
    }
    // Effect maps are built once, here, before any tile needs them (#276).
    prepare_effects(&doc.layers, rect, cx, |f| {
        photocraft_color::convert::with_cmyk_space(cmyk, || in_blend_space(lab, hdr, f));
    });
    let run = |t: Rect| {
        photocraft_color::convert::with_cmyk_space(cmyk, || {
            let mut b = multichannel::backdrop(doc, t);
            in_blend_space(lab, hdr, || composite_stack_at(&doc.layers, &mut b, cx, advanced::Scope::ROOT));
            b
        })
    };
    // Tiles are written straight into the output, one row of tiles (a band) at a time, so the
    // peak is the output plus the tiles in flight, not a second full-size copy.
    let w = rect.width() as usize;
    let mut out = Buffer::transparent(rect);
    let band_tiles = |y0: i32| {
        let y1 = y0.saturating_add(tile).min(rect.y1);
        let mut v = Vec::new();
        let mut x = rect.x0;
        while x < rect.x1 {
            let x1 = x.saturating_add(tile).min(rect.x1);
            v.push(Rect::new(x, y0, x1, y1));
            x = x1;
        }
        v
    };
    let put = |band: &mut [[f32; 4]], y0: i32, part: &Buffer| {
        let pw = part.rect.width() as usize;
        for (row, src) in part.px.chunks_exact(pw).enumerate() {
            let o = ((part.rect.y0 - y0) as usize + row) * w + (part.rect.x0 - rect.x0) as usize;
            band[o..o + pw].copy_from_slice(src);
        }
    };
    let band_len = w * tile as usize;
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    {
        use rayon::prelude::*;
        let bands = rect.height().div_ceil(tile as u32) as usize;
        if bands >= 2 * rayon::current_num_threads() {
            // Enough bands to keep every core busy: one band per task, its tiles in turn.
            out.px.par_chunks_mut(band_len).enumerate().for_each(|(i, band)| {
                let y0 = rect.y0 + i as i32 * tile;
                for t in band_tiles(y0) {
                    put(band, y0, &run(t));
                }
            });
        } else {
            // Few, wide bands: the tiles of each band in parallel, and the bands too, so a band's
            // slowest tile doesn't leave the other cores idle until the next band starts. Each
            // tile is written as soon as it is done, so only the tiles in flight are held.
            out.px.par_chunks_mut(band_len).enumerate().for_each(|(i, band)| {
                let y0 = rect.y0 + i as i32 * tile;
                let band = std::sync::Mutex::new(band);
                band_tiles(y0).into_par_iter().for_each(|t| {
                    let part = run(t);
                    let mut band = band.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                    put(&mut band, y0, &part);
                });
            });
        }
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    for (i, band) in out.px.chunks_mut(band_len).enumerate() {
        let y0 = rect.y0 + i as i32 * tile;
        for t in band_tiles(y0) {
            put(band, y0, &run(t));
        }
    }
    out
}

/// Composite each of `rects` (one buffer per rectangle, in order), sharing one preparation
/// (patterns, tone tables, effect maps over their union) and, in the threads build and natively,
/// rendering them in parallel. Each buffer equals [`render`] of its rectangle: a tiled canvas
/// renders the tiles a frame needs with this.
pub fn render_rects(doc: &Document, rects: &[Rect]) -> Vec<Buffer> {
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    let cx = Ctx::for_doc(doc, &patterns);
    let union = rects.iter().filter(|r| !r.is_empty()).fold(Rect::EMPTY, |u, r| if u.is_empty() { *r } else { u.union(r) });
    if union.is_empty() {
        return rects.iter().map(|r| Buffer::transparent(*r)).collect();
    }
    let lab = doc.mode == photocraft_color::ColorMode::Lab;
    let hdr = doc.depth == photocraft_color::SampleType::F32;
    let cmyk = cmyk_space(doc);
    let cmyk = cmyk.as_ref();
    cx.set_fx_clip(union);
    prepare_effects(&doc.layers, union, &cx, |f| {
        photocraft_color::convert::with_cmyk_space(cmyk, || in_blend_space(lab, hdr, f));
    });
    let run = |t: &Rect| {
        if t.is_empty() {
            return Buffer::transparent(*t);
        }
        photocraft_color::convert::with_cmyk_space(cmyk, || {
            let mut b = multichannel::backdrop(doc, *t);
            in_blend_space(lab, hdr, || composite_stack_at(&doc.layers, &mut b, &cx, advanced::Scope::ROOT));
            b
        })
    };
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    if rects.len() > 1 {
        use rayon::prelude::*;
        return rects.par_iter().map(run).collect();
    }
    rects.iter().map(run).collect()
}

/// [`render_rects`], but compositing `doc`'s layers over the given backdrops (one per rectangle,
/// their own rects) instead of the document's empty backdrop: the layers above a cached composite
/// of the layers below them (a Move drag on the CPU canvas re-blends only the moving layers).
pub fn render_rects_over(doc: &Document, backdrops: Vec<Buffer>) -> Vec<Buffer> {
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    let cx = Ctx::for_doc(doc, &patterns);
    let union = backdrops.iter().map(|b| b.rect).filter(|r| !r.is_empty()).fold(Rect::EMPTY, |u, r| if u.is_empty() { r } else { u.union(&r) });
    if union.is_empty() || doc.layers.is_empty() {
        return backdrops;
    }
    let lab = doc.mode == photocraft_color::ColorMode::Lab;
    let hdr = doc.depth == photocraft_color::SampleType::F32;
    let cmyk = cmyk_space(doc);
    let cmyk = cmyk.as_ref();
    cx.set_fx_clip(union);
    prepare_effects(&doc.layers, union, &cx, |f| {
        photocraft_color::convert::with_cmyk_space(cmyk, || in_blend_space(lab, hdr, f));
    });
    let run = |mut b: Buffer| {
        if b.rect.is_empty() {
            return b;
        }
        photocraft_color::convert::with_cmyk_space(cmyk, || {
            in_blend_space(lab, hdr, || composite_stack_at(&doc.layers, &mut b, &cx, advanced::Scope::ROOT));
        });
        b
    };
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    if backdrops.len() > 1 {
        use rayon::prelude::*;
        return backdrops.into_par_iter().map(run).collect();
    }
    backdrops.into_iter().map(run).collect()
}

/// `top` laid over `bottom` (Normal, straight alpha), where their rects overlap. A run of layers
/// that all [`composite_as_over`] composites to the same pixels (up to float rounding) as their
/// own composite over transparency laid over the backdrop with this.
pub fn over(bottom: &mut Buffer, top: &Buffer) {
    let r = bottom.rect.intersect(&top.rect);
    if r.is_empty() {
        return;
    }
    let (bw, tw) = (bottom.rect.width() as usize, top.rect.width() as usize);
    for y in r.y0..r.y1 {
        let bo = (y - bottom.rect.y0) as usize * bw + (r.x0 - bottom.rect.x0) as usize;
        let to = (y - top.rect.y0) as usize * tw + (r.x0 - top.rect.x0) as usize;
        let n = r.width() as usize;
        for (b, t) in bottom.px[bo..bo + n].iter_mut().zip(&top.px[to..to + n]) {
            let ta = t[3];
            if ta <= 0.0 {
                continue;
            }
            if ta >= 1.0 {
                *b = *t;
                continue;
            }
            let ba = b[3] * (1.0 - ta);
            let a = ta + ba;
            *b = if a > 0.0 { [(t[0] * ta + b[0] * ba) / a, (t[1] * ta + b[1] * ba) / a, (t[2] * ta + b[2] * ba) / a, a] } else { [0.0; 4] };
        }
    }
}

/// Whether `layer` composites as its own image over the backdrop (Normal blend, no text gamma,
/// effects, Blend If, channel restrictions, knockout or other advanced blending, not clipped and
/// not an adjustment; a pass-through group when all its children do), so that a run of such
/// layers can be flattened once and laid [`over`] whatever is below it. Hidden layers do.
pub fn composites_as_over(layer: &Layer, mode: photocraft_color::ColorMode) -> bool {
    if !layer.visible {
        return true;
    }
    if layer.clipped
        || matches!(layer.content, LayerContent::Adjustment(_))
        || effects::has_effects(layer)
        || blend_if_active(layer, mode)
        || channel_weights(layer, mode).is_some()
        || advanced::advanced_active(layer, &[])
        || !layer.advanced.is_default()
        || text_gamma(layer) != 1.0
    {
        return false;
    }
    match (&layer.content, layer.blend) {
        (LayerContent::Group(g), BlendMode::PassThrough) => {
            layer.fill_opacity >= 1.0 && g.artboard.is_none() && g.children.iter().all(|c| composites_as_over(c, mode))
        }
        (LayerContent::Group(g), BlendMode::Normal) => g.artboard.is_none() && no_knockout(&g.children),
        (_, BlendMode::Normal) => true,
        _ => false,
    }
}

/// Whether `layer` changes an opaque backdrop's colour affinely, `c -> m·c + n` per pixel and
/// channel (Normal, Multiply and Screen blending; isolated groups in those modes; pass-through
/// groups of such layers; effects in those modes; clipped layers in those modes), so that a run of
/// such layers is captured exactly by its composite over black and over white ([`apply_affine`]).
/// Type layers blend in a gamma space: affine where their glyphs are opaque, approximated on the
/// anti-aliased edges (`approx_text`).
pub fn composites_affinely(layer: &Layer, mode: photocraft_color::ColorMode, approx_text: bool) -> bool {
    use photocraft_doc::Effect;
    if !layer.visible {
        return true;
    }
    let affine_mode = |m: BlendMode| matches!(m, BlendMode::Normal | BlendMode::Multiply | BlendMode::Screen);
    if matches!(layer.content, LayerContent::Adjustment(_))
        || blend_if_active(layer, mode)
        || channel_weights(layer, mode).is_some()
        || !layer.advanced.is_default()
        || (text_gamma(layer) != 1.0 && !approx_text)
    {
        return false;
    }
    let fx_ok = layer.effects.items.iter().filter(|e| e.enabled()).all(|e| match e {
        Effect::DropShadow(s) | Effect::InnerShadow(s) => affine_mode(s.common.blend),
        Effect::OuterGlow(g) | Effect::InnerGlow(g) => affine_mode(g.common.blend),
        Effect::Stroke(st) => affine_mode(st.common.blend),
        Effect::ColorOverlay { common, .. } | Effect::GradientOverlay { common, .. } | Effect::PatternOverlay { common, .. } => affine_mode(common.blend),
        Effect::Satin(s) => affine_mode(s.common.blend),
        Effect::BevelEmboss(_) => false,
    });
    if !fx_ok {
        return false;
    }
    match (&layer.content, layer.blend) {
        (LayerContent::Group(g), BlendMode::PassThrough) => {
            g.artboard.is_none() && !effects::has_effects(layer) && g.children.iter().all(|c| composites_affinely(c, mode, approx_text))
        }
        (LayerContent::Group(g), m) => g.artboard.is_none() && affine_mode(m) && no_knockout(&g.children),
        (_, m) => affine_mode(m),
    }
}

/// Lay a cached affine map over `buf`: `black` and `white` are the same layers composited over
/// opaque black and opaque white, so each colour becomes `black + (white − black)·c`. Pixels of
/// `buf` that aren't opaque are left alone; returns whether every pixel was opaque (else the
/// caller composites those layers itself).
pub fn apply_affine(buf: &mut Buffer, black: &Buffer, white: &Buffer) -> bool {
    let r = buf.rect.intersect(&black.rect).intersect(&white.rect);
    if r != buf.rect {
        return false;
    }
    if buf.px.iter().any(|p| p[3] < 1.0) {
        return false;
    }
    let (bw, kw) = (buf.rect.width() as usize, black.rect.width() as usize);
    for y in r.y0..r.y1 {
        let bo = (y - buf.rect.y0) as usize * bw;
        let ko = (y - black.rect.y0) as usize * kw + (r.x0 - black.rect.x0) as usize;
        let n = r.width() as usize;
        for ((p, k), w) in buf.px[bo..bo + n].iter_mut().zip(&black.px[ko..ko + n]).zip(&white.px[ko..ko + n]) {
            for c in 0..3 {
                p[c] = (k[c] + (w[c] - k[c]) * p[c]).clamp(0.0, 1.0);
            }
        }
    }
    true
}

fn no_knockout(layers: &[Layer]) -> bool {
    layers.iter().all(|l| l.advanced.knockout == photocraft_doc::Knockout::None && l.children().is_none_or(no_knockout))
}

/// Runs `f` with this thread's document blend flags set ([`psblend::LAB_MIX`], [`psblend::HDR`]),
/// then restores the values they had (also on unwind): a nested rayon job can run another tile
/// on this thread mid-composite, and clearing the flags there broke the outer tile (#1112).
pub(crate) fn in_blend_space<R>(lab: bool, hdr: bool, f: impl FnOnce() -> R) -> R {
    psblend::with_lab_mix(lab, || psblend::with_hdr(hdr, f))
}

/// The document's own CMYK profile for reading its CMYK pixels (`None`: not a CMYK document,
/// untagged, or the built-in coated CMYK). Enter it with `photocraft_color::convert::with_cmyk_space`
/// around code that converts the document's CMYK pixels or colours to RGB.
pub fn cmyk_space(doc: &Document) -> Option<std::sync::Arc<photocraft_color::convert::CmykSpace>> {
    if doc.mode != photocraft_color::ColorMode::Cmyk {
        return None;
    }
    photocraft_color::convert::CmykSpace::for_profile(doc.icc_profile.as_ref())
}

/// Pixels per band of [`render_bands`] (a 14000 px wide band is ~600 rows, ~130 MB of f32).
pub const BAND_PIXELS: u64 = 8 << 20;

/// Composite `rect` in horizontal bands, top to bottom, handing each band to `sink` (tiles inside
/// a band render in parallel). Peak memory is one band instead of the whole composite, so huge
/// documents can be exported or displayed without a full-size float copy. `band_rows` of 0 picks
/// about [`BAND_PIXELS`] per band; bands are whole multiples of [`RENDER_TILE`] rows.
pub fn render_bands<E>(doc: &Document, rect: Rect, band_rows: i32, mut sink: impl FnMut(Buffer) -> Result<(), E>) -> Result<(), E> {
    if rect.is_empty() {
        return Ok(());
    }
    // Keep prepared pixels and compiled vector masks across bands without retaining any rendered band.
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    let cx = Ctx::for_doc(doc, &patterns);
    let rows = band_rows_for(rect.width(), band_rows);
    let mut y = rect.y0;
    while y < rect.y1 {
        let y1 = y.saturating_add(rows).min(rect.y1);
        sink(render_tiled_with(doc, Rect::new(rect.x0, y, rect.x1, y1), RENDER_TILE, &cx))?;
        y = y1;
    }
    Ok(())
}

/// Band height for [`render_bands`]: `requested` rounded up to whole tiles, or ~[`BAND_PIXELS`].
pub fn band_rows_for(width: u32, requested: i32) -> i32 {
    let want = if requested > 0 { i64::from(requested) } else { (BAND_PIXELS / u64::from(width.max(1))) as i64 };
    let tiles = (want + i64::from(RENDER_TILE) - 1) / i64::from(RENDER_TILE);
    (tiles.clamp(1, 1 << 16) * i64::from(RENDER_TILE)) as i32
}

/// The document's composite as a pixel surface in `fmt` (straight RGBA converted to its model
/// and depth), optionally flattened over an opaque `background`; rendered and written in bands,
/// so no full-size float composite is held. Tiles equal to the default pixel are pruned.
///
/// Fully transparent pixels are written as the default pixel. Converting transparent black
/// leaves colour behind in CMYK (K = 100%) and Lab (neutral a/b = 0.5), and those pixels would
/// otherwise count as content: the result would span the whole canvas.
///
/// A CMYK document flattened to CMYK is composited in its ink values instead, so flattening
/// doesn't round-trip its pixels through RGB: plain Normal layers by [`native::flatten_cmyk`]
/// (#3778), and masks, groups, clipping, other blend modes and the other layer kinds by [`cmyk`]
/// where its layers mix on their inks (#2220). Otherwise the composite goes back to CMYK through
/// the document's own profile, the one its layers were read through. A Lab document of plain
/// layers is composited in its L, a and b ([`native_lab::flatten_lab`]).
pub fn flatten_to_surface(doc: &Document, fmt: photocraft_color::PixelFormat, background: Option<[f32; 3]>) -> Surface {
    composite_to_surface(doc, doc.bounds(), fmt, background)
}

/// [`flatten_to_surface`] over `rect` only (the rest of the surface stays empty).
pub fn composite_to_surface(doc: &Document, rect: Rect, fmt: photocraft_color::PixelFormat, background: Option<[f32; 3]>) -> Surface {
    let paper = background.is_some_and(|bg| bg == [1.0; 3]);
    if (background.is_none() || paper)
        && let Some(s) = native::flatten_cmyk(doc, rect, fmt, paper)
    {
        return s;
    }
    if (background.is_none() || paper)
        && let Some(s) = native_lab::flatten_lab(doc, rect, fmt, paper)
    {
        return s;
    }
    let space = cmyk_space(doc);
    let to_cmyk = fmt.mode == photocraft_color::ColorMode::Cmyk && doc.mode == photocraft_color::ColorMode::Cmyk;
    if to_cmyk && cmyk::renders_inks(doc) {
        return composite_inks_to_surface(doc, rect, fmt, background, space.as_ref());
    }
    // The composite separates back to CMYK through the document's own profile, the one its
    // pixels were read through.
    let mut s = Surface::new(fmt);
    let n = fmt.channels();
    let empty = s.default_pixel();
    let mut data = Vec::new();
    let _ = render_bands(doc, rect, 0, |band| -> Result<(), ()> {
        let band = match background {
            Some(bg) => band.over_background(bg),
            None => band,
        };
        data.clear();
        data.resize(band.px.len() * n, 0.0);
        photocraft_color::convert::with_cmyk_space(space.as_ref(), || {
            for (p, out) in band.px.iter().zip(data.chunks_exact_mut(n)) {
                if fmt.alpha && (p[3] <= 0.0 || p[3].is_nan()) && empty.len() == n {
                    out.copy_from_slice(&empty);
                } else {
                    photocraft_raster::from_rgba_into(&fmt, *p, out);
                }
            }
        });
        s.write_region(band.rect, &data);
        Ok(())
    });
    s.prune();
    s
}

/// One CMYK pixel's output channels into `data`: its inks (over `paper` when given) and alpha.
fn push_ink_pixel(data: &mut Vec<f32>, p: &[f32; 5], paper: Option<[f32; 4]>, fmt: photocraft_color::PixelFormat, empty: &[f32]) {
    if paper.is_none() && fmt.alpha && (p[4] <= 0.0 || p[4].is_nan()) && empty.len() == fmt.channels() {
        data.extend_from_slice(empty);
        return;
    }
    // Over the paper (fully covered pixels keep their inks exactly), or straight.
    let ink = |c: usize| match paper {
        Some(bg) if p[4] < 1.0 => p[c] * p[4] + bg[c] * (1.0 - p[4]),
        _ => p[c],
    };
    data.extend((0..4).map(ink));
    if fmt.alpha {
        data.push(if paper.is_some() { 1.0 } else { p[4] });
    }
}

/// [`composite_to_surface`] for a CMYK document flattened to CMYK, composited in its inks.
fn composite_inks_to_surface(
    doc: &Document,
    rect: Rect,
    fmt: photocraft_color::PixelFormat,
    background: Option<[f32; 3]>,
    space: Option<&std::sync::Arc<photocraft_color::convert::CmykSpace>>,
) -> Surface {
    let mut s = Surface::new(fmt);
    let n = fmt.channels();
    let empty = s.default_pixel();
    let mut data = Vec::new();
    // White paper is no ink; another background colour goes through the profile.
    let paper = background
        .map(|bg| if bg == [1.0; 3] { [0.0; 4] } else { photocraft_color::convert::with_cmyk_space(space, || photocraft_color::convert::rgb_to_cmyk(bg)) });
    let _ = cmyk::render_cmyk_bands(doc, rect, 0, |band| -> Result<(), ()> {
        data.clear();
        data.reserve(band.px.len() * n);
        for p in &band.px {
            push_ink_pixel(&mut data, p, paper, fmt, &empty);
        }
        s.write_region(band.rect, &data);
        Ok(())
    });
    s.prune();
    s
}

/// Composite the full canvas.
pub fn flatten(doc: &Document) -> Buffer {
    render(doc, doc.bounds())
}

/// Render an arbitrary subset: a single layer (e.g. for thumbnails), isolated.
/// Rects past [`MAX_RENDER_PIXELS`] give an empty buffer.
pub fn render_layer(layer: &Layer, rect: Rect) -> Buffer {
    if u64::from(rect.width()) * u64::from(rect.height()) > MAX_RENDER_PIXELS {
        return Buffer::transparent(Rect::EMPTY);
    }
    let mut buf = Buffer::transparent(rect);
    let patterns = pattern::PreparedPatterns::new(&[], pattern::PREPARED_PATTERN_BYTES);
    composite_stack_at(
        std::slice::from_ref(layer),
        &mut buf,
        &Ctx {
            canvas: rect,
            transfer: adjust::Transfer::Srgb,
            light: photocraft_doc::GlobalLight::default(),
            patterns: &patterns,
            mode: photocraft_color::ColorMode::Rgb,
            depth: photocraft_color::SampleType::F32,
            vector_masks: RenderVectorMasks::default(),
            fx_maps: Default::default(),
            fx_clip: Default::default(),
            fx_over_budget: false,
            tone_luts: prepare_tones(std::slice::from_ref(layer), 0),
            doc: None,
            layer_keys: Default::default(),
        },
        advanced::Scope::ROOT,
    );
    buf
}

/// [`render_layer`] in `doc`'s context: the layer alone over `rect`, with the document's patterns,
/// Global Light, colour mode, depth and canvas (fill layers and gradients are laid out on it), as
/// the canvas composites it. Bakes (Scripts › Flatten All Layer Effects) render through this so
/// the pixels match what was on screen (#5719). Rects past [`MAX_RENDER_PIXELS`] give an empty
/// buffer.
pub fn render_layer_in(doc: &Document, layer: &Layer, rect: Rect) -> Buffer {
    if rect.is_empty() {
        return Buffer::transparent(rect);
    }
    if u64::from(rect.width()) * u64::from(rect.height()) > MAX_RENDER_PIXELS {
        return Buffer::transparent(Rect::EMPTY);
    }
    let patterns = pattern::PreparedPatterns::new(&doc.patterns, pattern::PREPARED_PATTERN_BYTES).in_mode(doc.pixel_format().mode);
    // A lone layer: a deep knockout reveals transparency, not the document's Background.
    let cx = Ctx { doc: None, ..Ctx::for_doc(doc, &patterns) };
    let lab = doc.mode == photocraft_color::ColorMode::Lab;
    let hdr = doc.depth == photocraft_color::SampleType::F32;
    let cmyk = cmyk_space(doc);
    photocraft_color::convert::with_cmyk_space(cmyk.as_ref(), || {
        let mut buf = Buffer::transparent(rect);
        in_blend_space(lab, hdr, || composite_stack_at(std::slice::from_ref(layer), &mut buf, &cx, advanced::Scope::ROOT));
        buf
    })
}
