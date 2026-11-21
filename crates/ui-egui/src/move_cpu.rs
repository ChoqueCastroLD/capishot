//! Move drags on the CPU canvas: each frame re-blends only the moving layers.
//!
//! A Move drag shows the document with the moving layers at the pointer (`move_ui`), and the
//! canvas recomposited the whole stack over where they were and are, every frame: 140 ms a frame
//! for a layer of a 200-layer 4000 x 6000 page. The layers below the moving run don't change
//! during the drag, and nor do those above it; so for the drag the CPU canvas keeps, per tile,
//! made the first time a tile needs them:
//!
//! - the composite of the layers below the run (exact: the same stack prefix the full composite
//!   starts with);
//! - for the topmost layers that change an opaque backdrop affinely
//!   ([`photocraft_compose::composites_affinely`]: Normal, Multiply, Screen, their effects and
//!   groups), their composite over opaque black and over opaque white, which give every pixel's
//!   `c -> n + m·c` ([`photocraft_compose::apply_affine`]; exact for those modes, type layers'
//!   anti-aliased edges approximated);
//!
//! and per frame composites only the moving run and the layers between it and that top over the
//! cached below, then applies the top's map. Pixels that aren't opaque (no Background) take the top
//! layers composited for real.
//!
//! The stack is seen with mix-free pass-through groups opened up (their children composite
//! straight into the backdrop, `photocraft_compose`), so a layer in such a group moves as if it
//! were at the top level. Anything this can't split falls back to the full composite: moving
//! layers that aren't one contiguous run of that stack, a run clipped to a layer below it, Lab,
//! 32-bit, CMYK or multichannel documents, spot channels, artboards. The preview is replaced by
//! the exact composite when the drag ends.

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_compose::Buffer;
use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect as DRect;

/// What the move preview shows, for the CPU canvas (from `move_ui::cpu_plan`).
pub(crate) struct Plan {
    /// The document before the drag.
    pub base: Arc<Document>,
    /// The layers that move (`move_ui::MovePreview::ids`).
    pub ids: Vec<LayerId>,
    pub doc: DocId,
    pub revision: u64,
    /// The drag's offset (document pixels) the preview shows.
    pub offset: (i32, i32),
}

/// The drag's cached composites below and above the moving run, at one level.
pub(crate) struct Split {
    key: (DocId, u64, Vec<LayerId>, u32),
    /// The per-frame layers (`start..top`) before the drag, reduced to the level once, the layers
    /// in them that move, and the level's factor: each frame shifts only those, by the offset in
    /// level pixels (reducing the moved full-resolution layers every frame cost as much as
    /// compositing them, and the full-resolution moved document isn't needed at all).
    middle: (Document, Vec<LayerId>, i64),
    below: Arc<Document>,
    /// The affine top, and the same layers for pixels that aren't opaque.
    above: Option<Arc<Document>>,
    below_tiles: HashMap<DRect, Buffer>,
    /// The top over opaque black and over opaque white, per tile.
    above_tiles: HashMap<DRect, (Buffer, Buffer)>,
    /// The one-blit path ([`Blit`]), when the drag can take it.
    blit: Option<Blit>,
}

/// A drag whose moving run is one layer that composites as its own image over the rest
/// ([`photocraft_compose::composites_as_over`]) under an affine top (or none): its frames are 8-bit
/// blits of images made once, as on the GPU canvas (`gpu_canvas::above`): the moving layer alone
/// (the sprite), at the offset, over the below's tiles, the top's map laid over that
/// ([`photocraft_compose::blit`]). No float composite, conversion or allocation but the output.
struct Blit {
    /// The sprite's extent (level pixels, at offset 0) and its premultiplied pixels.
    rect: DRect,
    px: Vec<[u8; 4]>,
    /// A run that changes an opaque backdrop affinely (layer effects, type, a blend mode, clipped
    /// layers) is kept instead as its composite over opaque black and over opaque white (`px`
    /// and this), laid over the below as the top is.
    white: Option<Vec<[u8; 4]>>,
    /// The below's and the top's tiles in 8 bits, made from the float ones once.
    below: HashMap<DRect, Vec<[u8; 4]>>,
    above: HashMap<DRect, (Vec<[u8; 4]>, Vec<[u8; 4]>)>,
    /// The documents the sprite is composited from (the run alone; over black and over white),
    /// and the rows of it composited so far: made a band at a time ([`Split::prepare`]).
    sources: Vec<Document>,
    rows: i32,
}

