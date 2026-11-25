//! Move drags on the GPU canvas from caches made ahead (the drag cache).
//!
//! While a Move drag goes on, nothing in the document changes but where the moving layers are.
//! So, as Photoshop, Affinity or Krita do, the canvas keeps what doesn't move as images made once:
//!
//! - **below**: the layers under the moving ones, composited (the view's part, at its level);
//! - **sprite**: the moving run (the layer or group, with its effects and clipped layers)
//!   composited alone, over its whole extent (also past the canvas edges: a drag brings those
//!   pixels into view), at the same level. A run that composites as its own image laid over the
//!   rest ([`photocraft_compose::composites_as_over`]) is one image; any other that changes an
//!   opaque backdrop affinely (layer effects, type, a blend mode, clipped layers: Outer Glow,
//!   Gradient Overlay, Stroke, Drop Shadow...) is kept as its composite over opaque black and
//!   over opaque white, laid over the below as the above is;
//! - **above**: the layers over the moving ones composited over opaque black (`k`) and over opaque
//!   white (`w`). For layers that change an opaque backdrop affinely
//!   ([`photocraft_compose::composites_affinely`]: Normal, Multiply and Screen layers, their
//!   effects, masks, clipping and groups) a pixel `c` under them comes out as `k + (w - k) * c`,
//!   as on the CPU canvas (`move_cpu`).
//!
//! The run's effects, smart filters and type are thus drawn once per drag, not per frame: what
//! only moves is shifted (`moves_with_layer`: effects that follow the layer). A drag frame is one
//! draw per display tile ([`DRAG_WGSL`]); no plan, no layer pages, no allocations but the frame's
//! command buffer, and the Move preview is the document as it is (no moved copy per frame).
//! Those images are made ahead, off the drag's frames: while the Move tool is current and the
//! canvas is idle, for the layer a press would move (the one under the pointer with Auto-Select,
//! else the selected ones), pieces sized to what is left of the frame's budget
//! ([`GpuCanvas::prepare_drag`]). When a drag ends the cache is rebased on the moved document
//! ([`GpuCanvas::rebase_drag`]): the release frame and a drag of the same layers again come from
//! it at once, and the refinement composites the release's region exactly once the edits pause.
//! A drag that starts before its images are ready draws its frames whole, at the view's level
//! (as sharp as any frame: what doesn't move is never shown at a lower resolution), what is left
//! of a short frame going to the images, and switches to them once they are.
//!
//! When the run is composited every frame instead (it doesn't look the same wherever it goes, or
//! its blend isn't affine) only the above is kept: the frame composites the layers up to the
//! moving ones, the run drawn in place, and lays it over (`layered`). Where neither can be exact
//! the frame is composited whole:
//!
//! - a layer above (or in the run, without the layered path) that doesn't composite affinely
//!   (Overlay, Difference, adjustments, Blend If, knockout, Bevel and Emboss...);
//! - what lies under them might not be opaque everywhere (no opaque Background under the moving
//!   layers, a knockout): over a transparent pixel they composite differently;
//! - Lab, CMYK, multichannel or 32-bit documents, spot channels, artboards, linear documents the
//!   canvas stores sRGB-encoded; effects anchored to the canvas (a Gradient Overlay not aligned
//!   with the layer, a pattern not linked to it) or a fill layer in the run;
//! - the images don't fit the canvas budget or a texture side.
//!
//! The images are at the view's level of detail (screen resolution, not the document's), counted
//! in the canvas budget (`Resources::canvas_bytes`, the first thing `make_room` lets go of): half
//! floats, 8 bits on devices of 4 GB or less. At a level above 0 the offset is rounded to the
//! level's pixels, as on the CPU canvas; type edges are approximated in the above, as there. The
//! release composites the moved document exactly.

use photocraft_doc::{Document, Layer, LayerId};
use photocraft_geom::Rect;

use super::{CanvasLookup, FORMAT, FORMAT_HIGH, GpuCanvas, Resources, area, now_ms, texel_bytes, wgpu};

/// One cached image: a texture covering `rect` (level pixels, in its document's coordinates).
pub(super) struct Map {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    rect: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Below,
    Sprite,
    /// The moving run over opaque black and over opaque white ([`DragMode::Affine`]).
    SpriteBlack,
    SpriteWhite,
    Black,
    White,
}

/// How a drag frame uses the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragMode {
    /// One draw: sprite over below, the above laid over.
    Sprite,
    /// One draw: the moving run kept as its composite over black and over white (layers with
    /// effects, type, clipped layers: anything that changes an opaque backdrop affinely) laid over
    /// the below as the above is, `k + (w - k) * c`.
    Affine,
    /// The layers up to the moving ones composited, the above laid over.
    Layered,
}

/// The images of one drag (see the module docs).
pub(super) struct DragCache {
    doc: u64,
    level: u32,
    format: wgpu::TextureFormat,
    /// What the view shows (level pixels): the below, the above and the layered composite.
    view: Rect,
    /// The moving layers, and where their run starts and ends in `doc.layers`.
    ids: Vec<LayerId>,
    root: usize,
    top: usize,
    mode: DragMode,
    /// The document as the images show it (its layers, and the rest without them).
    layers: Vec<Layer>,
    shell: Document,
    /// Where the sprite's document starts (document pixels, a multiple of the level's pixel).
    sprite_at: (i32, i32),
    below: Option<Map>,
    sprite: Option<Map>,
    sprite_black: Option<Map>,
    sprite_white: Option<Map>,
    black: Option<Map>,
    white: Option<Map>,
    /// The layered mode's per-frame composite.
    lower: Option<Map>,
    /// Pieces still to draw (level pixels of their map's document).
    todo: Vec<(Kind, Rect)>,
    /// Per display tile (its rect): the draw's uniforms and bindings, made once.
    binds: std::collections::HashMap<[u32; 4], (wgpu::Buffer, wgpu::BindGroup)>,
}

impl DragCache {
    /// The document they belong to.
    pub(super) fn doc(&self) -> u64 {
        self.doc
    }

    pub(super) fn bytes(&self) -> u64 {
        let b = texel_bytes(self.format);
        [&self.below, &self.sprite, &self.sprite_black, &self.sprite_white, &self.black, &self.white, &self.lower].iter().filter_map(|m| m.as_ref()).map(|m| area(m.rect) * b).fold(0, u64::saturating_add)
    }

    fn ready(&self) -> bool {
        self.todo.is_empty()
    }

    pub(super) fn is_ready(&self) -> bool {
        self.ready()
    }

    /// Ready for `ids` moving on `doc` as it is, at `level` (whatever the view).
    pub(super) fn ready_for(&self, doc: &Document, ids: &[LayerId], level: u32) -> bool {
        self.ready() && self.level == level && self.doc == doc.id.0 && self.ids == ids && self.layers == doc.layers
    }

