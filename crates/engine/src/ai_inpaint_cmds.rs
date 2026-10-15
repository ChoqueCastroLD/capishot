//! Edit › Clean with AI (`edit.inpaintAi`): erase what the selection covers (text, SFX,
//! watermarks) and fill it with plausible manga background and screentone, using Manga LaMa
//! (`photocraft-ai-lama`). The Remove tool's `ai` option runs the same fill on its brushed hole.
//!
//! The engine only plans and pastes ([`photocraft_ai_lama::tiles`]: one crop per group of
//! masked blobs with context margin, at most `maxSide` pixels a side, resized when larger); the
//! model runs behind the injected [`Inpainter`] service ([`Session::set_inpainter`]): natively
//! `photocraft_ai_lama::native::NativeInpainter` on wgpu or the CPU, in the browser the separately
//! loaded AI worker module (the shared AI runtime). Without a service the command fails with
//! [`UNAVAILABLE`].
//!
//! It runs as a background job (progress per tile, cancellable), and its result lands in one undo
//! step: by default on a new layer above (masked pixels only, alpha = selection coverage; the
//! original stays untouched), or on the layer itself (`output: "current"`). Only pixels the
//! selection covers change; soft selection edges blend.

use std::sync::Arc;
use std::sync::mpsc;

use photocraft_ai_lama::LamaError;
use photocraft_ai_lama::service::{Hooks, Inpainter, ModelState, Request};
use photocraft_ai_lama::tiles::{self, TileOptions};
use photocraft_color::PixelFormat;
use photocraft_doc::{Layer, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::jobs::JobCtx;
use crate::{EngineError, Result, Session};

/// The command id.
pub const ID: &str = "edit.inpaintAi";
const LABEL: &str = "Clean with AI";
/// Why the command fails in a build without an inpainting service.
pub const UNAVAILABLE: &str = "AI cleaning (Manga LaMa) is not available in this build";
/// Largest region (selection plus context) read for one run.
const MAX_PIXELS: u64 = 64 << 20;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

impl Session {
    /// Injects (or removes) the service that runs Manga LaMa.
    pub fn set_inpainter(&mut self, inpainter: Option<Arc<dyn Inpainter>>) {
        self.inpainter = inpainter;
    }

    /// The injected inpainting service, if any.
    pub fn inpainter(&self) -> Option<&Arc<dyn Inpainter>> {
        self.inpainter.as_ref()
    }
}

/// Validated `maxSide`, `margin` and `allowDownload`.
#[derive(Clone)]
pub(crate) struct AiOptions {
    pub tiles: TileOptions,
    pub allow_download: bool,
}

pub(crate) fn ai_options(p: &Value, cmd: &str) -> Result<AiOptions> {
    let bad = |m: String| EngineError::BadParams { cmd: cmd.into(), msg: m };
    let uint = |k: &str, d: u64, lo: u64, hi: u64| -> Result<u64> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(d),
            Some(v) => v.as_u64().filter(|x| (lo..=hi).contains(x)).ok_or_else(|| bad(format!("`{k}` must be an integer in {lo}..={hi}"))),
        }
    };
    let max_side = uint("maxSide", 512, 64, 2048)?;
    if !max_side.is_multiple_of(8) {
        return Err(bad("`maxSide` must be a multiple of 8".into()));
    }
    let margin = uint("margin", 64, 0, 1024)?;
    let allow_download = match p.get("allowDownload") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("`allowDownload` must be true or false".into()))?,
    };
    Ok(AiOptions { tiles: TileOptions { max_side: max_side as usize, margin: margin as usize }, allow_download })
}

/// The service, if it can run now (else the reason, for the user).
pub(crate) fn ready_inpainter(s: &Session, allow_download: bool) -> Result<Arc<dyn Inpainter>> {
    let svc = s.inpainter.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    match svc.model_state() {
        ModelState::Ready => Ok(svc),
        ModelState::NeedsDownload { .. } if allow_download => Ok(svc),
        // The browser's cache answers asynchronously; with consent the worker finds out itself.
        ModelState::Checking if allow_download => Ok(svc),
        ModelState::NeedsDownload { bytes } => Err(EngineError::Other(format!(
            "Manga LaMa needs a one-time download of {} MB first (run again with \"allowDownload\": true)",
            bytes.div_ceil(1 << 20)
        ))),
        ModelState::Checking => Err(EngineError::Other("still checking for the Manga LaMa model; try again in a moment".into())),
        ModelState::Unavailable(why) => Err(EngineError::Other(why)),
    }
}

