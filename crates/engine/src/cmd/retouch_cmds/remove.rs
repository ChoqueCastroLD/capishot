//! The Remove Tool as a command: paint over something and it is replaced by its surroundings.
//!
//! The stroke's footprint is the hole. A stroke that closes around an object also removes what it
//! encloses (`closeLoops`), and the hole grows by [`GROW`] pixels so the object's soft edge and
//! halo go with it. The hole is completed from a window around the stroke by patch-based
//! completion with texture features (`photocraft_algo::remove`, `photocraft_algo::nonlocal`). The
//! selection, when there is one, limits what changes. Large holes take seconds, so the command
//! runs as a background job (#210): with progress, cancellable, and the document unchanged until
//! it applies, in one undo step.
//!
//! Several strokes can go in one removal (`strokes`): Photoshop's *Remove after each stroke*
//! off, where the strokes collect and are applied together, so the fill sees every hole at once
//! instead of sampling from an area the next stroke is about to remove. With `newLayer` the fill
//! lands on a new layer above the target, only where the hole is, and the target stays as it was
//! (Photoshop's *Create a new layer*; with Sample All Layers it fills from the composite).
//!
//! A `strokes` entry with `"erase": true` takes its footprint back out of the hole (the mask
//! painted with Paint mask, then run's eraser, or ⌥): entries apply in order, so a later stroke
//! paints over an earlier erasure, and an erased pixel stays out however the hole grows.

use std::sync::Arc;

use super::content_aware_move::{Target, replace, write_fill};
use super::patch::coverage;
use super::{
    Document, EngineError, LayerId, PixelFormat, Rect, Region, Result, SampleLayers, Session, Stroke, Value, alpha_index, bad, clamp_samples, composite_region,
    damage_json, flag, json, parse_brush, reject_huge_stroke, targets_pixels,
};
use photocraft_doc::Layer;
use photocraft_raster::{from_rgba_into, to_rgba};

const CMD: &str = "paint.remove";

/// Largest stroke, in pixels of its bounding box (as the Patch Tool's and Content-Aware Move's).
/// Checked before the stroke's coverage is rendered, which allocates its whole bounding box, also
/// where it leaves the canvas.
const MAX_PIXELS: u64 = 16 << 20;

/// Cap on the sampling margin around the stroke (the window grows with the stroke).
const MAX_MARGIN: i32 = 512;

/// The least margin around Remove with AI's strokes: room for the model's context past the
/// farthest the hole grows.
const AI_CONTEXT: i32 = 256;

/// How far the hole grows beyond the stroke, in pixels.
const GROW: usize = 2;

/// Seed of the completion, so a removal is reproducible.
const SEED: u64 = 0x0052_e30e;

/// The sampling margin around the stroke: three quarters of its larger side, at least 32 pixels
/// (in u64, so a huge extent can't overflow; a margin beyond i32 saturates).
fn sampling_margin(area: Rect) -> i32 {
    let ext = u64::from(area.width().max(area.height()));
    i32::try_from(ext * 3 / 4).unwrap_or(i32::MAX).max(32)
}

/// What a removal does, checked before any work starts.
struct Plan {
    id: Option<LayerId>,
    /// One stroke (`points`), or several applied together (`strokes`).
    strokes: Vec<Stroke>,
    /// Per stroke: it erases from the hole instead of adding to it (`"erase": true`).
    erase: Vec<bool>,
    /// Where the fill samples from: the strokes plus a margin, on the canvas.
    window: Rect,
    close_loops: bool,
    all_layers: bool,
    /// The fill goes to a new layer above the target instead of into it.
    new_layer: bool,
    /// `ai`: fill with Manga LaMa through the injected service instead of patch completion.
    ai: Option<(std::sync::Arc<dyn photocraft_ai_lama::service::Inpainter>, crate::ai_inpaint_cmds::AiOptions)>,
    /// Remove with AI: the hole grows over the ink the strokes touch
    /// ([`super::ink_mask`]) instead of the plain [`GROW`].
    ink: bool,
}

