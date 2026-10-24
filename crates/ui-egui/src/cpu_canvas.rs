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
//! Layer effects and vector masks are scaled with the level ([`photocraft_compose::proxy`]); a
//! document with a stroked shape layer, whose vector stroke is drawn at render time in document
//! pixels ([`photocraft_compose::proxy::lod_faithful`]), takes its zoomed-out tiles from the
//! full-resolution composite of the tile's area instead.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use egui::{Color32, Mesh, Pos2, Rect, TextureOptions, pos2};
use photocraft_compose::mip::MipCache;
use photocraft_doc::Document;
use photocraft_geom::Rect as DRect;

use crate::PhotocraftApp;
use crate::canvas::ViewXform;

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
    /// Frame this tile was last drawn on screen.
    used: u64,
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
    /// A Move drag's cached composites below and above the moving layers (`move_cpu`).
    move_split: Option<crate::move_cpu::Split>,
}

/// Missing tiles composited in the background: the canvas keeps drawing (other levels stand in)
/// while they are, so zooming and panning a heavy document never waits for the compositor.
struct Pending {
    /// The (revision, preview key) and level they were composited for.
    state: (u64, u64),
    level: u32,
    rx: std::sync::mpsc::Receiver<Vec<(TileKey, DRect, egui::ColorImage)>>,
}

