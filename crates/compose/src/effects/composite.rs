use super::bevel::{BevelPaint, TextureCtx, bevel_geom, bevel_maps, satin_map};
use super::distance::{INSIDE_EPS, Metric, dist_inside_by, dist_outside_by, local_coverage, stroke_outside_exact};
use super::paint::{Rec, dissolves_in, glow_map, paint_color, paint_fx, paint_glow, paint_pattern, rgb, shadow_map};
use super::{
    BlendMode, Buffer, Effect, FxMaps, FxPaint, GlobalLight, Layer, MAX_REACH, Map, PREPARED_PATTERN_BYTES, Pattern, Placement, PreparedPatterns, Rect,
    ShadowSource, StrokePosition, psblend,
};

/// Build every effect map for `layer` from its alpha `shape` over `rect`.
pub fn build_maps(layer: &Layer, shape: Vec<f32>, rect: Rect, light: &GlobalLight, tex: &TextureCtx) -> FxMaps {
    let prepared = PreparedPatterns::new(tex.patterns, PREPARED_PATTERN_BYTES);
    build_maps_prepared(layer, shape, rect, light, tex, &prepared)
}

/// How many region-sized arrays [`build_maps_prepared`] builds for `layer` (what
/// [`FxMaps::bytes`] counts): the shape, each effect's maps, and a stroke's distance fields. Kept
/// next to the builder so the effect cache's estimate (#8330) follows it.
pub(crate) fn map_count(layer: &Layer) -> usize {
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let per: usize = items
        .iter()
        .map(|e| match e {
            Effect::DropShadow(_) | Effect::InnerShadow(_) | Effect::OuterGlow(_) | Effect::InnerGlow(_) | Effect::Satin(_) => 1,
            Effect::BevelEmboss(_) => 2,
            _ => 0,
        })
        .sum();
    let out_widths: Vec<f32> =
        items.iter().filter_map(|e| if let Effect::Stroke(st) = e { Some(stroke_widths(st).1) } else { None }).filter(|w| *w > 0.0).collect();
    let has_stroke = items.iter().any(|e| matches!(e, Effect::Stroke(_)));
    let outside = usize::from(out_widths.iter().any(|&w| !stroke_outside_exact(w))) + usize::from(out_widths.iter().any(|&w| stroke_outside_exact(w)));
    let stroke = if has_stroke { 1 + outside } else { 0 };
    let vector_outline =
        if has_stroke && matches!(layer.content, photocraft_doc::LayerContent::Shape(_)) && crate::effect_outline(layer).is_none() { outside } else { 0 };
    1 + per + stroke + vector_outline
}

pub(crate) fn build_maps_prepared(
    layer: &Layer,
    shape: Vec<f32>,
    rect: Rect,
    light: &GlobalLight,
    tex: &TextureCtx,
    patterns: &PreparedPatterns<'_>,
) -> FxMaps {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let shape = Map { w, h, v: shape };
    let items: Vec<&Effect> = layer.effects.items.iter().filter(|e| e.enabled()).collect();
    let mut bevel_paint = vec![BevelPaint::Inner; items.len()];
    let caster = stroked_caster(&shape, &items);
    let per: Vec<Vec<Map>> = items
        .iter()
        .enumerate()
        .map(|(i, e)| match e {
            Effect::DropShadow(s) => {
                let from = match (&caster, s.source) {
                    (Some(c), ShadowSource::FillAndStroke) => c,
                    _ => &shape,
                };
                vec![shadow_map(from, s, light, false, (rect.x0, rect.y0))]
            }
            Effect::InnerShadow(s) => vec![shadow_map(&shape, s, light, true, (rect.x0, rect.y0))],
            Effect::OuterGlow(g) => vec![glow_map(&shape, g, false, (rect.x0, rect.y0))],
            Effect::InnerGlow(g) => vec![glow_map(&shape, g, true, (rect.x0, rect.y0))],
            Effect::Satin(s) => vec![satin_map(&shape, s)],
            Effect::BevelEmboss(b) => {
                let (maps, paint) = bevel_maps(&shape, b, light, tex, patterns);
                bevel_paint[i] = paint;
                maps
            }
            _ => Vec::new(),
        })
        .collect();
    let has_stroke = items.iter().any(|e| matches!(e, Effect::Stroke(_)));
    let vector_shape = matches!(layer.content, photocraft_doc::LayerContent::Shape(_));
    // A stroke's outer edge keeps the 5 × 5 chamfer while its outside width is under
    // `STROKE_CHAMFER_MAX` — Photoshop matches the chamfer's sub-pixel edge seeding at small
    // offsets — and uses the exact Euclidean transform at or above it, where the chamfer's octagon
    // facets a wide stroke (#3646). Inside stroke distances always keep the chamfer. Each outer
    // field is built only when a stroke of that width is present; the compositor picks per stroke,
    // so one layer may hold both a narrow and a wide stroke.
    let out_widths: Vec<f32> =
        items.iter().filter_map(|e| if let Effect::Stroke(st) = e { Some(stroke_widths(st).1) } else { None }).filter(|w| *w > 0.0).collect();
    let chamfer_out = out_widths.iter().any(|&w| !stroke_outside_exact(w));
    let exact_out = out_widths.iter().any(|&w| stroke_outside_exact(w));
    let (din, dout, dout_exact) = if has_stroke {
        (
            Some(dist_inside_by(&shape, Metric::Chamfer)),
            chamfer_out.then(|| dist_outside_by(&shape, Metric::Chamfer)),
            exact_out.then(|| dist_outside_by(&shape, Metric::Euclidean)),
        )
    } else {
        (None, None, None)
    };
    // Without an outline (unfilled shapes), estimate the vector outline from local coverage; it
    // picks the chamfer or the exact transform the same way.
    let outline = crate::effect_outline(layer).is_some();
    let (vdout, vdout_exact) = if has_stroke && vector_shape && !outline {
        let lc = local_coverage(&shape);
        (chamfer_out.then(|| dist_outside_by(&lc, Metric::Chamfer)), exact_out.then(|| dist_outside_by(&lc, Metric::Euclidean)))
    } else {
        (None, None)
    };
    let mut frames: Vec<(u32, Rect)> = Vec::new();
    for e in &items {
        if let Effect::Stroke(st) = e
            && matches!(st.paint, FxPaint::Gradient(_))
        {
            let out_w = stroke_widths(st).1;
            if !frames.iter().any(|f| f.0 == out_w.to_bits()) {
                frames.push((out_w.to_bits(), stroke_frame(&shape, out_w, rect)));
            }
        }
    }
    FxMaps { rect, shape, per, bevel_paint, din, dout, dout_exact, vdout, vdout_exact, outline, frames }
}