    /// Whether the images show `doc` (with `ids` moving) for a view of `view` at `level`.
    fn shows(&self, doc: &Document, ids: &[LayerId], level: u32, view: Rect) -> bool {
        self.doc == doc.id.0
            && self.level == level
            && self.ids == ids
            && self.view.intersect(&view) == view
            && self.layers == doc.layers
            && self.shell == shell_of(doc)
    }

    fn map(&self, k: Kind) -> Option<&Map> {
        match k {
            Kind::Below => self.below.as_ref(),
            Kind::Sprite => self.sprite.as_ref(),
            Kind::SpriteBlack => self.sprite_black.as_ref(),
            Kind::SpriteWhite => self.sprite_white.as_ref(),
            Kind::Black => self.black.as_ref(),
            Kind::White => self.white.as_ref(),
        }
    }

    /// The document `k`'s map composites. Its id is its own, derived from the document's and
    /// the same every time: the layers it shares with the document keep their pages and effect
    /// maps in the compositor (they are the same layers), and nothing is forgotten when it is
    /// done. A fresh id per build, forgotten after, dropped the shared layers' pages and effect
    /// maps with it, so every build uploaded every layer and drew every effect again (the
    /// 216-310 ms `gpu.composite_above` freezes of 0.13.30 on an RX 570).
    fn source(&self, k: Kind, doc: &Document) -> Option<Document> {
        let mut d = doc.clone();
        d.id = photocraft_doc::DocId(doc.id.0 ^ (0x9e37_79b9_7f4a_7c15u64.wrapping_mul(k as u64 + 1)));
        match k {
            Kind::Below => d.layers.truncate(self.root),
            Kind::Sprite | Kind::SpriteBlack | Kind::SpriteWhite => {
                d.layers = doc.layers.get(self.root..self.top)?.to_vec();
                let rect = self.map(k)?.rect;
                let (x, y) = self.sprite_at;
                let roots: Vec<LayerId> = d.layers.iter().map(|l| l.id).collect();
                d = photocraft_engine::layer_multi_cmds::moved(&d, &roots, -x, -y).ok()?;
                d.size = photocraft_geom::Size::new((rect.width() << self.level).max(1), (rect.height() << self.level).max(1));
                if k != Kind::Sprite {
                    let v = if k == Kind::SpriteBlack { 0.0 } else { 1.0 };
                    let backdrop = Layer::new("backdrop", photocraft_doc::LayerContent::Fill(photocraft_doc::Fill::Solid(photocraft_color::Color::rgb(v, v, v))));
                    d.layers.insert(0, backdrop);
                }
            }
            Kind::Black | Kind::White => {
                let v = if k == Kind::Black { 0.0 } else { 1.0 };
                let backdrop = Layer::new("backdrop", photocraft_doc::LayerContent::Fill(photocraft_doc::Fill::Solid(photocraft_color::Color::rgb(v, v, v))));
                d.layers = std::iter::once(backdrop).chain(doc.layers.get(self.top..)?.iter().cloned()).collect();
            }
        }
        Some(d)
    }
}

/// The document without its layers (and with no id): what the images depend on besides them.
fn shell_of(doc: &Document) -> Document {
    let mut s = doc.clone();
    s.layers = Vec::new();
    s.id = photocraft_doc::DocId(0);
    s
}

fn knockout_free(layers: &[Layer], depth: usize) -> bool {
    depth < 64 && layers.iter().all(|l| l.advanced.knockout == photocraft_doc::Knockout::None && l.children().is_none_or(|c| knockout_free(c, depth + 1)))
}

/// Whether `l` looks the same wherever it is moved (only shifted): its effects follow it (a
/// Gradient Overlay or gradient Stroke aligned with the layer, a pattern linked to it), and it is
/// no fill layer (those fill a frame of the canvas).
pub(crate) fn moves_with_layer(l: &Layer, depth: usize) -> bool {
    use photocraft_doc::{Effect, FxPaint, LayerContent};
    let gradient_ok = |p: &FxPaint| match p {
        FxPaint::Color(_) => true,
        FxPaint::Gradient(g) => g.align,
        FxPaint::Pattern { .. } => false,
    };
    let fx_ok = l.effects.items.iter().filter(|e| e.enabled()).all(|e| match e {
        Effect::GradientOverlay { gradient, .. } => gradient.align,
        Effect::PatternOverlay { link, .. } => *link,
        Effect::Stroke(s) => gradient_ok(&s.paint),
        _ => true,
    });
    depth < 64 && fx_ok && !matches!(l.content, LayerContent::Fill(_)) && l.children().is_none_or(|c| c.iter().all(|k| moves_with_layer(k, depth + 1)))
}

fn draws(l: &Layer, depth: usize) -> bool {
    l.visible && (depth < 64) && l.children().is_none_or(|c| c.iter().any(|k| draws(k, depth + 1)))
}

fn holds(l: &Layer, ids: &[LayerId], depth: usize) -> bool {
    ids.contains(&l.id) || (depth < 64 && l.children().is_some_and(|c| c.iter().any(|k| holds(k, ids, depth + 1))))
}

fn movable(l: &Layer, depth: usize) -> bool {
    depth < 64 && photocraft_gpu::plan::movable_in_place(l) && l.children().is_none_or(|c| c.iter().all(|k| movable(k, depth + 1)))
}

/// Whether `bg` (the Background) composites as an opaque layer would if its pixels are: plain,
/// visible, Normal at full opacity, no mask, effect or blending option.
fn plain_background(bg: &Layer, mode: photocraft_color::ColorMode) -> bool {
    bg.visible
        && bg.blend == photocraft_color::BlendMode::Normal
        && bg.opacity >= 1.0
        && bg.fill_opacity >= 1.0
        && !bg.clipped
        && !bg.mask.as_ref().is_some_and(|m| m.enabled)
        && !bg.vector_mask.as_ref().is_some_and(|m| m.enabled)
        && !photocraft_compose::effects::has_effects(bg)
        && !photocraft_compose::blend_if_active(bg, mode)
        && bg.excluded_channels == 0
        && bg.advanced.is_default()
}

/// Whether a Background (its id and surface fingerprint) is opaque everywhere on the canvas, read
/// a tile at a time over calls ([`OpaqueBg::step`]): a page decoded from a file has a tile of its
/// own everywhere, and reading all of an 8K page's alpha took 130 ms in the frame that started the
/// drag cache on the owner's PC (`drag.new`, the opening of a 7680x4320 page; 73 ms after a crop,
/// 24 ms on a 2000x3000 page).
pub(super) struct OpaqueBg {
    id: LayerId,
    print: u64,
    canvas: Rect,
    /// The answer, once every tile is read (or one isn't opaque).
    known: Option<bool>,
    /// The next tile to read, row by row over the canvas's tiles.
    next: usize,
    /// Tiles read whole (a tile shared by many places, as a solid fill's, is read once).
    seen: std::collections::HashSet<usize>,
}

impl OpaqueBg {
    fn new(id: LayerId, print: u64, canvas: Rect) -> Self {
        Self { id, print, canvas, known: None, next: 0, seen: Default::default() }
    }

