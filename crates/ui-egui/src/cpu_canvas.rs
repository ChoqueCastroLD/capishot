//! CPU canvas: the document drawn from 256² tiles composited at display resolution.
//!
//! Without the GPU canvas (no WebGPU/WebGL compositor, a lost device, `?cpu`, flipped views)
//! the document used to be composited whole, at full resolution, into one texture of up to
//! 4096² per change. Here only the tiles in view are composited, at the mip level the zoom
//! needs (level `l` is `1/2^l` resolution, from [`photocraft_compose::mip::MipCache`]'s
//! box-filtered layers, so a zoomed-out view costs about `1/4^l` of the work), and kept as
//! textures: panning draws what is cached, an edit redraws only the tiles its damage reaches
//! (only that part of them), and tiles not yet drawn at a new zoom are shown from another level
//! until they are. Level 0 is the exact composite (`photocraft_compose::render`), so 100% and
//! closer show the same pixels as before.
//!
//! An edit's tiles are composited on the worker pool, all of them in one batch, while the frame
//! goes on ([`Batch`]): the window's thread only hands the batch over and, when it is done, puts
//! its textures in place, all in the same frame. The canvas shows the last complete state until
//! then, never part of an edit, and a frame is never held by a composite. The batch re-blends
//! only the layers the edit changed, over composites of the layers below and above them kept per
//! tile at the view's level (`move_cpu::Split`), which are made ahead, off the frame, around the
//! layers the next edit most likely changes.
//!
//! Layer effects and vector masks are scaled with the level ([`photocraft_compose::proxy`]); a
//! document with a stroked shape layer, whose vector stroke is drawn at render time in document
//! pixels ([`photocraft_compose::proxy::lod_faithful`]), takes its zoomed-out tiles from the
//! full-resolution composite of the tile's area instead.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use egui::{Color32, Mesh, Pos2, Rect, TextureOptions, pos2};
use photocraft_compose::mip::MipCache;
use photocraft_doc::{DocId, Document, LayerId};
use photocraft_geom::Rect as DRect;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;
use crate::move_cpu::{Plan, Split, Update};

/// Tile side in level pixels.
pub const TILE: i32 = 256;
/// Extra pixels rendered around each tile, so bilinear filtering across tile edges blends with
/// the neighbour's real pixels (no seams).
const GUTTER: i32 = 1;
/// Deepest level used.
const MAX_LEVEL: u32 = 10;
/// Missing (never drawn) tiles composited per frame beyond the first batch: the rest show from
/// another level and are drawn over the next frames, so zooming and panning stay responsive.
const FRAME_BUDGET_MS: f64 = 12.0;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TileKey {
    pub level: u32,
    pub tx: i32,
    pub ty: i32,
}

struct TileTex {
    tex: egui::TextureHandle,
    /// The level pixels the texture holds (the tile and its gutter, clipped to the level).
    held: DRect,
    /// Document pixels that changed since the texture was drawn (`None`: current).
    dirty: Option<DRect>,
    /// The document pixels of it the batch in flight is drawing (`None`: none; they were `dirty`
    /// when it left, and are again if it is lost).
    drawing: Option<DRect>,
    /// Frame this tile was last drawn on screen.
    used: u64,
    /// Drawn through a split's top map (`move_cpu`): the document as it is, within a rounding
    /// step, type edges approximated; composited exactly once the edits pause.
    approx: bool,
}

/// One document's CPU canvas on one display (lives in `canvas::CanvasCache`).
#[derive(Default)]
pub struct TileCanvas {
    /// (revision, preview key with the display key folded in) the tiles show.
    shown: Option<(u64, u64)>,
    size: [u32; 2],
    tiles: HashMap<TileKey, TileTex>,
    /// The reduced layers and documents, shared with the worker pool (which builds a level the
    /// view zooms to while the frame goes on).
    levels: Arc<wasm_sync::Mutex<Levels>>,
    frame: u64,
    /// Level of the last frame (tests and the perf readout).
    pub(crate) level: u32,
    /// Tiles being composited on the worker pool, off the UI thread (threads build, desktop).
    pending: Option<Pending>,
    /// The cached composites below and above the layers an edit changes (`move_cpu`); with the
    /// batch in flight while one is.
    split: Option<Split>,
    /// The split from before the Free Transform box opened, kept while it is open (the two
    /// within the one budget) for the frame it closes in: the document is then the one it was
    /// made for, with the layer where the box left it.
    spare: Option<Split>,
    /// The batch the worker pool is compositing.
    flight: Option<Flight>,
    /// The layers the frame asked the split to be around (the Move tool's press targets, else the
    /// selected layers), and the ones it is around: those, or the layers the last edit changed.
    wanted: Vec<LayerId>,
    anchor: Vec<LayerId>,
    /// What the split's tiles for the view were last made ahead for.
    ahead: Option<Ahead>,
    /// They are made for what the canvas shows now (the next edit's first frame is drawn from
    /// them).
    pub(crate) ahead_ready: bool,
    /// The split is the one a Move press would drag (`move_ui`'s lazy preview).
    split_ready: bool,
    /// The Free Transform box stood in for a layer of the document in the last frame.
    stood_in: bool,
    /// The tiles on screen are of the document under the Free Transform box (the box draws the
    /// layer it stands in for over them: [`under_box`]).
    under_box: bool,
    /// The app's frame that last drew this canvas.
    painted: u64,
    /// When tiles drawn through a split's top map may be composited exactly (`now_ms`): once the
    /// edits have paused this long.
    refine_after: f64,
    /// Where the last frame drew the document.
    xf: Option<ViewXform>,
    /// The page as it was before its size changed (a crop, its undo, Image or Canvas Size), shown
    /// where the last frame drew it until the new page's tiles in view are all composited.
    before: Option<Before>,
}

/// The tiles of a page whose size changed, as the last frame drew them ([`TileCanvas::before`]).
struct Before {
    tiles: Vec<(TileKey, TileTex)>,
    size: [u32; 2],
    xf: ViewXform,
}

/// Missing tiles composited in the background: the canvas keeps drawing (other levels stand in)
/// while they are, so zooming and panning a heavy document never waits for the compositor.
struct Pending {
    /// The (revision, preview key) and level they were composited for.
    state: (u64, u64),
    level: u32,
    rx: std::sync::mpsc::Receiver<Vec<(TileKey, DRect, egui::ColorImage)>>,
}

/// Whether there is a worker pool to composite on, off the UI thread (the threads build, desktop).
#[cfg(not(test))]
pub(crate) const POOL: bool = cfg!(any(not(target_arch = "wasm32"), target_feature = "atomics"));

impl std::fmt::Debug for TileCanvas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TileCanvas").field("tiles", &self.tiles.len()).field("level", &self.level).finish()
    }
}

impl TileCanvas {
    /// The (revision, preview key ^ display key) the tiles show.
    pub(crate) fn shown(&self) -> Option<(u64, u64)> {
        self.shown
    }

    /// The canvas showed a preview its document now reproduces (`canvas::shown_as_document`):
    /// count the tiles as showing the document (`key`: its preview key with the display's).
    pub(crate) fn shown_as_document(&mut self, was_preview: impl Fn(u64) -> bool, key: u64) {
        if let Some((_, k)) = &mut self.shown
            && was_preview(*k)
        {
            *k = key;
        }
    }

    /// Bytes of tile textures held (RGBA8).
    pub fn texture_bytes(&self) -> usize {
        self.tiles.values().map(|t| t.held.width() as usize * t.held.height() as usize * 4).sum()
    }

    /// Bytes of reduced layer tiles held.
    pub fn mip_bytes(&self) -> usize {
        self.levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner).mips.bytes()
    }

    #[cfg(test)]
    pub(crate) fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Mark what changed: `damage` in document pixels (`None`: everything).
    fn invalidate(&mut self, damage: Option<DRect>) {
        for (k, t) in &mut self.tiles {
            let f = 1i64 << k.level;
            let reach = scale_rect(t.held, f);
            let d = match damage {
                Some(d) if d.is_empty() => continue,
                Some(d) => {
                    let d = d.intersect(&reach);
                    if d.is_empty() {
                        continue;
                    }
                    d
                }
                None => reach,
            };
            t.dirty = Some(t.dirty.map_or(d, |o| o.union(&d)));
        }
    }
}

/// Reduced layers ([`MipCache`]) and the reduced documents of the document last drawn, by level.
#[derive(Default)]
struct Levels {
    mips: MipCache,
    docs: Vec<(u32, Weak<Document>, Arc<Document>)>,
}

