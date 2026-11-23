//! Edits on the CPU canvas: each frame re-blends only the layers that change.
//!
//! A Move drag shows the document with the moving layers at the pointer (`move_ui`), and the
//! canvas recomposited the whole stack over where they were and are, every frame: 140 ms a frame
//! for a layer of a 200-layer 4000 x 6000 page. Every other edit did the same over its damage: a
//! brush dab, a keystroke, hiding a layer, a transform's commit (260 to 340 ms for a layer as large
//! as a 1920 x 1080 page of 20 layers). The layers below the ones an edit changes don't change with
//! it, and nor do those above; so the CPU canvas keeps, per tile, made the first time a tile needs
//! them (or ahead, while nothing else is going on):
//!
//! - the composite of the layers below the run (exact: the same stack prefix the full composite
//!   starts with);
//! - for the topmost layers that change an opaque backdrop affinely
//!   ([`photocraft_compose::composites_affinely`]: Normal, Multiply, Screen, their effects and
//!   groups), their composite over opaque black and over opaque white, which give every pixel's
//!   `c -> n + m·c` ([`photocraft_compose::apply_affine`]; exact for those modes, type layers'
//!   anti-aliased edges approximated);
//!
//! and per frame composites only the run and the layers between it and that top over the cached
//! below, then applies the top's map. Pixels that aren't opaque (no Background) take the top
//! layers composited for real.
//!
//! The split is anchored at the layers that change: a Move drag's moving layers (and, with the
//! Move tool in hand, the layers a press would move), else the layers the last edit changed or the
//! selected ones. An edit that changes only layers between the cached below and top keeps the
//! tiles ([`Split::update`]); one that changes a layer elsewhere is composited whole, once, and
//! the split is made again around what it changed.
//!
//! The stack is seen with mix-free pass-through groups opened up (their children composite
//! straight into the backdrop, `photocraft_compose`), so a layer in such a group changes as if it
//! were at the top level. Anything this can't split falls back to the full composite: a run
//! clipped to a layer below it, Lab, 32-bit, CMYK or multichannel documents, spot channels,
//! artboards. A tile drawn through the top's map is replaced by the exact composite once the
//! edits pause.

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_compose::Buffer;
use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId};
use photocraft_geom::Rect as DRect;

/// What the canvas shows through a split: a Move preview (from `move_ui::cpu_plan`), or the
/// document as it is around the layers an edit changes (offset 0).
#[derive(Clone)]
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

/// The cached composites below and above the run of layers that change, at one level.
pub(crate) struct Split {
    key: (DocId, u64, Vec<LayerId>, u32),
    /// The per-frame layers (`lo..top`) as the split's document has them, reduced to the level
    /// once, the layers in them that move, and the level's factor: each frame of a drag shifts
    /// only those, by the offset in level pixels (reducing the moved full-resolution layers every
    /// frame cost as much as compositing them, and the full-resolution moved document isn't
    /// needed at all).
    middle: (Document, Vec<LayerId>, i64),
    below: Arc<Document>,
    /// The affine top, and the same layers for pixels that aren't opaque.
    above: Option<Arc<Document>>,
    below_tiles: HashMap<DRect, Buffer>,
    /// The top over opaque black and over opaque white, per tile.
    above_tiles: HashMap<DRect, (Buffer, Buffer)>,
    /// The one-blit path ([`Blit`]), when the drag can take it.
    blit: Option<Blit>,
    /// The document the split was made from (its last rebase's or update's) and where in its open
    /// stack the cached below ends (`lo`), the run starts and ends, and the top starts
    /// ([`Split::rebase`], [`Split::update`]).
    base: Arc<Document>,
    lo: usize,
    run: (usize, usize),
    top: usize,
    /// Where the moving layers stand relative to the split's images (document pixels, a multiple
    /// of the level's factor): the offsets of the drags it was rebased on, added to each frame's.
    origin: (i32, i32),
    /// The most its tiles may hold (bytes): past it a tile isn't made and the frame is composited
    /// whole ([`Split::set_budget`]).
    budget: usize,
}