    fn is_for(&self, id: LayerId, print: u64, canvas: Rect) -> bool {
        self.id == id && self.print == print && self.canvas == canvas
    }

    /// Read tiles of `bg` until the answer is known or `more` says the call's time is up (a tile
    /// at least per call): `None` until it is known.
    fn step(&mut self, bg: &Layer, mode: photocraft_color::ColorMode, more: &mut dyn FnMut() -> bool) -> Option<bool> {
        if self.known.is_none() {
            self.known = self.read(bg, mode, more);
        }
        self.known
    }

    fn read(&mut self, bg: &Layer, mode: photocraft_color::ColorMode, more: &mut dyn FnMut() -> bool) -> Option<bool> {
        let Some(s) = bg.surface().filter(|_| plain_background(bg, mode)) else { return Some(false) };
        let format = s.format();
        if !format.alpha {
            return Some(true);
        }
        let canvas = self.canvas;
        if canvas.is_empty() {
            return Some(true);
        }
        let n = format.channels().max(1);
        let default_opaque = s.default_pixel().last().is_some_and(|a| *a >= 1.0);
        let t = photocraft_geom::TILE_SIZE;
        let (tx0, ty0, tx1, ty1) = (canvas.x0.div_euclid(t), canvas.y0.div_euclid(t), (canvas.x1 - 1).div_euclid(t), (canvas.y1 - 1).div_euclid(t));
        let across = (tx1 - tx0 + 1) as usize;
        let count = across * (ty1 - ty0 + 1) as usize;
        while self.next < count {
            let i = self.next;
            self.next += 1;
            let c = photocraft_geom::TileCoord::new(tx0 + (i % across) as i32, ty0 + (i / across) as i32);
            let Some(tile) = s.tile(c) else {
                if default_opaque {
                    continue;
                }
                return Some(false);
            };
            let (cell, r) = (c.rect(), c.rect().intersect(&canvas));
            if r == cell && !self.seen.insert(std::sync::Arc::as_ptr(tile) as usize) {
                continue;
            }
            let bytes = tile.bytes();
            for y in r.y0..r.y1 {
                let row = ((y - cell.y0) * t) as usize;
                let (a, b) = ((row + (r.x0 - cell.x0) as usize) * n + n - 1, (row + (r.x1 - cell.x0) as usize) * n);
                let opaque = match format.sample {
                    // Most pages: the alpha bytes compared as they are stored.
                    photocraft_color::SampleType::U8 => bytes.get(a..b).is_some_and(|px| px.iter().step_by(n).all(|v| *v == u8::MAX)),
                    sample => (a..b).step_by(n).all(|i| photocraft_color::read_sample(bytes, sample, i) >= 1.0),
                };
                if !opaque {
                    return Some(false);
                }
            }
            if self.next < count && !more() {
                return None;
            }
        }
        Some(true)
    }
}

/// Where the sprite of the run `layers` starts (document pixels, on the level's grid) and its
/// rect (level pixels from there): the run's whole extent, past the canvas edges too.
fn sprite_extent(layers: &[Layer], level: u32) -> ((i32, i32), Rect) {
    let step = 1i32 << level;
    let all = Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
    let extent = layers.iter().filter_map(|l| photocraft_compose::change_bounds(l, all)).fold(Rect::EMPTY, |a, b| if a.is_empty() { b } else { a.union(&b) });
    let (sx, sy) = (extent.x0.div_euclid(step) * step, extent.y0.div_euclid(step) * step);
    ((sx, sy), Rect::new(0, 0, (extent.x1 - sx + step - 1) / step, (extent.y1 - sy + step - 1) / step))
}

/// Level pixels to document pixels.
fn unlevel(r: Rect, level: u32) -> Rect {
    Rect::new(r.x0 << level, r.y0 << level, r.x1 << level, r.y1 << level)
}

/// Rows of `r` of about `px` pixels each.
fn bands(r: Rect, px: u64) -> Vec<Rect> {
    let rows = (px / u64::from(r.width().max(1))).max(1) as i32;
    let mut out = Vec::new();
    let mut y = r.y0;
    while y < r.y1 {
        let y1 = y.saturating_add(rows).min(r.y1);
        out.push(Rect::new(r.x0, y, r.x1, y1));
        y = y1;
    }
    out
}

/// Pixels of one piece of a cache image (at most).
const PIECE_PX: u64 = 256 * 1024;

/// The smallest piece worth drawing (pixels).
const MIN_PIECE_PX: u64 = 64 * 1024;