/// The largest sprite kept (pixels): past it the drag takes the float path.
const MAX_SPRITE_PX: u64 = 64 << 20;

impl Blit {
    /// The sprite of the run `layers` (in the level's document `d`): one image when it is one
    /// layer laid over the rest (`over`), else its composite over black and over white; `None`
    /// when it is too big.
    fn new(d: &Document, layers: &[Layer], over: bool) -> Option<Blit> {
        let all = DRect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
        let rect = layers.iter().filter_map(|l| photocraft_compose::change_bounds(l, all)).reduce(|a, b| a.union(&b))?;
        if rect.is_empty() || u64::from(rect.width()) * u64::from(rect.height()) > MAX_SPRITE_PX {
            return None;
        }
        let ids: Vec<LayerId> = layers.iter().map(|l| l.id).collect();
        let mut alone = d.clone();
        alone.layers = layers.to_vec();
        let mut alone = photocraft_engine::layer_multi_cmds::moved(&alone, &ids, -rect.x0, -rect.y0).ok()?;
        alone.size = photocraft_geom::Size::new(rect.width(), rect.height());
        let over_fill = |v: f32| {
            let mut d = alone.clone();
            let fill = Layer::new("backdrop", LayerContent::Fill(photocraft_doc::Fill::Solid(photocraft_color::Color::rgb(v, v, v))));
            d.layers.insert(0, fill);
            d
        };
        let sources = if over { vec![alone] } else { vec![over_fill(0.0), over_fill(1.0)] };
        let n = rect.width() as usize * rect.height() as usize;
        Some(Blit {
            rect,
            px: vec![[0; 4]; n],
            white: (!over).then(|| vec![[0; 4]; n]),
            below: HashMap::new(),
            above: HashMap::new(),
            sources,
            rows: 0,
        })
    }

    /// Whether the sprite is all composited.
    fn ready(&self) -> bool {
        self.rows >= self.rect.height() as i32
    }

    /// Composite the sprite's next band of rows (about [`BAND_PX`] pixels).
    fn band(&mut self) {
        let w = self.rect.width() as usize;
        let rows = ((BAND_PX / w.max(1) as u64).max(1) as i32).min(self.rect.height() as i32 - self.rows);
        if rows <= 0 {
            return;
        }
        let band = DRect::new(0, self.rows, self.rect.width() as i32, self.rows + rows);
        let at = self.rows as usize * w;
        for (i, src) in self.sources.iter().enumerate() {
            let px = photocraft_compose::render(src, band).to_premultiplied_rgba8();
            let dst = if i == 0 { Some(&mut self.px) } else { self.white.as_mut() };
            if let Some(d) = dst.and_then(|d| d.get_mut(at..at + px.len())) {
                d.copy_from_slice(&px);
            }
        }
        self.rows += rows;
    }
}

/// Pixels of one band of a sprite composited at once.
const BAND_PX: u64 = 128 * 1024;

/// A pass-through group whose children composite straight into the backdrop (no opacity, fill,
/// mask, effects or clipped layers to mix): its children can stand in the stack in its place.
fn opens(l: &Layer, next_clipped: bool) -> bool {
    matches!(&l.content, LayerContent::Group(g) if g.artboard.is_none()
        && g.children.first().is_none_or(|c| !c.clipped))
        && l.visible
        && l.blend == photocraft_color::BlendMode::PassThrough
        && l.opacity >= 1.0
        && l.fill_opacity >= 1.0
        && l.mask.is_none()
        && l.vector_mask.is_none()
        && !photocraft_compose::effects::has_effects(l)
        && !next_clipped
}

/// The stack as the compositor sees it with mix-free pass-through groups opened up ([`opens`]),
/// bottom first.
fn open_stack(layers: &[Layer]) -> Vec<&Layer> {
    fn rec<'a>(layers: &'a [Layer], out: &mut Vec<&'a Layer>, depth: usize) {
        for (i, l) in layers.iter().enumerate() {
            let next_clipped = layers.get(i + 1).is_some_and(|n| n.clipped);
            match &l.content {
                LayerContent::Group(g) if depth < 64 && opens(l, next_clipped) => rec(&g.children, out, depth + 1),
                _ => out.push(l),
            }
        }
    }
    let mut out = Vec::new();
    rec(layers, &mut out, 0);
    out
}