/// The outside width of every enabled stroke in `items` that paints (positive width and opacity),
/// with its opacity.
fn outside_strokes<'a>(items: impl IntoIterator<Item = &'a Effect>) -> impl Iterator<Item = (f32, f32)> {
    items.into_iter().filter_map(|e| match e {
        Effect::Stroke(st) if st.common.enabled => {
            let w = stroke_widths(st).1;
            (w.is_finite() && w > 0.0 && st.common.opacity > 0.0).then_some((w.min(MAX_REACH), st.common.opacity.clamp(0.0, 1.0)))
        }
        _ => None,
    })
}

/// How far past the shape a drop shadow cast from the fill and strokes
/// ([`ShadowSource::FillAndStroke`]) starts: the widest outside stroke.
pub(crate) fn caster_reach(items: &[Effect]) -> f32 {
    outside_strokes(items).map(|(w, _)| w).fold(0.0, f32::max)
}

/// The shape a [`ShadowSource::FillAndStroke`] drop shadow is cast from: the layer's alpha joined
/// with the outside band of each enabled stroke at its opacity, the band the compositor paints
/// (`outside_share`). `None` when no such shadow or no outside stroke is enabled.
fn stroked_caster(shape: &Map, items: &[&Effect]) -> Option<Map> {
    let wanted = items.iter().any(|e| matches!(e, Effect::DropShadow(s) if s.source == ShadowSource::FillAndStroke));
    let strokes: Vec<(f32, f32)> = outside_strokes(items.iter().copied()).collect();
    if !wanted || strokes.is_empty() {
        return None;
    }
    let chamfer = strokes.iter().any(|&(w, _)| !stroke_outside_exact(w)).then(|| dist_outside_by(shape, Metric::Chamfer));
    let exact = strokes.iter().any(|&(w, _)| stroke_outside_exact(w)).then(|| dist_outside_by(shape, Metric::Euclidean));
    let mut out = shape.clone();
    for (w, opacity) in strokes {
        let Some(d) = (if stroke_outside_exact(w) { exact.as_ref() } else { chamfer.as_ref() }) else { continue };
        for (v, &di) in out.v.iter_mut().zip(d) {
            *v = v.max((w + 0.5 - di).clamp(0.0, 1.0) * opacity);
        }
    }
    Some(out)
}

/// Far outside any shape (distance fill for cropped distance fields).
const FAR: f32 = 1.0e9;

/// Keep an exterior effect behind the layer's shape when Fill is partly or wholly
/// transparent. The visible part beneath a fully opaque fill is already covered by the
/// layer, so avoid attenuating anti-aliased edges twice in that case.
fn knock_out_exterior(map: &mut Map, shape: &Map, fill_opacity: f32) {
    let see_through = 1.0 - fill_opacity.clamp(0.0, 1.0);
    for (coverage, alpha) in map.v.iter_mut().zip(&shape.v) {
        *coverage *= 1.0 - alpha * see_through;
    }
}

/// How far the layer lets a knocked-out drop shadow show beneath its own shape (Layer Knocks Out
/// Drop Shadow), 0 = covered, 1 = knocked out wholly. In a normal mode an opaque layer covers the
/// shadow anyway, so only see-through fill knocks it out (and anti-aliased edges are not
/// attenuated twice). In any other mode the layer blends with what's beneath it, shadow and all:
/// an Exclusion or Difference layer would invert it, Multiply darken by it, Screen show it as
/// grey. Photoshop keeps it out from under the shape, so here it's knocked out wholly.
pub fn shadow_see_through(layer: &Layer) -> f32 {
    if matches!(layer.blend, BlendMode::Normal | BlendMode::Dissolve | BlendMode::PassThrough) { 1.0 - layer.fill_opacity.clamp(0.0, 1.0) } else { 1.0 }
}

/// Blend Interior Effects as Group off (Photoshop's default): the layer's mode applies to its
/// own pixels only, and the interior effects paint over the blended result. The layer is merged
/// into the backdrop before they paint, so an Overlay colour overlay on a black Color-mode layer
/// tints the grey beneath it (#3996). A Normal layer merges only when an interior effect blends
/// in another mode: below 100 % fill that effect sees the backdrop through the layer, so a Soft
/// Light overlay on a Fill 0 % layer soft-lights the backdrop instead of showing its own colour
/// (#6232). Normal effects give the same result either way, and the merge would count a
/// half-transparent backdrop twice, so those layers keep the plain composite.
pub fn premerges_layer(layer: &Layer) -> bool {
    !layer.advanced.blend_interior
        && match layer.blend {
            BlendMode::PassThrough | BlendMode::Dissolve => false,
            BlendMode::Normal => blends_inside(layer),
            _ => true,
        }
}