/// What an edit left of a split ([`Split::update`]).
#[derive(Debug, PartialEq)]
pub(crate) enum Update {
    /// The document differs only between the cached below and top (or not at all): the tiles hold.
    Held,
    /// Layers below the run or in the top changed (the layers that differ, in either document):
    /// the tiles don't hold.
    Elsewhere(Vec<LayerId>),
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

/// Whether what the composite depends on besides the layers is the same in `a` and `b`.
fn same_frame(a: &Document, b: &Document) -> bool {
    a.id == b.id
        && a.size == b.size
        && a.mode == b.mode
        && a.depth == b.depth
        && a.icc_profile == b.icc_profile
        && a.global_light == b.global_light
        && a.patterns == b.patterns
        && a.color_table == b.color_table
        && a.duotone == b.duotone
        && a.channels == b.channels
        && a.quick_mask == b.quick_mask
}

/// `shell` (a document without layers) with `layers`.
fn part(shell: &Document, layers: &[&Layer]) -> Document {
    let mut d = shell.clone();
    d.layers = layers.iter().map(|l| (*l).clone()).collect();
    d
}

/// The per-frame part `lo..top` of `stack` (the open stack of `base`) reduced to the level, with
/// the layers of it that move (`ids`, or a moving group's children that stand in the stack) and
/// the level's factor; and the one-blit path's sprite when the run `start..end` can take it.
fn middle_of(
    base: &Document,
    shell: &Document,
    stack: &[&Layer],
    (lo, (start, end), top): (usize, (usize, usize), usize),
    ids: &[LayerId],
    level: u32,
    reduce: &mut dyn FnMut(&Document) -> Document,
) -> ((Document, Vec<LayerId>, i64), Option<Blit>) {
    let between = stack.get(lo..top).unwrap_or_default();
    let d = reduce(&part(shell, between));
    // What moves inside it: the moving layers themselves where the reduced document has them,
    // else (a moving group opened up) its children in the stack.
    let mut moving = Vec::new();
    for id in ids {
        if d.layer(*id).is_some() {
            moving.push(*id);
        } else if let Some(g) = base.layer(*id) {
            let mut inside = Vec::new();
            collect_ids(g, &mut inside);
            moving.extend(between.iter().map(|l| l.id).filter(|i| inside.contains(i)));
        }
    }
    // The run kept as an image (one layer laid over the rest), or as its affine map (layer
    // effects, type, clipped layers: anything that changes an opaque backdrop affinely), when it
    // looks the same wherever it goes and the per-frame part is the run alone (no layer between
    // the cached below and it, or between it and the top).
    let mode = base.mode;
    let run = stack.get(start..end).unwrap_or_default();
    let shifts = lo == start && top == end && !run.is_empty() && run.first().is_some_and(|l| !l.clipped) && run.iter().all(|l| crate::gpu_canvas::moves_with_layer(l, 0));
    let over = run.len() == 1 && run.iter().all(|l| photocraft_compose::composites_as_over(l, mode));
    let affine = run.iter().all(|l| photocraft_compose::composites_affinely(l, mode, true));
    let blit = (shifts && (over || affine)).then(|| Blit::new(&d, &d.layers, over)).flatten();
    ((d, moving, 1i64 << level), blit)
}

/// `f` over `items` with their indices, on the worker pool where this thread may wait for it.
pub(crate) fn each<T: Send>(items: &mut [T], f: impl Fn(usize, &mut T) + Sync) {
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    {
        // Not from the page's own thread in the browser threads build: it is no pool worker and
        // would wait for a pool that may be busy for longer than the loop takes on its own.
        if cfg!(not(target_arch = "wasm32")) || rayon::current_thread_index().is_some() {
            use rayon::prelude::*;
            items.par_iter_mut().enumerate().for_each(|(i, t)| f(i, t));
            return;
        }
    }
    items.iter_mut().enumerate().for_each(|(i, t)| f(i, t));
}

fn area(r: DRect) -> usize {
    r.width() as usize * r.height() as usize
}

impl Split {
    /// The split for `plan` at `level` (`reduce` makes a level's document), or `None` when the
    /// document can't be split around the plan's layers.
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
        if std::env::var_os("PHOTOCRAFT_MOVE_SPLIT_DEBUG").is_some() {
            let blocker = stack.get(top.saturating_sub(1)).map(|l| (l.name.clone(), l.blend, l.content.kind_name()));
            eprintln!("move split: stack {} start {start} end {end} top {top}; highest non-affine {blocker:?}", stack.len());
        }
        let (middle, blit) = middle_of(base, &shell, &stack, (start, (start, end), top), &plan.ids, level, reduce);
        let below = Arc::new(reduce(&part(&shell, stack.get(..start)?)));
        let above = (top < stack.len()).then(|| Arc::new(reduce(&part(&shell, stack.get(top..).unwrap_or_default()))));
        Some(Split {
            key: (plan.doc, plan.revision, plan.ids.clone(), level),
            middle,
            below,
            above,
            below_tiles: HashMap::new(),
            above_tiles: HashMap::new(),
            blit,
            base: base.clone(),
            lo: start,
            run: (start, end),
            top,
            origin: (0, 0),
            budget: usize::MAX,
        })
    }

