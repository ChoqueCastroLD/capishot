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

use super::content_aware_move::{Target, replace};
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

/// The least margin around Remove with AI's strokes: the model's context (128 px) past the
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
    /// Where the fill samples from: the strokes plus a margin, on the canvas.
    window: Rect,
    close_loops: bool,
    all_layers: bool,
    /// The fill goes to a new layer above the target instead of into it.
    new_layer: bool,
    /// `ai`: fill with Manga LaMa through the injected service instead of patch completion.
    ai: Option<(std::sync::Arc<dyn photocraft_ai_lama::service::Inpainter>, crate::ai_inpaint_cmds::AiOptions)>,
    /// Remove with AI: the hole grows over the ink the strokes touch, and the fill feathers in
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
fn parse_strokes(s: &Session, p: &Value) -> Result<(Vec<Stroke>, Option<LayerId>)> {
    let Some(list) = p.get("strokes") else {
        let (stroke, id) = parse_brush(s, p, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        return Ok((vec![stroke], id));
    };
    const ENTRY: &str = "each `strokes` entry is a point list or {\"points\", \"size\"?, \"hardness\"?, \"spacing\"?}";
    let list = list.as_array().ok_or_else(|| bad(CMD, ENTRY))?;
    if list.is_empty() {
        return Err(bad(CMD, "`strokes` is empty"));
    }
    let mut strokes = Vec::with_capacity(list.len());
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
                o.insert("points".into(), points.clone());
                for k in ["size", "hardness", "spacing"] {
                    if let Some(v) = e.get(k) {
                        o.insert(k.into(), v.clone());
                    }
                }
            }
            points => {
                o.insert("points".into(), points.clone());
            }
        }
        let (stroke, layer) = parse_brush(s, &one, CMD)?;
        reject_huge_stroke(CMD, &stroke)?;
        strokes.push(stroke);
        id = layer;
    }
    Ok((strokes, id))
}

fn plan(s: &Session, p: &Value, ink: bool) -> Result<Plan> {
    let (strokes, id) = parse_strokes(s, p)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let canvas = d.doc.bounds();
    let bounds = strokes.iter().map(stroke_bounds).reduce(|a, b| a.union(&b)).ok_or_else(|| bad(CMD, "`strokes` is empty"))?;
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
        let opts = crate::ai_inpaint_cmds::ai_options(s, p, CMD)?;
        Some((crate::ai_inpaint_cmds::ready_inpainter(s, &opts)?, opts))
    } else {
        None
    };
    Ok(Plan { id, strokes, window, close_loops, all_layers, new_layer, ai, ink })
}

/// The strokes' footprints and their enclosed areas over the plan's window.
fn painted(plan: &Plan) -> Result<Vec<bool>> {
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let mut m = vec![false; w * h];
    for stroke in &plan.strokes {
        let (bounds, cov) = super::coverage(CMD, stroke)?;
        let bw = bounds.width() as usize;
        for (i, c) in cov.iter().enumerate() {
            let (x, y) = (bounds.x0 + (i % bw.max(1)) as i32, bounds.y0 + (i / bw.max(1)) as i32);
            if *c > 0.0 && win.contains(x, y) {
                let j = (y - win.y0) as usize * w + (x - win.x0) as usize;
                if let Some(v) = m.get_mut(j) {
                    *v = true;
                }
            }
        }
    }
    if plan.close_loops {
        m = photocraft_algo::remove::close_loops(w, h, &m);
    }
    Ok(m)
}

/// The hole and its paste-back weights (`None`: all 1) over the plan's window: the painted area
/// grown by [`GROW`] (or by the ink it touches, see [`Plan::ink`]), limited to the selection.
/// Empty when nothing would change.
#[allow(clippy::type_complexity)]
fn hole(plan: &Plan, doc: &Document, img: &Region, fmt: &photocraft_color::PixelFormat) -> Result<(Vec<bool>, Option<Vec<f32>>, Option<Vec<bool>>)> {
    let win = plan.window;
    let (w, h) = (win.width() as usize, win.height() as usize);
    let painted = painted(plan)?;
    let (mut m, mut weight, hide) = if plan.ink {
        let page = doc.bounds();
        let brush = plan.strokes.iter().map(|s| s.brush.size).fold(0.0, f32::max);
        let g = super::ink_mask::Grow::for_page(page.width().min(page.height()), brush);
        let lum = super::ink_mask::luminance(&crate::ai_inpaint_cmds::to_rgb(fmt, &img.data));
        let hole = super::ink_mask::expand(w, h, &lum, &painted, g);
        (hole.mask, Some(hole.weight), Some(hole.hide))
    } else {
        (photocraft_algo::remove::dilate(w, h, &painted, GROW), None, None)
    };
    if let Some(sel) = doc.selection.as_ref() {
        for (i, c) in coverage(sel, win).into_iter().enumerate() {
            if c <= 0.0 {
                m[i] = false;
                if let Some(wt) = weight.as_mut() {
                    wt[i] = 0.0;
                }
            }
        }
    }
    Ok((m, weight, hide))
}

/// The window's pixels and the hole, read on `doc` (a snapshot) before the fill.
struct Prepared {
    img: Region,
    fmt: photocraft_color::PixelFormat,
    hole: Vec<bool>,
    /// The paste-back weights over the hole (`None`: a hard edge).
    weight: Option<Vec<f32>>,
    /// What the model doesn't see besides the hole (Remove with AI: the letters around it).
    hide: Option<Vec<bool>>,
}