/// `true` if an enabled effect painted inside the layer (with its content) blends in a mode
/// other than Normal.
fn blends_inside(layer: &Layer) -> bool {
    let blends = |m: BlendMode| m != BlendMode::Normal;
    layer.effects.items.iter().filter(|e| e.enabled()).any(|e| match e {
        Effect::ColorOverlay { common, .. } | Effect::GradientOverlay { common, .. } | Effect::PatternOverlay { common, .. } => blends(common.blend),
        Effect::Satin(s) => blends(s.common.blend),
        Effect::InnerGlow(g) => blends(g.common.blend),
        Effect::InnerShadow(s) => blends(s.common.blend),
        Effect::Stroke(s) => stroke_widths(s).0 > 0.0 && blends(s.common.blend),
        Effect::BevelEmboss(b) => bevel_geom(b).paint == BevelPaint::Inner && (blends(b.highlight.blend) || blends(b.shadow.blend)),
        Effect::DropShadow(_) | Effect::OuterGlow(_) => false,
    })
}

/// Composites `content` (the layer's own pixels over `big`, alpha already
/// masked, clipped layers applied) plus its effects into `backdrop`.
pub fn composite_with_effects(layer: &Layer, content: &Buffer, backdrop: &mut Buffer, maps: &FxMaps, layer_bounds: Rect, patterns: &[Pattern]) {
    let prepared = PreparedPatterns::new(patterns, PREPARED_PATTERN_BYTES);
    composite_with_effects_prepared(layer, content, backdrop, maps, layer_bounds, &prepared, None);
}

pub(crate) fn composite_with_effects_prepared(
    layer: &Layer,
    content: &Buffer,
    backdrop: &mut Buffer,
    maps: &FxMaps,
    layer_bounds: Rect,
    patterns: &PreparedPatterns<'_>,
    vstroke: Option<VectorStroke<'_>>,
) {
    record_effects(layer, content, backdrop, maps, layer_bounds, patterns, vstroke).replay(layer, backdrop);
}

/// What a layer with effects adds over a backdrop, recorded once and blended over any backdrop
/// ([`Recorded::replay`]): the colour and coverage of each effect painted behind or around the
/// layer, the layer with its interior effects, in the order they blend. The effect maps and
/// paints (gradients, distance fields, strokes) are worked out here; a replay only blends. Like
/// the GPU canvas's effect maps, it is the layer's alone, so the compositor keeps it per layer
/// state and tile (`fx_cache::recorded_effects`) and a tile composited again after an edit
/// rebuilds only the effects of the layers that changed.
pub(crate) struct Recorded {
    /// Where anything paints; pixels outside it are left as they are.
    rect: Rect,
    ops: Vec<Op>,
    /// Not made from the backdrop it was recorded over (see [`premerges_layer`]): it may be
    /// replayed over any.
    pub(crate) reusable: bool,
}

/// One blend of a [`Recorded`] effect pass, over its rect.
enum Op {
    /// A paint: each pixel's colour and coverage, blended in `mode`.
    Paint { mode: BlendMode, src: Vec<[f32; 4]> },
    /// The outside parts of the strokes, each a paint over the backdrop beneath them taking its
    /// share of the pixel (see [`EffectPass::paint_outside_strokes`]), and their shares summed.
    Strokes { strokes: Vec<(BlendMode, Vec<[f32; 4]>, Vec<f32>)>, cover: Vec<f32> },
    /// The layer with its interior effects, in its mode and coverage gamma.
    Layer { mode: BlendMode, gamma: f32, lay: Vec<[f32; 4]> },
}

impl Op {
    /// The op over `to`, a part of `from`.
    fn crop(&self, from: Rect, to: Rect) -> Op {
        fn part<T: Copy>(v: &[T], from: Rect, to: Rect) -> Vec<T> {
            // (A stroke that painted nothing has no colours.)
            if v.is_empty() {
                return Vec::new();
            }
            let w = from.width() as usize;
            (to.y0..to.y1)
                .flat_map(|y| {
                    let row = (y - from.y0) as usize * w;
                    v[row + (to.x0 - from.x0) as usize..row + (to.x1 - from.x0) as usize].iter().copied()
                })
                .collect()
        }
        match self {
            Op::Paint { mode, src } => Op::Paint { mode: *mode, src: part(src, from, to) },
            Op::Strokes { strokes, cover } => Op::Strokes {
                strokes: strokes.iter().map(|(m, src, share)| (*m, part(src, from, to), part(share, from, to))).collect(),
                cover: part(cover, from, to),
            },
            Op::Layer { mode, gamma, lay } => Op::Layer { mode: *mode, gamma: *gamma, lay: part(lay, from, to) },
        }
    }

    /// Blends it into `work` (over the op's rect).
    fn apply(&self, work: &mut [[f32; 4]]) {
        match self {
            Op::Paint { mode, src } => {
                for (p, s) in work.iter_mut().zip(src) {
                    if s[3] > 0.0 {
                        *p = psblend::composite(*mode, *p, *s, 1.0);
                    }
                }
            }
            Op::Strokes { strokes, cover } => {
                for (i, (p, c)) in work.iter_mut().zip(cover).enumerate() {
                    if *c <= 0.0 {
                        continue;
                    }
                    // Premultiplied Σ share × the stroke blended over the backdrop.
                    let base = *p;
                    let mut a = [0.0f32; 4];
                    for (mode, src, share) in strokes {
                        let s = share[i];
                        if s > 0.0 {
                            let b = psblend::composite(*mode, base, src[i], 1.0);
                            for c in 0..3 {
                                a[c] += s * b[c] * b[3];
                            }
                            a[3] += s * b[3];
                        }
                    }
                    let k = 1.0 - c.min(1.0);
                    let alpha = p[3] * k + a[3];
                    if alpha > 0.0 {
                        *p = [(p[0] * p[3] * k + a[0]) / alpha, (p[1] * p[3] * k + a[1]) / alpha, (p[2] * p[3] * k + a[2]) / alpha, alpha.min(1.0)];
                    }
                }
            }
            Op::Layer { mode, gamma, lay } => {
                for (wp, lp) in work.iter_mut().zip(lay) {
                    if lp[3] > 0.0 {
                        *wp = psblend::composite_gamma(*mode, *wp, *lp, 1.0, *gamma);
                    }
                }
            }
        }
    }