    /// The document is now `doc` (an edit, an undo, a preview of one), revision `revision`: when
    /// it differs from the split's only in layers between the cached below and top, the split is
    /// kept for it, with those layers as they are now ([`Update::Held`]: its tiles still show
    /// what is below and above them). Otherwise the split is left as it was and the layers that
    /// differ are returned: the caller composites the frame whole and makes a new split.
    pub(crate) fn update(&mut self, doc: &Arc<Document>, revision: u64, reduce: &mut dyn FnMut(&Document) -> Document) -> Update {
        if Arc::ptr_eq(&self.base, doc) {
            self.key.1 = revision;
            return Update::Held;
        }
        let (was, now) = (open_stack(&self.base.layers), open_stack(&doc.layers));
        let same_below = was.iter().zip(&now).take_while(|(a, b)| a == b).count();
        let rest = was.len().min(now.len()) - same_below;
        let same_above = was.iter().rev().zip(now.iter().rev()).take(rest).take_while(|(a, b)| a == b).count();
        let above = was.len().saturating_sub(self.top);
        let held = splittable(doc) && same_frame(&self.base, doc) && same_below >= self.lo && same_above >= above;
        let at = held.then(|| now.len() - above).and_then(|top| run_of(&now, doc, &self.key.2).filter(|r| r.0 >= self.lo && r.1 <= top).map(|run| (run, top)));
        let Some((run, top)) = at else {
            let mut ids: Vec<LayerId> = Vec::new();
            for l in was.get(same_below..was.len() - same_above).into_iter().chain(now.get(same_below..now.len() - same_above)).flatten() {
                if !ids.contains(&l.id) {
                    ids.push(l.id);
                }
            }
            return Update::Elsewhere(ids);
        };
        if same_below + same_above < was.len().max(now.len()) {
            let mut shell = (**doc).clone();
            shell.layers = Vec::new();
            let level = self.key.3;
            let (middle, mut blit) = middle_of(doc, &shell, &now, (self.lo, run, top), &self.key.2, level, reduce);
            // The 8-bit copies of the tiles still hold.
            if let (Some(new), Some(old)) = (blit.as_mut(), self.blit.take()) {
                (new.below, new.above) = (old.below, old.above);
            }
            (self.middle, self.blit, self.run, self.top, self.origin) = (middle, blit, run, top, (0, 0));
        }
        self.base = doc.clone();
        self.key.1 = revision;
        Update::Held
    }