/// Document `doc` reduced to `level`, from the shared [`Levels`] (the UI thread or a worker).
fn reduced(levels: &wasm_sync::Mutex<Levels>, doc: &Arc<Document>, level: u32) -> Arc<Document> {
    if level == 0 {
        return doc.clone();
    }
    let mut l = {
        let _s = crate::perf_span::span("cpu.levels_wait");
        levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some((_, _, r)) = l.docs.iter().find(|(lv, w, _)| *lv == level && w.upgrade().is_some_and(|d| Arc::ptr_eq(&d, doc))) {
        return r.clone();
    }
    let _s = crate::perf_span::span("cpu.mips");
    l.mips.begin();
    let r = Arc::new(l.mips.document(doc, level));
    l.docs.retain(|(lv, w, _)| *lv != level && w.upgrade().is_some_and(|d| Arc::ptr_eq(&d, doc)));
    l.docs.push((level, Arc::downgrade(doc), r.clone()));
    r
}

/// Document rect `r` (level pixels) in document pixels at factor `f`.
fn scale_rect(r: DRect, f: i64) -> DRect {
    let c = |v: i32| (i64::from(v) * f).clamp(-(1 << 30), 1 << 30) as i32;
    DRect::new(c(r.x0), c(r.y0), c(r.x1), c(r.y1))
}

/// Document pixels `r` as level pixels at factor `f`, rounded out.
fn level_rect(r: DRect, f: i64) -> DRect {
    let lo = |v: i32| i64::from(v).div_euclid(f) as i32;
    let hi = |v: i32| (i64::from(v) + f - 1).div_euclid(f) as i32;
    DRect::new(lo(r.x0), lo(r.y0), hi(r.x1), hi(r.y1))
}

/// The mip level for `zoom` (device pixels per document pixel): the coarsest whose pixels are
/// still no larger than a device pixel.
pub fn level_for(zoom: f32, size: [u32; 2]) -> u32 {
    if !(zoom.is_finite() && zoom > 0.0) || zoom >= 0.5 {
        return 0;
    }
    let l = (1.0 / zoom).log2().floor().max(0.0) as u32;
    // Not past the level where the document is a single pixel.
    let longest = size[0].max(size[1]).max(1);
    l.min(MAX_LEVEL).min(31 - longest.leading_zeros())
}

/// The level-pixel bounds of the document at factor `f`.
fn level_bounds(size: [u32; 2], f: i64) -> DRect {
    let c = |v: u32| ((i64::from(v) + f - 1) / f) as i32;
    DRect::new(0, 0, c(size[0]), c(size[1]))
}

/// Tile `k`'s own level pixels (without gutter), clipped to the level.
fn tile_rect(k: TileKey, size: [u32; 2]) -> DRect {
    let b = level_bounds(size, 1i64 << k.level);
    DRect::new(k.tx * TILE, k.ty * TILE, (k.tx + 1) * TILE, (k.ty + 1) * TILE).intersect(&b)
}

/// The tiles of `level` covering `visible` (document pixels).
pub(crate) fn tiles_in(visible: DRect, level: u32, size: [u32; 2]) -> Vec<TileKey> {
    let f = 1i64 << level;
    let r = level_rect(visible, f).intersect(&level_bounds(size, f));
    if r.is_empty() {
        return Vec::new();
    }
    let (x0, y0) = (r.x0.div_euclid(TILE), r.y0.div_euclid(TILE));
    let (x1, y1) = ((r.x1 - 1).div_euclid(TILE), (r.y1 - 1).div_euclid(TILE));
    let mut v = Vec::with_capacity(((x1 - x0 + 1) * (y1 - y0 + 1)).max(0) as usize);
    // Centre first: the tiles drawn first when a frame can't draw them all.
    for ty in y0..=y1 {
        for tx in x0..=x1 {
            v.push(TileKey { level, tx, ty });
        }
    }
    let (cx, cy) = ((x0 + x1) as f32 / 2.0, (y0 + y1) as f32 / 2.0);
    v.sort_by(|a, b| {
        let d = |k: &TileKey| (k.tx as f32 - cx).powi(2) + (k.ty as f32 - cy).powi(2);
        d(a).total_cmp(&d(b))
    });
    v
}

/// Budget for tile textures: a share of the Memory Usage allowance and of the device's memory,
/// 24–192 MB (at 256² RGBA8 tiles, a 4K screen at 100% is about 35 MB, a phone's about 13 MB).
fn texture_budget(app: &PhotocraftApp) -> usize {
    let allowance = app.session.prefs().performance.history_budget_bytes();
    let device = crate::gpu_canvas::physical_memory().map_or(usize::MAX, |b| usize::try_from(b / 64).unwrap_or(usize::MAX));
    (allowance / 16).min(device).clamp(24 << 20, 192 << 20)
}

/// Budget for reduced layer tiles.
fn mip_budget(app: &PhotocraftApp) -> usize {
    let allowance = app.session.prefs().performance.history_budget_bytes();
    let device = crate::gpu_canvas::physical_memory().map_or(usize::MAX, |b| usize::try_from(b / 32).unwrap_or(usize::MAX));
    (allowance / 8).min(device).clamp(32 << 20, 512 << 20)
}

/// Monitor RGBA8, premultiplied as egui stores textures.
fn tile_image(display: Option<&photocraft_engine::display_color::CanvasDisplay>, buf: &photocraft_compose::Buffer) -> egui::ColorImage {
    let size = [buf.rect.width() as usize, buf.rect.height() as usize];
    let px = match display {
        Some(d) if !d.is_identity() => {
            let img = d.to_rgba8(buf);
            photocraft_compose::premultiply_rgba8(img.pixels.as_chunks::<4>().0)
        }
        _ => buf.to_premultiplied_rgba8(),
    };
    egui::ColorImage::new(size, px.into_iter().map(|p| Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3])).collect())
}

/// Budget for the split's tiles (`move_cpu::Split`): float composites below and above the edited
/// layers for the tiles in view, 48 bytes a pixel at most. A share of the Memory Usage allowance
/// and of the device's memory, 32-512 MB; a view that needs more is composited whole.
fn split_budget(app: &PhotocraftApp) -> usize {
    let allowance = app.session.prefs().performance.history_budget_bytes();
    let device = crate::gpu_canvas::physical_memory().map_or(usize::MAX, |b| usize::try_from(b / 32).unwrap_or(usize::MAX));
    (allowance / 8).min(device).clamp(32 << 20, 512 << 20)
}

/// The budget of a frame for making the split ahead where there is no worker pool (milliseconds).
const SPLIT_AHEAD_MS: f64 = 6.0;

/// How long a batch on the worker pool goes on making the split ahead (milliseconds): an edit
/// that comes meanwhile waits for it to come back.
const AHEAD_FLIGHT_MS: f64 = 12.0;

/// How long a frame waits for an edit's batch before it goes on without it (milliseconds).
#[cfg(not(test))]
const FLIGHT_WAIT_MS: f64 = 4.0;

/// The budget of a drag frame for the split's sprite while it isn't ready (milliseconds).
const DRAG_BAND_MS: f64 = 2.0;

/// How long the edits must pause before tiles drawn through a split's top map are composited
/// exactly (milliseconds): an edit that follows at once keeps the pool's time.
const REFINE_AFTER_MS: f64 = 400.0;

/// Texture bytes a frame uploads for the tiles of a batch that landed (about 15 whole tiles). A
/// batch with more is uploaded into new textures over the next frames while the canvas goes on
/// showing the tiles it has, and they all replace those in the frame the last one is uploaded:
/// a page-size edit uploaded its 10 MB of tiles in the frame it landed in (11 to 39 ms of a frame
/// on this server, more on a phone's WebGL).
pub(crate) const UPLOAD_BUDGET: usize = 4 << 20;

/// Composite `rects` (level pixels at factor `f`) from `source` (the reduced document), or
/// without one by reducing the full-resolution composite of `doc`.
fn render_level(doc: &Document, source: Option<&Document>, f: i64, rects: &[DRect]) -> Vec<photocraft_compose::Buffer> {
    match source {
        Some(src) => photocraft_compose::render_rects(src, rects),
        None => rects
            .iter()
            .map(|r| {
                let d = scale_rect(*r, f).intersect(&doc.bounds());
                let mut b = photocraft_compose::render_reduced_rect(doc, d, r.width(), r.height());
                b.rect = *r;
                b
            })
            .collect(),
    }
}

/// What to draw for one tile.
#[derive(Clone, Copy)]
enum Job {
    /// The whole tile (with gutter).
    Full(TileKey, DRect),
    /// Part of an existing tile's texture: level pixels.
    Part(TileKey, DRect),
}

impl Job {
    fn key(&self) -> TileKey {
        match self {
            Job::Full(k, _) | Job::Part(k, _) => *k,
        }
    }

    fn rect(&self) -> DRect {
        match self {
            Job::Full(_, r) | Job::Part(_, r) => *r,
        }
    }
}

/// How a batch of tiles is drawn.
#[derive(Clone)]
enum Through {
    /// A Move drag's frame (or, `release`, its release from the split rebased on it): the plan's
    /// layers at its offset over the split's tiles (`move_cpu`). `lazy`: the document shown is
    /// the one before the drag, so a frame the split can't draw composites the moved copy.
    Move { plan: Plan, lazy: bool, release: bool },
    /// The document as it is, through the split around `ids` where the edit left its tiles
    /// standing ([`Split::update`]); else through one made again around what the edit changed.
    Edit { ids: Vec<LayerId>, id: DocId, revision: u64 },
    /// The exact composite of every layer.
    Whole,
}

/// Tiles to composite with everything that takes: drawn on the worker pool while the frame goes
/// on, or in the frame where there is no pool ([`draw`]).
struct Batch {
    doc: Arc<Document>,
    level: u32,
    /// Each job with the level pixels its tile holds.
    jobs: Vec<(Job, DRect)>,
    /// Whether the maps of the split's top are made for the jobs' tiles that lack one (a drag's
    /// frames: the ones that follow are drawn from them); else those tiles take the top layers
    /// composited for real, and their maps are made ahead.
    maps: bool,
    display: Option<Arc<photocraft_engine::display_color::CanvasDisplay>>,
    levels: Arc<wasm_sync::Mutex<Levels>>,
    split: Option<Split>,
    /// The split before `split` ([`TileCanvas::spare`]).
    spare: Option<Split>,
    /// The Free Transform box opens or closes in this frame ([`HANDOVER_WAIT_MS`]).
    handover: bool,
    through: Through,
    /// The split's tiles to make ahead (the tiles in view), for about `ahead_ms`; `sprite`: the
    /// image a Move drag would blit too.
    ahead: Vec<DRect>,
    ahead_ms: f64,
    sprite: bool,
    /// Set when an edit is waiting for the split: what is made ahead stops at the next tiles.
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// The most the split's tiles may hold, and the tiles in view (kept whatever they cost).
    budget: usize,
    view: Vec<DRect>,
}

/// What a [`Batch`] made.
struct Drawn {
    split: Option<Split>,
    spare: Option<Split>,
    tiles: Vec<(Job, egui::ColorImage)>,
    from_split: bool,
    /// Drawn through the split's top map: composited exactly once the edits pause.
    approx: bool,
    /// The layers the split is around now, when the batch made or used one.
    anchor: Option<Vec<LayerId>>,
    /// Nothing is left to make ahead (or no more can be made).
    ahead_done: bool,
    /// What the batch took, and how much of that held the window's thread: all of it when it was
    /// drawn in the frame, the frame's wait when the pool drew it, none when it landed later.
    ms: f64,
    held_ms: f64,
}