static DRAG_CACHE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turn the drag cache off (or back on, the default): measurements compare both in one build.
pub fn set_above_cache(on: bool) {
    DRAG_CACHE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Drag frames drawn from the cache, by mode (tests and diagnostics).
static SPRITE_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static LAYERED_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Milliseconds from the last cache piece's submission to the GPU's completion (f64 bits, 0: none).
static PIECE_GPU_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long the last cache piece took from its submission to the GPU's completion, in
/// milliseconds (tests and diagnostics; `None`: not measured yet).
pub fn piece_gpu_ms() -> Option<f64> {
    Some(f64::from_bits(PIECE_GPU_MS.load(std::sync::atomic::Ordering::Relaxed))).filter(|v| *v > 0.0)
}

/// Drag frames drawn from the cache so far: (one-draw sprite frames, layered frames).
pub fn drag_frames() -> (u64, u64) {
    (SPRITE_FRAMES.load(std::sync::atomic::Ordering::Relaxed), LAYERED_FRAMES.load(std::sync::atomic::Ordering::Relaxed))
}

/// A drag plan that can't be told yet ([`GpuCanvas::drag_plan`]).
struct Pending;

impl GpuCanvas {
    /// How a drag of `ids` over `doc` can use a cache, and where the moving run starts and ends
    /// in `doc.layers` (see the module docs); `None` when it can't. [`Pending`] while it can't
    /// tell yet: the Background's tiles are read over calls, until `more` says no.
    fn drag_plan(res: &mut Resources, doc: &Document, ids: &[LayerId], encode_srgb: bool, more: &mut dyn FnMut() -> bool) -> Result<Option<(DragMode, usize, usize)>, Pending> {
        use photocraft_color::{ColorMode, SampleType};
        if ids.is_empty()
            || encode_srgb
            || !matches!(doc.mode, ColorMode::Rgb | ColorMode::Grayscale)
            || doc.depth == SampleType::F32
            || !doc.channels.is_empty()
            || doc.has_artboards()
            || !DRAG_CACHE.load(std::sync::atomic::Ordering::Relaxed)
        {
            return Ok(None);
        }
        let Some(root) = doc.layers.iter().position(|l| holds(l, ids, 0)) else { return Ok(None) };
        let mut top = root + 1;
        while doc.layers.get(top).is_some_and(|l| l.clipped) {
            top += 1;
        }
        let (Some(run), Some(above)) = (doc.layers.get(root..top), doc.layers.get(top..)) else { return Ok(None) };
        if doc.layers.get(root).is_none_or(|l| l.clipped) {
            return Ok(None);
        }
        let above_draws = above.iter().any(|l| draws(l, 0));
        // Whether what lies under the layers above (and under the run) is opaque everywhere.
        let mut opaque = None;
        let mut opaque_to = |res: &mut Resources| -> Result<bool, Pending> {
            if opaque.is_none() {
                opaque = if knockout_free(doc.layers.get(..top).unwrap_or_default(), 0) { Self::opaque_under(res, doc, root, more) } else { Some(false) };
            }
            opaque.ok_or(Pending)
        };
        let above_ok = !above_draws || (above.iter().all(|l| photocraft_compose::composites_affinely(l, doc.mode, true)) && opaque_to(res)?);
        if !above_ok {
            return Ok(None);
        }
        // The run kept as an image: it looks the same wherever it goes, and composites as its
        // own image laid over the rest (one sprite), or changes an opaque backdrop affinely (two).
        let shifts = run.iter().all(|l| moves_with_layer(l, 0));
        if shifts && top == root + 1 && run.iter().all(|l| photocraft_compose::composites_as_over(l, doc.mode)) {
            return Ok(Some((DragMode::Sprite, root, top)));
        }
        if shifts && run.iter().all(|l| photocraft_compose::composites_affinely(l, doc.mode, true)) && opaque_to(res)? {
            return Ok(Some((DragMode::Affine, root, top)));
        }
        // Else the run is composited every frame, from its pages at the offset (drawn in place),
        // with only the above kept.
        Ok((above_draws && run.iter().all(|l| movable(l, 0))).then_some((DragMode::Layered, root, top)))
    }

    /// Whether everything under `doc.layers[root..]` is opaque: an opaque Background below it.
    /// `None` while its tiles are still being read, over calls until `more` says no.
    fn opaque_under(res: &mut Resources, doc: &Document, root: usize, more: &mut dyn FnMut() -> bool) -> Option<bool> {
        let Some(bg) = doc.background().filter(|_| root > 0) else { return Some(false) };
        let Some(s) = bg.surface() else { return Some(false) };
        let print = photocraft_gpu::surface_fingerprint(s);
        if !res.opaque_bg.as_ref().is_some_and(|o| o.is_for(bg.id, print, doc.bounds())) {
            res.opaque_bg = Some(OpaqueBg::new(bg.id, print, doc.bounds()));
        }
        res.opaque_bg.as_mut()?.step(bg, doc.mode, more)
    }

    /// Make (or keep making) the drag cache for moving `ids` of `doc` in a view showing `visible`
    /// at `level`: pieces for up to `budget_ms` this call. Returns whether pieces are left (the
    /// caller asks for another frame).
    pub fn prepare_drag(&self, doc: &Document, ids: &[LayerId], visible: Rect, level: u32, encode_srgb: bool, budget_ms: f64) -> bool {
        let _s = crate::perf_span::span("canvas.drag_cache");
        let t0 = now_ms();
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let key = doc.id.0;
        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.canvas_mut(self.id) else { return false };
        let Some(d_level) = res.docs.get(&key).map(super::DocTextures::max_level) else { return false };
        let level = level.min(d_level).min(photocraft_gpu::MAX_LOD);
        let view = photocraft_gpu::level_rect(visible.intersect(&doc.bounds()), level).intersect(&photocraft_gpu::level_bounds(doc.bounds(), level));
        if view.is_empty() {
            return false;
        }
        if !res.drag.as_ref().is_some_and(|c| c.shows(doc, ids, level, view)) {
            res.drag = None;
            let _s = crate::perf_span::span("drag.new");
            // (Pending: the Background's opacity is still being read; the next call goes on.)
            let Some((mode, root, top)) = (match Self::drag_plan(res, doc, ids, encode_srgb, &mut || now_ms() - t0 < budget_ms) {
                Ok(plan) => plan,
                Err(Pending) => return true,
            }) else {
                return false;
            };
            let Some(cache) = self.new_cache(res, doc, ids, view, level, mode, root, top) else { return false };
            res.drag = Some(cache);
        }
        let Some(mut cache) = res.drag.take() else { return false };
        let mut comp = match self.take_compositor(res, false) {
            Ok(c) => c,
            Err(_) => {
                res.drag = Some(cache);
                return false;
            }
        };
        let mut sources: Vec<(Kind, Document)> = Vec::new();
        let mut failed = false;
        // Pieces sized to what is left of the budget, at the rate the last ones took (a piece
        // that wouldn't fit is cut to rows that do).
        let mut drawn = 0;
        while let Some(&(kind, piece)) = cache.todo.first() {
            let left = budget_ms - (now_ms() - t0);
            if left <= 0.0 {
                break;
            }
            // At least the smallest piece per call with budget left (else a rate measured on a
            // slow first piece, its pages uploaded, would never let another one go).
            let fits_px = if res.piece_rate > 0.0 { ((left / res.piece_rate).max(0.0) as u64).max(MIN_PIECE_PX) } else { PIECE_PX };
            if drawn > 0 && fits_px <= MIN_PIECE_PX && left < res.piece_rate * MIN_PIECE_PX as f64 {
                break;
            }
            drawn += 1;
            let piece = if area(piece) > fits_px {
                let rows = (fits_px / u64::from(piece.width().max(1))).max(1) as i32;
                let (head, tail) = (Rect::new(piece.x0, piece.y0, piece.x1, piece.y0 + rows), Rect::new(piece.x0, piece.y0 + rows, piece.x1, piece.y1));
                if let Some(first) = cache.todo.first_mut() {
                    *first = (kind, head);
                }
                cache.todo.insert(1, (kind, tail));
                head
            } else {
                piece
            };
            let src = match sources.iter().find(|(k, _)| *k == kind) {
                Some((_, d)) => d,
                None => {
                    let _s = crate::perf_span::span("drag.source");
                    let Some(d) = cache.source(kind, doc) else {
                        failed = true;
                        break;
                    };
                    sources.push((kind, d));
                    match sources.last() {
                        Some((_, d)) => d,
                        None => break,
                    }
                }
            };
            let Some(map) = cache.map(kind) else {
                failed = true;
                break;
            };
            // Its effect maps and layer pages first, within the budget: the piece is drawn on a
            // later call when they aren't all there yet (one piece built every styled layer's maps
            // and uploaded every layer's pages in one frame).
            let region = unlevel(piece, level).intersect(&src.bounds());
            let ready = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| comp.prepare(device, queue, src, region, level, &mut || now_ms() - t0 < budget_ms)));
            match ready {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => break,
                Ok(Err(_)) | Err(_) => {
                    failed = true;
                    break;
                }
            }
            // (The rate below is the render's: what was prepared is not drawn again.)
            let started = now_ms();
            let render_span = crate::perf_span::span("drag.render");
            let rendered = render_into(device, queue, res, &mut comp, src, region, level, map, cache.format);
            drop(render_span);
            if rendered.is_err() {
                failed = true;
                break;
            }
            let rate = (now_ms() - started) / area(piece).max(1) as f64;
            // The GPU's time for it too (on a GPU the work is queued, the CPU time is small): the
            // next pieces are sized by whichever is slower, so the map builds never queue up a
            // long hitch on the GPU either.
            let gpu_rate = f64::from_bits(res.piece_gpu_rate.load(std::sync::atomic::Ordering::Relaxed));
            let rate = rate.max(gpu_rate);
            res.piece_rate = if res.piece_rate > 0.0 { res.piece_rate * 0.6 + rate * 0.4 } else { rate };
            let (gpu_rate, px) = (res.piece_gpu_rate.clone(), area(piece).max(1) as f64);
            queue.on_submitted_work_done(move || {
                let ms = now_ms() - started;
                PIECE_GPU_MS.store(ms.to_bits(), std::sync::atomic::Ordering::Relaxed);
                gpu_rate.store((ms / px).to_bits(), std::sync::atomic::Ordering::Relaxed);
            });
            cache.todo.remove(0);
        }
        res.compositor = Some(comp);
        if failed {
            return false;
        }
        let more = !cache.ready();
        res.drag = Some(cache);
        more
    }

    /// A new, empty cache (its images allocated, every piece to draw), or `None` when it doesn't
    /// fit the budget or a texture side.
    #[allow(clippy::too_many_arguments)]
    fn new_cache(&self, res: &mut Resources, doc: &Document, ids: &[LayerId], view: Rect, level: u32, mode: DragMode, root: usize, top: usize) -> Option<DragCache> {
        let device = &self.rs.device;
        let side = device.limits().max_texture_dimension_2d;
        let small = super::physical_memory().is_some_and(|b| b <= super::LOW_MEMORY);
        let format = if res.encode_pipeline_high.is_some() && !small { FORMAT_HIGH } else { FORMAT };
        let above = doc.layers.get(top..)?.iter().any(|l| draws(l, 0));
        // The sprite: the moving unit's whole extent, on the level's grid.
        let ((sx, sy), sprite_rect) = sprite_extent(doc.layers.get(root..top)?, level);
        let fits = |r: Rect| !r.is_empty() && r.width() <= side && r.height() <= side;
        let mut want: Vec<(Kind, Rect)> = Vec::new();
        match mode {
            DragMode::Sprite => {
                if !fits(sprite_rect) || !fits(view) {
                    return None;
                }
                want.push((Kind::Below, view));
                want.push((Kind::Sprite, sprite_rect));
            }
            DragMode::Affine => {
                if !fits(sprite_rect) || !fits(view) {
                    return None;
                }
                want.push((Kind::Below, view));
                want.push((Kind::SpriteBlack, sprite_rect));
                want.push((Kind::SpriteWhite, sprite_rect));
            }
            DragMode::Layered => {
                if !fits(view) {
                    return None;
                }
            }
        }
        if above {
            want.push((Kind::Black, view));
            want.push((Kind::White, view));
        }
        let lower = mode == DragMode::Layered;
        let bytes = want.iter().map(|(_, r)| area(*r)).sum::<u64>().saturating_add(if lower { area(view) } else { 0 }) * texel_bytes(format);
        while res.canvas_bytes().saturating_add(bytes) > res.canvas_budget && res.drop_oldest_view_set() > 0 {}
        if res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
            return None;
        }
        let make = |rect: Rect| new_map(device, rect, format);
        let get = |k: Kind| want.iter().find(|(w, _)| *w == k).map(|(_, r)| make(*r));
        let todo = want.iter().flat_map(|(k, r)| bands(*r, PIECE_PX).into_iter().map(move |b| (*k, b))).collect();
        Some(DragCache {
            doc: doc.id.0,
            level,
            format,
            view,
            ids: ids.to_vec(),
            root,
            top,
            mode,
            layers: doc.layers.clone(),
            shell: shell_of(doc),
            sprite_at: (sx, sy),
            below: get(Kind::Below),
            sprite: get(Kind::Sprite),
            sprite_black: get(Kind::SpriteBlack),
            sprite_white: get(Kind::SpriteWhite),
            black: get(Kind::Black),
            white: get(Kind::White),
            lower: lower.then(|| make(view)),
            todo,
            binds: Default::default(),
        })
    }

    /// A Move drag frame of `doc` over `region` (document pixels, what the view shows) at
    /// `level`, from the drag cache when it is ready for it; `None` otherwise (the caller
    /// composites the frame whole; nothing was written then).
    #[allow(clippy::too_many_arguments)]
    pub(super) fn drag_frame(&self, doc: &Document, region: Rect, level: u32, encode_srgb: bool, ids: &[LayerId], offset: (i32, i32)) -> Option<photocraft_gpu::Stats> {
        let _s = crate::perf_span::span("gpu.drag_frame");
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let key = doc.id.0;
        let mut renderer = self.rs.renderer.write();
        let res = renderer.callback_resources.canvas_mut(self.id)?;
        let d_level = res.docs.get(&key)?.max_level();
        let level = level.min(d_level).min(photocraft_gpu::MAX_LOD);
        let region = region.intersect(&doc.bounds());
        let lregion = photocraft_gpu::level_rect(region, level).intersect(&photocraft_gpu::level_bounds(doc.bounds(), level));
        if lregion.is_empty() || !res.drag.as_ref().is_some_and(|c| c.ready() && c.shows(doc, ids, level, lregion)) {
            return None;
        }
        let mut cache = res.drag.take()?;
        let written = match cache.mode {
            DragMode::Sprite | DragMode::Affine => draw_sprite(device, queue, res, key, &mut cache, lregion, offset, encode_srgb),
            DragMode::Layered => self.draw_layered(res, doc, &cache, region, encode_srgb),
        };
        let mode = cache.mode;
        res.drag = Some(cache);
        if !written {
            return None;
        }
        let res: &Resources = res;
        let _mips = crate::perf_span::span("drag.mips");
        if let Some(d) = res.docs.get(&key) {
            d.regenerate_mips_from(device, queue, res, [region.x0 as u32, region.y0 as u32, region.x1 as u32, region.y1 as u32], level);
        }
        self.pace.submitted(queue);
        let counter = if mode == DragMode::Layered { &LAYERED_FRAMES } else { &SPRITE_FRAMES };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(photocraft_gpu::Stats { level, pixels: area(lregion), ..Default::default() })
    }

    /// A drag of `ids` on document `doc` (key) ended moved by `offset` (`doc` is the document
    /// after it): when the cache shows the document before it, it shows `doc` too, its run only
    /// shifted (`moves_with_layer`); it is kept for `doc` (the release frame, and a drag of the
    /// same layers again, come from it at once). A cache still being made is kept too (what is
    /// drawn of it stays right): it goes on in the next idle frames. Whether it was kept.
    pub(super) fn rebase_drag(&self, doc: &Document, ids: &[LayerId], offset: (i32, i32)) -> bool {
        let mut renderer = self.rs.renderer.write();
        let Some(res) = renderer.callback_resources.canvas_mut(self.id) else { return false };
        let Some(c) = res.drag.as_mut().filter(|c| c.doc == doc.id.0 && c.ids == ids) else { return false };
        let step = 1i32 << c.level;
        let same_shape = c.layers.len() == doc.layers.len() && c.shell == shell_of(doc);
        let unmoved = |r: std::ops::Range<usize>| c.layers.get(r.clone()).zip(doc.layers.get(r)).is_some_and(|(a, b)| a == b);
        let shifted = c.layers.get(c.root..c.top).zip(doc.layers.get(c.root..c.top)).is_some_and(|(was, now)| {
            was.iter().zip(now).all(|(a, b)| {
                let all = Rect::new(i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4);
                a.id == b.id && photocraft_compose::change_bounds(a, all).map(|r| r.translate(offset.0, offset.1)) == photocraft_compose::change_bounds(b, all)
            })
        });
        let ok = offset.0 % step == 0
            && offset.1 % step == 0
            && c.mode != DragMode::Layered
            && same_shape
            && unmoved(0..c.root)
            && unmoved(c.top..c.layers.len())
            && shifted;
        if ok {
            c.layers = doc.layers.clone();
            c.sprite_at = (c.sprite_at.0 + offset.0, c.sprite_at.1 + offset.1);
            c.binds.clear();
            return true;
        }
        // The run looks different where it landed (a shape cut at the canvas edge renders again,
        // its effects with it): the images of what didn't move stay, and only the run's are made
        // again (a piece per idle frame, as ahead), so a drag of it again doesn't wait for the
        // whole cache (2-3 s of layers below and above on a 4000x6000 page of 80 layers).
        let keep = c.mode != DragMode::Layered && same_shape && unmoved(0..c.root) && unmoved(c.top..c.layers.len());
        let remade = keep && doc.layers.get(c.root..c.top).is_some_and(|run| {
            let (at, rect) = sprite_extent(run, c.level);
            let side = self.rs.device.limits().max_texture_dimension_2d;
            if rect.is_empty() || rect.width() > side || rect.height() > side {
                return false;
            }
            let (device, format) = (&self.rs.device, c.format);
            // New images: a render leaves what it skips (nothing there) as it was.
            for m in [&mut c.sprite, &mut c.sprite_black, &mut c.sprite_white] {
                if m.is_some() {
                    *m = Some(new_map(device, rect, format));
                }
            }
            c.sprite_at = at;
            c.layers = doc.layers.clone();
            c.binds.clear();
            let kinds = [Kind::Sprite, Kind::SpriteBlack, Kind::SpriteWhite];
            c.todo.retain(|(k, _)| !kinds.contains(k));
            let held: Vec<Kind> = kinds.into_iter().filter(|k| c.map(*k).is_some()).collect();
            for k in held {
                c.todo.extend(bands(rect, PIECE_PX).into_iter().map(|b| (k, b)));
            }
            true
        });
        if !remade || res.canvas_bytes() > res.canvas_budget {
            res.drag = None;
        }
        false
    }

    /// The frame of a release (no drag on `doc` any more): when the drag that ended can be
    /// rebased on `doc` ([`GpuCanvas::rebase_drag`]), drawn from its cache at offset 0, its region
    /// left to the refinement for an exact composite once the edits pause (`Stale::approx`).
    pub(super) fn release_frame(&self, doc: &Document, region: Rect, level: u32, encode_srgb: bool) -> Option<photocraft_gpu::Stats> {
        let (ids, offset) = {
            let mut r = self.rs.renderer.write();
            let res = r.callback_resources.canvas_mut(self.id)?;
            if !res.released.as_ref().is_some_and(|(k, _, _)| *k == doc.id.0) {
                return None;
            }
            let (_, ids, offset) = res.released.take()?;
            (ids, offset)
        };
        if !self.rebase_drag(doc, &ids, offset) {
            return None;
        }
        let st = self.drag_frame(doc, region, level, encode_srgb, &ids, (0, 0))?;
        self.with_doc(doc.id.0, |res, d| {
            d.stale().approx(region.intersect(&doc.bounds()));
            res.reduced_at = super::now_ms();
        });
        Some(st)
    }

    /// The layered mode's frame: the layers up to the moving ones composited (with the
    /// document's own lower-stack cache) into the cache's `lower`, the above laid over it.
    fn draw_layered(&self, res: &mut Resources, doc: &Document, cache: &DragCache, region: Rect, encode_srgb: bool) -> bool {
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let (Some(lower_map), Some(_), Some(_)) = (&cache.lower, &cache.black, &cache.white) else { return false };
        let mut lower = doc.clone();
        lower.layers.truncate(cache.top);
        let Ok(mut comp) = self.take_compositor(res, true) else { return false };
        let ok = render_into(device, queue, res, &mut comp, &lower, region, cache.level, lower_map, cache.format).is_ok();
        res.compositor = Some(comp);
        ok && apply_layered(device, queue, res, doc.id.0, cache, photocraft_gpu::level_rect(region, cache.level).intersect(&cache.view), encode_srgb)
    }
}