/// The part `start..end` of `stack` the moving layers `ids` (and everything inside them) span,
/// with the layers clipped to its last one: what composites per frame (layers between moving
/// ones too, so the moving layers needn't be one run). `None` when a moving layer isn't in the
/// stack (inside a group that isn't opened up) or the span starts clipped to a layer below it.
fn run_of(stack: &[&Layer], doc: &Document, ids: &[LayerId]) -> Option<(usize, usize)> {
    fn holds(l: &Layer, id: LayerId) -> bool {
        l.id == id || l.children().is_some_and(|c| c.iter().any(|k| holds(k, id)))
    }
    fn subtree(l: &Layer, out: &mut Vec<LayerId>) {
        out.push(l.id);
        for c in l.children().unwrap_or_default() {
            subtree(c, out);
        }
    }
    // Every layer inside a moving one (a moving group opened up stands in the stack as its
    // children), and every stack entry that holds a moving layer (a group not opened up is
    // composited whole, with its moved child).
    let mut inside = Vec::new();
    for id in ids {
        subtree(doc.layer(*id)?, &mut inside);
    }
    let moving = |l: &Layer| inside.contains(&l.id) || ids.iter().any(|id| holds(l, *id));
    let pos: Vec<usize> = stack.iter().enumerate().filter(|(_, l)| moving(l)).map(|(i, _)| i).collect();
    let (start, last) = (*pos.first()?, *pos.last()?);
    if stack.get(start)?.clipped {
        return None;
    }
    let mut end = last + 1;
    while stack.get(end).is_some_and(|l| l.clipped) {
        end += 1;
    }
    Some((start, end))
}

fn collect_ids(l: &Layer, out: &mut Vec<LayerId>) {
    out.push(l.id);
    for c in l.children().unwrap_or_default() {
        collect_ids(c, out);
    }
}

/// Documents this can split.
fn splittable(doc: &Document) -> bool {
    use photocraft_color::{ColorMode, SampleType};
    matches!(doc.mode, ColorMode::Rgb | ColorMode::Grayscale) && doc.depth != SampleType::F32 && doc.channels.is_empty() && !doc.has_artboards()
}

impl Split {
    /// The split for `plan` at `level` (`reduce` makes a level's document), or `None` when the drag
    /// can't be split.
    pub(crate) fn new(plan: &Plan, level: u32, reduce: &mut dyn FnMut(&Document) -> Document) -> Option<Split> {
        let base = &plan.base;
        if !splittable(base) {
            return None;
        }
        let stack = open_stack(&base.layers);
        let (start, end) = run_of(&stack, base, &plan.ids)?;
        let mode = base.mode;
        // The affine top: the longest run of the topmost layers that change an opaque backdrop
        // affinely, not starting inside a clipping group.
        let mut top = stack.len();
        while top > end && stack.get(top - 1).is_some_and(|l| photocraft_compose::composites_affinely(l, mode, true)) {
            top -= 1;
        }
        while top < stack.len() && stack.get(top).is_some_and(|l| l.clipped) {
            top += 1;
        }
        let mut shell = (**base).clone();
        shell.layers = Vec::new();
        let part = |layers: &[&Layer]| {
            let mut d = shell.clone();
            d.layers = layers.iter().map(|l| (*l).clone()).collect();
            d
        };
        if std::env::var_os("PHOTOCRAFT_MOVE_SPLIT_DEBUG").is_some() {
            let blocker = stack.get(top.saturating_sub(1)).map(|l| (l.name.clone(), l.blend, l.content.kind_name()));
            eprintln!("move split: stack {} start {start} end {end} top {top}; highest non-affine {blocker:?}", stack.len());
        }
        let middle = {
            let d = reduce(&part(stack.get(start..top).unwrap_or_default()));
            // What moves inside it: the moving layers themselves where the reduced document has
            // them, else (a moving group opened up) its children in the stack.
            let mut moving = Vec::new();
            for id in &plan.ids {
                if d.layer(*id).is_some() {
                    moving.push(*id);
                } else if let Some(g) = base.layer(*id) {
                    let mut inside = Vec::new();
                    collect_ids(g, &mut inside);
                    moving.extend(stack.get(start..top).unwrap_or_default().iter().map(|l| l.id).filter(|i| inside.contains(i)));
                }
            }
            (d, moving, 1i64 << level)
        };
        let below = Arc::new(reduce(&part(stack.get(..start)?)));
        let above = (top < stack.len()).then(|| Arc::new(reduce(&part(stack.get(top..).unwrap_or_default()))));
        // The run kept as an image (one layer laid over the rest), or as its affine map (layer
        // effects, type, clipped layers: anything that changes an opaque backdrop affinely), when
        // it looks the same wherever it goes; the middle is then the run alone (no layer between
        // it and the top).
        let run = stack.get(start..end).unwrap_or_default();
        let shifts = top == end && !run.is_empty() && run.first().is_some_and(|l| !l.clipped) && run.iter().all(|l| crate::gpu_canvas::moves_with_layer(l, 0));
        let over = run.len() == 1 && run.iter().all(|l| photocraft_compose::composites_as_over(l, mode));
        let affine = run.iter().all(|l| photocraft_compose::composites_affinely(l, mode, true));
        let blit = (shifts && (over || affine)).then(|| Blit::new(&middle.0, &middle.0.layers, over)).flatten();
        Some(Split {
            key: (plan.doc, plan.revision, plan.ids.clone(), level),
            middle,
            below,
            above,
            below_tiles: HashMap::new(),
            above_tiles: HashMap::new(),
            blit,
        })
    }