/// Reads what the fill needs. `None` when the layer's own window is fully transparent: nothing to
/// remove and nothing to fill from.
fn prepare(doc: &mut Document, plan: &Plan, p: &Value) -> Result<Option<Prepared>> {
    let win = plan.window;
    let (img, fmt) = {
        let (surf, _) = crate::channel_cmds::target_surface(doc, plan.id, p)?;
        let fmt = surf.format();
        let img = if plan.all_layers { composite_region(doc, None, SampleLayers::All, win, fmt) } else { Region::read(surf, win) };
        (img, fmt)
    };
    let (hole, weight, hide) = hole(plan, doc, &img, &fmt)?;
    if !hole.iter().any(|m| *m) {
        return Err(bad(CMD, "the stroke doesn't cover anything that can change (check the selection)"));
    }
    if hole.iter().all(|m| *m) {
        return Err(bad(CMD, "nothing is left around the stroke to fill it from"));
    }
    // A no-op without the fill or a history step, as the other retouch tools on an empty layer.
    if !plan.all_layers && alpha_index(&fmt).is_some_and(|a| img.data.chunks_exact(img.ch).all(|px| px.get(a).is_none_or(|v| *v <= 0.0))) {
        return Ok(None);
    }
    Ok(Some(Prepared { img, fmt, hole, weight, hide }))
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
    let n = fmt.channels();
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
    let mut l = Layer::raster(doc.next_layer_name("Layer"), fmt);
    crate::pixels_mut(&mut l)?.write_region(out.rect, &data);
    // Above the sampled layer and in its clipping mask, like Layer via Copy.
    let nid = crate::layer_place::insert_new(doc, id, l, crate::layer_place::Place::Above);
    Ok((out.rect, nid))
}

/// The window with the AI's RGB `generated` in the hole (alpha kept), in the layer's format,
/// blended by the hole's paste-back weights.
fn encode_ai(prep: Prepared, rect: Rect, generated: &[f32]) -> Result<(Region, Vec<bool>)> {
    let Prepared { img, fmt, hole, weight, .. } = prep;
    let n = fmt.channels().max(1);
    if generated.len() != hole.len() * 3 || img.data.len() != hole.len() * n {
        return Err(EngineError::Other("AI fill: window size mismatch".into()));
    }
    // Remove with AI: a bubble's fill takes the bubble's white exactly.
    let settled;
    let generated = if weight.is_some() {
        let mut g = generated.to_vec();
        super::ink_mask::settle_paper(rect.width() as usize, rect.height() as usize, &crate::ai_inpaint_cmds::to_rgb(&fmt, &img.data), &hole, &mut g);
        settled = g;
        &settled[..]
    } else {
        generated
    };
    let mut data = img.data.clone();
    let mut enc = [0.0f32; 8];
    for (i, m) in hole.iter().enumerate() {
        if *m {
            let orig = to_rgba(&fmt, &img.data[i * n..(i + 1) * n]);
            let g = &generated[i * 3..i * 3 + 3];
            let k = weight.as_ref().and_then(|wt| wt.get(i)).copied().unwrap_or(1.0);
            let mix = |c: usize| orig[c] + (g[c] - orig[c]) * k;
            from_rgba_into(&fmt, [mix(0), mix(1), mix(2), orig[3]], &mut enc);
            data[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
        }
    }
    clamp_samples(&fmt, &mut data);
    Ok((Region { rect, ch: img.ch, data }, hole))
}

/// Applies a filled window to the live document as one undo step (into the target, or onto a new
/// layer above it with `new_layer`).
fn apply_fill(s: &mut Session, label: &'static str, id: Option<LayerId>, p: &Value, new_layer: bool, filled: Option<(Region, Vec<bool>)>) -> Result<Value> {
    let Some((out, hole)) = filled else { return Ok(json!({"changed": false})) };
    if new_layer {
        let (damage, nid) = s.edit(label, |doc, active| {
            let r = fill_to_new_layer(doc, id, &out, &hole)?;
            *active = Some(r.1);
            Ok(r)
        })?;
        s.hint_damage(damage);
        return Ok(json!({ "damage": damage_json(damage), "layer": nid.0 }));
    }
    let damage = s.edit(label, |doc, _| replace(doc, Target { id, all_layers: false }, p, &out, &hole))?;
    s.hint_damage(damage);
    Ok(json!({ "damage": damage_json(damage) }))
}

pub(super) fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    run_labelled(s, p, "Remove", false)
}

/// Remove with AI's strokes: [`remove`] (in its AI mode) as one undo step named `label`, the hole
/// grown over the ink the strokes touch and feathered in ([`super::ink_mask`]).
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
        let mut work = (*doc).clone();
        let Some(prep) = prepare(&mut work, &plan, &p)? else { return Ok(json!({"changed": false})) };
        let win = plan.window;
        let (w, h) = (win.width() as usize, win.height() as usize);
        let rgb = crate::ai_inpaint_cmds::to_rgb(&prep.fmt, &prep.img.data);
        let cover: Vec<f32> = prep.hole.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect();
        let hidden = prep.hide.as_ref().map(|hide| prep.hole.iter().zip(hide).map(|(m, x)| if *m || *x { 1.0 } else { 0.0 }).collect());
        return crate::jobs::run_async(
            s,
            label,
            true,
            move |ctx, done| {
                ctx.progress(0.0, label);
                crate::ai_inpaint_cmds::ai_fill_rgb_async(
                    svc,
                    ctx.clone(),
                    rgb,
                    cover,
                    hidden,
                    w,
                    h,
                    opts,
                    Box::new(move |r| done.finish(r.and_then(|(generated, _)| encode_ai(prep, win, &generated)).map(Some))),
                );
            },
            move |s, filled| apply_fill(s, label, id, &p, new_layer, filled),
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