/// The stroke's bounding box (points ± the brush radius), before any coverage is rendered.
fn stroke_bounds(stroke: &Stroke) -> Rect {
    let r = f64::from(stroke.brush.size) / 2.0 + 2.0;
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for q in &stroke.points {
        (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
    }
    // `check_coords` keeps points within ±MAX_COORD and sizes are capped, so these fit an i32.
    Rect::new((x0 - r).floor() as i32, (y0 - r).floor() as i32, (x1 + r).ceil() as i32, (y1 + r).ceil() as i32)
}

/// The strokes of the request: `strokes` (several, each a list of points, sharing the brush
/// and the target) or the one in `points`.
fn parse_strokes(s: &Session, p: &Value) -> Result<(Vec<Stroke>, Vec<bool>, Option<LayerId>)> {
    let Some(list) = p.get("strokes") else {
        let (stroke, id) = parse_brush(s, p, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        return Ok((vec![stroke], vec![false], id));
    };
    const ENTRY: &str = "each `strokes` entry is a point list or {\"points\", \"size\"?, \"hardness\"?, \"spacing\"?, \"erase\"?}";
    let list = list.as_array().ok_or_else(|| bad(CMD, ENTRY))?;
    if list.is_empty() {
        return Err(bad(CMD, "`strokes` is empty"));
    }
    let mut strokes = Vec::with_capacity(list.len());
    let mut erase = Vec::with_capacity(list.len());
    let mut id = None;
    for entry in list {
        let mut one = p.clone();
        let Some(o) = one.as_object_mut() else { return Err(bad(CMD, "params must be an object")) };
        o.remove("strokes");
        match entry {
            // A stroke drawn with another brush than the rest (the waiting strokes of Remove after
            // each stroke off keep the size, hardness and spacing each was drawn with).
            Value::Object(e) => {
                let points = e.get("points").ok_or_else(|| bad(CMD, ENTRY))?;
                erase.push(match e.get("erase") {
                    None | Some(Value::Null) => false,
                    Some(v) => v.as_bool().ok_or_else(|| bad(CMD, "`erase` must be a boolean"))?,
                });
                o.insert("points".into(), points.clone());
                for k in ["size", "hardness", "spacing"] {
                    if let Some(v) = e.get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
            }
            points => {
                o.insert("points".into(), points.clone());
                erase.push(false);
            }
        }
        let (stroke, layer) = parse_brush(s, &one, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        strokes.push(stroke);
        id = layer;
    }
    Ok((strokes, erase, id))
}

fn plan(s: &Session, p: &Value, ink: bool) -> Result<Plan> {
    let (strokes, erase, id) = parse_strokes(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    // An erasing stroke only takes away: the painted strokes alone set the window.
    let bounds = strokes
        .iter()
        .zip(&erase)
        .filter(|(_, e)| !**e)
        .map(|(s, _)| stroke_bounds(s))
        .reduce(|a, b| a.union(&b))
        .ok_or_else(|| bad(CMD, "nothing to remove: every stroke erases"))?;
    let pixels = u64::from(bounds.width()) * u64::from(bounds.height());
    if pixels > MAX_PIXELS {
        return Err(bad(CMD, format!("the stroke is too large to remove ({pixels} pixels, at most {MAX_PIXELS})")));
    }
    let area = bounds.intersect(&canvas);
    if area.is_empty() {
        return Err(bad(CMD, "the stroke is outside the canvas"));
    }
    let margin = if ink { sampling_margin(area).max(AI_CONTEXT) } else { sampling_margin(area) };
    let window = area.inflate(margin.min(MAX_MARGIN)).intersect(&canvas);
    let close_loops = flag(p, "closeLoops", true);
    let all_layers = flag(p, "sampleAllLayers", false) && targets_pixels(p);
    // A mask or channel has no "above": the fill goes where the stroke went.
    let new_layer = flag(p, "newLayer", false) && targets_pixels(p);
    let ai = if flag(p, "ai", false) {
        let opts = crate::ai_inpaint_cmds::ai_options(s, p, CMD, true)?;
        Some((crate::ai_inpaint_cmds::ready_inpainter(s, &opts)?, opts))
    } else {
        None
    };
    Ok(Plan { id, strokes, erase, window, close_loops, all_layers, new_layer, ai, ink })
}

/// The strokes' footprints over the plan's window, and where the last stroke over a pixel erased
/// it (kept out of the hole).
fn footprints(plan: &Plan) -> Result<(Vec<bool>, Vec<bool>)> {
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let mut m = vec![false; w * h];
    let mut erased = no_erasure(plan);
    for (stroke, erase) in plan.strokes.iter().zip(&plan.erase) {
        let (bounds, cov) = super::coverage(CMD, stroke)?;
        mark(&mut m, &mut erased, win, bounds, &cov, *erase);
    }
    Ok((m, erased))
}

/// Where the strokes erased, none yet: empty when no stroke erases.
fn no_erasure(plan: &Plan) -> Vec<bool> {
    let (w, h) = (plan.window.width() as usize, plan.window.height() as usize);
    vec![false; if plan.erase.contains(&true) { w * h } else { 0 }]
}

/// The largest brush that paints (an erasing stroke doesn't widen how far the hole grows).
fn brush_size(plan: &Plan) -> f32 {
    plan.strokes.iter().zip(&plan.erase).filter(|(_, e)| !**e).map(|(s, _)| s.brush.size).fold(0.0, f32::max)
}

/// `m` without what the strokes erased, however far it grew.
fn unerase(mut m: Vec<bool>, erased: &[bool]) -> Vec<bool> {
    for (v, e) in m.iter_mut().zip(erased) {
        *v &= !*e;
    }
    m
}

/// `footprints` with the areas the strokes enclose, with `closeLoops`.
fn enclose(plan: &Plan, m: Vec<bool>) -> Vec<bool> {
    let (w, h) = (plan.window.width() as usize, plan.window.height() as usize);
    if plan.close_loops { photocraft_algo::remove::close_loops(w, h, &m) } else { m }
}

/// The luminance of the window's pixels, which Remove with AI's hole grows by (see [`Plan::ink`]).
fn ink_luminance(plan: &Plan, img: &Region, fmt: &photocraft_color::PixelFormat) -> Option<Vec<f32>> {
    plan.ink.then(|| super::ink_mask::luminance(&crate::ai_inpaint_cmds::to_rgb(fmt, &img.data)))
}

/// The painted area grown by [`GROW`] (or, with `lum`, by the ink it touches).
fn grow(plan: &Plan, doc: &Document, painted: &[bool], lum: Option<&[f32]>) -> Vec<bool> {
    let (w, h) = (plan.window.width() as usize, plan.window.height() as usize);
    match lum {
        Some(lum) => {
            let page = doc.bounds();
            let g = super::ink_mask::Grow::for_page(page.width().min(page.height()), brush_size(plan));
            super::ink_mask::expand(w, h, lum, painted, g)
        }
        None => photocraft_algo::remove::dilate(w, h, painted, GROW),
    }
}

/// `m` limited to the selection, then checked: an error when nothing would change or nothing is
/// left to fill from.
fn limit(plan: &Plan, doc: &Document, mut m: Vec<bool>) -> Result<Vec<bool>> {
    if let Some(sel) = doc.selection.as_ref() {
        for (i, c) in coverage(sel, plan.window).into_iter().enumerate() {
            if c <= 0.0
                && let Some(v) = m.get_mut(i)
            {
                *v = false;
            }
        }
    }
    if !m.iter().any(|m| *m) {
        return Err(bad(CMD, "the stroke doesn't cover anything that can change (check the selection)"));
    }
    if m.iter().all(|m| *m) {
        return Err(bad(CMD, "nothing is left around the stroke to fill it from"));
    }
    Ok(m)
}

/// The window's pixels and the hole, read on `doc` (a snapshot) before the fill.
struct Prepared {
    img: Region,
    fmt: photocraft_color::PixelFormat,
    hole: Vec<bool>,
}

/// `win` of the window's pixels in the target's format.
fn read(doc: &mut Document, plan: &Plan, p: &Value, win: Rect) -> Result<(Region, photocraft_color::PixelFormat)> {
    let (surf, _) = crate::channel_cmds::target_surface(doc, plan.id, p)?;
    let fmt = surf.format();
    let img = if plan.all_layers { composite_region(doc, None, SampleLayers::All, win, fmt) } else { Region::read(surf, win) };
    Ok((img, fmt))
}

/// `None` when the layer's own window is fully transparent: nothing to remove and nothing to fill
/// from (a no-op without the fill or a history step, as the other retouch tools on an empty layer).
fn unless_empty(plan: &Plan, prep: Prepared) -> Option<Prepared> {
    let empty = !plan.all_layers && alpha_index(&prep.fmt).is_some_and(|a| prep.img.data.chunks_exact(prep.img.ch).all(|px| px.get(a).is_none_or(|v| *v <= 0.0)));
    (!empty).then_some(prep)
}

/// Reads what the fill needs (see [`unless_empty`] for `None`).
fn prepare(doc: &mut Document, plan: &Plan, p: &Value) -> Result<Option<Prepared>> {
    let (img, fmt) = read(doc, plan, p, plan.window)?;
    let (painted, erased) = footprints(plan)?;
    let painted = enclose(plan, painted);
    let lum = ink_luminance(plan, &img, &fmt);
    let hole = limit(plan, doc, unerase(grow(plan, doc, &painted, lum.as_deref()), &erased))?;
    Ok(unless_empty(plan, Prepared { img, fmt, hole }))
}

/// Remove with AI's hole over what the model sees, and the snapshot it reads (`None`: nothing to
/// do, see [`unless_empty`]).
type AiInput = Option<AiHole>;

/// Remove with AI's hole, cut to what the model sees (`at`), and the snapshot the window was read
/// on: the fill reads its pixels again from it a block at a time ([`InWindow`]).
struct AiHole {
    doc: Document,
    fmt: photocraft_color::PixelFormat,
    hole: Vec<bool>,
    at: Rect,
}

/// Rows of the window a part of [`prepare_ai`] reads at a time.
const BAND: i32 = 128;

/// Stroke points a part of [`prepare_ai`] renders at a time.
const POINTS: usize = 2;

/// Pixels a part of [`prepare_ai`]'s floods (closing loops, following ink) looks at, at most.
const FLOOD: usize = 1 << 17;

/// What Remove with AI's fill reads, as [`prepare`] without keeping the window's pixels (only the
/// ink's luminance and whether the layer has any), the hole cut to what the model sees, in parts
/// ([`crate::jobs::Prep`]: one per step between frames in the single-threaded browser build), each
/// a pass over a band of the window or a share of a flood: none holds a phone's page for long.
fn prepare_ai(doc: Arc<Document>, plan: Arc<Plan>, p: Value, margin: usize) -> crate::jobs::Prep<AiInput> {
    let win = plan.window;
    let page = doc.bounds();
    let g = super::ink_mask::Grow::for_page(page.width().min(page.height()), brush_size(&plan));
    let a = AiPrep {
        plan,
        p,
        margin,
        doc,
        g,
        work: None,
        fmt: None,
        blank: true,
        painted: Vec::new(),
        erased: Vec::new(),
        lum: Vec::new(),
        rows: Vec::new(),
        ink: Vec::new(),
        zone: Vec::new(),
        take: Vec::new(),
        stage: Stage::Read(win.y0),
    };
    crate::jobs::Prep::then(move |_| a.step())
}

/// Where [`prepare_ai`] is.
enum Stage {
    /// Reading the window's rows from this one.
    Read(i32),
    /// Rendering stroke `.0` from its point `.1`.
    Stroke(usize, usize, Option<Box<photocraft_paint::retouch::StrokeCoverage>>),
    Enclose(Option<photocraft_algo::remove::CloseLoops>),
    /// The ink map's horizontal pass, then its vertical one.
    InkRows,
    InkCols(Box<super::ink_mask::ColsMax>),
    /// How far runs of ink may reach: the stroke dilated along rows, then columns.
    Zone,
    ZoneCols(Vec<bool>),
    /// Following the runs of ink.
    Runs(super::ink_mask::Runs),
    /// The stroke grown by the dilation (rows, columns) and the runs it takes.
    Grow,
    GrowCols(Vec<bool>),
    Taken(Vec<bool>),
    /// The hole limited to the selection and checked, then cut to what the model sees.
    Limit(Vec<bool>),
    Cut(Vec<bool>),
}

struct AiPrep {
    plan: Arc<Plan>,
    p: Value,
    margin: usize,
    doc: Arc<Document>,
    g: super::ink_mask::Grow,
    /// The snapshot read from (the target surface is resolved on it), from the first part.
    work: Option<Document>,
    fmt: Option<photocraft_color::PixelFormat>,
    /// The layer's own window is fully transparent so far ([`unless_empty`]).
    blank: bool,
    painted: Vec<bool>,
    /// Where the strokes erased (empty when none does), kept out of the hole.
    erased: Vec<bool>,
    lum: Vec<f32>,
    rows: Vec<f32>,
    ink: Vec<bool>,
    zone: Vec<bool>,
    take: Vec<bool>,
    stage: Stage,
}

fn not_read() -> EngineError {
    EngineError::Other("internal error: the window wasn't read".into())
}

impl AiPrep {
    /// One part, and what comes next.
    fn step(mut self) -> Result<crate::jobs::Prep<AiInput>> {
        use crate::jobs::Prep;
        let plan = Arc::clone(&self.plan);
        let win = plan.window;
        let (w, h) = (win.width() as usize, win.height() as usize);
        let g = self.g;
        self.stage = match std::mem::replace(&mut self.stage, Stage::Grow) {
            Stage::Read(y) => {
                let work = self.work.get_or_insert_with(|| (*self.doc).clone());
                let band = Rect::new(win.x0, y, win.x1, (y + BAND).min(win.y1));
                let (img, fmt) = read(work, &plan, &self.p, band)?;
                self.fmt = Some(fmt);
                // Only what the hole needs: the band's pixels are read again by the fill, a block
                // at a time, instead of the whole window kept in floats.
                self.blank &=
                    !plan.all_layers && alpha_index(&fmt).is_some_and(|a| img.data.chunks_exact(img.ch).all(|px| px.get(a).is_none_or(|v| *v <= 0.0)));
                if plan.ink {
                    self.lum.reserve_exact((w * h).saturating_sub(self.lum.len()));
                    self.lum.extend(super::ink_mask::luminance(&crate::ai_inpaint_cmds::to_rgb(&fmt, &img.data)));
                }
                if band.y1 >= win.y1 {
                    self.painted = vec![false; w * h];
                    self.erased = no_erasure(&plan);
                    Stage::Stroke(0, 0, None)
                } else {
                    Stage::Read(band.y1)
                }
            }
            Stage::Stroke(i, at, cov) => match plan.strokes.get(i) {
                None => Stage::Enclose(None),
                Some(stroke) => {
                    let mut cov = cov.unwrap_or_else(|| Box::new(photocraft_paint::retouch::StrokeCoverage::new(stroke)));
                    let next = (at + POINTS).min(stroke.points.len());
                    cov.push(stroke.points.get(at..next).unwrap_or_default());
                    if next < stroke.points.len() {
                        Stage::Stroke(i, next, Some(cov))
                    } else {
                        let (bounds, c) = cov.finish(super::MAX_STROKE_PIXELS).ok_or_else(|| bad(CMD, format!("stroke too large (at most {} px)", super::MAX_STROKE_PIXELS)))?;
                        let erase = plan.erase.get(i).copied().unwrap_or(false);
                        mark(&mut self.painted, &mut self.erased, win, bounds, &c, erase);
                        Stage::Stroke(i + 1, 0, None)
                    }
                }
            },
            Stage::Enclose(c) => {
                if !plan.close_loops {
                    self.after_enclose()
                } else {
                    let mut c = c.unwrap_or_else(|| photocraft_algo::remove::CloseLoops::new(w, h, &self.painted));
                    if c.advance(&self.painted, FLOOD) {
                        self.painted = c.finish(&self.painted);
                        self.after_enclose()
                    } else {
                        Stage::Enclose(Some(c))
                    }
                }
            }
            Stage::InkRows => {
                if super::ink_mask::applies(w, h, &self.lum, &self.painted) {
                    self.rows = super::ink_mask::ink_rows(w, h, &self.lum, g);
                    Stage::InkCols(Box::new(super::ink_mask::ink_cols_start(w, h, g)))
                } else {
                    // Nothing to grow over: the hole is what was painted (as `ink_mask::expand`).
                    self.lum = Vec::new();
                    Stage::Limit(std::mem::take(&mut self.painted))
                }
            }
            Stage::InkCols(mut c) => {
                if c.advance(&mut self.rows, BAND as usize * 2) {
                    self.ink = super::ink_mask::ink_cols_finish(&std::mem::take(&mut self.lum), &std::mem::take(&mut self.rows));
                    Stage::Zone
                } else {
                    Stage::InkCols(c)
                }
            }
            Stage::Zone => Stage::ZoneCols(photocraft_algo::remove::dilate_rows(w, h, &self.painted, g.zone_reach())),
            Stage::ZoneCols(rows) => {
                self.zone = photocraft_algo::remove::dilate_cols(w, h, &rows, g.zone_reach());
                Stage::Runs(super::ink_mask::Runs::new(w * h))
            }
            Stage::Runs(mut r) => {
                let until = r.next.saturating_add(FLOOD);
                if r.advance(w, h, &self.painted, &self.ink, &self.zone, until) {
                    self.take = r.take;
                    (self.ink, self.zone) = (Vec::new(), Vec::new());
                    Stage::Grow
                } else {
                    Stage::Runs(r)
                }
            }
            Stage::Grow => Stage::GrowCols(photocraft_algo::remove::dilate_rows(w, h, &self.painted, self.grow_by())),
            Stage::GrowCols(rows) => Stage::Taken(photocraft_algo::remove::dilate_cols(w, h, &rows, self.grow_by())),
            Stage::Taken(grown) => {
                let painted = std::mem::take(&mut self.painted);
                if plan.ink { Stage::Limit(super::ink_mask::combine(w, h, &painted, &grown, &std::mem::take(&mut self.take))) } else { Stage::Limit(grown) }
            }
            Stage::Limit(grown) => {
                let work = self.work.as_ref().ok_or_else(not_read)?;
                Stage::Cut(limit(&plan, work, unerase(grown, &std::mem::take(&mut self.erased)))?)
            }
            Stage::Cut(hole) => {
                let (Some(doc), Some(fmt)) = (self.work.take(), self.fmt) else { return Err(not_read()) };
                if self.blank {
                    return Ok(Prep::Done(None));
                }
                // What edit.inpaintAi would read for a selection of the hole's shape: the same
                // context around it gives the same fill.
                let at = crate::ai_inpaint_cmds::context_region(hole_bounds(&hole, win), doc.bounds(), self.margin).intersect(&win);
                return Ok(Prep::Done(Some(AiHole { doc, fmt, hole: crop_hole(hole, win, at), at })));
            }
        };
        Ok(Prep::then(move |_| self.step()))
    }

    /// How far the stroke grows: as `ink_mask::finish`, else [`GROW`].
    fn grow_by(&self) -> usize {
        if self.plan.ink { self.g.dilate } else { GROW }
    }

    /// After the strokes and what they enclose: the ink's luminance (Remove with AI) or the plain
    /// dilation.
    fn after_enclose(&self) -> Stage {
        if self.plan.ink { Stage::InkRows } else { Stage::Grow }
    }
}

/// Marks where `cov` (over `bounds`) covers anything in `m` (over `win`), or with `erase` clears
/// it there and marks it in `erased` (when that isn't empty).
fn mark(m: &mut [bool], erased: &mut [bool], win: Rect, bounds: Rect, cov: &[f32], erase: bool) {
    let w = win.width() as usize;
    let bw = (bounds.width() as usize).max(1);
    for (i, c) in cov.iter().enumerate() {
        let (x, y) = (bounds.x0 + (i % bw) as i32, bounds.y0 + (i / bw) as i32);
        if *c > 0.0 && win.contains(x, y) {
            let j = (y - win.y0) as usize * w + (x - win.x0) as usize;
            if let Some(v) = m.get_mut(j) {
                *v = !erase;
            }
            if let Some(v) = erased.get_mut(j) {
                *v = erase;
            }
        }
    }
}

/// The bounds of the set pixels of `hole` over `win`.
fn hole_bounds(hole: &[bool], win: Rect) -> Rect {
    let w = (win.width() as usize).max(1);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for (i, _) in hole.iter().enumerate().filter(|(_, m)| **m) {
        (x0, y0, x1, y1) = (x0.min(i % w), y0.min(i / w), x1.max(i % w + 1), y1.max(i / w + 1));
    }
    if x0 > x1 {
        return win;
    }
    Rect::new(win.x0 + x0 as i32, win.y0 + y0 as i32, win.x0 + x1 as i32, win.y0 + y1 as i32)
}

/// `hole` (over `win`) cut to `rect`, which lies within `win`, in place.
fn crop_hole(mut hole: Vec<bool>, win: Rect, rect: Rect) -> Vec<bool> {
    let rect = rect.intersect(&win);
    if rect == win || rect.is_empty() {
        return hole;
    }
    let ww = win.width() as usize;
    let (dx, dy) = ((rect.x0 - win.x0) as usize, (rect.y0 - win.y0) as usize);
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    // Row `y` of the cut starts at or after where it goes: moving rows in order never overwrites
    // one still to move.
    for y in 0..h {
        let from = (y + dy) * ww + dx;
        hole.copy_within(from..from + w, y * w);
    }
    hole.truncate(w * h);
    hole.shrink_to_fit();
    hole
}

/// The filled window and the hole it fills, by patch-based completion on `doc` (a snapshot).
fn run_remove(doc: &mut Document, plan: &Plan, p: &Value, ctx: &crate::jobs::JobCtx, label: &str) -> Result<Option<(Region, Vec<bool>)>> {
    let Some(Prepared { img, fmt, hole, .. }) = prepare(doc, plan, p)? else { return Ok(None) };
    ctx.check()?;
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let mut data = ctx
        .stage(0.0, 1.0, label, |ctl| photocraft_algo::remove::remove_with(w, h, img.ch, &img.data, &hole, SEED, ctl))
        .map_err(|_| EngineError::Cancelled)?;
    // The fill's seam blending can step slightly past the storable range.
    clamp_samples(&fmt, &mut data);
    Ok(Some((Region { rect: win, ch: img.ch, data }, hole)))
}

/// The fill on a new layer above `id`: the hole's pixels (opaque where the source had no alpha),
/// transparent elsewhere. Returns the damage and the new layer.
fn fill_to_new_layer(doc: &mut Document, id: Option<LayerId>, out: &Region, hole: &[bool]) -> Result<(Rect, LayerId)> {
    let base = doc.pixel_format();
    let fmt = PixelFormat::new(base.mode, base.sample, true);
    let data = new_layer_pixels(fmt.channels(), out, hole)?;
    let mut l = Layer::raster(doc.next_layer_name("Layer"), fmt);
    crate::pixels_mut(&mut l)?.write_region(out.rect, &data);
    // Above the sampled layer and in its clipping mask, like Layer via Copy.
    let nid = crate::layer_place::insert_new(doc, id, l, crate::layer_place::Place::Above);
    Ok((out.rect, nid))
}

/// The pixels of a new layer (`n` channels, alpha last) holding `out` where `hole` is set (opaque
/// where `out` has no alpha), transparent elsewhere.
fn new_layer_pixels(n: usize, out: &Region, hole: &[bool]) -> Result<Vec<f32>> {
    let colours = n.saturating_sub(1);
    if out.ch != n && out.ch != colours {
        return Err(EngineError::Other("internal error: the fill's channels don't match the document".into()));
    }
    let mut data = vec![0.0f32; hole.len().saturating_mul(n)];
    for ((dst, src), m) in data.chunks_exact_mut(n).zip(out.data.chunks_exact(out.ch)).zip(hole) {
        if !*m {
            continue;
        }
        for (d, s) in dst.iter_mut().zip(src.iter().take(colours)) {
            *d = *s;
        }
        if let Some(a) = dst.last_mut() {
            *a = if out.ch == n { src.last().copied().unwrap_or(1.0) } else { 1.0 };
        }
    }
    Ok(data)
}

/// Where Remove with AI's result goes.
enum AiOut {
    /// Into the target, as [`replace`] writes a fill (`lock`: its transparency is locked).
    Into { lock: bool },
    /// On a new layer above it, only the hole ([`fill_to_new_layer`]).
    NewLayer,
    /// On a layer of cleans (`output: new | single`), as `edit.inpaintAi` writes one.
    Clean(crate::ai_inpaint_cmds::DocSink),
}

/// Remove with AI's fill as it reads and writes: the hole, the pixels the model sees read from
/// the snapshot a block at a time, and the result in the layer's format, written block by block
/// into `out` (a copy of the target, or the new layer).
struct Window {
    hole: AiHole,
    plan: Arc<Plan>,
    p: Value,
    out: crate::ai_inpaint_cmds::Out,
    mode: AiOut,
}

/// The [`Window`] both reads from (a [`Source`](crate::ai_inpaint_cmds::work::Source)) and writes
/// into (a [`Sink`](crate::ai_inpaint_cmds::work::Sink)).
#[derive(Clone)]
struct InWindow(Arc<std::sync::Mutex<Window>>);

impl InWindow {
    fn lock(&self) -> std::sync::MutexGuard<'_, Window> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `r` (within the cut at `at`) on the page.
fn page_rect(at: Rect, r: photocraft_ai_lama::tiles::Rect) -> Rect {
    Rect::from_xywh(at.x0 + r.x as i32, at.y0 + r.y as i32, r.w as u32, r.h as u32)
}

impl Window {
    /// The window's pixels over `r` in the target's format, read on the snapshot.
    fn read(&mut self, r: Rect) -> Result<Region> {
        read(&mut self.hole.doc, &self.plan, &self.p, r).map(|(img, _)| img)
    }
}

impl crate::ai_inpaint_cmds::work::Source for InWindow {
    fn cover(&mut self, r: photocraft_ai_lama::tiles::Rect, out: &mut Vec<f32>) {
        let st = self.lock();
        let w = st.hole.at.width() as usize;
        out.clear();
        for y in r.y..r.y + r.h {
            out.extend(st.hole.hole[y * w + r.x..y * w + r.x + r.w].iter().map(|m| if *m { 1.0 } else { 0.0 }));
        }
    }

    fn rgb(&mut self, r: photocraft_ai_lama::tiles::Rect, out: &mut Vec<f32>) {
        let mut st = self.lock();
        let (at, fmt) = (st.hole.at, st.hole.fmt);
        out.clear();
        if let Ok(img) = st.read(page_rect(at, r)) {
            *out = crate::ai_inpaint_cmds::to_rgb(&fmt, &img.data);
        }
    }
}

impl crate::ai_inpaint_cmds::work::Sink for InWindow {
    fn put(&mut self, r: photocraft_ai_lama::tiles::Rect, rgb: &[f32], cover: &[f32]) -> Result<()> {
        if !cover.iter().any(|c| *c > 0.0) {
            return Ok(());
        }
        let mut st = self.lock();
        let (at, fmt) = (st.hole.at, st.hole.fmt);
        let rect = page_rect(at, r);
        if let AiOut::Clean(sink) = &mut st.mode {
            return sink.put(r, rgb, cover);
        }
        // The fill in the layer's format: the AI's RGB with the window's own alpha.
        let mut src = st.read(rect)?;
        let n = src.ch;
        let mut enc = [0.0f32; 8];
        for (j, px) in src.data.chunks_exact_mut(n).enumerate() {
            if cover[j] > 0.0 {
                let g = &rgb[j * 3..j * 3 + 3];
                from_rgba_into(&fmt, [g[0], g[1], g[2], to_rgba(&fmt, px)[3]], &mut enc);
                px.copy_from_slice(&enc[..n]);
                // The fill's seam blending can step slightly past the storable range.
                clamp_samples(&fmt, px);
            }
        }
        let hole: Vec<bool> = cover.iter().map(|c| *c > 0.0).collect();
        let mut out = st.out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match st.mode {
            AiOut::Into { lock } => {
                let mut px = out.read_region(rect);
                write_fill(&mut px, &src, &hole, alpha_index(&fmt).filter(|_| lock))?;
                out.write_region(rect, &px);
            }
            AiOut::NewLayer => {
                let px = new_layer_pixels(out.format().channels(), &src, &hole)?;
                out.write_region(rect, &px);
            }
            AiOut::Clean(_) => {}
        }
        Ok(())
    }
}

/// What an AI fill ran on, for the reply (`device`, `model`, `lite`, `cpu`, `fellBack`, `ms`), as
/// `edit.inpaintAi` reports it.
struct AiRun {
    device: String,
    run: crate::ai_inpaint_cmds::FillRun,
    ms: u64,
}

impl AiRun {
    /// `after` is the service's device once the run ended (a GPU that failed mid-run reports the
    /// CPU it fell back to); `before`, the one it was going to use, when `after` is unknown.
    fn new(after: &str, before: &str, run: crate::ai_inpaint_cmds::FillRun, started: web_time::Instant) -> Self {
        let device = if after.is_empty() { before } else { after };
        let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        Self { device: device.to_string(), run, ms }
    }

    fn report(self, o: &mut serde_json::Map<String, Value>) {
        o.insert("device".into(), json!(self.device));
        o.insert("model".into(), json!(self.run.model.id));
        o.insert("lite".into(), json!(self.run.model.lite));
        // `device` too: `CAPISHOT_AI_DEVICE=cpu` forces the CPU without the request knowing.
        o.insert("cpu".into(), json!(self.run.cpu || self.device == "CPU"));
        o.insert("fellBack".into(), json!(self.run.fell_back));
        if let Some(g) = self.run.guard {
            o.insert("gpuGuard".into(), json!(g.id()));
        }
        o.insert("ms".into(), json!(self.ms));
    }
}

/// Applies a filled window to the live document as one undo step (into the target, or onto a new
/// layer above it with `new_layer`).
fn apply_fill(s: &mut Session, label: &'static str, id: Option<LayerId>, p: &Value, new_layer: bool, filled: Option<(Region, Vec<bool>)>) -> Result<Value> {
    let Some((out, hole)) = filled else { return Ok(json!({"changed": false})) };
    if new_layer {
        let keep_active = p.get("keepActive").and_then(Value::as_bool).unwrap_or(false);
        let (damage, nid) = s.edit(label, |doc, active| {
            let r = fill_to_new_layer(doc, id, &out, &hole)?;
            if !keep_active {
                *active = Some(r.1);
            }
            Ok(r)
        })?;
        s.hint_damage(damage);
        return Ok(json!({ "damage": damage_json(damage), "layer": nid.0 }));
    }
    let damage = s.edit(label, |doc, _| replace(doc, Target { id, all_layers: false }, p, &out, &hole))?;
    s.hint_damage(damage);
    Ok(json!({ "damage": damage_json(damage) }))
}

/// Where Remove with AI's result goes, and a copy of what it writes into: the target, a new layer
/// above it, or (Remove with AI's `output: new | single`) a layer of cleans.
fn ai_out(hole: &mut AiHole, id: Option<LayerId>, p: &Value, new_layer: bool) -> Result<(AiOut, crate::ai_inpaint_cmds::Out)> {
    use crate::ai_inpaint_cmds::Output;
    let shared = |s: photocraft_raster::Surface| Arc::new(std::sync::Mutex::new(s));
    if !new_layer {
        let (surf, lock) = crate::channel_cmds::target_surface(&mut hole.doc, id, p)?;
        return Ok((AiOut::Into { lock }, shared(surf.clone())));
    }
    match (Output::parse(p, Output::Current)?, id) {
        (Output::Current, _) | (_, None) => {
            let base = hole.doc.pixel_format();
            Ok((AiOut::NewLayer, shared(photocraft_raster::Surface::new(PixelFormat::new(base.mode, base.sample, true)))))
        }
        (output, Some(_)) => {
            let (sink, out) = crate::ai_inpaint_cmds::clean_sink(&hole.doc, &output, hole.at);
            Ok((AiOut::Clean(sink), out))
        }
    }
}

/// Remove with AI's result as one undo step: what it wrote over `at` into the target, a new layer
/// or a layer of cleans.
fn apply_ai(s: &mut Session, label: &'static str, id: Option<LayerId>, p: &Value, done: Option<(AiOut, photocraft_raster::Surface, Rect)>) -> Result<Value> {
    let Some((mode, out, at)) = done else { return Ok(json!({"changed": false})) };
    if let AiOut::Into { .. } = mode {
        s.edit(label, |doc, _| {
            let (surf, _) = crate::channel_cmds::target_surface(doc, id, p)?;
            crate::ai_inpaint_cmds::put_region(surf, &out, at);
            Ok(())
        })?;
        s.hint_damage(at);
        return Ok(json!({ "damage": damage_json(at) }));
    }
    let keep_active = p.get("keepActive").and_then(Value::as_bool).unwrap_or(false);
    let nid = s.edit(label, |doc, active| {
        let nid = match (&mode, id) {
            (AiOut::Clean(_), Some(id)) => {
                crate::ai_inpaint_cmds::land_clean(doc, id, &crate::ai_inpaint_cmds::Output::parse(p, crate::ai_inpaint_cmds::Output::Current)?, out, at)?
            }
            _ => {
                let mut l = Layer::raster(doc.next_layer_name("Layer"), out.format());
                *crate::pixels_mut(&mut l)? = out;
                // Above the sampled layer and in its clipping mask, like Layer via Copy.
                crate::layer_place::insert_new(doc, id, l, crate::layer_place::Place::Above)
            }
        };
        if !keep_active {
            *active = Some(nid);
        }
        Ok(nid)
    })?;
    s.hint_damage(at);
    Ok(json!({ "damage": damage_json(at), "layer": nid.0 }))
}

pub(super) fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    run_labelled(s, p, "Remove", false)
}

/// Remove with AI's strokes: [`remove`] (in its AI mode) as one undo step named `label`, the hole
/// grown over the ink the strokes touch ([`super::ink_mask`]) and filled as `edit.inpaintAi` fills
/// a selection of its shape.
pub(crate) fn remove_ink_labelled(s: &mut Session, p: &Value, label: &'static str) -> Result<Value> {
    run_labelled(s, p, label, true)
}

fn run_labelled(s: &mut Session, p: &Value, label: &'static str, ink: bool) -> Result<Value> {
    let mut plan = plan(s, p, ink)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = d.doc.clone();
    let p = p.clone();
    let id = plan.id;
    let new_layer = plan.new_layer;
    // The AI mode answers asynchronously (in the browser from the AI Web Worker, which can't be
    // waited for on the page's threads): read the window here, fill it tile by tile through the
    // inpainter, and apply when the last tile is back.
    if let Some((svc, opts)) = plan.ai.take() {
        let device = svc.device();
        let margin = opts.context_margin();
        let pp = p.clone();
        let wp = p.clone();
        let plan = Arc::new(plan);
        let wplan = plan.clone();
        return crate::jobs::run_async_prepared(
            s,
            label,
            true,
            // The window's pixels and the hole (seconds for a big stroke on a phone): on the
            // browser threads build's pool, a part per step in the single-threaded one, not in
            // one go on the page's thread.
            move |_ctx| Ok(prepare_ai(doc, plan, pp, margin)),
            // The AI answers asynchronously (in the browser from the AI Web Worker, which can't
            // be waited for on the page's threads): fill it tile by tile through the inpainter,
            // and apply when the last tile is back.
            move |ctx, prepared: AiInput, done| {
                let Some(mut hole) = prepared else { return done.finish(Ok((None, None))) };
                let (mode, out) = match ai_out(&mut hole, id, &wp, new_layer) {
                    Ok(o) => o,
                    Err(e) => return done.finish(Err(e)),
                };
                let at = hole.at;
                let (w, h) = (at.width() as usize, at.height() as usize);
                ctx.progress(0.0, label);
                // What ran, as edit.inpaintAi reports it: the device read after the run (a GPU
                // failure mid-run falls back to the CPU), the model and the time it took.
                let started = web_time::Instant::now();
                let svc2 = svc.clone();
                let window = InWindow(Arc::new(std::sync::Mutex::new(Window { hole, plan: wplan, p: wp, out: out.clone(), mode })));
                let tile = photocraft_geom::TILE_SIZE;
                let phase = (at.x0.rem_euclid(tile) as usize, at.y0.rem_euclid(tile) as usize);
                let work = crate::ai_inpaint_cmds::work::Work::new(w, h, phase, Box::new(window.clone()), Box::new(window.clone()));
                crate::ai_inpaint_cmds::ai_fill_async(
                    svc,
                    ctx.clone(),
                    work,
                    opts,
                    Box::new(move |r| {
                        let ran = r.as_ref().map(|run| AiRun::new(&svc2.device(), &device, *run, started)).ok();
                        let filled = r.and_then(|_| {
                            let st = Arc::try_unwrap(window.0).map_err(|_| EngineError::Other("AI fill: the window is still in use".into()))?;
                            let Window { mode, out: written, .. } = st.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
                            drop(out);
                            Ok((Some((mode, crate::ai_inpaint_cmds::take_out(written)?, at)), ran))
                        });
                        done.finish(filled);
                    }),
                );
            },
            move |s, (done, ran): (Option<(AiOut, photocraft_raster::Surface, Rect)>, Option<AiRun>)| {
                let mut v = apply_ai(s, label, id, &p, done)?;
                if let (Some(o), Some(ran)) = (v.as_object_mut(), ran) {
                    ran.report(o);
                }
                Ok(v)
            },
        );
    }
    crate::jobs::run(
        s,
        label,
        true,
        {
            let p = p.clone();
            move |ctx| {
                ctx.progress(0.0, label);
                let mut work = (*doc).clone();
                run_remove(&mut work, &plan, &p, ctx, label)
            }
        },
        move |s, filled| apply_fill(s, label, id, &p, new_layer, filled),
    )
}