/// Whether missing tiles are composited off the UI thread. Tests composite in the frame, so they
/// see the result at once.
const ASYNC_TILES: bool = cfg!(all(not(test), any(not(target_arch = "wasm32"), target_feature = "atomics")));

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

    /// Document `doc` reduced to `level` (cached for the document state).
    fn reduced(&self, doc: &Arc<Document>, level: u32) -> Arc<Document> {
        reduced(&self.levels, doc, level)
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
    let mut l = levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((_, _, r)) = l.docs.iter().find(|(lv, w, _)| *lv == level && w.upgrade().is_some_and(|d| Arc::ptr_eq(&d, doc))) {
        return r.clone();
    }
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
enum Job {
    /// The whole tile (with gutter).
    Full(TileKey, DRect),
    /// Part of an existing tile's texture: level pixels.
    Part(TileKey, DRect),
}

/// Draw document `idx`'s CPU canvas: tiles in view at the zoom's level, composited as needed.
/// `zoom` is device pixels per document pixel (`View::zoom`).
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
    let Some(target) = crate::canvas::cpu_target(app, idx, output) else { return };
    let crate::canvas::CpuTarget { mut doc, revision, preview_key, display, display_key, damage, id } = target;
    let size = [doc.size.width, doc.size.height];
    let level = level_for(zoom, size);
    let key = crate::canvas::cache_key(id, output);
    let move_plan = crate::move_ui::cpu_plan(app, idx, &doc);
    // A lazy Move preview (`move_ui`: the document as it is, the canvas shifting the moving layers
    // itself) that the split can't draw this frame: shifted here, as `move_ui` would have.
    let mut lazy = move_plan.as_ref().is_some_and(|p| Arc::ptr_eq(&p.base, &doc) && p.offset != (0, 0));
    let shift = |doc: &mut Arc<Document>, lazy: &mut bool| {
        if std::mem::take(lazy)
            && let Some(p) = move_plan.as_ref()
            && let Ok(d) = photocraft_engine::layer_multi_cmds::moved(&p.base, &p.ids, p.offset.0, p.offset.1)
        {
            *doc = Arc::new(d);
        }
    };
    let tex_budget = texture_budget(app);
    let mip_budget = mip_budget(app);
    let cache = app.canvases.entry(key).or_default();
    let tc = cache.tiles.get_or_insert_with(Default::default);
    tc.frame += 1;
    tc.level = level;
    let frame = tc.frame;
    if tc.size != size {
        tc.tiles.clear();
        tc.size = size;
        tc.shown = None;
    }
    if move_plan.is_none() {
        tc.move_split = None;
    }
    let now = (revision, preview_key ^ display_key);
    if tc.shown != Some(now) {
        tc.invalidate(if tc.shown.is_some() { damage } else { None });
        tc.shown = Some(now);
    }
    // Tiles the worker pool finished for this state: in place of what stood in for them.
    if let Some(p) = &tc.pending {
        match p.rx.try_recv() {
            Ok(done) => {
                if p.state == now && p.level == level {
                    for (k, held, img) in done {
                        if tc.tiles.get(&k).is_some_and(|t| t.dirty.is_none() && t.held == held) {
                            continue;
                        }
                        let tex = ctx.load_texture(format!("cpu-tile-{}-{}-{}-{}-{}", id.0, key.1, k.level, k.tx, k.ty), img, TextureOptions::LINEAR);
                        tc.tiles.insert(k, TileTex { tex, held, dirty: None, used: frame });
                    }
                }
                tc.pending = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => tc.pending = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }
    // Work for the tiles in view.
    let want = tiles_in(visible.intersect(&doc.bounds()), level, size);
    let mut stale = Vec::new();
    let mut missing = Vec::new();
    let f = 1i64 << level;
    let bounds = level_bounds(size, f);
    for &k in &want {
        let own = tile_rect(k, size);
        let held = own.inflate(GUTTER).intersect(&bounds);
        match tc.tiles.get(&k) {
            None => missing.push(Job::Full(k, held)),
            Some(t) if t.held != held => stale.push(Job::Full(k, held)),
            Some(t) => {
                if let Some(d) = t.dirty {
                    let part = level_rect(d, f).intersect(&t.held);
                    if part.is_empty() {
                        stale.push(Job::Part(k, part));
                    } else if u64::from(part.width()) * u64::from(part.height()) * 2 > u64::from(held.width()) * u64::from(held.height()) {
                        stale.push(Job::Full(k, held));
                    } else {
                        stale.push(Job::Part(k, part));
                    }
                }
            }
        }
    }
    let t0 = crate::gpu_canvas::now_ms();
    // The document to composite the level from (`None`: reduce the full-resolution composite),
    // found when a tile needs drawing.
    let mut source: Option<Option<Arc<Document>>> = None;
    // Changed tiles first, all of them (an edit shows at once); then missing tiles while the frame
    // has time (at least one batch).
    let mut px = 0u64;
    let mut batches = vec![stale];
    let batch = rayon_batch();
    let mut rest = missing.into_iter().peekable();
    let mut first = true;
    let mut composite_ms = 0.0;
    let mut upload_ms = 0.0;
    let mut move_split_frames = 0u64;
    loop {
        let jobs = match batches.pop() {
            Some(j) => j,
            None => {
                if ASYNC_TILES || rest.peek().is_none() || (!first && crate::gpu_canvas::now_ms() - t0 > FRAME_BUDGET_MS) {
                    break;
                }
                first = false;
                rest.by_ref().take(batch).collect()
            }
        };
        if jobs.is_empty() {
            continue;
        }
        let rects: Vec<DRect> = jobs
            .iter()
            .map(|j| match j {
                Job::Full(_, r) | Job::Part(_, r) => *r,
            })
            .collect();
        let c0 = crate::gpu_canvas::now_ms();
        let faithful = level == 0 || photocraft_compose::proxy::lod_faithful(&doc);
        // A Move drag: only the moving layers are re-blended over cached composites (`move_cpu`).
        let mut split_bufs = None;
        if let Some(plan) = move_plan.as_ref().filter(|_| faithful) {
            let held: Vec<(DRect, DRect)> = jobs
                .iter()
                .map(|j| match j {
                    Job::Full(_, r) => (*r, *r),
                    Job::Part(k, r) => (*r, tc.tiles.get(k).map_or(*r, |t| t.held)),
                })
                .collect();
            let levels = tc.levels.clone();
            let mut reduce = |d: &Document| {
                if level == 0 {
                    return d.clone();
                }
                let mut l = levels.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                l.mips.document(d, level)
            };
            if !tc.move_split.as_ref().is_some_and(|s| s.is_for(plan, level)) {
                tc.move_split = crate::move_cpu::Split::new(plan, level, &mut reduce);
            }
            split_bufs = tc.move_split.as_mut().and_then(|s| s.render(plan.offset, &held));
            if split_bufs.is_some() {
                move_split_frames += 1;
            }
        }
        let bufs = match split_bufs {
            Some(b) => b,
            None => {
                if lazy {
                    shift(&mut doc, &mut lazy);
                    source = None;
                }
                let source = source.get_or_insert_with(|| faithful.then(|| tc.reduced(&doc, level)));
                render_level(&doc, source.as_deref(), f, &rects)
            }
        };
        let c1 = crate::gpu_canvas::now_ms();
        composite_ms += c1 - c0;
        for (job, buf) in jobs.into_iter().zip(bufs) {
            px += u64::from(buf.rect.width()) * u64::from(buf.rect.height());
            match job {
                Job::Full(k, held) => {
                    let img = tile_image(display.as_deref(), &buf);
                    match tc.tiles.get_mut(&k) {
                        Some(t) if t.tex.size() == img.size => {
                            t.tex.set(img, TextureOptions::LINEAR);
                            t.held = held;
                            t.dirty = None;
                        }
                        _ => {
                            let tex = ctx.load_texture(format!("cpu-tile-{}-{}-{}-{}-{}", id.0, key.1, k.level, k.tx, k.ty), img, TextureOptions::LINEAR);
                            tc.tiles.insert(k, TileTex { tex, held, dirty: None, used: frame });
                        }
                    }
                }
                Job::Part(k, part) => {
                    if let Some(t) = tc.tiles.get_mut(&k) {
                        if !part.is_empty() {
                            let img = tile_image(display.as_deref(), &buf);
                            t.tex.set_partial([(part.x0 - t.held.x0) as usize, (part.y0 - t.held.y0) as usize], img, TextureOptions::LINEAR);
                        }
                        t.dirty = None;
                    }
                }
            }
        }
        upload_ms += crate::gpu_canvas::now_ms() - c1;
    }
    // Missing tiles left: composited on the worker pool while the frame goes on.
    if lazy && rest.peek().is_some() {
        // Missing tiles go to the worker pool with the document as shown.
        shift(&mut doc, &mut lazy);
    }
    let split_ready = move_plan.as_ref().is_some_and(|p| tc.move_split.as_ref().is_some_and(|s| s.is_for(p, level)));
    if ASYNC_TILES && rest.peek().is_some() && tc.pending.is_none() {
        let jobs: Vec<(TileKey, DRect)> = rest
            .by_ref()
            .take(batch * 2)
            .filter_map(|j| match j {
                Job::Full(k, held) => Some((k, held)),
                Job::Part(..) => None,
            })
            .collect();
        // A level not built yet for this document is reduced on the worker too.
        let faithful = level == 0 || photocraft_compose::proxy::lod_faithful(&doc);
        let ready = source.clone().flatten();
        let (tx, rx) = std::sync::mpsc::channel();
        tc.pending = Some(Pending { state: now, level, rx });
        let (doc, display, levels) = (doc.clone(), display.clone(), tc.levels.clone());
        spawn(move || {
            let source = if faithful { Some(ready.unwrap_or_else(|| reduced(&levels, &doc, level))) } else { None };
            let rects: Vec<DRect> = jobs.iter().map(|(_, r)| *r).collect();
            let bufs = render_level(&doc, source.as_deref(), f, &rects);
            let done = jobs.into_iter().zip(bufs).map(|((k, held), b)| (k, held, tile_image(display.as_deref(), &b))).collect();
            let _ = tx.send(done);
        });
    }
    // Look for the worker pool's tiles in a moment (the egui context can't be woken from a
    // worker on the web); without it, the next batch at once.
    if tc.pending.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(20));
    }
    let more = !ASYNC_TILES && rest.peek().is_some();
    if px > 0 {
        app_perf(app, key, px, composite_ms, upload_ms);
    }
    app.perf_counters.move_split += move_split_frames.min(1);
    app.cpu_move_split = split_ready.then_some((id, revision, app.frame));
    let Some(tc) = app.canvases.get_mut(&key).and_then(|c| c.tiles.as_mut()) else { return };
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
        && let Ok(mut l) = tc.levels.try_lock()
    {
        l.mips.trim(mip_budget);
    }
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

fn app_perf(app: &mut PhotocraftApp, _key: (photocraft_doc::DocId, u32), px: u64, composite_ms: f64, upload_ms: f64) {
    app.perf.record("tiles", px, composite_ms, upload_ms);
}

/// Run `f` on the worker pool.
#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
fn spawn(f: impl FnOnce() + Send + 'static) {
    rayon::spawn(f);
}

#[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
fn spawn(f: impl FnOnce() + Send + 'static) {
    f();
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
        // An edit with no damage rectangle redraws every tile in view, whole.
        app.session.active_mut().unwrap().last_damage = None;
        app.session.active_mut().unwrap().revision += 1;
        let delta = frame(&mut app, &ctx, 1.0, [600.0, 450.0]);
        assert_eq!(delta.set.len(), before);
        assert!(delta.set.iter().all(|(_, d)| d.iter().all(|d| d.pos.is_none())));
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
            let source = (tc.level > 0).then(|| tc.reduced(&doc, tc.level));
            for (k, t) in tc.tiles.iter().filter(|(k, _)| k.level == tc.level) {
                let (w, h, got) = &mirror[&t.tex.id()];
                assert_eq!((*w, *h), (t.held.width() as usize, t.held.height() as usize));
                let want = render_level(&doc, source.as_deref(), f, &[t.held]).remove(0);
                let want = tile_image(None, &want);
                let want: Vec<[u8; 4]> = want.pixels.iter().map(|c| c.to_array()).collect();
                let bad = got.iter().zip(&want).position(|(a, b)| a != b);
                assert!(bad.is_none(), "zoom {zoom} tile {k:?} held {:?}: first wrong pixel at {:?}: {:?} vs {:?}", t.held, bad.map(|i| (i % w, i / w)), bad.map(|i| got[i]), bad.map(|i| want[i]));
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
}