fn lama_err(e: LamaError) -> EngineError {
    match e {
        LamaError::Cancelled => EngineError::Cancelled,
        other => EngineError::Other(format!("Manga LaMa: {other}")),
    }
}

/// Maps a request's progress onto `lo..hi` of the job.
struct JobHooks {
    ctx: JobCtx,
    lo: f32,
    hi: f32,
    status: String,
}

impl Hooks for JobHooks {
    fn progress(&self, fraction: f32, status: &str) {
        let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
        self.ctx.progress(self.lo + (self.hi - self.lo) * f, if status.is_empty() { &self.status } else { status });
    }
    fn cancelled(&self) -> bool {
        self.ctx.cancelled()
    }
}

/// Runs one request and waits for its `done` (the job's worker thread may block; the
/// single-threaded browser build can't, so there only a synchronous answer works).
fn run_blocking(svc: &Arc<dyn Inpainter>, req: Request, hooks: Arc<JobHooks>) -> Result<Vec<f32>> {
    let ctx = hooks.ctx.clone();
    let (tx, rx) = mpsc::channel();
    svc.run(req, hooks, Box::new(move |r| drop(tx.send(r))));
    if cfg!(all(target_arch = "wasm32", not(target_feature = "atomics"))) {
        return match rx.try_recv() {
            Ok(r) => r.map_err(lama_err),
            Err(_) => Err(EngineError::Other(format!("{UNAVAILABLE} (it needs the multithreaded web build)"))),
        };
    }
    let mut waited_cancelled = 0u32;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(r) => return r.map_err(lama_err),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(EngineError::Other("Manga LaMa: the AI service dropped the request".into())),
            Err(mpsc::RecvTimeoutError::Timeout) if ctx.cancelled() => {
                // The service should answer Cancelled promptly; don't hang on one that doesn't.
                waited_cancelled += 1;
                if waited_cancelled > 40 {
                    return Err(EngineError::Cancelled);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

/// The AI fill of a `w×h` region: `rgb` interleaved RGB 0..=1, `cover` the hole's coverage.
/// Returns the generated RGB for every pixel (equal to `rgb` outside the hole), with job progress
/// over `lo..hi`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ai_fill_rgb(
    svc: &Arc<dyn Inpainter>,
    ctx: &JobCtx,
    rgb: &[f32],
    cover: &[f32],
    w: usize,
    h: usize,
    opts: &AiOptions,
    (lo, hi): (f32, f32),
) -> Result<Vec<f32>> {
    let plan = tiles::plan(cover, w, h, &opts.tiles).map_err(|e| bad(e.to_string()))?;
    if plan.is_empty() {
        return Err(bad("nothing to clean: the area is empty"));
    }
    let binary: Vec<f32> = cover.iter().map(|c| if *c > 0.0 { 1.0 } else { 0.0 }).collect();
    let mut generated = rgb.to_vec();
    let n = plan.len() as f32;
    for (i, t) in plan.iter().enumerate() {
        ctx.check()?;
        let input = tiles::extract(rgb, cover, w, h, t).map_err(|e| bad(e.to_string()))?;
        let (a, b) = (lo + (hi - lo) * i as f32 / n, lo + (hi - lo) * (i + 1) as f32 / n);
        let status = if plan.len() > 1 { format!("{LABEL} ({} of {})", i + 1, plan.len()) } else { LABEL.to_string() };
        let hooks = Arc::new(JobHooks { ctx: ctx.clone(), lo: a, hi: b, status });
        let out = run_blocking(svc, Request { tile: input, allow_download: opts.allow_download }, hooks)?;
        tiles::paste(&mut generated, &binary, w, h, t, &out).map_err(lama_err)?;
        ctx.progress(b, "");
    }
    Ok(generated)
}

/// [`ai_fill_rgb`] without blocking: each tile's request starts from the previous one's `done`,
/// and `finish` gets the generated RGB. Works with an inpainter that answers later (the
/// browser's AI worker), so `edit.inpaintAi` runs everywhere through `jobs::run_async`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ai_fill_rgb_async(
    svc: Arc<dyn Inpainter>,
    ctx: JobCtx,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    w: usize,
    h: usize,
    opts: AiOptions,
    finish: Box<dyn FnOnce(Result<Vec<f32>>) + Send>,
) {
    let plan = match tiles::plan(&cover, w, h, &opts.tiles) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return finish(Err(bad("nothing to clean: the area is empty"))),
        Err(e) => return finish(Err(bad(e.to_string()))),
    };
    let binary: Vec<f32> = cover.iter().map(|c| if *c > 0.0 { 1.0 } else { 0.0 }).collect();
    let generated = rgb.clone();
    let state = FillState { svc, ctx, rgb, cover, binary, generated, w, h, allow_download: opts.allow_download, plan, next: 0, finish };
    fill_step(state);
}