    fn bytes(&self) -> usize {
        let px = std::mem::size_of::<[f32; 4]>();
        match self {
            Op::Paint { src, .. } => src.len() * px,
            Op::Strokes { strokes, cover } => cover.len() * 4 + strokes.iter().map(|(_, src, share)| src.len() * px + share.len() * 4).sum::<usize>(),
            Op::Layer { lay, .. } => lay.len() * px,
        }
    }
}

impl Recorded {
    /// Blends it into `backdrop` at `layer`'s opacity (or Dissolve).
    pub(crate) fn replay(&self, layer: &Layer, backdrop: &mut Buffer) {
        let rect = self.rect.intersect(&backdrop.rect);
        if rect.is_empty() {
            return;
        }
        let (w, bw) = (rect.width() as usize, backdrop.rect.width() as usize);
        let at = |y: i32| (y - backdrop.rect.y0) as usize * bw + (rect.x0 - backdrop.rect.x0) as usize;
        let mut before = Vec::with_capacity(w * rect.height() as usize);
        for y in rect.y0..rect.y1 {
            before.extend_from_slice(&backdrop.px[at(y)..at(y) + w]);
        }
        let mut work = before.clone();
        let cropped: Vec<Op>;
        let ops = if rect == self.rect {
            &self.ops
        } else {
            // (A backdrop that doesn't cover the pass: replayed where they meet.)
            cropped = self.ops.iter().map(|o| o.crop(self.rect, rect)).collect();
            &cropped
        };
        for op in ops {
            op.apply(&mut work);
        }
        let op = layer.opacity;
        for y in rect.y0..rect.y1 {
            let row = (y - rect.y0) as usize * w;
            for (dx, out) in backdrop.px[at(y)..at(y) + w].iter_mut().enumerate() {
                let (a, b) = (before[row + dx], work[row + dx]);
                *out = if layer.blend == BlendMode::Dissolve {
                    // Dissolve applies layer opacity to the completed interior/exterior effect
                    // stack as pixel coverage, not continuous gray interpolation (#3051).
                    if crate::dissolve_noise(rect.x0 + dx as i32, y) < op { b } else { a }
                } else {
                    mix_premul(a, b, op)
                };
            }
        }
    }

    /// Bytes it holds.
    pub(crate) fn bytes(&self) -> usize {
        self.ops.iter().map(Op::bytes).sum::<usize>() + std::mem::size_of::<Self>()
    }
}

/// Records `layer`'s effects over `content`'s rect (the layer's own pixels there, alpha already
/// masked, clipped layers applied). `backdrop` is read only by a layer whose interior effects
/// paint over it merged with the backdrop ([`premerges_layer`]); such a record holds for that
/// backdrop alone (not [`Recorded::reusable`]).
pub(crate) fn record_effects(
    layer: &Layer,
    content: &Buffer,
    backdrop: &Buffer,
    maps: &FxMaps,
    layer_bounds: Rect,
    patterns: &PreparedPatterns<'_>,
    vstroke: Option<VectorStroke<'_>>,
) -> Recorded {
    #[cfg(test)]
    RECORDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pass = EffectPass::new(layer, maps, content.rect, layer_bounds, patterns, vstroke.as_ref());
    let big = pass.big;
    let shape = pass.effect_shape(content);
    let mut ops = Ops { ops: Vec::new(), reach: Rect::EMPTY };
    // Exterior effects: painted behind the layer, over the backdrop.
    pass.paint_exterior(&mut ops, &shape);

    // The layer: content at fill opacity, then interior effects. Interior effects are painted
    // relative to the layer's shape (alpha = coverage within the shape), then the shape's alpha
    // applies: a colour overlay at 100 % replaces the colour of a half-transparent edge pixel and
    // keeps its alpha, as in Photoshop.
    let gamma = crate::text_gamma(layer);
    let premerge = premerges_layer(layer);
    let mut lay = pass.layer_pixels(content, &shape);
    if premerge {
        let b = backdrop;
        // The backdrop with the exterior effects, where the layer merges.
        let mut work = vec![[0.0f32; 4]; pass.w * pass.h];
        for y in b.rect.y0.max(big.y0)..b.rect.y1.min(big.y1) {
            for x in b.rect.x0.max(big.x0)..b.rect.x1.min(big.x1) {
                work[((y - big.y0) as usize) * pass.w + (x - big.x0) as usize] = b.get(x, y);
            }
        }
        for op in &ops.ops {
            op.apply(&mut work);
        }
        for (lp, wp) in lay.px.iter_mut().zip(&work) {
            *lp = psblend::composite_gamma(layer.blend, *wp, *lp, 1.0, gamma);
        }
    }
    let strokes: Vec<&photocraft_doc::StrokeFx> = pass.items.iter().filter_map(|e| if let Effect::Stroke(s) = e { Some(s) } else { None }).collect();
    let fields = pass.stroke_fields(!strokes.is_empty());
    pass.paint_interior(&mut lay, &shape);
    pass.paint_vector_stroke(&mut lay, &shape);
    pass.paint_inside_strokes(&mut lay, &shape, &strokes, &fields);
    pass.paint_bevel(&mut To::Lay(&mut lay), BevelPaint::Inner, Some(&shape));
    // The shape's own alpha.
    for (p, a) in lay.px.iter_mut().zip(&shape.v) {
        p[3] *= a.min(1.0);
    }
    pass.paint_outside_strokes(&mut ops, &lay, &shape, &strokes, &fields);
    pass.paint_bevel(&mut To::Ops(&mut ops, big), BevelPaint::Outer, None);

    // Layer (with interior effects) onto the exterior result, in the layer's mode.
    let mode = if premerge || layer.blend == BlendMode::PassThrough { BlendMode::Normal } else { layer.blend };
    let reach = reach_of(big, &lay.px, |p| p[3] > 0.0);
    ops.push(Op::Layer { mode, gamma, lay: lay.px }, reach);
    // Emboss styles shade the composited layer (their maps cover inside and outside). Painting
    // them into the layer before its (text-gamma) composite left a type layer's lit top edges up
    // to 6/255 dark (psd-tools layer_effects Emboss: 7.8 → 5.7/255).
    pass.paint_bevel(&mut To::Ops(&mut ops, big), BevelPaint::Both, None);
    // Only where something paints.
    let Ops { ops, reach: rect } = ops;
    let ops = if rect.is_empty() {
        Vec::new()
    } else if rect == big {
        ops
    } else {
        ops.iter().map(|o| o.crop(big, rect)).collect()
    };
    Recorded { rect, ops, reusable: !premerge }
}