/// An empty cache image covering `rect`.
fn new_map(device: &wgpu::Device, rect: Rect, format: wgpu::TextureFormat) -> Map {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pc_drag_cache"),
        size: wgpu::Extent3d { width: rect.width(), height: rect.height(), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = t.create_view(&Default::default());
    Map { _texture: t, view, rect }
}

/// Composite `region` (document pixels) of `doc` at `level` with `comp` into `map`, stored
/// premultiplied as composited (no clamp to 1, no sRGB encoding).
#[allow(clippy::too_many_arguments)]
fn render_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    res: &Resources,
    comp: &mut photocraft_gpu::Compositor,
    doc: &Document,
    region: Rect,
    level: u32,
    map: &Map,
    format: wgpu::TextureFormat,
) -> Result<photocraft_gpu::Stats, photocraft_gpu::Unsupported> {
    let pipe = res.encode_pipeline(format).ok_or_else(|| photocraft_gpu::Unsupported("no cache pipeline".into()))?;
    let bgl = &res.encode_bgl;
    let rect = map.rect;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        comp.render_lod(device, queue, doc, region, level, |enc, out| {
            let r = out.rect.intersect(&rect);
            if r.is_empty() {
                return;
            }
            let offset = [out.rect.x0 - rect.x0 - out.origin[0] as i32, out.rect.y0 - rect.y0 - out.origin[1] as i32, 0, 1];
            let ubuf = super::init_buffer(device, "pc_drag_store", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_drag_store"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(out.view) },
                    wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
                ],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pc_drag_store"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &map.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_scissor_rect((r.x0 - rect.x0) as u32, (r.y0 - rect.y0) as u32, r.width(), r.height());
            pass.draw(0..3, 0..1);
        })
    }));
    result.unwrap_or_else(|_| Err(photocraft_gpu::Unsupported("the GPU compositor stopped (internal error)".into())))
}