struct FillState {
    svc: Arc<dyn Inpainter>,
    ctx: JobCtx,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    binary: Vec<f32>,
    generated: Vec<f32>,
    w: usize,
    h: usize,
    allow_download: bool,
    plan: Vec<tiles::Tile>,
    next: usize,
    finish: Box<dyn FnOnce(Result<Vec<f32>>) + Send>,
}

fn fill_step(mut st: FillState) {
    let Some(t) = st.plan.get(st.next).cloned() else {
        let FillState { generated, finish, .. } = st;
        return finish(Ok(generated));
    };
    if let Err(e) = st.ctx.check() {
        return (st.finish)(Err(e));
    }
    let input = match tiles::extract(&st.rgb, &st.cover, st.w, st.h, &t) {
        Ok(i) => i,
        Err(e) => return (st.finish)(Err(bad(e.to_string()))),
    };
    let n = st.plan.len() as f32;
    let i = st.next;
    let (a, b) = (i as f32 / n, (i + 1) as f32 / n);
    let status = if st.plan.len() > 1 { format!("{LABEL} ({} of {})", i + 1, st.plan.len()) } else { LABEL.to_string() };
    let hooks = Arc::new(JobHooks { ctx: st.ctx.clone(), lo: a, hi: b, status });
    let svc = st.svc.clone();
    let req = Request { tile: input, allow_download: st.allow_download };
    svc.run(
        req,
        hooks,
        Box::new(move |r| {
            let out = match r {
                Ok(out) => out,
                Err(e) => return (st.finish)(Err(lama_err(e))),
            };
            if let Err(e) = tiles::paste(&mut st.generated, &st.binary, st.w, st.h, &t, &out) {
                return (st.finish)(Err(lama_err(e)));
            }
            st.ctx.progress(b, "");
            st.next += 1;
            fill_step(st);
        }),
    );
}

/// The Remove tool's AI mode: fills the `hole` of a `w×h` window whose pixels `data` are in
/// `fmt`, returning the window in `fmt` with only hole pixels changed (alpha kept).
#[allow(clippy::too_many_arguments)]
pub(crate) fn ai_fill_encoded(
    svc: &Arc<dyn Inpainter>,
    ctx: &JobCtx,
    fmt: &PixelFormat,
    data: &[f32],
    hole: &[bool],
    w: usize,
    h: usize,
    opts: &AiOptions,
) -> Result<Vec<f32>> {
    let n = fmt.channels().max(1);
    if data.len() != w * h * n || hole.len() != w * h {
        return Err(EngineError::Other("AI fill: window size mismatch".into()));
    }
    let rgb = to_rgb(fmt, data);
    let cover: Vec<f32> = hole.iter().map(|m| if *m { 1.0 } else { 0.0 }).collect();
    let generated = ai_fill_rgb(svc, ctx, &rgb, &cover, w, h, opts, (0.0, 1.0))?;
    let mut out = data.to_vec();
    let mut enc = [0.0f32; 8];
    for (i, m) in hole.iter().enumerate() {
        if *m {
            let a = to_rgba(fmt, &data[i * n..(i + 1) * n])[3];
            let g = &generated[i * 3..i * 3 + 3];
            from_rgba_into(fmt, [g[0], g[1], g[2], a], &mut enc);
            out[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
        }
    }
    Ok(out)
}

/// Interleaved RGB (0..=1) of pixels in `fmt`.
pub(crate) fn to_rgb(fmt: &PixelFormat, px: &[f32]) -> Vec<f32> {
    let n = fmt.channels().max(1);
    let mut out = Vec::with_capacity(px.len() / n * 3);
    for p in px.chunks_exact(n) {
        let c = to_rgba(fmt, p);
        out.extend(c[..3].iter().map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }));
    }
    out
}