/// Effect passes recorded in this process (tests count them, in a process of their own).
#[cfg(test)]
pub(crate) static RECORDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Where a paint of the pass goes: into the layer's pixels, or recorded as an [`Op`] over the
/// pass's region, to blend over the backdrop.
enum To<'b> {
    Lay(&'b mut Buffer),
    Ops(&'b mut Ops, Rect),
}

impl To<'_> {
    fn color(&mut self, m: &Map, c: [f32; 3], blend: BlendMode, opacity: f32) {
        match self {
            To::Lay(lay) => paint_color(*lay, m, c, blend, opacity),
            To::Ops(ops, rect) => record(ops, *rect, |r| paint_color(r, m, c, blend, opacity)),
        }
    }
}

/// The ops of a pass being recorded and the pixels they change together.
struct Ops {
    ops: Vec<Op>,
    reach: Rect,
}

impl Ops {
    fn push(&mut self, op: Op, reach: Rect) {
        if !reach.is_empty() {
            self.reach = if self.reach.is_empty() { reach } else { self.reach.union(&reach) };
            self.ops.push(op);
        }
    }
}

/// Records one paint into `ops` (nothing when it paints no pixel).
fn record(ops: &mut Ops, rect: Rect, paint: impl FnOnce(&mut Rec)) {
    let mut r = Rec::new(rect);
    paint(&mut r);
    ops.push(Op::Paint { mode: r.mode, src: r.src }, r.reach);
}