    /// Whether this split is the one for `plan` at `level`.
    pub(crate) fn is_for(&self, plan: &Plan, level: u32) -> bool {
        self.key.0 == plan.doc && self.key.1 == plan.revision && self.key.2 == plan.ids && self.key.3 == level
    }

    /// Composite `jobs` (each a level rect and the tile rect it lies in) of the preview `moved`:
    /// the cached below, the moving run and what isn't in the affine top over it, then the top.
    /// `None` when `moved` doesn't have this split's stack (the caller composites it whole).
    pub(crate) fn render(&mut self, offset: (i32, i32), jobs: &[(DRect, DRect)]) -> Option<Vec<Buffer>> {
        // The middle layers shifted by the offset in level pixels (rounded above level 0: the
        // preview shows the layer on the level's pixel grid; the release composites it exactly).
        let (d, moving, f) = &self.middle;
        let lv = |v: i32| (f64::from(v) / *f as f64).round() as i32;
        let middle = photocraft_engine::layer_multi_cmds::moved(d, moving, lv(offset.0), lv(offset.1)).ok()?;
        self.fill(jobs);
        let backdrops: Vec<Buffer> = jobs.iter().map(|(r, held)| crop(self.below_tiles.get(held), *r)).collect();
        let mut out = photocraft_compose::render_rects_over(&middle, backdrops);
        if let Some(above) = &self.above {
            for (b, (_, held)) in out.iter_mut().zip(jobs) {
                let applied = self.above_tiles.get(held).is_some_and(|(k, w)| photocraft_compose::apply_affine(b, k, w));
                if !applied {
                    // Not opaque here: the top layers composited for real.
                    if let Some(done) = photocraft_compose::render_rects_over(above, vec![std::mem::replace(b, Buffer::transparent(DRect::EMPTY))]).pop() {
                        *b = done;
                    }
                }
            }
        }
        Some(out)
    }