/// One frame of the sprite mode into document `key`'s display tiles over `lregion` (level
/// pixels): per tile, its uniforms written and one draw. Whether it was written.
#[allow(clippy::too_many_arguments)]
fn draw_sprite(device: &wgpu::Device, queue: &wgpu::Queue, res: &Resources, key: u64, cache: &mut DragCache, lregion: Rect, offset: (i32, i32), encode_srgb: bool) -> bool {
    let Some(d) = res.docs.get(&key) else { return false };
    let Some(pipe) = res.drag_pipeline(d.format) else { return false };
    let affine = cache.mode == DragMode::Affine;
    let (Some(below), Some(sprite), Some(sprite2)) = (
        &cache.below,
        if affine { &cache.sprite_black } else { &cache.sprite },
        if affine { &cache.sprite_white } else { &cache.sprite },
    ) else {
        return false;
    };
    let level = cache.level;
    let keep_hdr = i32::from(d.format == FORMAT_HIGH);
    let above = cache.black.is_some() && cache.white.is_some();
    // The offset in level pixels (rounded to the level's grid above level 0).
    let lv = |v: i32| (f64::from(v) / f64::from(1i32 << level)).round() as i32;
    let (ox, oy) = (lv(offset.0), lv(offset.1));
    let (sx, sy) = (cache.sprite_at.0 >> level, cache.sprite_at.1 >> level);
    let view = cache.view;
    let record = crate::perf_span::span("drag.record");
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_drag_frame") });
    let tiles: Vec<_> = d
        .tiles
        .iter()
        .filter_map(|t| {
            let target = t.levels.get(level as usize)?;
            let [tx, ty, tw, th] = [t.rect[0] >> level, t.rect[1] >> level, (t.rect[2] >> level).max(1), (t.rect[3] >> level).max(1)].map(|v| v as i32);
            let r = lregion.intersect(&Rect::from_xywh(tx, ty, tw as u32, th as u32));
            (!r.is_empty()).then_some((t, target, tx, ty, r))
        })
        .collect();
    for (t, target, tx, ty, r) in tiles {
        let words: [i32; 8] = [tx - view.x0, ty - view.y0, i32::from(encode_srgb), keep_hdr, tx - sx - ox, ty - sy - oy, i32::from(above), i32::from(affine)];
        let bytes: Vec<u8> = words.iter().flat_map(|v| v.to_le_bytes()).collect();
        let entry = cache.binds.entry(t.rect).or_insert_with(|| {
            let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("pc_drag_frame"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let k = cache.black.as_ref().map_or(&below.view, |m| &m.view);
            let w = cache.white.as_ref().map_or(&below.view, |m| &m.view);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_drag_frame"),
                layout: &res.drag_bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&below.view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&sprite.view) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&sprite2.view) },
                    wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::TextureView(k) },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(w) },
                    wgpu::BindGroupEntry { binding: 5, resource: ubuf.as_entire_binding() },
                ],
            });
            (ubuf, bg)
        });
        queue.write_buffer(&entry.0, 0, &bytes);
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pc_drag_frame"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &entry.1, &[]);
        pass.set_scissor_rect((r.x0 - tx) as u32, (r.y0 - ty) as u32, r.width(), r.height());
        pass.draw(0..3, 0..1);
    }
    let buf = enc.finish();
    drop(record);
    // On a software adapter (lavapipe) the draw runs here, on the CPU; on a GPU it is queued.
    let _s = crate::perf_span::span("drag.submit");
    queue.submit([buf]);
    true
}