/// The pixels of `rect` (its values `v`, row by row) where `on` holds.
fn reach_of<T>(rect: Rect, v: &[T], on: impl Fn(&T) -> bool) -> Rect {
    let w = rect.width().max(1) as usize;
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for (i, p) in v.iter().enumerate() {
        if on(p) {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x0 == usize::MAX { Rect::EMPTY } else { Rect::new(rect.x0 + x0 as i32, rect.y0 + y0 as i32, rect.x0 + x1 as i32, rect.y0 + y1 as i32) }
}

/// `true` where the effect shape covers a pixel enough for interior effects to paint.
fn inside(a: f32) -> bool {
    a > INSIDE_EPS
}

/// An effect map as a fraction of the shape's own coverage.
fn relative_to_shape(m: Map, shape: &Map) -> Map {
    let v = m.v.iter().zip(&shape.v).map(|(m, a)| if inside(*a) { (m / a).min(1.0) } else { 0.0 }).collect();
    Map { w: m.w, h: m.h, v }
}

/// A stroke's distance fields, cropped to the render region; only the ones a stroke needs exist.
#[derive(Default)]
struct StrokeFields {
    /// Distance inside the shape.
    din: Vec<f32>,
    dout: Option<Vec<f32>>,
    dout_exact: Option<Vec<f32>>,
    vdout: Option<Vec<f32>>,
    vdout_exact: Option<Vec<f32>>,
}

/// What compositing one layer's effects shares between its stages.
struct EffectPass<'a> {
    layer: &'a Layer,
    maps: &'a FxMaps,
    patterns: &'a PreparedPatterns<'a>,
    vstroke: Option<&'a VectorStroke<'a>>,
    /// The region effects are painted over: the content's rect.
    big: Rect,
    w: usize,
    h: usize,
    /// Layer bounds (gradients aligned with the layer use the whole layer, independent of the
    /// render rect).
    sb: Rect,
    /// Linked patterns tile from the effects reference point (else the layer's top-left).
    anchor: (f64, f64),
    /// The enabled effects. Multiple instances: the first listed is on top, so paint in reverse.
    items: Vec<&'a Effect>,
}

impl<'a> EffectPass<'a> {
    fn new(
        layer: &'a Layer,
        maps: &'a FxMaps,
        big: Rect,
        layer_bounds: Rect,
        patterns: &'a PreparedPatterns<'a>,
        vstroke: Option<&'a VectorStroke<'a>>,
    ) -> Self {
        let sb = layer_bounds;
        Self {
            layer,
            maps,
            patterns,
            vstroke,
            big,
            w: big.width() as usize,
            h: big.height() as usize,
            sb,
            anchor: layer.effects.reference.unwrap_or((f64::from(sb.x0), f64::from(sb.y0))),
            items: layer.effects.items.iter().filter(|e| e.enabled()).collect(),
        }
    }

    /// The enabled effects with their indexes, last listed first.
    fn rev(&self) -> impl Iterator<Item = (usize, &'a Effect)> + '_ {
        self.items.iter().copied().enumerate().rev()
    }

    /// Effect `i`'s map `k`, cropped to the region.
    fn fx(&self, i: usize, k: usize) -> Map {
        self.maps.crop(&self.maps.per[i][k], self.big, 0.0)
    }

    /// The vector stroke's layer-mask value at pixel `i` (1 with no mask).
    fn kmask(&self, i: usize) -> f32 {
        self.vstroke.and_then(|v| v.mask).and_then(|m| m.get(i)).copied().unwrap_or(1.0)
    }

    /// Alpha `a` of pixel `i` joined with the vector stroke's.
    fn union(&self, i: usize, a: f32) -> f32 {
        self.vstroke.and_then(|v| v.stroke.px.get(i)).map_or(a, |s| self.kmask(i) * (a + s[3] * (1.0 - a)))
    }

    /// Within an outline (or a split-off vector stroke), or without Transparency Shapes Layer,
    /// the content's own transparency acts like fill opacity: the effects still cover the whole
    /// shape.
    fn relative(&self) -> bool {
        !self.layer.advanced.transparency_shapes || self.maps.outline || self.vstroke.is_some()
    }

    /// The effect shape: the content's alpha (with a split-off vector stroke, `content` is the
    /// unmasked fill and the mask applies to fill ∪ stroke), joined with a filled shape's outline.
    fn effect_shape(&self, content: &Buffer) -> Map {
        let (w, h, big) = (self.w, self.h, self.big);
        // Transparency Shapes Layer off: the shape is the whole layer (its masks, as the maps were
        // built), and the content's own transparency acts like fill opacity within it.
        if !self.layer.advanced.transparency_shapes {
            self.maps.crop(&self.maps.shape, big, 0.0)
        } else if self.maps.outline {
            let o = self.maps.crop(&self.maps.shape, big, 0.0);
            Map { w, h, v: o.v.iter().zip(&content.px).enumerate().map(|(i, (o, p))| o.max(self.union(i, p[3]))).collect() }
        } else {
            Map { w, h, v: content.px.iter().enumerate().map(|(i, p)| self.union(i, p[3])).collect() }
        }
    }

    /// Paints the effects behind the layer: drop shadows, then outer glows.
    fn paint_exterior(&self, ops: &mut Ops, shape: &Map) {
        for (i, e) in self.rev() {
            if let Effect::DropShadow(s) = e {
                let mut m = self.fx(i, 0);
                if s.knocks_out {
                    // The layer hides the shadow beneath it where its fill is see-through (at 100 %
                    // fill in a normal mode it covers it anyway; at 0 % the shape shows the bare
                    // backdrop), and wholly in a mode that blends with it ([`shadow_see_through`]).
                    knock_out_exterior(&mut m, shape, 1.0 - shadow_see_through(self.layer));
                }
                record(ops, self.big, |r| paint_color(r, &m, rgb(&s.color), s.common.blend, s.common.opacity));
            }
        }
        for (i, e) in self.rev() {
            if let Effect::OuterGlow(g) = e {
                let mut m = self.fx(i, 0);
                // The glow is behind the shape, not painted across its interior when Fill is 0 %.
                // Preserve the existing anti-aliased edge when the fill is fully opaque.
                knock_out_exterior(&mut m, shape, self.layer.fill_opacity);
                record(ops, self.big, |r| paint_glow(r, &m, g, self.sb, self.anchor, self.big, self.patterns));
            }
        }
    }

    /// Blend Interior Effects as Group: the interior effects (overlays, satin, inner glow) are
    /// combined with the content first, and fill opacity applies to the combination.
    fn interior_group(&self) -> bool {
        self.layer.advanced.blend_interior && self.layer.fill_opacity < 1.0
    }

    /// The layer's own pixels at fill opacity, with their alpha made relative to `shape`.
    fn layer_pixels(&self, content: &Buffer, shape: &Map) -> Buffer {
        let content_fill = if self.interior_group() { 1.0 } else { self.layer.fill_opacity };
        let relative = self.relative();
        let lay_alpha = |i: usize, p: &[f32; 4], a: f32| {
            if !inside(a) {
                0.0
            } else if relative {
                content_fill * (self.kmask(i) * p[3] / a).min(1.0)
            } else {
                content_fill
            }
        };
        Buffer { rect: self.big, px: content.px.iter().zip(&shape.v).enumerate().map(|(i, (p, a))| [p[0], p[1], p[2], lay_alpha(i, p, *a)]).collect() }
    }

    /// Paints the interior effects other than strokes and bevels: overlays, satin, inner glow
    /// and inner shadow.
    fn paint_interior(&self, lay: &mut Buffer, shape: &Map) {
        let (w, h, big, sb, anchor, patterns) = (self.w, self.h, self.big, self.sb, self.anchor, self.patterns);
        let full = Map { w, h, v: shape.v.iter().map(|a| if inside(*a) { 1.0 } else { 0.0 }).collect() };
        for (_, e) in self.rev() {
            if let Effect::PatternOverlay { common, name, id, scale, angle, link, phase } = e {
                paint_pattern(lay, &full, patterns, name, id, Placement::anchored(anchor, *link, *phase, *scale, *angle), big, common.blend, common.opacity);
            }
        }
        for (_, e) in self.rev() {
            if let Effect::GradientOverlay { common, gradient, .. } = e {
                paint_fx(lay, &full, &FxPaint::Gradient(gradient.clone()), sb, anchor, big, common.blend, common.opacity, patterns)
            }
        }
        for (_, e) in self.rev() {
            if let Effect::ColorOverlay { common, color } = e {
                paint_color(lay, &full, rgb(color), common.blend, common.opacity);
            }
        }
        for (i, e) in self.rev() {
            if let Effect::Satin(s) = e {
                let m = relative_to_shape(self.fx(i, 0), shape);
                paint_color(lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
            }
        }
        for (i, e) in self.rev() {
            if let Effect::InnerGlow(g) = e {
                paint_glow(lay, &relative_to_shape(self.fx(i, 0), shape), g, sb, anchor, big, patterns);
            }
        }
        if self.interior_group() {
            for p in &mut lay.px {
                p[3] *= self.layer.fill_opacity.max(0.0);
            }
        }
        for (i, e) in self.rev() {
            if let Effect::InnerShadow(s) = e {
                let m = relative_to_shape(self.fx(i, 0), shape);
                paint_color(lay, &m, rgb(&s.color), s.common.blend, s.common.opacity);
            }
        }
    }

    /// A stroked shape's vector stroke: Photoshop draws it above the fill's clipped layers and
    /// interior effects, below the stroke effect (psd-tools stroke-composite).
    fn paint_vector_stroke(&self, lay: &mut Buffer, shape: &Map) {
        let Some(vs) = self.vstroke else {
            return;
        };
        let fill = self.layer.fill_opacity;
        for (i, ((p, s), a)) in lay.px.iter_mut().zip(&vs.stroke.px).zip(&shape.v).enumerate() {
            if s[3] > 0.0 && inside(*a) {
                *p = psblend::composite(BlendMode::Normal, *p, [s[0], s[1], s[2], fill * (self.kmask(i) * s[3] / a).min(1.0)], 1.0);
            }
        }
    }

    /// The distance fields the strokes need, cropped to the region; empty unless `ready` (the
    /// layer has a stroke).
    ///
    /// A stroke's outer edge picks the chamfer or the exact field by its width
    /// ([`stroke_outside_exact`]); both are cropped here, and only the ones a stroke needs exist.
    fn stroke_fields(&self, ready: bool) -> StrokeFields {
        if !ready {
            return StrokeFields::default();
        }
        let (maps, big) = (self.maps, self.big);
        let crop_out = |v: &Option<Vec<f32>>| v.as_ref().map(|b| maps.crop_vec(b, big, FAR));
        StrokeFields {
            din: maps.din.as_ref().map(|a| maps.crop_vec(a, big, 0.0)).unwrap_or_default(),
            dout: crop_out(&maps.dout),
            dout_exact: crop_out(&maps.dout_exact),
            vdout: crop_out(&maps.vdout),
            vdout_exact: crop_out(&maps.vdout_exact),
        }
    }

    /// A gradient stroke aligned with the layer spans the stroke's own extent.
    fn stroke_frame(&self, st: &photocraft_doc::StrokeFx) -> Rect {
        self.maps.stroke_frame(st).unwrap_or(self.sb)
    }

    /// Strokes. Inside parts are painted over the layer (bottom instance first); outside parts
    /// are slid beneath it, so the first listed (top) instance is processed first.
    fn paint_inside_strokes(&self, lay: &mut Buffer, shape: &Map, strokes: &[&photocraft_doc::StrokeFx], fields: &StrokeFields) {
        for st in strokes.iter().rev().copied() {
            let (in_w, _) = stroke_widths(st);
            if in_w <= 0.0 {
                continue;
            }
            let mut m = Map::new(self.w, self.h, 0.0);
            for ((mv, a), dv) in m.v.iter_mut().zip(&shape.v).zip(&fields.din) {
                *mv = if inside(*a) { (in_w + 0.5 - dv).clamp(0.0, 1.0) } else { 0.0 };
            }
            paint_fx(lay, &m, &st.paint, self.stroke_frame(st), self.anchor, self.big, st.common.blend, st.common.opacity, self.patterns);
        }
    }

    /// Bevel and emboss effects whose paint is `kind`. Inner ones paint into the layer relative
    /// to its `shape`; the others paint the maps as they are.
    fn paint_bevel(&self, target: &mut To<'_>, kind: BevelPaint, shape: Option<&Map>) {
        for (i, e) in self.rev() {
            if let Effect::BevelEmboss(b) = e
                && self.maps.bevel_paint[i] == kind
            {
                let map = |k: usize| match shape {
                    Some(shape) => relative_to_shape(self.fx(i, k), shape),
                    None => self.fx(i, k),
                };
                target.color(&map(0), rgb(&b.highlight_color), b.highlight.blend, b.highlight.opacity);
                target.color(&map(1), rgb(&b.shadow_color), b.shadow.blend, b.shadow.opacity);
            }
        }
    }

    /// The outside parts of the strokes, recorded to blend onto the backdrop beneath the layer
    /// with their own mode. A higher stroke covers the ones below it (Photoshop: a Multiply stroke listed
    /// above a wider Normal stroke multiplies the backdrop, not the lower stroke). Each stroke's
    /// share is its coverage × opacity not yet taken by strokes above; for Normal strokes this
    /// equals stacking them top over bottom.
    fn paint_outside_strokes(&self, ops: &mut Ops, lay: &Buffer, shape: &Map, strokes: &[&photocraft_doc::StrokeFx], fields: &StrokeFields) {
        if !strokes.iter().any(|st| stroke_widths(st).1 > 0.0) {
            return;
        }
        let (w, h) = (self.w, self.h);
        let mut cover = vec![0.0f32; w * h];
        let mut painted = Vec::new();
        for st in strokes.iter().copied() {
            let (_, out_w) = stroke_widths(st);
            if out_w <= 0.0 {
                continue;
            }
            let share = self.outside_share(st, out_w, shape, lay, fields, &mut cover);
            // The stroke blended over the backdrop at full coverage, where it has a share.
            let ones = Map { w, h, v: share.iter().map(|&s| if s > 0.0 { 1.0 } else { 0.0 }).collect() };
            // The share already carries the Dissolve choice, so the paint itself composites as Normal.
            let paint_blend = if st.common.blend == BlendMode::Dissolve { BlendMode::Normal } else { st.common.blend };
            let mut r = Rec::new(self.big);
            paint_fx(&mut r, &ones, &st.paint, self.stroke_frame(st), self.anchor, self.big, paint_blend, 1.0, self.patterns);
            painted.push((r.mode, r.src, share));
        }
        let reach = reach_of(self.big, &cover, |c| *c > 0.0);
        ops.push(Op::Strokes { strokes: painted, cover }, reach);
    }

    /// The share of each pixel an outside stroke `st` (outside width `out_w`) takes: its coverage
    /// × opacity not yet taken by the strokes above, which `cover` accumulates.
    fn outside_share(&self, st: &photocraft_doc::StrokeFx, out_w: f32, shape: &Map, lay: &Buffer, fields: &StrokeFields, cover: &mut [f32]) -> Vec<f32> {
        let (w, big) = (self.w, self.big);
        // Shape layers without a filled outline stroke their vector outline, estimated from local
        // coverage; every other layer strokes its shape. The width then picks the field.
        let vector_shape = matches!(self.layer.content, photocraft_doc::LayerContent::Shape(_));
        let vector_outline = vector_shape && !self.maps.outline;
        // Shape layers: Photoshop strokes the vector outline (estimated from local coverage
        // for unfilled shapes); the stroke never shows through the shape's pixels. Along a
        // filled shape's outline it covers the part of each edge pixel outside the path.
        let exact = stroke_outside_exact(out_w);
        let d: &[f32] = match (vector_outline, exact) {
            (true, true) => fields.vdout_exact.as_deref(),
            (true, false) => fields.vdout.as_deref(),
            (false, true) => fields.dout_exact.as_deref(),
            (false, false) => fields.dout.as_deref(),
        }
        .unwrap_or_default();
        let mut share = vec![0.0f32; w * self.h];
        for (i, sh) in share.iter_mut().enumerate() {
            let band = (out_w + 0.5 - d[i]).clamp(0.0, 1.0);
            let k = if inside(shape.v[i]) { if vector_outline { 0.0 } else { band * outline_share(shape.v[i], lay.px[i][3]) } } else { band };
            let mut k = k * st.common.opacity;
            if st.common.blend == BlendMode::Dissolve {
                let (x, y) = (i64::from(big.x0) + (i % w) as i64, i64::from(big.y0) + (i / w) as i64);
                k = if k > 0.0 && dissolves_in(x, y, k) { 1.0 } else { 0.0 };
            }
            *sh = k * (1.0 - cover[i]);
            cover[i] += *sh;
        }
        share
    }
}

/// Coverage of an outside stroke beneath a filled shape's edge pixel, for a pixel the outline
/// covers `cov` of and the layer (with its interior effects) ends at alpha `l`. Photoshop treats
/// the stroke and the shape as disjoint areas: the stroke fills the part outside the path and
/// the layer adds its own alpha (psd-tools stroke-effects: a 58 % covered edge pixel of an
/// opaque fill ends opaque, one of a faded fill at 42 % + its alpha). Composited beneath the
/// layer, `share + l (1 - share) = (1 - cov) + l`.
pub fn outline_share(cov: f32, l: f32) -> f32 {
    let outside = (1.0 - cov.clamp(0.0, 1.0)).max(0.0);
    if outside <= 1e-6 {
        return 0.0;
    }
    let rest = 1.0 - l.clamp(0.0, 1.0);
    if rest <= 1e-6 { 1.0 } else { (outside / rest).min(1.0) }
}

/// (inside width, outside width) of a stroke.
pub fn stroke_widths(st: &photocraft_doc::StrokeFx) -> (f32, f32) {
    match st.position {
        StrokePosition::Outside => (0.0, st.size),
        StrokePosition::Inside => (st.size, 0.0),
        StrokePosition::Center => (st.size / 2.0, st.size / 2.0),
    }
}

/// The frame a gradient stroke aligned with the layer is laid out in: the pixel bounds of the
/// effect shape (over `rect`) grown by the stroke's outside width less one pixel. Photoshop
/// spans the gradient over the stroke's extent, not the layer's: a 4 px outside stroke around a
/// 22 px square runs its 90° gradient over 28 px (psd-tools stroke-effects), a 3 px one around
/// 36 px of type over 40 px (psd-tools effect-stroke-gradient).
fn stroke_frame(shape: &Map, out_w: f32, rect: Rect) -> Rect {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..shape.h {
        for x in 0..shape.w {
            if shape.v[y * shape.w + x] > INSIDE_EPS {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 == usize::MAX {
        return rect;
    }
    let grow = if out_w.is_finite() { (out_w.min(MAX_REACH).ceil() as i32 - 1).max(0) } else { 0 };
    Rect::new(rect.x0 + x0 as i32, rect.y0 + y0 as i32, rect.x0 + x1 as i32, rect.y0 + y1 as i32).inflate(grow)
}

/// A stroked shape's vector stroke, composited apart from its fill (which the compositor then
/// passes unmasked): the stroke's pixels over the render rect and the layer's mask values.
pub(crate) struct VectorStroke<'a> {
    pub stroke: &'a Buffer,
    pub mask: Option<&'a [f32]>,
}

/// Premultiplied linear interpolation between two straight-alpha pixels.
fn mix_premul(a: [f32; 4], b: [f32; 4], k: f32) -> [f32; 4] {
    let alpha = a[3] + (b[3] - a[3]) * k;
    if alpha <= 0.0 {
        return [0.0; 4];
    }
    let mut out = [0.0; 4];
    for c in 0..3 {
        out[c] = (a[c] * a[3] + (b[c] * b[3] - a[c] * a[3]) * k) / alpha;
    }
    out[3] = alpha;
    out
}