    /// The drag this split drew ended, and `layer.translate` committed its offset `offset` as
    /// revision `revision` of `doc`: when `doc` is the split's document with the run only shifted
    /// by it (the other layers the same ones, an offset on the level's pixel grid), the
    /// split is kept for `doc`, as the GPU canvas's drag cache is (`gpu_canvas::above`): the
    /// release's tiles and a drag of the same layers again are drawn from it at once, instead of
    /// compositing the stack again (100-160 ms a frame on a credits page). Whether it was.
    pub(crate) fn rebase(&mut self, doc: &Arc<Document>, revision: u64, offset: (i32, i32)) -> bool {
        let f = i32::try_from(self.middle.2).unwrap_or(i32::MAX);
        let (start, end) = self.run;
        if f <= 0 || offset.0 % f != 0 || offset.1 % f != 0 || doc.id != self.key.0 || doc.size != self.base.size {
            return false;
        }
        let (was, now) = (open_stack(&self.base.layers), open_stack(&doc.layers));
        let same = |a: &[&Layer], b: &[&Layer]| a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x == y);
        let all = DRect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
        let shifted = |a: &[&Layer], b: &[&Layer]| {
            a.len() == b.len()
                && a.iter().zip(b).all(|(x, y)| x.id == y.id && photocraft_compose::change_bounds(x, all).map(|r| r.translate(offset.0, offset.1)) == photocraft_compose::change_bounds(y, all))
        };
        let ok = was.len() == now.len()
            && same(was.get(..start).unwrap_or_default(), now.get(..start).unwrap_or_default())
            && same(was.get(end..).unwrap_or_default(), now.get(end..).unwrap_or_default())
            && shifted(was.get(start..end).unwrap_or_default(), now.get(start..end).unwrap_or_default());
        if ok {
            self.key.1 = revision;
            self.base = doc.clone();
            self.origin = (self.origin.0 + offset.0, self.origin.1 + offset.1);
        }
        ok
    }

    /// The tiles of `old`'s below that this split's below shows too, copied: `old` was made for
    /// the same document at the same level and the layers below its run are the ones below this
    /// one's (an edit that changed the run and layers over it: the box of Free Transform hides
    /// the layer and what covers it). The frame the split is made for then composites nothing
    /// below the run.
    pub(crate) fn inherit(&mut self, old: &Split) {
        if old.key.0 != self.key.0 || old.key.3 != self.key.3 || old.lo != self.lo || !same_frame(&old.base, &self.base) {
            return;
        }
        let (was, now) = (open_stack(&old.base.layers), open_stack(&self.base.layers));
        if was.get(..self.lo).is_none_or(|a| now.get(..self.lo) != Some(a)) {
            return;
        }
        for (rect, tile) in &old.below_tiles {
            if self.bytes().saturating_add(area(*rect) * 16) > self.budget {
                break;
            }
            self.below_tiles.insert(*rect, tile.clone());
        }
    }

    /// The layers this split is around.
    pub(crate) fn ids(&self) -> &[LayerId] {
        &self.key.2
    }

    /// Whether this split is of document `doc` at `level`.
    pub(crate) fn of(&self, doc: DocId, level: u32) -> bool {
        self.key.0 == doc && self.key.3 == level
    }

    /// Whether this split is the one for `plan` at `level`.
    pub(crate) fn is_for(&self, plan: &Plan, level: u32) -> bool {
        // A later revision whose layers are the same ones (the history step a move leaves, a
        // selection) still has this split's stack.
        self.moves(plan.doc, &plan.ids, level) && (self.key.1 == plan.revision || Arc::ptr_eq(&self.base, &plan.base) || self.base.layers == plan.base.layers)
    }

    /// Whether this split is around `ids` of document `doc` at `level` (whatever its state).
    pub(crate) fn moves(&self, doc: DocId, ids: &[LayerId], level: u32) -> bool {
        self.key.0 == doc && self.key.2 == ids && self.key.3 == level
    }

    /// Composite `jobs` (each a level rect and the tile rect it lies in) with the run at
    /// `offset`: the cached below, the run and what isn't in the affine top over it, then the
    /// top. The below's tiles are made where missing; the top's maps too with `maps` (a drag's
    /// frames: the ones that follow are drawn from them), else a tile without its map takes the
    /// top layers composited for real (an edit costs no more than the whole composite, and its
    /// maps are made ahead, off the frame). With the buffers, whether a map stood in for the top
    /// in any (a map is within a float rounding of its layers and approximates type layers'
    /// anti-aliased edges: such a tile is composited exactly once the edits pause). `None` when
    /// a tile it needs doesn't fit the budget (the caller composites the frame whole).
    pub(crate) fn render(&mut self, offset: (i32, i32), jobs: &[(DRect, DRect)], maps: bool) -> Option<(Vec<Buffer>, bool)> {
        // The middle layers shifted by the offset in level pixels (rounded above level 0: the
        // preview shows the layer on the level's pixel grid; the release composites it exactly).
        let (d, moving, f) = &self.middle;
        let lv = |v: i32| (f64::from(v) / *f as f64).round() as i32;
        let (ox, oy) = (offset.0 + self.origin.0, offset.1 + self.origin.1);
        let middle = photocraft_engine::layer_multi_cmds::moved(d, moving, lv(ox), lv(oy)).ok()?;
        if !self.fill(jobs, maps) {
            return None;
        }
        let backdrops: Vec<Buffer> = jobs.iter().map(|(r, held)| crop(self.below_tiles.get(held), *r)).collect();
        let out = photocraft_compose::render_rects_over(&middle, backdrops);
        let Some(above) = &self.above else { return Some((out, false)) };
        let tiles = &self.above_tiles;
        let mut out: Vec<(Buffer, bool)> = out.into_iter().map(|b| (b, false)).collect();
        each(&mut out, |i, (b, mapped)| *mapped = jobs.get(i).and_then(|(_, held)| tiles.get(held)).is_some_and(|(k, w)| photocraft_compose::apply_affine(b, k, w)));
        // No map yet, or not opaque there: the top layers composited for real, in one pass.
        let real: Vec<usize> = out.iter().enumerate().filter(|(_, o)| !o.1).map(|(i, _)| i).collect();
        let backdrops: Vec<Buffer> = out.iter_mut().filter(|o| !o.1).map(|o| std::mem::replace(&mut o.0, Buffer::transparent(DRect::EMPTY))).collect();
        if !real.is_empty() {
            for (i, done) in real.iter().zip(photocraft_compose::render_rects_over(above, backdrops)) {
                if let Some(o) = out.get_mut(*i) {
                    o.0 = done;
                }
            }
        }
        let mapped = out.iter().any(|o| o.1);
        Some((out.into_iter().map(|o| o.0).collect(), mapped))
    }

    /// [`Split::render`] by the one-blit path, as premultiplied RGBA8 per job: `None` when the
    /// drag can't take it (or a pixel under the top isn't opaque: the caller renders the frame).
    pub(crate) fn render_rgba8(&mut self, offset: (i32, i32), jobs: &[(DRect, DRect)]) -> Option<Vec<Vec<[u8; 4]>>> {
        self.blit.as_ref().filter(|b| b.ready())?;
        let f = self.middle.2;
        let lv = |v: i32| (f64::from(v) / f as f64).round() as i32;
        let at = (lv(offset.0 + self.origin.0), lv(offset.1 + self.origin.1));
        if !self.fill(jobs, true) {
            return None;
        }
        let blit = self.blit.as_ref()?;
        let top = self.above.is_some();
        let mut out: Vec<Option<Vec<[u8; 4]>>> = vec![None; jobs.len()];
        each(&mut out, |i, o| *o = jobs.get(i).and_then(|(r, held)| blit.job(*r, *held, at, top)));
        out.into_iter().collect()
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
        (self.below_kept() && !self.below_tiles.contains_key(&held))
            || (self.above.is_some() && !self.above_tiles.contains_key(&held))
            || self.blit.as_ref().is_some_and(|b| !b.below.contains_key(&held))
    }

    /// Whether the below's tiles are kept: not when no layer is below the run (a transparent
    /// tile is all there would be), unless the one-blit path copies from them.
    fn below_kept(&self) -> bool {
        !self.below.layers.is_empty() || self.blit.is_some()
    }

    /// Bytes its tiles hold.
    pub(crate) fn bytes(&self) -> usize {
        let below: usize = self.below_tiles.keys().map(|r| area(*r) * 16).sum();
        let above: usize = self.above_tiles.keys().map(|r| area(*r) * 32).sum();
        let blit: usize = self.blit.as_ref().map_or(0, |b| b.below.keys().map(|r| area(*r) * 4).sum::<usize>() + b.above.keys().map(|r| area(*r) * 8).sum::<usize>());
        below + above + blit
    }

    /// Hold at most `budget` bytes of tiles: when it holds more, the tiles that aren't in `view`
    /// go (the view's stay whatever they cost; [`Split::fill`] makes no more past the budget).
    pub(crate) fn set_budget(&mut self, budget: usize, view: &[DRect]) {
        self.budget = budget;
        if self.bytes() > budget {
            self.below_tiles.retain(|r, _| view.contains(r));
            self.above_tiles.retain(|r, _| view.contains(r));
            if let Some(b) = self.blit.as_mut() {
                b.below.retain(|r, _| view.contains(r));
                b.above.retain(|r, _| view.contains(r));
            }
        }
    }

    /// The below's tiles `jobs` hold (and with `maps` the top's maps), made where missing.
    /// `false`, with nothing made, when they don't fit the budget.
    pub(crate) fn fill(&mut self, jobs: &[(DRect, DRect)], maps: bool) -> bool {
        let mut held: Vec<DRect> = jobs.iter().map(|(_, h)| *h).collect();
        held.sort_by_key(|r| (r.y0, r.x0, r.y1, r.x1));
        held.dedup();
        let new_below: Vec<DRect> = held.iter().copied().filter(|h| self.below_kept() && !self.below_tiles.contains_key(h)).collect();
        let new_above: Vec<DRect> = held.iter().copied().filter(|h| maps && self.above.is_some() && !self.above_tiles.contains_key(h)).collect();
        let eight = if self.blit.is_some() { (4, 8) } else { (0, 0) };
        let more: usize = new_below.iter().map(|r| area(*r) * (16 + eight.0)).sum::<usize>() + new_above.iter().map(|r| area(*r) * (32 + eight.1)).sum::<usize>();
        if more > 0 && self.bytes().saturating_add(more) > self.budget {
            return false;
        }
        if !new_below.is_empty() {
            for b in photocraft_compose::render_rects(&self.below, &new_below) {
                self.below_tiles.insert(b.rect, b);
            }
        }
        if let Some(above) = self.above.as_ref().filter(|_| !new_above.is_empty()) {
            let backdrops: Vec<Buffer> = new_above.iter().flat_map(|r| [Buffer::filled(*r, [0.0, 0.0, 0.0, 1.0]), Buffer::filled(*r, [1.0, 1.0, 1.0, 1.0])]).collect();
            let mut maps = photocraft_compose::render_rects_over(above, backdrops).into_iter();
            while let (Some(k), Some(w)) = (maps.next(), maps.next()) {
                self.above_tiles.insert(k.rect, (k, w));
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
        true
    }
}

impl Blit {
    /// The level rect `r` of the tile `held`, premultiplied RGBA8: the below's copy, the sprite
    /// at `(ox, oy)` over it, the top's map (`top`: there is one) over that. `None` when a copy
    /// isn't made yet or a pixel under the top isn't opaque.
    fn job(&self, r: DRect, held: DRect, (ox, oy): (i32, i32), top: bool) -> Option<Vec<[u8; 4]>> {
        let below = self.below.get(&held)?;
        let above = if top { Some(self.above.get(&held)?) } else { None };
        let (rw, hw, sw) = (r.width() as usize, held.width() as usize, self.rect.width() as usize);
        let mut px = vec![[0u8; 4]; rw * r.height() as usize];
        // The sprite's columns on this job, at the offset.
        let (sx0, sx1) = (r.x0.max(self.rect.x0 + ox), r.x1.min(self.rect.x1 + ox));
        for y in r.y0..r.y1 {
            let o = (y - r.y0) as usize * rw;
            let h = (y - held.y0) as usize * hw + (r.x0 - held.x0) as usize;
            let row = px.get_mut(o..o + rw)?;
            let brow = below.get(h..h + rw)?;
            row.copy_from_slice(brow);
            let sy = y - oy;
            if sx0 < sx1 && sy >= self.rect.y0 && sy < self.rect.y1 {
                let (a, b) = ((sx0 - r.x0) as usize, (sx1 - r.x0) as usize);
                let s = (sy - self.rect.y0) as usize * sw + (sx0 - ox - self.rect.x0) as usize;
                match &self.white {
                    None => photocraft_compose::blit::over_row(row.get_mut(a..b)?, brow.get(a..b)?, self.px.get(s..s + (b - a))?),
                    Some(white) => {
                        if !photocraft_compose::blit::affine_row(row.get_mut(a..b)?, self.px.get(s..s + (b - a))?, white.get(s..s + (b - a))?) {
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
        Some(px)
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
                let (got, mapped) = split.render((dx, dy), &jobs, true).expect("rendered");
                assert!(mapped, "the top's map stood in for its layers");
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
            let got = split.render((37, 21), &jobs, true).expect("rendered").0;
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

    /// A white page, an ink layer, then Normal, Multiply and Screen layers over it (one with a
    /// drop shadow). The ink layer's id and the ids above it, bottom first.
    fn inked() -> (Document, LayerId, Vec<LayerId>) {
        let mut d = Document::with_background("t", Size::new(300, 200), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
        let fmt = d.pixel_format();
        let mut under = Layer::raster("under", fmt);
        under.surface_mut().unwrap().fill_rect(DRect::new(0, 0, 200, 150), &[0.9, 0.8, 0.2, 0.8]);
        d.insert_above(None, under);
        let mut ink = Layer::raster("ink", fmt);
        ink.surface_mut().unwrap().fill_rect(DRect::new(40, 30, 160, 120), &[0.1, 0.3, 0.8, 0.9]);
        let ink = d.insert_above(None, ink);
        let mut over = Vec::new();
        for (i, blend) in [BlendMode::Multiply, BlendMode::Normal, BlendMode::Screen].into_iter().enumerate() {
            let mut l = Layer::raster(format!("over {i}"), fmt);
            let x = 30 + i as i32 * 60;
            l.surface_mut().unwrap().fill_rect(DRect::new(x, 20, x + 110, 170), &[0.8 - 0.2 * i as f32, 0.4, 0.3 + 0.2 * i as f32, 0.5]);
            l.blend = blend;
            if i == 1 {
                l.effects.items = vec![Effect::default_drop_shadow()];
            }
            over.push(d.insert_above(None, l));
        }
        (d, ink, over)
    }

    fn worst(got: &Buffer, want: &Buffer) -> f32 {
        assert_eq!(got.rect, want.rect);
        got.px.iter().zip(&want.px).flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs())).fold(0.0f32, f32::max)
    }

    /// Edits of the layers between the cached below and the top (paint on the layer, hide it, a
    /// new layer over it, an undo back) keep the split and its tiles: each is drawn by
    /// compositing only those layers, and equals the full composite of the edited document.
    #[test]
    fn an_edit_between_the_below_and_the_top_keeps_the_tiles() {
        let (d, ink, _) = inked();
        let base = Arc::new(d);
        let plan = Plan { base: base.clone(), ids: vec![ink], doc: base.id, revision: 1, offset: (0, 0) };
        let mut split = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        let all = DRect::new(0, 0, 300, 200);
        let jobs = [(all, all)];
        assert!(split.fill(&jobs, true));
        assert!(!split.needs(all));
        let mut painted = (*base).clone();
        painted.layer_mut(ink).unwrap().surface_mut().unwrap().fill_rect(DRect::new(100, 60, 260, 190), &[0.9, 0.1, 0.1, 1.0]);
        let mut hidden = painted.clone();
        hidden.layer_mut(ink).unwrap().visible = false;
        let mut added = painted.clone();
        let mut patch = Layer::raster("patch", added.pixel_format());
        patch.surface_mut().unwrap().fill_rect(DRect::new(10, 10, 90, 90), &[0.0, 0.0, 0.0, 0.7]);
        added.insert_above(Some(ink), patch);
        for (revision, edited) in [painted, hidden, added, (*base).clone()].into_iter().enumerate() {
            let edited = Arc::new(edited);
            let here = photocraft_compose::composites_here();
            assert_eq!(split.update(&edited, revision as u64 + 2, &mut |d| d.clone()), Update::Held, "edit {revision}");
            let (got, mapped) = split.render((0, 0), &jobs, false).expect("rendered");
            assert!(mapped);
            assert_eq!(photocraft_compose::composites_here() - here, 1, "the tiles held: only the layers between the below and the top were composited");
            let want = photocraft_compose::render(&edited, all);
            assert!(worst(&got[0], &want) <= 1.0 / 255.0, "edit {revision}: off by {}", worst(&got[0], &want));
        }
    }

    /// An edit of a layer below the run or in the top doesn't keep the split: the layers that
    /// differ come back, for a new split around them, and the split stays as it was.
    #[test]
    fn an_edit_below_the_run_or_in_the_top_is_elsewhere() {
        let (d, ink, over) = inked();
        let base = Arc::new(d);
        let plan = Plan { base: base.clone(), ids: vec![ink], doc: base.id, revision: 1, offset: (0, 0) };
        let mut split = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        let under = base.layers[1].id;
        for id in [under, over[2]] {
            let mut edited = (*base).clone();
            edited.layer_mut(id).unwrap().opacity = 0.3;
            assert_eq!(split.update(&Arc::new(edited), 2, &mut |d| d.clone()), Update::Elsewhere(vec![id]));
            assert!(split.is_for(&plan, 0), "left as it was");
        }
        // Two layers swapped: both differ.
        let mut swapped = (*base).clone();
        swapped.layers.swap(1, 2);
        assert_eq!(split.update(&Arc::new(swapped), 2, &mut |d| d.clone()), Update::Elsewhere(vec![under, ink]));
        // A larger canvas: nothing of the split holds, whatever the layers.
        let mut resized = (*base).clone();
        resized.size = Size::new(400, 200);
        assert_eq!(split.update(&Arc::new(resized), 2, &mut |d| d.clone()), Update::Elsewhere(Vec::new()));
    }

    /// Without the top's maps a tile takes the top layers composited for real: the exact
    /// composite, the below's tile kept for the edits that follow, no map made.
    #[test]
    fn without_its_map_a_tile_is_the_exact_composite() {
        let (d, ink, _) = inked();
        let base = Arc::new(d);
        let plan = Plan { base: base.clone(), ids: vec![ink], doc: base.id, revision: 1, offset: (0, 0) };
        let mut split = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        let all = DRect::new(0, 0, 300, 200);
        let (got, mapped) = split.render((0, 0), &[(DRect::new(20, 10, 280, 190), all)], false).expect("rendered");
        assert!(!mapped);
        assert_eq!(got[0], photocraft_compose::render(&base, DRect::new(20, 10, 280, 190)));
        assert!(split.needs(all), "its map is still to make");
        assert!((300 * 200 * 16..300 * 200 * 32).contains(&split.bytes()), "the below's tile is kept, no map: {}", split.bytes());
    }

    /// Tiles past the budget aren't made (the frame is composited whole), and a smaller budget
    /// lets go of the tiles out of view.
    #[test]
    fn tiles_stay_within_the_budget() {
        let (d, ink, _) = inked();
        let base = Arc::new(d);
        let plan = Plan { base: base.clone(), ids: vec![ink], doc: base.id, revision: 1, offset: (0, 0) };
        let mut split = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        let (left, right) = (DRect::new(0, 0, 150, 200), DRect::new(150, 0, 300, 200));
        assert!(split.fill(&[(left, left)], true));
        let tile = split.bytes();
        assert!(tile >= 150 * 200 * 48, "a float tile below, two for the top's map: {tile}");
        split.set_budget(tile, &[]);
        assert!(!split.fill(&[(right, right)], true), "no room for a second tile");
        assert!(split.render((0, 0), &[(right, right)], false).is_none());
        assert_eq!(split.bytes(), tile, "nothing made");
        // The view moved to the right tile: the left one goes when the budget shrinks under it.
        split.set_budget(tile - 1, &[right]);
        assert_eq!(split.bytes(), 0);
        split.set_budget(tile, &[right]);
        assert!(split.fill(&[(right, right)], true));
    }

    /// A split made again around the run and the layers over it (they were hidden: the box of
    /// Free Transform) takes the below's tiles of the one before, and composites nothing for them.
    #[test]
    fn a_split_with_the_same_below_inherits_its_tiles() {
        let (d, ink, over) = inked();
        let base = Arc::new(d);
        let plan = Plan { base: base.clone(), ids: vec![ink], doc: base.id, revision: 1, offset: (0, 0) };
        let mut split = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        let all = DRect::new(0, 0, 300, 200);
        assert!(split.fill(&[(all, all)], true));
        let mut hidden = (*base).clone();
        for id in [ink].iter().chain(&over) {
            hidden.layer_mut(*id).unwrap().visible = false;
        }
        let hidden = Arc::new(hidden);
        let Update::Elsewhere(changed) = split.update(&hidden, 2, &mut |d| d.clone()) else { panic!("layers of the top changed") };
        assert_eq!(changed.len(), 4);
        let plan = Plan { base: hidden.clone(), ids: changed, doc: base.id, revision: 2, offset: (0, 0) };
        let mut under = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        under.inherit(&split);
        let here = photocraft_compose::composites_here();
        let (got, mapped) = under.render((0, 0), &[(all, all)], false).expect("rendered");
        assert_eq!(photocraft_compose::composites_here() - here, 1, "the below came with the old split's tiles");
        assert!(!mapped);
        assert_eq!(got[0], photocraft_compose::render(&hidden, all));
        // Another document's split, or one with other layers below, gives nothing.
        let mut other = (*base).clone();
        other.layers[1].opacity = 0.5;
        let other = Arc::new(other);
        let plan = Plan { base: other.clone(), ids: vec![ink], doc: base.id, revision: 3, offset: (0, 0) };
        let mut apart = Split::new(&plan, 0, &mut |d| d.clone()).expect("splittable");
        apart.inherit(&split);
        assert_eq!(apart.bytes(), 0);
    }
}