/// Composite a batch. Runs on a pool worker or, without a pool, in the frame.
fn draw(mut b: Batch) -> Drawn {
    let t0 = crate::gpu_canvas::now_ms();
    let level = b.level;
    let f = 1i64 << level;
    let faithful = level == 0 || photocraft_compose::proxy::lod_faithful(&b.doc);
    let levels = b.levels.clone();
    let mut reduce = |d: &Document| {
        if level == 0 {
            return d.clone();
        }
        let mut l = levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        l.mips.document(d, level)
    };
    let held: Vec<(DRect, DRect)> = b.jobs.iter().map(|(j, h)| (j.rect(), *h)).collect();
    let mut split = b.split.take();
    let mut spare = b.spare.take();
    let mut anchor = None;
    let mut images: Option<Vec<egui::ColorImage>> = None;
    let mut bufs = None;
    let mut approx = false;
    // Whether the split shows the batch's document as it is (its tiles can be made ahead).
    let mut current = false;
    if faithful {
        match &b.through {
            Through::Move { plan, release, .. } => {
                let kept = split
                    .as_mut()
                    .is_some_and(|s| s.is_for(plan, level) || (s.moves(plan.doc, &plan.ids, level) && s.update(&plan.base, plan.revision, &mut reduce) == Update::Held));
                if !kept {
                    split = Split::new(plan, level, &mut reduce);
                }
                if let Some(s) = split.as_mut() {
                    s.set_budget(b.budget, &b.view);
                    anchor = Some(plan.ids.clone());
                    if !held.is_empty() {
                        // The one-blit path where the canvas shows the document's values as they are.
                        if b.display.as_deref().is_none_or(|d| d.is_identity()) && (b.maps || held.iter().all(|h| !s.needs(h.1))) {
                            images = s.render_rgba8(plan.offset, &held).map(|px| {
                                let mut out: Vec<egui::ColorImage> = held.iter().map(|_| egui::ColorImage::default()).collect();
                                crate::move_cpu::each(&mut out, |i, o| {
                                    if let (Some((r, _)), Some(p)) = (held.get(i), px.get(i)) {
                                        *o = egui::ColorImage::from_rgba_premultiplied([r.width() as usize, r.height() as usize], p.as_flattened());
                                    }
                                });
                                out
                            });
                        }
                        if images.is_none() {
                            // Its sprite still being made: a band of it, the frame on the float path.
                            s.prepare(DRAG_BAND_MS);
                            bufs = s.render(plan.offset, &held, b.maps).map(|r| r.0);
                        }
                        approx = *release;
                    }
                }
            }
            Through::Edit { ids, id, revision } => {
                let plan = |ids: &[LayerId]| Plan { base: b.doc.clone(), ids: ids.to_vec(), doc: *id, revision: *revision, offset: (0, 0) };
                let kept = split.as_mut().filter(|s| s.moves(*id, ids, level)).map(|s| s.update(&b.doc, *revision, &mut reduce));
                // The Free Transform box closed: the split from before it opened, when it holds
                // for the document (the layer is back, moved or not).
                let back = b.handover && spare.as_mut().filter(|s| s.of(*id, level)).is_some_and(|s| s.update(&b.doc, *revision, &mut reduce) == Update::Held);
                let mut kept_spare = false;
                match kept {
                    _ if back => {
                        split = spare.take();
                        anchor = split.as_ref().map(|s| s.ids().to_vec());
                    }
                    Some(Update::Held) => {}
                    // An edit somewhere else: the split made again around what it changed, with
                    // the tiles of the old one's below when its below is the same. When the box
                    // opened (it hides the layer and what covers it), the old one is kept for
                    // the frame it closes in.
                    Some(Update::Elsewhere(changed)) if !changed.is_empty() => {
                        let mut new = Split::new(&plan(&changed), level, &mut reduce);
                        if let (Some(new), Some(old)) = (new.as_mut(), split.as_ref()) {
                            new.set_budget(if b.handover { b.budget / 2 } else { b.budget }, &b.view);
                            new.inherit(old);
                        }
                        let old = std::mem::replace(&mut split, new);
                        if b.handover {
                            (spare, kept_spare) = (old, true);
                        }
                        anchor = Some(changed);
                    }
                    _ => {
                        split = Split::new(&plan(ids), level, &mut reduce);
                        anchor = Some(ids.clone());
                    }
                }
                // (A spare the closing box didn't take back is of no use.)
                if b.handover && !kept_spare {
                    spare = None;
                }
                // Two splits share the budget.
                let budget = if spare.is_some() { b.budget / 2 } else { b.budget };
                if let Some(s) = spare.as_mut() {
                    s.set_budget(budget, &b.view);
                }
                if let Some(s) = split.as_mut() {
                    s.set_budget(budget, &b.view);
                    current = true;
                    if !held.is_empty()
                        && let Some((drawn, mapped)) = s.render((0, 0), &held, b.maps)
                    {
                        (bufs, approx) = (Some(drawn), mapped);
                    }
                }
            }
            Through::Whole => {}
        }
    }
    let from_split = images.is_some() || bufs.is_some();
    if images.is_none() && !held.is_empty() {
        let bufs = bufs.unwrap_or_else(|| {
            approx = false;
            let moved = match &b.through {
                Through::Move { plan, lazy: true, .. } => photocraft_engine::layer_multi_cmds::moved(&plan.base, &plan.ids, plan.offset.0, plan.offset.1).ok().map(Arc::new),
                _ => None,
            };
            let doc = moved.as_ref().unwrap_or(&b.doc);
            let source = faithful.then(|| reduced(&b.levels, doc, level));
            let rects: Vec<DRect> = held.iter().map(|h| h.0).collect();
            render_level(doc, source.as_deref(), f, &rects)
        });
        let mut out: Vec<egui::ColorImage> = bufs.iter().map(|_| egui::ColorImage::default()).collect();
        let display = b.display.as_deref();
        crate::move_cpu::each(&mut out, |i, o| {
            if let Some(buf) = bufs.get(i) {
                *o = tile_image(display, buf);
            }
        });
        images = Some(out);
    }
    // The split's tiles for the rest of the view, and a Move drag's sprite: made now, while
    // nothing needs them, so the next edit's first frame is drawn from them.
    let mut ahead_done = true;
    if let Some(s) = split.as_mut().filter(|_| current && !b.ahead.is_empty()) {
        let t1 = crate::gpu_canvas::now_ms();
        if b.sprite {
            ahead_done = s.prepare(b.ahead_ms);
        }
        let todo: Vec<(DRect, DRect)> = b.ahead.iter().filter(|h| s.needs(**h)).map(|h| (*h, *h)).collect();
        let over = |t1: f64| crate::gpu_canvas::now_ms() - t1 >= b.ahead_ms || b.stop.load(std::sync::atomic::Ordering::Relaxed);
        // A few tiles at a time, the below's and then the top's maps: an edit that comes
        // meanwhile waits for one of those at most.
        'tiles: for some in todo.chunks(rayon_batch()) {
            for maps in [false, true] {
                if over(t1) {
                    ahead_done = false;
                    break 'tiles;
                }
                if !s.fill(some, maps) {
                    // Past the split's budget: no more can be made.
                    break 'tiles;
                }
            }
        }
    }
    let tiles = b.jobs.iter().map(|(j, _)| *j).zip(images.unwrap_or_default()).collect();
    let ms = crate::gpu_canvas::now_ms() - t0;
    Drawn { split, spare, tiles, from_split, approx, anchor, ahead_done, ms, held_ms: ms }
}

/// A batch on the worker pool: the frame that sent it went on without it.
struct Flight {
    /// Where the batch comes from (`None`: it is here, being uploaded: [`Flight::staged`]).
    rx: Option<std::sync::mpsc::Receiver<Drawn>>,
    /// The batch, once here, while its tiles are uploaded over frames.
    staged: Option<Staged>,
    /// The (revision, preview key) and level it draws.
    state: (u64, u64),
    level: u32,
    /// What its tiles made ahead are for ([`TileCanvas::ahead`]).
    ahead: Option<Ahead>,
    /// The layers the split was around when it left.
    anchor: Vec<LayerId>,
    /// Tells a batch that only makes the split ahead to stop: an edit is waiting for the split.
    stop: Arc<std::sync::atomic::AtomicBool>,
    /// Its tiles are of the document under the Free Transform box, or of the document itself
    /// (`None`: it draws no tile).
    under_box: Option<bool>,
}

/// A batch that is here, its tiles' textures made a frame's [`UPLOAD_BUDGET`] at a time.
struct Staged {
    d: Drawn,
    /// The textures made so far for its tiles, in order (`None` for a part of a tile: it is
    /// uploaded into the tile's texture when the batch lands).
    made: Vec<Option<egui::TextureHandle>>,
}

impl Staged {
    fn new(d: Drawn) -> Self {
        Staged { made: Vec::with_capacity(d.tiles.len()), d }
    }

    /// Upload the next tiles within what is left of the frame's budget (`spent`, bytes, which it
    /// adds to; a frame that has spent nothing uploads at least one). Whether all are uploaded.
    fn upload(&mut self, ctx: &egui::Context, name: (DocId, u32), spent: &mut usize) -> bool {
        while let Some((job, img)) = self.d.tiles.get_mut(self.made.len()) {
            let Job::Full(k, _) = *job else {
                self.made.push(None);
                continue;
            };
            let bytes = img.size[0] * img.size[1] * 4;
            if *spent > 0 && *spent + bytes > UPLOAD_BUDGET {
                return false;
            }
            *spent += bytes;
            // (The image is the texture's now: what lands is its texture.)
            self.made.push(Some(load_tile(ctx, name, k, std::mem::take(img))));
        }
        true
    }
}

/// Texture bytes of the whole tiles of `d`.
fn full_bytes(d: &Drawn) -> usize {
    d.tiles.iter().filter(|(j, _)| matches!(j, Job::Full(..))).map(|(_, img)| img.size[0] * img.size[1] * 4).sum()
}

fn load_tile(ctx: &egui::Context, name: (DocId, u32), k: TileKey, img: egui::ColorImage) -> egui::TextureHandle {
    ctx.load_texture(format!("cpu-tile-{}-{}-{}-{}-{}", name.0.0, name.1, k.level, k.tx, k.ty), img, TextureOptions::LINEAR)
}

impl Flight {
    /// What the batch made, its textures made, when it is done (`Err`: it was lost). A batch
    /// with more texture bytes than the frame has left of its [`UPLOAD_BUDGET`] (`spent`) is
    /// uploaded over the next frames first.
    fn find(&mut self, ctx: &egui::Context, name: (DocId, u32), spent: &mut usize) -> Option<Result<(Drawn, Vec<Option<egui::TextureHandle>>), ()>> {
        if self.staged.is_none() {
            let d = match self.rx.as_ref()?.try_recv() {
                Ok(d) => Drawn { held_ms: 0.0, ..d },
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Some(Err(())),
            };
            if *spent + full_bytes(&d) <= UPLOAD_BUDGET {
                return Some(Ok((d, Vec::new())));
            }
            self.rx = None;
            self.staged = Some(Staged::new(d));
        } else if let Some(st) = self.staged.take_if(|st| st.made.is_empty() && *spent + full_bytes(&st.d) <= UPLOAD_BUDGET) {
            // (Handed over here whole: it fits the frame.)
            return Some(Ok((st.d, Vec::new())));
        }
        let st = self.staged.as_mut()?;
        if !st.upload(ctx, name, spent) {
            // (The rest in the next frame, at once.)
            ctx.request_repaint();
            return None;
        }
        let st = self.staged.take()?;
        Some(Ok((st.d, st.made)))
    }
}

/// What a split's tiles were made ahead for: the state the canvas showed (revision, preview
/// key), the layers the split is around, the level and the tiles in view (a hash of them).
type Ahead = ((u64, u64), Vec<LayerId>, u32, u64);

/// What a batch was sent for, when it lands.
struct Sent {
    /// It drew the state and level the canvas shows.
    fresh: bool,
    /// What its tiles made ahead are for.
    ahead: Option<Ahead>,
    /// The layers the split was around when it left.
    anchor: Vec<LayerId>,
}

/// What a frame drew, for the performance readout.
#[derive(Default)]
struct Stats {
    px: u64,
    /// Composites that held the frame (drawn in it, or waited for), and the worker pool's time
    /// for the ones it drew while the frames went on.
    composite_ms: f64,
    pool_ms: f64,
    upload_ms: f64,
    from_split: bool,
}