    /// [`Split::render`] by the one-blit path, as premultiplied RGBA8 per job: `None` when the
    /// drag can't take it (or a pixel under the top isn't opaque: the caller renders the frame).
    pub(crate) fn render_rgba8(&mut self, offset: (i32, i32), jobs: &[(DRect, DRect)]) -> Option<Vec<Vec<[u8; 4]>>> {
        self.blit.as_ref().filter(|b| b.ready())?;
        let f = self.middle.2;
        let lv = |v: i32| (f64::from(v) / f as f64).round() as i32;
        let (ox, oy) = (lv(offset.0), lv(offset.1));
        self.fill(jobs);
        let blit = self.blit.as_mut()?;
        let mut out = Vec::with_capacity(jobs.len());
        for (r, held) in jobs {
            if !blit.below.contains_key(held) {
                blit.below.insert(*held, self.below_tiles.get(held)?.to_premultiplied_rgba8());
            }
            if self.above.is_some() && !blit.above.contains_key(held) {
                let (k, w) = self.above_tiles.get(held)?;
                blit.above.insert(*held, (k.to_premultiplied_rgba8(), w.to_premultiplied_rgba8()));
            }
            let below = blit.below.get(held)?;
            let above = blit.above.get(held).filter(|_| self.above.is_some());
            let (rw, hw, sw) = (r.width() as usize, held.width() as usize, blit.rect.width() as usize);
            let mut px = vec![[0u8; 4]; rw * r.height() as usize];
            // The sprite's columns on this job, at the offset.
            let (sx0, sx1) = (r.x0.max(blit.rect.x0 + ox), r.x1.min(blit.rect.x1 + ox));
            for y in r.y0..r.y1 {
                let o = (y - r.y0) as usize * rw;
                let h = (y - held.y0) as usize * hw + (r.x0 - held.x0) as usize;
                let row = px.get_mut(o..o + rw)?;
                let brow = below.get(h..h + rw)?;
                row.copy_from_slice(brow);
                let sy = y - oy;
                if sx0 < sx1 && sy >= blit.rect.y0 && sy < blit.rect.y1 {
                    let (a, b) = ((sx0 - r.x0) as usize, (sx1 - r.x0) as usize);
                    let s = (sy - blit.rect.y0) as usize * sw + (sx0 - ox - blit.rect.x0) as usize;
                    match &blit.white {
                        None => photocraft_compose::blit::over_row(row.get_mut(a..b)?, brow.get(a..b)?, blit.px.get(s..s + (b - a))?),
                        Some(white) => {
                            if !photocraft_compose::blit::affine_row(row.get_mut(a..b)?, blit.px.get(s..s + (b - a))?, white.get(s..s + (b - a))?) {
                                return None;
                            }
                        }
                    }
                }
                if let Some((k, w)) = above
                    && !photocraft_compose::blit::affine_row(row, k.get(h..h + rw)?, w.get(h..h + rw)?)
                {
                    return None;
                }
            }
            out.push(px);
        }
        Some(out)
    }

    /// Make the one-blit path's sprite for up to `budget_ms` (a band at least). Whether it is
    /// ready (or there is none to make).
    pub(crate) fn prepare(&mut self, budget_ms: f64) -> bool {
        let Some(b) = self.blit.as_mut() else { return true };
        let t0 = crate::gpu_canvas::now_ms();
        while !b.ready() {
            b.band();
            if crate::gpu_canvas::now_ms() - t0 >= budget_ms {
                break;
            }
        }
        b.ready()
    }

    /// Whether the tile `held` still has to be made ([`Split::fill`]).
    pub(crate) fn needs(&self, held: DRect) -> bool {
        !self.below_tiles.contains_key(&held) || (self.above.is_some() && !self.above_tiles.contains_key(&held))
    }

    /// The below's (and the top's maps') tiles `jobs` hold, made where missing.
    pub(crate) fn fill(&mut self, jobs: &[(DRect, DRect)]) {
        let mut held: Vec<DRect> = jobs.iter().map(|(_, h)| *h).collect();
        held.sort_by_key(|r| (r.y0, r.x0, r.y1, r.x1));
        held.dedup();
        let new_below: Vec<DRect> = held.iter().copied().filter(|h| !self.below_tiles.contains_key(h)).collect();
        if !new_below.is_empty() {
            for b in photocraft_compose::render_rects(&self.below, &new_below) {
                self.below_tiles.insert(b.rect, b);
            }
        }
        if let Some(above) = &self.above {
            let new_above: Vec<DRect> = held.iter().copied().filter(|h| !self.above_tiles.contains_key(h)).collect();
            if !new_above.is_empty() {
                let backdrops: Vec<Buffer> =
                    new_above.iter().flat_map(|r| [Buffer::filled(*r, [0.0, 0.0, 0.0, 1.0]), Buffer::filled(*r, [1.0, 1.0, 1.0, 1.0])]).collect();
                let mut maps = photocraft_compose::render_rects_over(above, backdrops).into_iter();
                while let (Some(k), Some(w)) = (maps.next(), maps.next()) {
                    self.above_tiles.insert(k.rect, (k, w));
                }
            }
        }
        // The one-blit path's 8-bit copies, made with them.
        if let Some(blit) = self.blit.as_mut() {
            for h in &held {
                if !blit.below.contains_key(h)
                    && let Some(b) = self.below_tiles.get(h)
                {
                    blit.below.insert(*h, b.to_premultiplied_rgba8());
                }
                if self.above.is_some()
                    && !blit.above.contains_key(h)
                    && let Some((k, w)) = self.above_tiles.get(h)
                {
                    blit.above.insert(*h, (k.to_premultiplied_rgba8(), w.to_premultiplied_rgba8()));
                }
            }
        }
    }
}