/// The layered mode's above laid over its per-frame composite into document `key`'s display
/// tiles over `lrect` (level pixels). Whether it was written.
fn apply_layered(device: &wgpu::Device, queue: &wgpu::Queue, res: &Resources, key: u64, cache: &DragCache, lrect: Rect, encode_srgb: bool) -> bool {
    let Some(d) = res.docs.get(&key) else { return false };
    let Some(pipe) = res.apply_pipeline(d.format) else { return false };
    let (Some(lower), Some(k), Some(w)) = (&cache.lower, &cache.black, &cache.white) else { return false };
    let level = cache.level;
    let rect = cache.view;
    let keep_hdr = i32::from(d.format == FORMAT_HIGH);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_drag_apply") });
    for t in &d.tiles {
        let Some(target) = t.levels.get(level as usize) else { continue };
        let [tx, ty, tw, th] = [t.rect[0] >> level, t.rect[1] >> level, (t.rect[2] >> level).max(1), (t.rect[3] >> level).max(1)].map(|v| v as i32);
        let r = lrect.intersect(&Rect::from_xywh(tx, ty, tw as u32, th as u32));
        if r.is_empty() {
            continue;
        }
        let offset = [rect.x0 - tx, rect.y0 - ty, i32::from(encode_srgb), keep_hdr];
        let ubuf = super::init_buffer(device, "pc_drag_apply", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pc_drag_apply"),
            layout: &res.apply_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&lower.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&k.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&w.view) },
                wgpu::BindGroupEntry { binding: 3, resource: ubuf.as_entire_binding() },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pc_drag_apply"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_scissor_rect((r.x0 - tx) as u32, (r.y0 - ty) as u32, r.width(), r.height());
        pass.draw(0..3, 0..1);
    }
    queue.submit([enc.finish()]);
    true
}

/// The layered mode's pass: the above laid over the per-frame composite, `k + (w - k) * c` on
/// opaque pixels (the composite and both maps are opaque where this runs), then stored as the
/// encode pass stores a composite.
pub(super) const APPLY_WGSL: &str = r#"
@group(0) @binding(0) var lower: texture_2d<f32>;
@group(0) @binding(1) var over_black: texture_2d<f32>;
@group(0) @binding(2) var over_white: texture_2d<f32>;
@group(0) @binding(3) var<uniform> offset: vec4<i32>; // tile to map, encode sRGB, keep values above 1.0

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(pos.xy)) - offset.xy;
    let c = textureLoad(lower, p, 0);
    let k = textureLoad(over_black, p, 0);
    let w = textureLoad(over_white, p, 0);
    var rgb = max(k.rgb + (w.rgb - k.rgb) * c.rgb, vec3(0.0));
    if (offset.w == 0) {
        rgb = min(rgb, vec3(1.0));
    }
    if (offset.z != 0) {
        rgb = srgb_encode(rgb);
    }
    let a = clamp(c.a, 0.0, 1.0);
    return vec4(rgb * a, a);
}
"#;