impl TileCanvas {
    /// Composite `batch`: on the worker pool, waiting up to `wait` milliseconds for it so that an
    /// edit the pool composites in that time shows in its own frame (`None`: in the frame). What
    /// it made when it is done already; else the batch is in flight ([`TileCanvas::flight`]) and
    /// the frame goes on with the tiles it has.
    fn send(&mut self, ctx: &egui::Context, batch: Batch, state: (u64, u64), ahead: Option<Ahead>, wait: Option<f64>) -> Option<Drawn> {
        let Some(wait) = wait else { return Some(draw(batch)) };
        let (level, stop) = (batch.level, batch.stop.clone());
        let under_box = (!batch.jobs.is_empty()).then_some(self.stood_in);
        let (tx, rx) = std::sync::mpsc::channel();
        let wake = ctx.clone();
        spawn(move || {
            #[cfg(test)]
            let pace = Pace::start();
            let drawn = draw(batch);
            #[cfg(test)]
            pace.hold();
            let _ = tx.send(drawn);
            // (The browser's page isn't woken from a worker: it looks again in a moment.)
            if cfg!(not(target_arch = "wasm32")) {
                wake.request_repaint();
            }
        });
        // The page's own thread can't wait in the browser: it finds the tiles in its next frame.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let _s = crate::perf_span::span("cpu.wait");
            let t0 = crate::gpu_canvas::now_ms();
            if let Ok(d) = rx.recv_timeout(std::time::Duration::from_secs_f64(wait.max(0.0) / 1000.0)) {
                return Some(Drawn { held_ms: crate::gpu_canvas::now_ms() - t0, ..d });
            }
        }
        #[cfg(target_arch = "wasm32")]
        let _ = wait;
        self.flight = Some(Flight { rx: Some(rx), staged: None, state, level, ahead, anchor: self.anchor.clone(), stop, under_box });
        None
    }

    /// What a batch made, into the canvas: all its tiles in this frame (the canvas never shows
    /// part of an edit), the split back. `made`: the textures of its whole tiles, uploaded over
    /// the frames before ([`Staged`]; empty: they are made now).
    fn land(&mut self, ctx: &egui::Context, name: (DocId, u32), d: Drawn, made: Vec<Option<egui::TextureHandle>>, sent: Sent, stats: &mut Stats) {
        let t0 = crate::gpu_canvas::now_ms();
        let Sent { fresh, ahead, anchor } = sent;
        // (Not when the frames asked for other layers meanwhile: the next batch goes around those.)
        if let Some(a) = d.anchor.filter(|_| self.anchor == anchor) {
            self.anchor = a;
        }
        (self.split, self.spare) = (d.split, d.spare);
        if d.ahead_done && ahead.is_some() {
            self.ahead = ahead;
        }
        if d.approx && !d.tiles.is_empty() {
            self.refine_after = t0 + REFINE_AFTER_MS;
        }
        let frame = self.frame;
        stats.from_split |= d.from_split && !d.tiles.is_empty();
        stats.composite_ms += d.held_ms;
        stats.pool_ms += (d.ms - d.held_ms).max(0.0);
        let mut made = made.into_iter();
        for (job, img) in d.tiles {
            let r = job.rect();
            stats.px += u64::from(r.width()) * u64::from(r.height());
            // The tile's texture uploaded in earlier frames (`Staged`), or made now.
            let staged = made.next().flatten();
            match job {
                Job::Full(k, held) => match self.tiles.get_mut(&k) {
                    Some(t) if staged.is_none() && t.tex.size() == img.size => {
                        t.tex.set(img, TextureOptions::LINEAR);
                        (t.held, t.drawing, t.approx) = (held, None, d.approx);
                    }
                    // What changed while the batch was in flight is still to draw.
                    Some(t) => {
                        let dirty = t.dirty;
                        let tex = staged.unwrap_or_else(|| load_tile(ctx, name, k, img));
                        self.tiles.insert(k, TileTex { tex, held, dirty, drawing: None, used: frame, approx: d.approx });
                    }
                    None if fresh => {
                        let tex = staged.unwrap_or_else(|| load_tile(ctx, name, k, img));
                        self.tiles.insert(k, TileTex { tex, held, dirty: None, drawing: None, used: frame, approx: d.approx });
                    }
                    None => {}
                },
                Job::Part(k, part) => {
                    if let Some(t) = self.tiles.get_mut(&k) {
                        let fits = part.intersect(&t.held) == part && [part.width() as usize, part.height() as usize] == img.size;
                        if fits && !part.is_empty() {
                            t.tex.set_partial([(part.x0 - t.held.x0) as usize, (part.y0 - t.held.y0) as usize], img, TextureOptions::LINEAR);
                        }
                        let drawing = t.drawing.take();
                        if !fits {
                            // The tile isn't the one the part was drawn for: its area again.
                            t.dirty = merge(t.dirty, drawing);
                        }
                        t.approx |= d.approx;
                    }
                }
            }
        }
        stats.upload_ms += crate::gpu_canvas::now_ms() - t0;
    }

    /// The batch in flight, into the canvas when it is here and its textures are made within the
    /// frame's [`UPLOAD_BUDGET`] (`spent`); else it stays in flight. `shows` is the state and
    /// level the canvas shows now.
    fn take_flight(&mut self, ctx: &egui::Context, key: (DocId, u32), shows: ((u64, u64), u32), spent: &mut usize, stats: &mut Stats) {
        let Some(mut fl) = self.flight.take() else { return };
        match fl.find(ctx, key, spent) {
            Some(Ok((d, made))) => {
                if made.is_empty() {
                    *spent += full_bytes(&d);
                }
                self.under_box = fl.under_box.unwrap_or(self.under_box);
                self.land(ctx, key, d, made, Sent { fresh: (fl.state, fl.level) == shows, ahead: fl.ahead, anchor: fl.anchor }, stats);
            }
            Some(Err(())) => self.grounded(),
            None => self.flight = Some(fl),
        }
    }

    /// The batch in flight was lost (its worker died): what it was drawing is still to draw.
    fn grounded(&mut self) {
        for t in self.tiles.values_mut() {
            let drawing = t.drawing.take();
            t.dirty = merge(t.dirty, drawing);
        }
    }
}

/// The union of two damage rectangles (document pixels).
fn merge(a: Option<DRect>, b: Option<DRect>) -> Option<DRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a.is_empty() {
            b
        } else if b.is_empty() {
            a
        } else {
            a.union(&b)
        }),
        (a, b) => a.or(b),
    }
}

/// How long a frame waits for the worker pool to composite an edit's tiles before it goes on
/// with the tiles it has (milliseconds; `None`: they are composited in the frame, where there is
/// no pool). An edit the pool composites in this time shows in its own frame; a longer one a
/// frame or more later, complete, the window never held meanwhile.
fn flight_wait() -> Option<f64> {
    #[cfg(test)]
    {
        FLIGHT_WAIT.with(|w| w.get())
    }
    #[cfg(not(test))]
    {
        POOL.then_some(FLIGHT_WAIT_MS)
    }
}

#[cfg(test)]
thread_local! {
    /// Tests composite in the frame, so they see an edit at once; one that measures frames sets
    /// the wait the app has ([`set_flight_wait`]).
    static FLIGHT_WAIT: std::cell::Cell<Option<f64>> = const { std::cell::Cell::new(None) };
}

/// Composite edits on the worker pool, the frame waiting `ms` for them, as the app does (tests).
#[cfg(test)]
pub(crate) fn set_flight_wait(ms: Option<f64>) {
    FLIGHT_WAIT.with(|w| w.set(ms));
}