// ------------------------------------------------------------------ edit.inpaintAi

/// Everything the job needs, checked before it starts.
struct Plan {
    layer: LayerId,
    fmt: PixelFormat,
    region: Rect,
    layer_px: Vec<f32>,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    new_layer: bool,
    opts: AiOptions,
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if s.inpainter.is_none() {
        return Err(UNAVAILABLE.into());
    }
    crate::edit_menu_cmds::pixel_layer(s)?;
    let d = s.active().ok_or("no document open")?;
    if d.doc.selection.as_ref().is_none_or(|m| m.content_bounds().is_empty()) {
        return Err("select the text or object to clean".into());
    }
    Ok(())
}

fn plan(s: &Session, p: &Value) -> Result<Plan> {
    if !p.is_object() && !p.is_null() {
        return Err(bad("params must be a JSON object"));
    }
    let id = match p.get("layer").filter(|v| !v.is_null()) {
        Some(v) => {
            let l = v.as_u64().ok_or_else(|| bad("`layer` must be a layer id"))?;
            crate::edit_menu_cmds::pixel_layer_id(s, LayerId(l))
        }
        None => crate::edit_menu_cmds::pixel_layer(s),
    }
    .map_err(EngineError::Other)?;
    let new_layer = match p.get("output").and_then(Value::as_str) {
        None => match p.get("output") {
            None | Some(Value::Null) => true,
            Some(_) => return Err(bad("`output` must be \"new\" or \"current\"")),
        },
        Some("new") => true,
        Some("current") => false,
        Some(o) => return Err(bad(format!("unknown `output` {o:?} (new|current)"))),
    };
    let opts = ai_options(p, ID)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &d.doc;
    let canvas = doc.bounds();
    let sel = doc.selection.as_ref().ok_or_else(|| bad("make a selection around what to clean"))?;
    let sb = sel.content_bounds().intersect(&canvas);
    if sb.is_empty() {
        return Err(bad("the selection is outside the canvas"));
    }
    // Context around the selection: what every blob's tile can reach (`tiles::plan` grows each
    // blob by max(margin, its long side / 4)).
    let reach = (opts.tiles.margin as u64).max(u64::from(sb.width().max(sb.height())) / 4).min(1 << 20) as i32;
    let region = sb.inflate(reach).intersect(&canvas);
    let pixels = u64::from(region.width()) * u64::from(region.height());
    if pixels > MAX_PIXELS {
        return Err(bad(format!("the area is too large to clean at once ({pixels} pixels, at most {MAX_PIXELS}); select less")));
    }
    let (w, h) = (region.width() as usize, region.height() as usize);
    let surf = doc.layer(id).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(id))?;
    let fmt = surf.format();
    let layer_px = surf.read_region(region);
    if layer_px.len() != w * h * fmt.channels() {
        return Err(EngineError::Other("could not read the layer's pixels".into()));
    }
    let sc = sel.channels().max(1);
    let mut cover: Vec<f32> = sel.read_region(region).chunks_exact(sc).map(|p| if p[0].is_finite() { p[0].clamp(0.0, 1.0) } else { 0.0 }).collect();
    cover.resize(w * h, 0.0);
    if !cover.iter().any(|c| *c > 0.0) {
        return Err(bad("the selection doesn't cover any pixel"));
    }
    let rgb = to_rgb(&fmt, &layer_px);
    Ok(Plan { layer: id, fmt, region, layer_px, rgb, cover, new_layer, opts })
}