/// `r` of the tile composite `held` (transparent where it has none).
fn crop(held: Option<&Buffer>, r: DRect) -> Buffer {
    let mut out = Buffer::transparent(r);
    let Some(h) = held else { return out };
    let i = r.intersect(&h.rect);
    if i.is_empty() {
        return out;
    }
    let (hw, ow, n) = (h.rect.width() as usize, r.width() as usize, i.width() as usize);
    for y in i.y0..i.y1 {
        let ho = (y - h.rect.y0) as usize * hw + (i.x0 - h.rect.x0) as usize;
        let oo = (y - r.y0) as usize * ow + (i.x0 - r.x0) as usize;
        if let (Some(d), Some(s)) = (out.px.get_mut(oo..oo + n), h.px.get(ho..ho + n)) {
            d.copy_from_slice(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
    use photocraft_doc::{Effect, Layer, LayerMask, Size};

    /// A stack with Normal, Multiply and Screen layers, a text-free group, an effect and an
    /// adjustment above the moving layer: the split preview equals the full composite of the
    /// moved document within float rounding (one 8-bit step), at levels 0 and 1.
    #[test]
    fn split_preview_matches_the_full_composite() {
        let mut d = Document::with_background("t", Size::new(300, 200), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
        let fmt = d.pixel_format();
        let mut ids = Vec::new();
        for i in 0..12 {
            let mut l = Layer::raster(format!("L{i}"), fmt);
            let x = i * 23 % 220;
            l.surface_mut()
                .unwrap()
                .fill_rect(DRect::new(x, 10 + i * 9, x + 70, 120 + i * 5), &[0.1 * i as f32 % 1.0, 0.5, 0.9 - 0.05 * i as f32, 0.4 + 0.05 * i as f32]);
            l.blend = [BlendMode::Normal, BlendMode::Multiply, BlendMode::Normal, BlendMode::Screen][i as usize % 4];
            l.opacity = 0.6 + 0.03 * i as f32;
            if i == 7 {
                l.effects.items = vec![Effect::default_drop_shadow()];
            }
            if i == 9 {
                let mut m = LayerMask::reveal_all();
                m.surface.fill_rect(DRect::new(0, 0, 150, 100), &[0.3]);
                l.mask = Some(m);
            }
            ids.push(d.insert_above(None, l));
        }
        let moving = ids[4];
        let base = Arc::new(d);
        for level in [0u32, 1] {
            let plan = Plan { base: base.clone(), ids: vec![moving], doc: base.id, revision: 1, offset: (0, 0) };
            let mut mips = photocraft_compose::mip::MipCache::default();
            let mut reduce = |doc: &Document| if level == 0 { doc.clone() } else { mips.document(doc, level) };
            let mut split = Split::new(&plan, level, &mut reduce).expect("splittable");
            assert!(split.above.is_some(), "the layers above are an affine top");
            // Offsets on the level's pixel grid (the preview rounds others to it).
            for (dx, dy) in [(6, 4), (32, -12)] {
                let moved = photocraft_engine::layer_multi_cmds::moved(&base, &[moving], dx, dy).unwrap();
                let f = 1i32 << level;
                let held = DRect::new(0, 0, 300 / f, 200 / f);
                let jobs = [(DRect::new(10 / f, 20 / f, 200 / f, 150 / f), held), (held, held)];
                let got = split.render((dx, dy), &jobs).expect("rendered");
                let full = reduce(&moved);
                let want = photocraft_compose::render_rects(&full, &jobs.iter().map(|j| j.0).collect::<Vec<_>>());
                for (g, w) in got.iter().zip(&want) {
                    assert_eq!(g.rect, w.rect);
                    let worst = g.px.iter().zip(&w.px).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0f32, f32::max);
                    assert!(worst <= 1.0 / 255.0, "level {level} offset {dx},{dy}: off by {worst}");
                }
            }
        }
    }

    /// A layer inside a pass-through group on a page without a Background (transparent pixels take
    /// the top layers for real), and Multiply / Screen layers with drop shadows above it: the split
    /// preview still equals the full composite.
    #[test]
    fn nested_moves_and_transparent_pixels_match_the_full_composite() {
        let mut d = Document::new("t", Size::new(200, 160), ColorMode::Rgb, SampleType::U8);
        let fmt = d.pixel_format();
        let paint = |name: &str, r: DRect, c: [f32; 4], blend: BlendMode| {
            let mut l = Layer::raster(name, fmt);
            l.surface_mut().unwrap().fill_rect(r, &c);
            l.blend = blend;
            l
        };
        d.insert_above(None, paint("paper", DRect::new(0, 0, 150, 160), [1.0, 1.0, 0.9, 1.0], BlendMode::Normal));
        let inner = paint("inner", DRect::new(20, 20, 90, 100), [0.2, 0.4, 0.8, 0.9], BlendMode::Normal);
        let moving = inner.id;
        let mut group = Layer::group("g", vec![paint("under", DRect::new(10, 50, 120, 70), [0.9, 0.1, 0.1, 1.0], BlendMode::Normal), inner]);
        group.blend = BlendMode::PassThrough;
        let group_id = d.insert_above(None, group);
        let mut tone = paint("tone", DRect::new(0, 0, 200, 160), [0.7, 0.7, 0.7, 0.5], BlendMode::Multiply);
        tone.effects.items = vec![Effect::default_drop_shadow()];
        d.insert_above(None, tone);
        let glow = d.insert_above(None, paint("glow", DRect::new(60, 30, 180, 90), [0.3, 0.9, 0.3, 0.6], BlendMode::Screen));
        let base = Arc::new(d);
        // A layer in the group, the whole (opened-up) group, and two layers far apart.
        for ids in [vec![moving], vec![group_id], vec![moving, glow]] {
            let plan = Plan { base: base.clone(), ids: ids.clone(), doc: base.id, revision: 1, offset: (0, 0) };
            let mut reduce = |doc: &Document| doc.clone();
            let mut split = Split::new(&plan, 0, &mut reduce).expect("splits");
            let moved = photocraft_engine::layer_multi_cmds::moved(&base, &ids, 37, 21).unwrap();
            let all = DRect::new(0, 0, 200, 160);
            let jobs = [(DRect::new(5, 5, 170, 140), all), (all, all)];
            let got = split.render((37, 21), &jobs).expect("rendered");
            let want = photocraft_compose::render_rects(&moved, &[jobs[0].0, jobs[1].0]);
            for (g, w) in got.iter().zip(&want) {
                let worst = g.px.iter().zip(&w.px).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0f32, f32::max);
                assert!(worst <= 1.0 / 255.0, "{ids:?}: off by {worst}");
            }
        }
    }

    /// A Normal layer at the bottom of a stack of Normal, Multiply and Screen layers (an effect, a
    /// mask): its drag takes the one-blit path, and every frame equals the full composite of the
    /// moved document within 8-bit rounding, at levels 0 and 1, including pixels the layer brings
    /// in from past the canvas edge.
    #[test]
    fn one_blit_frames_match_the_full_composite() {
        let mut d = Document::with_background("t", Size::new(300, 200), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
        let fmt = d.pixel_format();
        let mut photo = Layer::raster("photo", fmt);
        let s = photo.surface_mut().unwrap();
        for i in 0..20 {
            s.fill_rect(DRect::new(-60 + i * 21, -30, -60 + i * 21 + 13, 240), &[0.05 * i as f32, 0.7 - 0.03 * i as f32, 0.4, 0.6 + 0.02 * i as f32]);
        }
        photo.opacity = 0.9;
        let moving = d.insert_above(None, photo);
        for i in 0..8 {
            let mut l = Layer::raster(format!("L{i}"), fmt);
            let x = i * 31 % 200;
            l.surface_mut().unwrap().fill_rect(DRect::new(x, 10 + i * 17, x + 90, 120 + i * 6), &[0.9 - 0.1 * i as f32, 0.3, 0.1 * i as f32, 0.5]);
            l.blend = [BlendMode::Normal, BlendMode::Multiply, BlendMode::Screen][i as usize % 3];
            if i == 3 {
                l.effects.items = vec![Effect::default_drop_shadow()];
            }
            if i == 5 {
                let mut m = LayerMask::reveal_all();
                m.surface.fill_rect(DRect::new(0, 0, 120, 200), &[0.25]);
                l.mask = Some(m);
            }
            d.insert_above(None, l);
        }
        let base = Arc::new(d);
        for level in [0u32, 1] {
            let plan = Plan { base: base.clone(), ids: vec![moving], doc: base.id, revision: 1, offset: (0, 0) };
            let mut mips = photocraft_compose::mip::MipCache::default();
            let mut reduce = |doc: &Document| if level == 0 { doc.clone() } else { mips.document(doc, level) };
            let mut split = Split::new(&plan, level, &mut reduce).expect("splittable");
            assert!(split.blit.is_some(), "one layer laid over the rest, an affine top: the one-blit path");
            assert!(split.render_rgba8((1, 1), &[(DRect::new(0, 0, 8, 8), DRect::new(0, 0, 8, 8))]).is_none(), "not before its sprite is made");
            assert!(split.prepare(f64::INFINITY));
            for (dx, dy) in [(6, 4), (32, -12), (-40, 20)] {
                let moved = photocraft_engine::layer_multi_cmds::moved(&base, &[moving], dx, dy).unwrap();
                let f = 1i32 << level;
                let held = DRect::new(0, 0, 300 / f, 200 / f);
                let jobs = [(DRect::new(10 / f, 20 / f, 200 / f, 150 / f), held), (held, held)];
                let got = split.render_rgba8((dx, dy), &jobs).expect("blitted");
                let full = reduce(&moved);
                let want = photocraft_compose::render_rects(&full, &jobs.iter().map(|j| j.0).collect::<Vec<_>>());
                for (g, w) in got.iter().zip(&want) {
                    let w = w.to_premultiplied_rgba8();
                    let worst = g.iter().zip(&w).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0);
                    assert!(worst <= 2, "level {level} offset {dx},{dy}: off by {worst}/255");
                }
            }
        }
    }

    /// Scattered moves, groups and nested layers split; Lab documents don't.
    #[test]
    fn scattered_or_nested_moves_fall_back() {
        let mut d = Document::with_background("t", Size::new(64, 64), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
        let fmt = d.pixel_format();
        let a = d.insert_above(None, Layer::raster("a", fmt));
        let _b = d.insert_above(None, Layer::raster("b", fmt));
        let c = d.insert_above(None, Layer::raster("c", fmt));
        let inner = Layer::raster("inner", fmt);
        let inner_id = inner.id;
        d.insert_above(None, Layer::group("g", vec![inner]));
        let base = Arc::new(d);
        let split =
            |ids: Vec<LayerId>| Split::new(&Plan { base: base.clone(), ids, doc: base.id, revision: 1, offset: (0, 0) }, 0, &mut |d| d.clone()).is_some();
        assert!(split(vec![a]));
        assert!(split(vec![a, c]), "not contiguous: the span between them composites per frame");
        assert!(split(vec![inner_id]), "inside a pass-through group: opened up");
        let mut iso = (*base).clone();
        if let Some(g) = iso.layers.last_mut() {
            g.blend = BlendMode::Normal;
        }
        let iso = Arc::new(iso);
        assert!(
            Split::new(&Plan { base: iso.clone(), ids: vec![inner_id], doc: iso.id, revision: 1, offset: (0, 0) }, 0, &mut |d| d.clone()).is_some(),
            "inside an isolated group: the group composites per frame"
        );
        let mut lab = (*base).clone();
        lab.mode = ColorMode::Lab;
        let lab = Arc::new(lab);
        assert!(Split::new(&Plan { base: lab.clone(), ids: vec![a], doc: lab.id, revision: 1, offset: (0, 0) }, 0, &mut |d| d.clone()).is_none(), "Lab");
    }
}