/// Draw document `idx`'s CPU canvas: tiles in view at the zoom's level, composited as needed.
/// `zoom` is device pixels per document pixel (`View::zoom`).
#[expect(clippy::cognitive_complexity, reason = "the tile pipeline in frame order")]
#[allow(clippy::too_many_arguments)]
pub fn paint(
    app: &mut PhotocraftApp,
    ctx: &egui::Context,
    painter: &egui::Painter,
    xf: &ViewXform,
    idx: usize,
    output: Option<u32>,
    zoom: f32,
    visible: DRect,
) {
    let target = {
        let _s = crate::perf_span::span("cpu.target");
        crate::canvas::cpu_target(app, idx, output)
    };
    let Some(target) = target else { return };
    let crate::canvas::CpuTarget { doc, revision, preview_key, display, display_key, damage, id } = target;
    let size = [doc.size.width, doc.size.height];
    let level = level_for(zoom, size);
    let key = crate::canvas::cache_key(id, output);
    let move_plan = {
        let _s = crate::perf_span::span("cpu.plan");
        crate::move_ui::cpu_plan(app, idx, &doc)
    };
    // A lazy Move preview (`move_ui`: the document as it is, the canvas shifting the moving layers
    // itself): a frame the split can't draw composites the moved copy, as `move_ui` would have.
    let lazy = move_plan.as_ref().is_some_and(|p| Arc::ptr_eq(&p.base, &doc) && p.offset != (0, 0));
    // The layers the split is made around while nothing is dragged: with the Move tool the ones
    // a press would move, so a drag's first frame is drawn from it as the others are; else the
    // selected ones, which the next stroke, keystroke or command most likely changes.
    let move_tool = app.ui.tool == crate::state::Tool::Move && app.drag.is_none();
    let wanted = if move_plan.is_some() {
        None
    } else if move_tool {
        Some(crate::move_ui::press_targets(app, idx))
    } else {
        Some(app.session.documents().get(idx).map(|st| st.selected_layers()).unwrap_or_default())
    };
    let released = app.cpu_released.take_if(|r| r.0 == id);
    // The Free Transform box draws this document's moving pixels itself (`transform_tool`).
    let stands_in = app.transform_preview.as_ref().is_some_and(|pv| pv.doc.id == id && pv.made());
    let painted = app.frame;
    let tex_budget = texture_budget(app);
    let mip_budget = mip_budget(app);
    let split_budget = split_budget(app);
    let wait = flight_wait();
    let pooled = wait.is_some();
    let pointer_down = ctx.input(|i| i.pointer.any_down());
    let cache = app.canvases.entry(key).or_default();
    let tc = cache.tiles.get_or_insert_with(Default::default);
    tc.frame += 1;
    tc.level = level;
    let frame = tc.frame;
    if tc.size != size {
        // A page that was shown stays on screen as the last frame drew it until all of the new
        // one in view is composited: the tiles of a new size come over several frames (on the
        // pool), and a frame never shows part of them (it showed the canvas blank, then filling).
        let drawn = std::mem::take(&mut tc.tiles).into_iter().filter(|(_, t)| t.used + 1 == frame);
        let mut drawn: Vec<(TileKey, TileTex)> = drawn.collect();
        // (Coarser levels first: finer ones over them, as the frame drew them.)
        drawn.sort_by_key(|(k, _)| std::cmp::Reverse(k.level));
        tc.before = match (tc.shown, tc.xf) {
            (Some(_), Some(xf)) if !drawn.is_empty() => Some(Before { tiles: drawn, size: tc.size, xf }),
            // (The page before that, still on screen: it stays.)
            _ => tc.before.take(),
        };
        tc.size = size;
        tc.shown = None;
    }
    if let Some(w) = wanted.as_ref().filter(|w| tc.wanted != **w) {
        tc.wanted.clone_from(w);
        tc.anchor.clone_from(w);
    }
    let now = (revision, preview_key ^ display_key);
    // The Free Transform box starts or stops standing in for the layer in this frame. The tiles
    // come when the pool has them, like any edit's: the box draws the layer over the canvas only
    // while its tiles are of the document under the box (`TileCanvas::under_box`).
    let handover = std::mem::replace(&mut tc.stood_in, stands_in) != stands_in;
    tc.painted = painted;
    if tc.shown != Some(now) {
        tc.invalidate(if tc.shown.is_some() { damage } else { None });
        tc.shown = Some(now);
    }
    let mut stats = Stats::default();
    // Texture bytes this frame uploaded for batches' tiles ([`UPLOAD_BUDGET`]).
    let mut spent = 0;
    // The batch in flight, when the pool is done with it and its textures are uploaded: its
    // tiles in place of the ones shown.
    if let Some(fl) = tc.flight.as_ref()
        && handover
    {
        fl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    tc.take_flight(ctx, key, (now, level), &mut spent, &mut stats);
    // A drag this canvas showed was committed at the offset it showed: the split is rebased on
    // the result and draws the release's tiles (`move_cpu::Split::rebase`), as the drag's frames
    // were drawn; they are composited exactly once the edits pause.
    // (The split checks itself that `doc` is its document with the run only shifted. A split in
    // flight comes back to an edit of its run, and keeps its tiles for it.)
    let release_plan = released.and_then(|(_, ids, offset)| {
        let split = tc.split.as_mut()?;
        (split.moves(id, &ids, level) && split.rebase(&doc, revision, offset)).then(|| Plan { base: doc.clone(), ids, doc: id, revision, offset: (0, 0) })
    });
    if move_plan.is_some() || release_plan.is_some() {
        tc.refine_after = crate::gpu_canvas::now_ms() + REFINE_AFTER_MS;
    }
    // Tiles the worker pool finished for this state: in place of what stood in for them.
    if let Some(p) = &tc.pending {
        match p.rx.try_recv() {
            Ok(done) => {
                if p.state == now && p.level == level {
                    for (k, held, img) in done {
                        if tc.tiles.get(&k).is_some_and(|t| t.dirty.is_none() && t.drawing.is_none() && t.held == held && !t.approx) {
                            continue;
                        }
                        let tex = ctx.load_texture(format!("cpu-tile-{}-{}-{}-{}-{}", id.0, key.1, k.level, k.tx, k.ty), img, TextureOptions::LINEAR);
                        tc.tiles.insert(k, TileTex { tex, held, dirty: None, drawing: None, used: frame, approx: false });
                    }
                }
                tc.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => tc.pending = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    // Work for the tiles in view.
    let setup = crate::perf_span::span("cpu.setup");
    let want = tiles_in(visible.intersect(&doc.bounds()), level, size);
    let mut stale = Vec::new();
    let mut missing = Vec::new();
    let f = 1i64 << level;
    let bounds = level_bounds(size, f);
    let held_of = |k: TileKey| tile_rect(k, size).inflate(GUTTER).intersect(&bounds);
    for &k in &want {
        let held = held_of(k);
        match tc.tiles.get_mut(&k) {
            None => missing.push(Job::Full(k, held)),
            // In flight.
            Some(t) if t.drawing.is_some() => {}
            Some(t) if t.held != held => stale.push(Job::Full(k, held)),
            Some(t) => {
                if let Some(d) = t.dirty {
                    let part = level_rect(d, f).intersect(&t.held);
                    if part.is_empty() {
                        // None of it in this tile.
                        t.dirty = None;
                    } else if u64::from(part.width()) * u64::from(part.height()) * 2 > u64::from(held.width()) * u64::from(held.height()) {
                        stale.push(Job::Full(k, held));
                    } else {
                        stale.push(Job::Part(k, part));
                    }
                }
            }
        }
    }
    let view: Vec<DRect> = want.iter().map(|k| held_of(*k)).collect();
    // What the split's tiles made ahead would be for now, and whether they are made.
    let ahead: Ahead = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        want.hash(&mut h);
        (now, tc.anchor.clone(), level, h.finish())
    };
    tc.ahead_ready = tc.ahead.as_ref() == Some(&ahead);
    let dragging = move_plan.is_some();
    // Tiles drawn through the split's top map, composited exactly once the edits pause and the
    // split for the next edit is made (as missing tiles are: on the worker pool, or in the frame's
    // budget). Each replaces its tile whole.
    let idle = !dragging && release_plan.is_none() && stale.is_empty() && missing.is_empty() && tc.flight.is_none() && tc.ahead_ready && !pointer_down;
    let approx: Vec<Job> = want.iter().filter_map(|k| tc.tiles.get(k).filter(|t| t.approx).map(|t| Job::Full(*k, t.held))).collect();
    let mut refining = false;
    if !approx.is_empty() {
        let wait = tc.refine_after - crate::gpu_canvas::now_ms();
        if !idle || wait > 0.0 {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait.max(16.0) / 1000.0));
        } else if tc.pending.is_none() {
            missing = approx;
            refining = true;
        }
    }
    drop(setup);
    let through = match (move_plan.as_ref(), release_plan) {
        (Some(plan), _) => Through::Move { plan: plan.clone(), lazy, release: false },
        (None, Some(plan)) => Through::Move { plan, lazy: false, release: true },
        (None, None) => Through::Edit { ids: tc.anchor.clone(), id, revision },
    };
    let mut more_ahead = false;
    // An edit, or a press that may start one: the batch making the split ahead comes back.
    if let Some(fl) = tc.flight.as_ref().filter(|fl| fl.ahead.is_some() && (!stale.is_empty() || pointer_down)) {
        fl.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // Nothing of the view changed and nothing is on its way: the tiles are of this state.
    if stale.is_empty() && tc.flight.is_none() {
        tc.under_box = stands_in;
    }
    // Changed tiles, all of them in one batch: the pool composites them while the frame goes on,
    // and they replace the ones shown together, in the frame that finds them done (this one when
    // the pool is quick). One batch at a time: what changes meanwhile goes in the next.
    if !stale.is_empty() && tc.flight.is_none() {
        for j in &stale {
            if let Some(t) = tc.tiles.get_mut(&j.key()) {
                t.drawing = Some(t.dirty.take().unwrap_or(DRect::EMPTY));
            }
        }
        let batch = Batch {
            doc: doc.clone(),
            level,
            jobs: stale.iter().map(|j| (*j, held_of(j.key()))).collect(),
            maps: dragging || pointer_down,
            display: display.clone(),
            levels: tc.levels.clone(),
            split: tc.split.take(),
            spare: tc.spare.take(),
            handover,
            through: through.clone(),
            ahead: Vec::new(),
            ahead_ms: 0.0,
            sprite: false,
            stop: Arc::default(),
            budget: split_budget,
            view: view.clone(),
        };
        let anchor = tc.anchor.clone();
        if let Some(d) = tc.send(ctx, batch, now, None, wait) {
            // Done in the frame's wait: shown now, or once its textures are uploaded.
            let stop = Arc::default();
            tc.flight = Some(Flight { rx: None, staged: Some(Staged::new(d)), state: now, level, ahead: None, anchor, stop, under_box: Some(stands_in) });
            tc.take_flight(ctx, key, (now, level), &mut spent, &mut stats);
        }
    } else if matches!(through, Through::Edit { .. }) && stale.is_empty() && missing.is_empty() && tc.flight.is_none() && tc.pending.is_none() && !tc.ahead_ready && !pointer_down {
        // Nothing to draw: the split's tiles for the view, made ahead a few at a time.
        let _s = crate::perf_span::span("cpu.split_ahead");
        let batch = Batch {
            doc: doc.clone(),
            level,
            jobs: Vec::new(),
            maps: false,
            display: None,
            levels: tc.levels.clone(),
            split: tc.split.take(),
            spare: tc.spare.take(),
            handover: false,
            through: through.clone(),
            ahead: view.clone(),
            ahead_ms: if wait.is_some() { AHEAD_FLIGHT_MS } else { SPLIT_AHEAD_MS },
            sprite: move_tool,
            stop: Arc::default(),
            budget: split_budget,
            view: view.clone(),
        };
        // (Off the frame it isn't waited for: nothing of it shows.)
        let anchor = tc.anchor.clone();
        if let Some(d) = tc.send(ctx, batch, now, Some(ahead.clone()), wait.map(|_| 0.0)) {
            more_ahead = !d.ahead_done;
            tc.land(ctx, key, d, Vec::new(), Sent { fresh: true, ahead: Some(ahead.clone()), anchor }, &mut stats);
        }
    }
    // Missing tiles while the frame has time (at least one batch), where there is no pool: the
    // rest show from another level and are drawn over the next frames.
    let t0 = crate::gpu_canvas::now_ms();
    let batch_size = rayon_batch();
    let mut rest = missing.into_iter().peekable();
    let mut first = true;
    while !pooled && rest.peek().is_some() && (first || crate::gpu_canvas::now_ms() - t0 <= FRAME_BUDGET_MS) {
        first = false;
        let jobs: Vec<(Job, DRect)> = rest.by_ref().take(batch_size).map(|j| (j, j.rect())).collect();
        // (Through the split only while it is here; exactly when these replace approximate tiles.)
        let through = if refining || tc.flight.is_some() { Through::Whole } else { through.clone() };
        let batch = Batch {
            doc: doc.clone(),
            level,
            jobs,
            maps: false,
            display: display.clone(),
            levels: tc.levels.clone(),
            split: tc.split.take(),
            spare: tc.spare.take(),
            handover: false,
            through,
            ahead: Vec::new(),
            ahead_ms: 0.0,
            sprite: false,
            stop: Arc::default(),
            budget: split_budget,
            view: view.clone(),
        };
        let anchor = tc.anchor.clone();
        tc.land(ctx, key, draw(batch), Vec::new(), Sent { fresh: true, ahead: None, anchor }, &mut stats);
    }
    // Missing tiles left: composited on the worker pool while the frame goes on.
    if pooled && rest.peek().is_some() && tc.pending.is_none() {
        let jobs: Vec<(TileKey, DRect)> = rest.by_ref().take(batch_size * 2).map(|j| (j.key(), j.rect())).collect();
        // A level not built yet for this document is reduced on the worker too.
        let faithful = level == 0 || photocraft_compose::proxy::lod_faithful(&doc);
        let (tx, rx) = std::sync::mpsc::channel();
        tc.pending = Some(Pending { state: now, level, rx });
        // (A lazy Move preview: the worker composites the document as shown.)
        let moved = move_plan.as_ref().filter(|_| lazy).map(|p| (p.base.clone(), p.ids.clone(), p.offset));
        let (doc, display, levels) = (doc.clone(), display.clone(), tc.levels.clone());
        spawn(move || {
            #[cfg(test)]
            let pace = Pace::start();
            let doc = moved.and_then(|(base, ids, o)| photocraft_engine::layer_multi_cmds::moved(&base, &ids, o.0, o.1).ok().map(Arc::new)).unwrap_or(doc);
            let source = faithful.then(|| reduced(&levels, &doc, level));
            let rects: Vec<DRect> = jobs.iter().map(|(_, r)| *r).collect();
            let bufs = render_level(&doc, source.as_deref(), f, &rects);
            let done = jobs.into_iter().zip(bufs).map(|((k, held), b)| (k, held, tile_image(display.as_deref(), &b))).collect();
            #[cfg(test)]
            pace.hold();
            let _ = tx.send(done);
        });
    }
    // Look for the worker pool's tiles in a moment (the egui context can't be woken from a
    // worker on the web); without it, the next batch at once.
    if tc.pending.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
    }
    if tc.flight.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(8));
    }
    tc.ahead_ready = tc.ahead.as_ref() == Some(&ahead);
    // More of the split to make ahead, or a batch just landed and the split's tiles are next.
    if more_ahead || (stats.px > 0 && !tc.ahead_ready && tc.flight.is_none()) {
        ctx.request_repaint();
    }
    let more = !pooled && rest.peek().is_some();
    // Made ahead counts too: the drag's first frame then shows the document as it is and lets
    // this canvas shift the moving layers (`move_ui::display_doc`), no moved copy made for it.
    // (While the split is in flight, what it was before it left.)
    if let Some(s) = tc.split.as_ref() {
        let press = move_plan.clone().or_else(|| wanted.filter(|w| move_tool && !w.is_empty()).map(|ids| Plan { base: doc.clone(), ids, doc: id, revision, offset: (0, 0) }));
        tc.split_ready = press.is_some_and(|p| s.is_for(&p, level));
    } else if tc.flight.is_none() {
        tc.split_ready = false;
    }
    let split_ready = tc.split_ready;
    if stats.px > 0 {
        app.perf.record("tiles", stats.px, stats.composite_ms, stats.upload_ms);
        app.perf.stage.pooled(stats.pool_ms);
    }
    app.perf_counters.move_split += u64::from(stats.from_split);
    app.cpu_move_split = split_ready.then_some((id, revision, app.frame));
    let Some(tc) = app.canvases.get_mut(&key).and_then(|c| c.tiles.as_mut()) else { return };
    tc.xf = Some(*xf);
    // The page before a size change, until the new one in view is all here.
    if tc.before.is_some() && !want.iter().all(|k| tc.tiles.get(k).is_some_and(|t| t.dirty.is_none() && t.drawing.is_none())) {
        for &k in &want {
            if let Some(t) = tc.tiles.get_mut(&k) {
                t.used = frame;
            }
        }
        if let Some(b) = &tc.before {
            let bounds = DRect::new(0, 0, b.size[0] as i32, b.size[1] as i32);
            for (k, t) in &b.tiles {
                let f = 1i64 << k.level;
                draw_tile(painter, &b.xf, &t.tex, t.held, f, scale_rect(tile_rect(*k, b.size), f).intersect(&bounds));
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
        tc.evict(tex_budget);
        return;
    }
    tc.before = None;
    // Draw: a tile not drawn yet at this level shows from the nearest level that has its area.
    let mut shown = 0usize;
    for &k in &want {
        if let Some(t) = tc.tiles.get_mut(&k) {
            t.used = frame;
            shown += 1;
            continue;
        }
        let own = scale_rect(tile_rect(k, size), f);
        for k2 in fallback_keys(k, size) {
            if let Some(t) = tc.tiles.get_mut(&k2) {
                t.used = frame;
                draw_tile(painter, xf, &t.tex, t.held, 1i64 << k2.level, own.intersect(&doc.bounds()));
            }
        }
    }
    for &k in &want {
        if let Some(t) = tc.tiles.get(&k) {
            let own = scale_rect(tile_rect(k, size), f).intersect(&doc.bounds());
            draw_tile(painter, xf, &t.tex, t.held, f, own);
        }
    }
    if more || (shown < want.len() && tc.pending.is_none()) {
        ctx.request_repaint();
    }
    // Memory: least recently drawn tiles beyond the budget go; reduced layers past theirs.
    tc.evict(tex_budget);
    if tc.pending.is_none()
        && tc.flight.is_none()
        && let Ok(mut l) = tc.levels.try_lock()
    {
        l.mips.trim(mip_budget);
    }
}

/// Whether document `idx`'s CPU canvas on `output` holds every tile `visible` needs at `zoom`'s
/// level, current, with nothing left on the worker pool: [`paint`] then draws it complete
/// (`canvas::prewarm`).
pub(crate) fn drawn_in(app: &PhotocraftApp, idx: usize, output: Option<u32>, zoom: f32, visible: DRect) -> bool {
    let Some(st) = app.session.documents().get(idx) else { return false };
    let size = [st.doc.size.width, st.doc.size.height];
    let Some(tc) = app.canvases.get(&crate::canvas::cache_key(st.doc.id, output)).and_then(|c| c.tiles.as_ref()) else { return false };
    tc.pending.is_none()
        && tc.flight.is_none()
        && tc.shown.is_some_and(|(revision, _)| revision == st.revision)
        && tiles_in(visible.intersect(&st.doc.bounds()), level_for(zoom, size), size).iter().all(|k| tc.tiles.get(k).is_some_and(|t| t.dirty.is_none() && t.drawing.is_none()))
}

/// Whether the active document's CPU canvases show its current state complete: no tile in view
/// changed or in flight (tests).
#[cfg(test)]
pub(crate) fn shows_document(app: &PhotocraftApp) -> bool {
    let Some(st) = app.session.active() else { return true };
    app.canvases.iter().filter(|(k, _)| k.0 == st.doc.id).filter_map(|(_, c)| c.tiles.as_ref()).all(|tc| {
        tc.before.is_none()
            && tc.shown.is_some_and(|(revision, _)| revision == st.revision)
            && tc.tiles.values().all(|t| t.used != tc.frame || (t.dirty.is_none() && t.drawing.is_none()))
    })
}

/// Whether the tiles on the screen of canvas `key` are of the document under the Free Transform
/// box, which then draws the layer it stands in for over them (`None`: the canvas isn't drawn in
/// tiles, or wasn't in this frame or the one before).
pub(crate) fn under_box(app: &PhotocraftApp, key: (DocId, u32)) -> Option<bool> {
    let tc = app.canvases.get(&key)?.tiles.as_ref()?;
    (tc.painted + 1 >= app.frame).then_some(tc.under_box)
}

/// Whether they also have nothing left to make: no batch in flight, the split made ahead, no
/// tile waiting to be composited exactly (tests).
#[cfg(test)]
pub(crate) fn settled(app: &PhotocraftApp) -> bool {
    let Some(st) = app.session.active() else { return true };
    shows_document(app)
        && app.canvases.iter().filter(|(k, _)| k.0 == st.doc.id).filter_map(|(_, c)| c.tiles.as_ref()).all(|tc| {
            tc.pending.is_none() && tc.flight.is_none() && tc.ahead_ready && tc.tiles.values().all(|t| t.used != tc.frame || !t.approx)
        })
}

impl TileCanvas {
    /// Drop the least recently drawn tiles until the textures fit `budget` bytes; the tiles this
    /// frame drew stay whatever they cost.
    fn evict(&mut self, budget: usize) {
        let mut bytes = self.texture_bytes();
        if bytes <= budget {
            return;
        }
        let frame = self.frame;
        let mut by_age: Vec<(u64, TileKey)> = self.tiles.iter().filter(|(_, t)| t.used != frame).map(|(k, t)| (t.used, *k)).collect();
        by_age.sort_unstable();
        for (_, k) in by_age {
            if bytes <= budget {
                break;
            }
            if let Some(t) = self.tiles.remove(&k) {
                bytes = bytes.saturating_sub(t.held.width() as usize * t.held.height() as usize * 4);
            }
        }
    }
}

/// Run `f` on the worker pool.
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
pub(crate) fn spawn(f: impl FnOnce() + Send + 'static) {
    rayon::spawn(f);
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
pub(crate) fn spawn(f: impl FnOnce() + Send + 'static) {
    f();
}

/// A worker pool several times slower than this machine's, for tests of weak machines (the
/// owner's PC composites 6 to 8 times slower than the build server): a job holds back what it
/// made for `slowdown - 1` times what it took, so it reaches the frames as late as it would there.
#[cfg(test)]
pub(crate) struct Pace(std::time::Instant);

#[cfg(test)]
static POOL_SLOWDOWN: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Make the worker pool's jobs take `times` as long (tests; 1: as they are).
#[cfg(test)]
pub(crate) fn set_pool_slowdown(times: u32) {
    POOL_SLOWDOWN.store(times.max(1), std::sync::atomic::Ordering::Relaxed);
}

#[cfg(test)]
impl Pace {
    pub(crate) fn start() -> Self {
        Self(std::time::Instant::now())
    }

    pub(crate) fn hold(self) {
        let times = POOL_SLOWDOWN.load(std::sync::atomic::Ordering::Relaxed);
        if times > 1 {
            std::thread::sleep(self.0.elapsed() * (times - 1));
        }
    }
}

/// Tiles composited per batch: one per worker in the threads build, a few otherwise.
fn rayon_batch() -> usize {
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    {
        rayon::current_num_threads().clamp(2, 16)
    }
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    {
        2
    }
}

/// Where to look for a stand-in for tile `k`: the coarser levels covering it, then the four
/// tiles of the next finer level.
fn fallback_keys(k: TileKey, size: [u32; 2]) -> Vec<TileKey> {
    let mut v = Vec::new();
    for up in 1..=4u32 {
        let l = k.level + up;
        if l > MAX_LEVEL {
            break;
        }
        v.push(TileKey { level: l, tx: k.tx >> up, ty: k.ty >> up });
    }
    if k.level > 0 {
        let l = k.level - 1;
        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            let c = TileKey { level: l, tx: k.tx * 2 + dx, ty: k.ty * 2 + dy };
            if !tile_rect(c, size).is_empty() {
                v.push(c);
            }
        }
    }
    v
}

/// Draw the part `doc_part` (document pixels) of a tile texture that holds level pixels `held`
/// at factor `f`.
fn draw_tile(painter: &egui::Painter, xf: &ViewXform, tex: &egui::TextureHandle, held: DRect, f: i64, doc_part: DRect) {
    let src = scale_rect(held, f);
    let part = doc_part.intersect(&src);
    if part.is_empty() {
        return;
    }
    let (w, h) = (src.width() as f32, src.height() as f32);
    let u = |x: i32| (x - src.x0) as f32 / w;
    let v = |y: i32| (y - src.y0) as f32 / h;
    let uv = Rect::from_min_max(pos2(u(part.x0), v(part.y0)), pos2(u(part.x1), v(part.y1)));
    let corners = [(part.x0, part.y0), (part.x1, part.y0), (part.x1, part.y1), (part.x0, part.y1)].map(|(x, y)| xf.to_screen(x as f32, y as f32));
    let uvs = [uv.min, pos2(uv.max.x, uv.min.y), uv.max, pos2(uv.min.x, uv.max.y)];
    let mut mesh = Mesh::with_texture(tex.id());
    for (p, t) in corners.into_iter().zip(uvs) {
        mesh.vertices.push(egui::epaint::Vertex { pos: Pos2::new(p.x, p.y), uv: t, color: Color32::WHITE });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashSet;

    fn app(w: u32, h: u32) -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
        app.run("layer.new.layer", json!({"name": "ink"})).unwrap();
        app.run("select.rect", json!({"x": 30, "y": 40, "width": 500, "height": 300, "mode": "replace"})).unwrap();
        app.run("edit.fill", json!({"contents": "color", "color": "#3366cc", "opacity": 70})).unwrap();
        app.run("select.deselect", json!({})).unwrap();
        app.run("paint.stroke", json!({"points": [[10, 10, 1], [w as f64 - 20.0, h as f64 - 30.0, 1]], "size": 25, "color": "#111111"})).unwrap();
        app
    }

    /// The texture uploads of one frame.
    struct Uploads {
        set: Vec<(egui::TextureId, Vec<egui::epaint::ImageDelta>)>,
    }

    /// One frame of the CPU canvas of document 0 at `zoom`, the whole document in view.
    fn frame(app: &mut PhotocraftApp, ctx: &egui::Context, zoom: f32, center: [f32; 2]) -> Uploads {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let xf = ViewXform { rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 800.0)), zoom, center, flip: false, rotation: 0.0, aspect: 1.0 };
            let visible = DRect::new(
                (center[0] - 500.0 / zoom) as i32,
                (center[1] - 400.0 / zoom) as i32,
                (center[0] + 500.0 / zoom) as i32 + 1,
                (center[1] + 400.0 / zoom) as i32 + 1,
            );
            paint(app, ui.ctx(), ui.painter(), &xf, 0, None, zoom, visible);
        });
        // Only the canvas's own textures (not egui's font atlas).
        let set = out.textures_delta.set.iter().filter(|(id, _)| !matches!(id, egui::TextureId::Managed(0))).map(|(id, d)| (*id, d.to_vec())).collect();
        out.textures_delta.clear();
        Uploads { set }
    }

    fn canvas(app: &PhotocraftApp) -> &TileCanvas {
        let id = app.session.documents()[0].doc.id;
        app.canvases[&crate::canvas::cache_key(id, None)].tiles.as_ref().unwrap()
    }

    #[test]
    fn premultiplied_pixels_are_the_ones_egui_would_make() {
        for a in 0..=255u8 {
            for c in 0..=255u8 {
                let p = photocraft_compose::premultiply_px([c, 255 - c, c / 2, a]);
                assert_eq!(Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]), Color32::from_rgba_unmultiplied(c, 255 - c, c / 2, a), "{c} {a}");
            }
        }
    }

    #[test]
    fn tiles_at_100_percent_are_the_exact_composite() {
        let mut app = app(600, 400);
        let ctx = egui::Context::default();
        let delta = frame(&mut app, &ctx, 1.0, [300.0, 200.0]);
        let doc = app.session.documents()[0].doc.clone();
        let tc = canvas(&app);
        assert_eq!((tc.level, tc.tile_count()), (0, 3 * 2));
        for t in tc.tiles.values() {
            let (_, set) = delta.set.iter().find(|(id, _)| *id == t.tex.id()).expect("uploaded");
            let egui::ImageData::Color(img) = &set.last().unwrap().image;
            let want = photocraft_compose::render(&doc, t.held).to_premultiplied_rgba8();
            let got: Vec<[u8; 4]> = img.pixels.iter().map(|c| c.to_array()).collect();
            assert_eq!(got, want, "{:?}", t.held);
        }
        // Nothing changed: the next frame composites nothing.
        app.perf.last_refresh = "none";
        let delta = frame(&mut app, &ctx, 1.0, [300.0, 200.0]);
        assert!(delta.set.is_empty() && app.perf.last_refresh == "none");
    }

    #[test]
    fn an_edit_redraws_only_what_it_touched() {
        let mut app = app(1200, 900);
        let ctx = egui::Context::default();
        for _ in 0..4 {
            frame(&mut app, &ctx, 1.0, [600.0, 450.0]);
        }
        let before = canvas(&app).tile_count();
        app.run("paint.stroke", json!({"points": [[700, 500, 1], [720, 510, 1]], "size": 9, "color": "#ff0000"})).unwrap();
        // What a dab reports (the engine's own damage for this command may be the layer's).
        app.session.active_mut().unwrap().last_damage = Some(DRect::new(690, 490, 730, 520));
        let delta = frame(&mut app, &ctx, 1.0, [600.0, 450.0]);
        assert_eq!(app.perf.last_refresh, "tiles");
        assert!(app.perf.last_refresh_px < 64 * 64, "{}", app.perf.last_refresh_px);
        assert!(delta.set.iter().all(|(_, d)| d.iter().all(|d| d.pos.is_some())), "partial uploads only");
        assert_eq!(canvas(&app).tile_count(), before);
        // An edit with no damage rectangle redraws every tile in view, whole (uploaded over two
        // frames: more than a frame's budget).
        app.session.active_mut().unwrap().last_damage = None;
        app.session.active_mut().unwrap().revision += 1;
        let mut set = frame(&mut app, &ctx, 1.0, [600.0, 450.0]).set;
        assert!(!shows_document(&app) && !set.is_empty());
        set.extend(frame(&mut app, &ctx, 1.0, [600.0, 450.0]).set);
        assert!(shows_document(&app));
        assert_eq!(set.len(), before);
        assert!(set.iter().all(|(_, d)| d.iter().all(|d| d.pos.is_none())));
    }

    /// Texture contents rebuilt from a frame's uploads (whole and partial), by texture.
    fn apply(mirror: &mut HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>, delta: &Uploads) {
        for (id, ds) in &delta.set {
            for d in ds {
                let egui::ImageData::Color(img) = &d.image;
                let px: Vec<[u8; 4]> = img.pixels.iter().map(|c| c.to_array()).collect();
                match d.pos {
                    None => {
                        mirror.insert(*id, (img.size[0], img.size[1], px));
                    }
                    Some([x, y]) => {
                        let (w, _, buf) = mirror.get_mut(id).expect("partial upload of a known texture");
                        for row in 0..img.size[1] {
                            let at = (y + row) * *w + x;
                            buf[at..at + img.size[0]].copy_from_slice(&px[row * img.size[0]..(row + 1) * img.size[0]]);
                        }
                    }
                }
            }
        }
    }

    /// After stroking a path on a new layer, every tile shows the exact composite: no row at the
    /// edge of the redrawn part is left transparent (the checkerboard showed through as a dashed
    /// line until the tile was redrawn whole).
    #[test]
    fn a_stroked_path_leaves_no_seam_in_the_tiles() {
        for zoom in [1.0f32, 0.928, 0.5] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.run("file.new", json!({"width": 800, "height": 800, "background": "white"})).unwrap();
            let ctx = egui::Context::default();
            let mut mirror = HashMap::new();
            for _ in 0..6 {
                let d = frame(&mut app, &ctx, zoom, [400.0, 400.0]);
                apply(&mut mirror, &d);
            }
            app.run("path.set", json!({"path": {"subpaths": [{"closed": true, "knots": [[100, 100], [700, 100], [700, 600], [100, 600]]}]}})).unwrap();
            app.run("layer.new.layer", json!({})).unwrap();
            app.run("path.stroke", json!({"width": 5, "color": "#111111"})).unwrap();
            for _ in 0..3 {
                let d = frame(&mut app, &ctx, zoom, [400.0, 400.0]);
                apply(&mut mirror, &d);
            }
            let doc = app.session.documents()[0].doc.clone();
            let tc = canvas(&app);
            let f = 1i64 << tc.level;
            let source = (tc.level > 0).then(|| reduced(&tc.levels, &doc, tc.level));
            for (k, t) in tc.tiles.iter().filter(|(k, _)| k.level == tc.level) {
                let (w, h, got) = &mirror[&t.tex.id()];
                assert_eq!((*w, *h), (t.held.width() as usize, t.held.height() as usize));
                let want = render_level(&doc, source.as_deref(), f, &[t.held]).remove(0);
                let want = tile_image(None, &want);
                let want: Vec<[u8; 4]> = want.pixels.iter().map(|c| c.to_array()).collect();
                let bad = got.iter().zip(&want).position(|(a, b)| a != b);
                assert!(
                    bad.is_none(),
                    "zoom {zoom} tile {k:?} held {:?}: first wrong pixel at {:?}: {:?} vs {:?}",
                    t.held,
                    bad.map(|i| (i % w, i / w)),
                    bad.map(|i| got[i]),
                    bad.map(|i| want[i])
                );
            }
        }
    }

    #[test]
    fn zoomed_out_views_composite_a_reduced_level() {
        let mut app = app(2000, 1600);
        let ctx = egui::Context::default();
        // Tiles beyond a frame's budget come over the next frames.
        let mut px = 0;
        for _ in 0..8 {
            app.perf.last_refresh_px = 0;
            frame(&mut app, &ctx, 0.3, [1000.0, 800.0]);
            px += app.perf.last_refresh_px;
        }
        let tc = canvas(&app);
        assert_eq!(tc.level, 1);
        // 1000×800 level pixels: 4×4 tiles, about a quarter of the document's pixels composited.
        assert_eq!(tc.tile_count(), 4 * 4);
        assert!(px < 2000 * 1600 / 3, "{px}");
        assert!(tc.mip_bytes() > 0);
        // Zooming in shows the level-1 tiles until level 0 is drawn, then both are kept.
        frame(&mut app, &ctx, 1.0, [1000.0, 800.0]);
        assert_eq!(canvas(&app).level, 0);
    }

    #[test]
    fn tile_textures_stay_within_their_budget() {
        let mut app = app(3000, 1000);
        let ctx = egui::Context::default();
        for cx in [500.0, 1500.0, 2500.0] {
            frame(&mut app, &ctx, 1.0, [cx, 500.0]);
        }
        let id = app.session.documents()[0].doc.id;
        let tc = app.canvases.get_mut(&crate::canvas::cache_key(id, None)).unwrap().tiles.as_mut().unwrap();
        let all = tc.texture_bytes();
        let shown: usize = tc.tiles.values().filter(|t| t.used == tc.frame).map(|t| t.held.width() as usize * t.held.height() as usize * 4).sum();
        assert!(shown < all);
        tc.evict(shown + 1);
        assert!(tc.texture_bytes() <= shown + 1);
        assert!(tc.tiles.values().all(|t| t.used == tc.frame), "only the tiles in view are left");
        tc.evict(0);
        assert_eq!(tc.texture_bytes(), shown, "the view's own tiles stay");
    }

    #[test]
    fn level_follows_zoom() {
        let s = [4000, 6000];
        assert_eq!(level_for(1.0, s), 0);
        assert_eq!(level_for(0.5, s), 0);
        assert_eq!(level_for(0.49, s), 1);
        assert_eq!(level_for(0.26, s), 1);
        assert_eq!(level_for(0.25, s), 2);
        assert_eq!(level_for(0.01, s), 6);
        assert_eq!(level_for(0.0, s), 0);
        assert_eq!(level_for(f32::NAN, s), 0);
        assert_eq!(level_for(1e-9, [3, 3]), 1, "not past a one-pixel document");
    }

    #[test]
    fn tiles_cover_the_view_and_stop_at_the_document() {
        let size = [1000, 600];
        let all = tiles_in(DRect::new(-50, -50, 5000, 5000), 0, size);
        assert_eq!(all.len(), 4 * 3);
        let l1 = tiles_in(DRect::new(0, 0, 1000, 600), 1, size);
        assert_eq!(l1.len(), 2 * 2);
        assert!(tiles_in(DRect::new(2000, 0, 3000, 600), 0, size).is_empty());
        let r = tile_rect(TileKey { level: 1, tx: 1, ty: 1 }, size);
        assert_eq!(r, DRect::new(256, 256, 500, 300));
    }

    /// A 1200 x 900 page: white, an ink layer (selected), and two layers over it (Multiply,
    /// Screen).
    fn stacked() -> PhotocraftApp {
        let mut app = app(1200, 900);
        let ink = app.session.active().unwrap().active_layer.unwrap();
        for (name, blend, color) in [("shade", "multiply", "#c0a080"), ("glow", "screen", "#203060")] {
            app.run("layer.new.layer", json!({"name": name})).unwrap();
            app.run("select.rect", json!({"x": 200, "y": 100, "width": 800, "height": 600, "mode": "replace"})).unwrap();
            app.run("edit.fill", json!({"contents": "color", "color": color, "opacity": 60})).unwrap();
            app.run("select.deselect", json!({})).unwrap();
            app.run("layer.setProps", json!({"blend": blend})).unwrap();
        }
        app.run("layer.select", json!({"layer": ink.0})).unwrap();
        app
    }

    /// Frames until the canvas has nothing left to draw or make (the pool's batches landed).
    fn settle(app: &mut PhotocraftApp, ctx: &egui::Context, mirror: &mut HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>) {
        for _ in 0..2000 {
            apply(mirror, &frame(app, ctx, 1.0, [600.0, 450.0]));
            if settled(app) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the canvas never settled");
    }

    /// The largest difference between the tiles' textures and the exact composite (8-bit steps).
    fn off(app: &PhotocraftApp, mirror: &HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>) -> u8 {
        off_from(app, mirror, &app.session.documents()[0].doc.clone())
    }

    /// [`off`] from the composite of `doc`.
    fn off_from(app: &PhotocraftApp, mirror: &HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>, doc: &Document) -> u8 {
        let mut worst = 0;
        for t in canvas(app).tiles.values() {
            let want = photocraft_compose::render(doc, t.held).to_premultiplied_rgba8();
            let (_, _, got) = &mirror[&t.tex.id()];
            worst = worst.max(got.iter().zip(&want).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0));
        }
        worst
    }

    /// An edit of a layer as large as the page, with the worker pool as the app has it: the
    /// window's thread composites nothing (the frame that takes the edit, and every frame until
    /// it shows, only hand over and look), the canvas shows the page as it was until the edit's
    /// tiles are all there, and they arrive in one frame, never part of them.
    #[test]
    fn an_edit_is_composited_off_the_frame_and_shown_whole() {
        let mut app = stacked();
        let ctx = egui::Context::default();
        // (No wait: the frame never finds the batch done before it goes on.)
        set_flight_wait(Some(0.0));
        let mut mirror = HashMap::new();
        settle(&mut app, &ctx, &mut mirror);
        assert_eq!(off(&app, &mirror), 0, "the page before the edit");
        let tiles = canvas(&app).tile_count();
        let page: usize = canvas(&app).tiles.values().map(|t| t.held.width() as usize * t.held.height() as usize * 4).sum();
        assert!(page > UPLOAD_BUDGET, "a page of more texture than a frame uploads ({page} bytes)");
        let before = app.session.documents()[0].doc.clone();
        // (Less the test's own composites.)
        let mut here = photocraft_compose::composites_here();
        let (held, pool) = (app.perf.stage.composite_ms, app.perf.stage.pool_ms);
        app.run("edit.fill", json!({"contents": "color", "color": "#22aa55", "opacity": 80})).unwrap();
        let mut frames = 0;
        let mut uploading = 0;
        let mut uploaded = HashSet::new();
        loop {
            let delta = frame(&mut app, &ctx, 1.0, [600.0, 450.0]);
            frames += 1;
            assert!(frames < 5000, "the edit never showed");
            let bytes: usize = delta.set.iter().flat_map(|(_, d)| d).map(|d| d.image.width() * d.image.height() * 4).sum();
            assert!(bytes <= UPLOAD_BUDGET, "a frame uploaded {bytes} bytes of tiles");
            uploaded.extend(delta.set.iter().map(|(id, _)| *id));
            apply(&mut mirror, &delta);
            if shows_document(&app) {
                break;
            }
            // Not there yet, or its textures on their way: the canvas shows the page before the edit, all of it.
            uploading += usize::from(!delta.set.is_empty());
            assert!(canvas(&app).tiles.values().all(|t| !uploaded.contains(&t.tex.id())), "no tile of the edit shows before the others");
            let checks = photocraft_compose::composites_here();
            assert_eq!(off_from(&app, &mirror, &before), 0, "the page before the edit");
            here += photocraft_compose::composites_here() - checks;
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(uploading >= 1, "the tiles were uploaded over more than one frame");
        assert!(canvas(&app).tiles.values().all(|t| uploaded.contains(&t.tex.id())), "every tile of the page in the same frame");
        assert_eq!(canvas(&app).tile_count(), tiles);
        assert_eq!(photocraft_compose::composites_here() - here, 0, "nothing composited on the window's thread");
        // The frames' record says so too: the pool's time isn't counted as the frame's (a frame
        // of 4 ms that landed a batch was logged as a stall of the 370 ms the pool had taken).
        assert_eq!(app.perf.stage.composite_ms, held, "no composite held a frame");
        assert!(app.perf.stage.pool_ms > pool, "the batch's time is the pool's");
        assert!(off(&app, &mirror) <= 1, "the edit as the full composite has it: off by {}", off(&app, &mirror));
        // Once the edits pause, the tiles drawn through the top's map are the exact composite.
        std::thread::sleep(std::time::Duration::from_millis(REFINE_AFTER_MS as u64 + 50));
        settle(&mut app, &ctx, &mut mirror);
        assert_eq!(off(&app, &mirror), 0);
        set_flight_wait(None);
    }

    /// One frame as [`frame`], and the textures it drew.
    fn frame_drawn(app: &mut PhotocraftApp, ctx: &egui::Context, center: [f32; 2]) -> HashSet<egui::TextureId> {
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let xf = ViewXform { rect: Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 800.0)), zoom: 1.0, center, flip: false, rotation: 0.0, aspect: 1.0 };
            let visible = DRect::new((center[0] - 500.0) as i32, (center[1] - 400.0) as i32, (center[0] + 500.0) as i32 + 1, (center[1] + 400.0) as i32 + 1);
            paint(app, ui.ctx(), ui.painter(), &xf, 0, None, 1.0, visible);
        });
        out.textures_delta.clear();
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Mesh(m) => Some(m.texture_id),
                _ => None,
            })
            .collect()
    }

    /// A crop (and its undo) changes the page's size: its new tiles are composited on the pool
    /// over several frames, and until they are all there the canvas shows the page as it was,
    /// where it was, never a blank or half-drawn canvas. It dropped every tile at once and drew
    /// the new ones as they came.
    #[test]
    fn a_new_page_size_shows_the_page_before_until_its_tiles_are_all_here() {
        let mut app = stacked();
        let ctx = egui::Context::default();
        set_flight_wait(Some(0.0));
        let mut mirror = HashMap::new();
        settle(&mut app, &ctx, &mut mirror);
        for (cmd, params, center) in [
            ("image.crop", json!({"x": 300, "y": 200, "width": 600, "height": 450}), [300.0, 225.0]),
            ("edit.undo", json!({}), [600.0, 450.0]),
        ] {
            let old: HashSet<egui::TextureId> = canvas(&app).tiles.values().map(|t| t.tex.id()).collect();
            app.run(cmd, params).unwrap();
            let mut frames = 0;
            loop {
                let drawn = frame_drawn(&mut app, &ctx, center);
                frames += 1;
                assert!(frames < 5000, "{cmd}: the new page never showed");
                assert!(!drawn.is_empty(), "{cmd}: frame {frames} drew nothing");
                if shows_document(&app) {
                    assert!(drawn.is_disjoint(&old), "{cmd}: the new page only");
                    break;
                }
                assert!(drawn.is_subset(&old), "{cmd}: frame {frames} drew part of the new page");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert!(frames > 1, "{cmd}: composited on the pool, over frames");
            settle(&mut app, &ctx, &mut mirror);
        }
        set_flight_wait(None);
    }

    /// An edit of the selected layer under other layers composites only that layer: the layers
    /// below it and the ones above were composited ahead, per tile, and the edit's frame is
    /// drawn from them. A tile the top's map drew is replaced by the exact composite once the
    /// edits pause.
    #[test]
    fn an_edit_under_other_layers_is_drawn_from_the_split() {
        let mut app = stacked();
        let ctx = egui::Context::default();
        let mut mirror = HashMap::new();
        settle(&mut app, &ctx, &mut mirror);
        let split = app.perf_counters.move_split;
        let here = photocraft_compose::composites_here();
        app.run("paint.stroke", json!({"points": [[300, 200, 1], [900, 700, 1]], "size": 60, "color": "#aa2200"})).unwrap();
        apply(&mut mirror, &frame(&mut app, &ctx, 1.0, [600.0, 450.0]));
        assert!(shows_document(&app));
        assert_eq!(app.perf_counters.move_split - split, 1, "drawn from the split");
        assert_eq!(photocraft_compose::composites_here() - here, 1, "one composite, of the layer that changed");
        assert!(off(&app, &mirror) <= 1, "off by {}", off(&app, &mirror));
        assert!(!settled(&app), "tiles the map drew are still to composite exactly");
        std::thread::sleep(std::time::Duration::from_millis(REFINE_AFTER_MS as u64 + 50));
        settle(&mut app, &ctx, &mut mirror);
        assert_eq!(off(&app, &mirror), 0);
        // An edit of another layer: the split is made again around it, the frame still exact.
        let top = app.session.documents()[0].doc.layers.last().unwrap().id;
        app.run("layer.hideLayers", json!({"layer": top.0})).unwrap();
        apply(&mut mirror, &frame(&mut app, &ctx, 1.0, [600.0, 450.0]));
        assert!(shows_document(&app));
        assert_eq!(off(&app, &mirror), 0, "composited for real: no map of the new top yet");
        assert_eq!(canvas(&app).anchor, vec![top]);
    }

    /// The frames in which the Free Transform box opened and closed waited for their tiles (55 to
    /// 240 ms of the window held on a page of many layers): now they go on, and the box takes
    /// over from the canvas, and hands back to it, in the frame the tiles land. In every frame
    /// either the canvas shows the layer or the box draws it, never both, never neither.
    #[test]
    fn the_transform_box_takes_over_and_hands_back_when_the_tiles_land() {
        let mut app = stacked();
        let ctx = egui::Context::default();
        let mut mirror = HashMap::new();
        settle(&mut app, &ctx, &mut mirror);
        // (No wait: an edit's own frame never has its tiles.)
        set_flight_wait(Some(0.0));
        let against = |app: &PhotocraftApp, mirror: &HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>, doc: &Document| {
            let mut worst = 0u8;
            for t in canvas(app).tiles.values() {
                let want = photocraft_compose::render(doc, t.held).to_premultiplied_rgba8();
                let (_, _, got) = &mirror[&t.tex.id()];
                worst = worst.max(got.iter().zip(&want).flat_map(|(a, b)| (0..4).map(move |c| a[c].abs_diff(b[c]))).max().unwrap_or(0));
            }
            worst
        };
        let doc = app.session.documents()[0].doc.clone();
        // Frames until the box draws the layer (`drawn`) or stops: each shows the layer once.
        let until = |app: &mut PhotocraftApp, mirror: &mut HashMap<egui::TextureId, (usize, usize, Vec<[u8; 4]>)>, under: &Document, drawn: bool| {
            for _ in 0..2000 {
                apply(mirror, &frame(app, &ctx, 1.0, [600.0, 450.0]));
                let box_draws = crate::transform_tool::draws_layer(app, None);
                assert!(against(app, mirror, if box_draws { under } else { &doc }) <= 1, "the box draws the layer ({box_draws}) and the canvas doesn't, or the other way round");
                if box_draws == drawn {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            panic!("the tiles never landed");
        };
        crate::transform_tool::begin(&mut app, &ctx).unwrap();
        apply(&mut mirror, &frame(&mut app, &ctx, 1.0, [600.0, 450.0]));
        let under = app.transform_preview.as_ref().unwrap().doc.clone();
        assert!(!crate::transform_tool::draws_layer(&app, None), "the frame the box opens in doesn't wait for the canvas under it");
        assert!(against(&app, &mirror, &doc) <= 1, "the canvas still shows the layer");
        until(&mut app, &mut mirror, &under, true);
        crate::transform_tool::cancel(&mut app);
        apply(&mut mirror, &frame(&mut app, &ctx, 1.0, [600.0, 450.0]));
        assert!(app.ui.transform.is_none() && crate::transform_tool::draws_layer(&app, None), "the frame it closes in doesn't wait: the box that closed still draws the layer");
        assert!(against(&app, &mirror, &under) <= 1, "over the canvas under it");
        until(&mut app, &mut mirror, &under, false);
        apply(&mut mirror, &frame(&mut app, &ctx, 1.0, [600.0, 450.0]));
        assert!(app.transform_closed.is_none(), "the box that closed is let go");
        set_flight_wait(None);
    }
}