fn inpaint_ai(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = plan(s, p)?;
    let svc = ready_inpainter(s, plan.opts.allow_download)?;
    let device = svc.device();
    crate::jobs::run_async(
        s,
        LABEL,
        true,
        move |ctx, done| {
            ctx.progress(0.0, LABEL);
            let (w, h) = (plan.region.width() as usize, plan.region.height() as usize);
            let (rgb, cover, opts) = (plan.rgb.clone(), plan.cover.clone(), plan.opts.clone());
            let svc2 = svc.clone();
            ai_fill_rgb_async(
                svc,
                ctx.clone(),
                rgb,
                cover,
                w,
                h,
                opts,
                Box::new(move |r| done.finish(r.map(|generated| (plan, generated, svc2.device())))),
            );
        },
        move |s, (plan, generated, dev)| apply(s, plan, &generated, if dev.is_empty() { device } else { dev }),
    )
}

fn apply(s: &mut Session, plan: Plan, generated: &[f32], device: String) -> Result<Value> {
    let Plan { layer: id, fmt, region, layer_px, cover, new_layer, .. } = plan;
    let n = fmt.channels();
    let count = cover.iter().filter(|c| **c > 0.0).count();
    let layer = s.edit(LABEL, |doc, active| {
        if new_layer {
            let mut l = Layer::raster(doc.next_layer_name("AI Clean"), PixelFormat { alpha: true, ..fmt });
            let lf = l.surface().map(|s| s.format()).unwrap_or(PixelFormat { alpha: true, ..fmt });
            let ln = lf.channels();
            let mut px = vec![0.0f32; cover.len() * ln];
            let mut enc = [0.0f32; 8];
            for (i, &c) in cover.iter().enumerate() {
                if c > 0.0 {
                    let g = &generated[i * 3..i * 3 + 3];
                    from_rgba_into(&lf, [g[0], g[1], g[2], c], &mut enc);
                    px[i * ln..(i + 1) * ln].copy_from_slice(&enc[..ln]);
                }
            }
            if let Some(surf) = l.surface_mut() {
                surf.write_region(region, &px);
                surf.prune();
            }
            let nid = doc.insert_above(Some(id), l);
            *active = Some(nid);
            return Ok(nid);
        }
        let surf = crate::edit_menu_cmds::writable_surface(doc, id)?;
        let mut out = layer_px.clone();
        let mut enc = [0.0f32; 8];
        for (i, &c) in cover.iter().enumerate() {
            if c > 0.0 {
                let o = to_rgba(&fmt, &layer_px[i * n..(i + 1) * n]);
                let g = &generated[i * 3..i * 3 + 3];
                let mix = |k: usize| o[k] + (g[k] - o[k]) * c;
                from_rgba_into(&fmt, [mix(0), mix(1), mix(2), o[3]], &mut enc);
                out[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
            }
        }
        surf.write_region(region, &out);
        Ok(id)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(region);
    }
    Ok(json!({
        "layer": layer.0,
        "pixels": count,
        "region": [region.x0, region.y0, region.width(), region.height()],
        "device": device,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Clean with AI",
        menu: &["Edit"],
        shortcut: None,
        params: r#"{"output"?: "new" (default: the result on a new layer above, alpha = selection coverage) | "current" (paint it into the layer), "layer"?: layer id (default: the active pixel layer), "maxSide"?: 64..2048, multiple of 8 (default 512: larger crops are downscaled to this working size), "margin"?: 0..1024 px of context around each masked blob (default 64), "allowDownload"?: bool (fetch the Manga LaMa weights, about 100 MB, when not cached; default false)} -> {"layer", "pixels", "region": [x,y,w,h], "device"}. Erases what the selection covers (text, SFX) and fills it with Manga LaMa. Background job, one undo step."#,
        enabled,
        run: inpaint_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_inpaint_cmds/tests.rs"]
mod tests;