/// The sprite mode's one draw: the sprite (premultiplied, zero outside it) at the offset over
/// the below (premultiplied), then, with layers above, `k + (w - k) * c` (opaque there), stored
/// as the encode pass stores a composite.
pub(super) const DRAG_WGSL: &str = r#"
@group(0) @binding(0) var below: texture_2d<f32>;
// The sprite (premultiplied), or with `b.w` the run over black...
@group(0) @binding(1) var sprite: texture_2d<f32>;
// ... and over white.
@group(0) @binding(2) var sprite_white: texture_2d<f32>;
@group(0) @binding(3) var over_black: texture_2d<f32>;
@group(0) @binding(4) var over_white: texture_2d<f32>;
// a: tile to below/above, encode sRGB, keep values above 1.0; b: tile to sprite, has above,
// the run kept as its affine map.
@group(0) @binding(5) var<uniform> u: array<vec4<i32>, 2>;

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(pos.xy));
    let a = u[0];
    let b = u[1];
    let bp = p + a.xy;
    let sp = p + b.xy;
    let size = vec2<i32>(textureDimensions(sprite));
    let inside = all(sp >= vec2(0)) && all(sp < size);
    var c = textureLoad(below, bp, 0);
    if (b.w != 0) {
        // The run over an opaque below: its map where it is, the below unchanged elsewhere.
        if (inside) {
            let sk = textureLoad(sprite, sp, 0);
            let sw = textureLoad(sprite_white, sp, 0);
            c = vec4(sk.rgb + (sw.rgb - sk.rgb) * c.rgb, 1.0);
        }
    } else if (inside) {
        let s = textureLoad(sprite, sp, 0);
        c = s + c * (1.0 - s.a);
    }
    var alpha = clamp(c.a, 0.0, 1.0);
    var rgb = vec3(0.0);
    if (b.z != 0) {
        let k = textureLoad(over_black, bp, 0);
        let w = textureLoad(over_white, bp, 0);
        rgb = k.rgb + (w.rgb - k.rgb) * c.rgb;
        alpha = 1.0;
    } else if (c.a > 0.0) {
        rgb = c.rgb / c.a;
    }
    rgb = max(rgb, vec3(0.0));
    if (a.w == 0) {
        rgb = min(rgb, vec3(1.0));
    }
    if (a.z != 0) {
        rgb = srgb_encode(rgb);
    }
    return vec4(rgb * alpha, alpha);
}
"#;

#[cfg(test)]
mod tests {
    use photocraft_color::{Color, ColorMode, SampleType};
    use photocraft_doc::{Document, Size};
    use photocraft_geom::Rect;

    /// Whether `bg` is opaque on `canvas`, read in one call.
    fn opaque(bg: &photocraft_doc::Layer, canvas: Rect, mode: ColorMode) -> bool {
        super::OpaqueBg::new(bg.id, 0, canvas).step(bg, mode, &mut || true).expect("read in one call")
    }

    /// An 8K page's background (a solid fill: tiles shared) is told opaque in a few
    /// milliseconds (0.9 s in the test build before: the whole layer read as floats), and a
    /// single transparent pixel, a missing tile or a page edge mid-tile are still seen.
    #[test]
    fn an_8k_background_is_told_opaque_at_once_and_exactly() {
        let mut doc = Document::with_background("8k", Size::new(7680, 4321), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let t = std::time::Instant::now();
        assert!(opaque(&doc.layers[0], doc.bounds(), doc.mode));
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(ms < 50.0, "{ms:.1} ms");
        let fmt = doc.pixel_format();
        let clear = photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 1.0, 0.0]);
        // The last row (the page ends inside its tiles).
        let s = doc.layers[0].surface_mut().expect("raster");
        s.fill_rect(Rect::new(7000, 4320, 7001, 4321), &clear);
        assert!(!opaque(&doc.layers[0], doc.bounds(), doc.mode), "a clear pixel on the last row");
        // Past the page: doesn't count.
        let mut doc = Document::with_background("8k", Size::new(7680, 4321), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let s = doc.layers[0].surface_mut().expect("raster");
        s.fill_rect(Rect::new(7000, 4330, 7001, 4331), &clear);
        assert!(opaque(&doc.layers[0], doc.bounds(), doc.mode), "a clear pixel past the page");
        // A semi-transparent fill.
        let doc = Document::with_background("8k", Size::new(600, 400), ColorMode::Rgb, SampleType::U8, Color { alpha: 0.5, ..Color::WHITE });
        assert!(!opaque(&doc.layers[0], doc.bounds(), doc.mode), "half transparent");
    }

    /// An 8K page decoded from a file has a tile of its own everywhere: its Background's alpha was
    /// read whole in the call that started the drag cache (`drag.new` 130 ms on the owner's PC
    /// opening a 7680x4320 page). Now it is read a tile at a time while the call has time, over
    /// calls, and tells the same: opaque, or the one clear pixel in its last tile.
    #[test]
    fn a_decoded_8k_background_is_read_over_calls_within_their_time() {
        let mut doc = Document::with_background("8k", Size::new(7680, 4320), ColorMode::Rgb, SampleType::U8, Color::WHITE);
        let fmt = doc.pixel_format();
        let t = photocraft_geom::TILE_SIZE;
        // Every tile its own (as decoded), opaque.
        let s = doc.layers[0].surface_mut().expect("raster");
        for ty in 0..4320 / t + 1 {
            for tx in 0..7680 / t {
                let v = ((tx * 7 + ty * 13) % 200) as f32 / 255.0;
                s.fill_rect(Rect::new(tx * t, ty * t, (tx + 1) * t, (ty + 1) * t), &photocraft_raster::from_rgba(&fmt, [v, v, v, 1.0]));
            }
        }
        let (bg, canvas) = (&doc.layers[0], doc.bounds());
        let whole = std::time::Instant::now();
        assert!(opaque(bg, canvas, doc.mode));
        let whole = whole.elapsed().as_secs_f64() * 1000.0;
        // Calls of 1 ms.
        let read = |bg: &photocraft_doc::Layer| {
            let mut o = super::OpaqueBg::new(bg.id, 0, canvas);
            let (mut calls, mut longest) = (0, 0.0f64);
            loop {
                calls += 1;
                let t0 = std::time::Instant::now();
                let r = o.step(bg, doc.mode, &mut || t0.elapsed().as_secs_f64() * 1000.0 < 1.0);
                longest = longest.max(t0.elapsed().as_secs_f64() * 1000.0);
                if let Some(r) = r {
                    return (r, calls, longest);
                }
            }
        };
        let (r, calls, longest) = read(bg);
        eprintln!("8K decoded background: whole {whole:.1} ms; in calls of 1 ms: {calls} calls, the longest {longest:.1} ms");
        assert!(r, "opaque");
        assert!(calls > 1, "read over calls");
        // (A call past its time by one tile at most: a 256x256 tile is well under a millisecond.)
        assert!(longest < 1.0 + whole / 100.0 + 2.0, "a call took {longest:.1} ms");
        let mut doc = doc.clone();
        let s = doc.layers[0].surface_mut().expect("raster");
        s.fill_rect(Rect::new(7679, 4319, 7680, 4320), &photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 1.0, 0.5]));
        assert!(!read(&doc.layers[0]).0, "the clear pixel in the last tile");
    }
}
