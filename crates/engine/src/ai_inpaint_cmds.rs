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
//!
//! `quality` (auto, fast, best) and `device` (auto, gpu, cpu) choose between Manga LaMa and the
//! lite MI-GAN (512 px tiles, a fraction of the work: phones, weak machines, CPUs), as
//! `image.upscaleAi` does; `model` names one explicitly. `threads` and `keepLoaded` are Advanced
//! settings, like `maxSide` and `margin`.

use std::sync::Arc;

use photocraft_ai::catalog::{self, ModelInfo};
use photocraft_ai_lama::LamaError;
use photocraft_ai_lama::service::{Hooks, Inpainter, ModelState, Request};
use photocraft_ai_lama::tiles::{self, TileOptions};
use photocraft_color::PixelFormat;
use photocraft_doc::{Layer, LayerId};
use photocraft_geom::Rect;
use photocraft_raster::{from_rgba_into, to_rgba};
use serde_json::{Value, json};

use crate::ai_models_cmds::not_downloaded;
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

/// Validated model, device, `maxSide`, `margin`, `threads`, `keepLoaded` and `allowDownload`.
#[derive(Clone)]
pub(crate) struct AiOptions {
    pub tiles: TileOptions,
    pub allow_download: bool,
    pub model: &'static ModelInfo,
    pub run: crate::ai_cmds::RunChoice,
    pub threads: usize,
    pub unload: bool,
    /// The params named the model (no automatic switch to the lite one after a GPU failure).
    pub pinned: bool,
}

impl AiOptions {
    fn request(&self, tile: tiles::TileInput) -> Request {
        Request { tile, allow_download: self.allow_download, model: self.model.id, cpu: self.run.choice.cpu, threads: self.threads, unload: self.unload }
    }
}

/// The inpainting model these params choose: `model` (an inpainting catalog id), else Manga
/// LaMa or, when `quality`/`device` resolve to lite, MI-GAN.
pub fn model_for(s: &Session, p: &Value, cmd: &str) -> Result<(&'static ModelInfo, crate::ai_cmds::RunChoice)> {
    let run = crate::ai_cmds::run_choice(s, p, cmd)?;
    let id = match p.get("model").and_then(Value::as_str).unwrap_or("auto") {
        "auto" | "" => {
            if run.choice.lite {
                catalog::MIGAN_ID
            } else {
                catalog::LAMA_ID
            }
        }
        id => id,
    };
    let model = catalog::model(id).filter(|m| m.content == catalog::Content::Inpaint).ok_or_else(|| EngineError::BadParams {
        cmd: cmd.into(),
        msg: format!("unknown inpainting model `{id}` ({} or {})", catalog::LAMA_ID, catalog::MIGAN_ID),
    })?;
    Ok((model, run))
}

/// The model these params choose and whether it can run now (the UI's download prompt).
pub fn model_state(s: &Session, p: &Value) -> Option<(&'static ModelInfo, ModelState)> {
    let (model, _) = model_for(s, p, ID).ok()?;
    Some((model, s.inpainter.as_ref()?.model_state_of(model.id)))
}

pub(crate) fn ai_options(s: &Session, p: &Value, cmd: &str) -> Result<AiOptions> {
    let bad = |m: String| EngineError::BadParams { cmd: cmd.into(), msg: m };
    let (model, run) = model_for(s, p, cmd)?;
    let uint = |k: &str, d: u64, lo: u64, hi: u64| -> Result<u64> { Ok(crate::ai_cmds::uint_param(p, k, lo, hi, cmd)?.unwrap_or(d)) };
    // MI-GAN works on at most 512 × 512; LaMa takes smaller crops on machines with little memory.
    let lite = model.arch == catalog::Arch::Migan;
    let default_side = if lite || !run.choice.small_tiles { 512 } else { 384 };
    let max_side = uint("maxSide", default_side, 64, 2048)?;
    if !max_side.is_multiple_of(8) {
        return Err(bad("`maxSide` must be a multiple of 8".into()));
    }
    let max_side = if lite { max_side.min(512) } else { max_side };
    let margin = uint("margin", 64, 0, 1024)?;
    let allow_download = match p.get("allowDownload") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("`allowDownload` must be true or false".into()))?,
    };
    let threads = uint("threads", 0, 0, 64)? as usize;
    let unload = !crate::ai_cmds::keep_loaded(p, cmd)?;
    let pinned = !matches!(p.get("model").and_then(Value::as_str).unwrap_or("auto"), "auto" | "");
    Ok(AiOptions { tiles: TileOptions { max_side: max_side as usize, margin: margin as usize }, allow_download, model, run, threads, unload, pinned })
}

/// The service, if it can run `opts.model` now (else the reason, for the user).
pub(crate) fn ready_inpainter(s: &Session, opts: &AiOptions) -> Result<Arc<dyn Inpainter>> {
    let svc = s.inpainter.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    let (title, allow_download) = (opts.model.title, opts.allow_download);
    match svc.model_state_of(opts.model.id) {
        ModelState::Ready => Ok(svc),
        ModelState::NeedsDownload { .. } if allow_download => Ok(svc),
        // The browser's cache answers asynchronously: run, and a missing model fails the run
        // with the same "isn't downloaded" error.
        ModelState::Checking => Ok(svc),
        ModelState::NeedsDownload { bytes } => Err(EngineError::Other(not_downloaded(title, bytes, "allowDownload"))),
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

/// Receives the generated RGB and the model that made it.
pub(crate) type FillDone = Box<dyn FnOnce(Result<(Vec<f32>, &'static ModelInfo)>) + Send>;

/// The AI fill of a `w×h` region (`rgb` interleaved RGB 0..=1, `cover` the hole's coverage,
/// `hidden` what the model doesn't see when more than the hole: [`super::retouch_cmds`]'s letters
/// around it),
/// without blocking: each tile's request starts from the previous one's `done`, and `finish`
/// gets the generated RGB for every pixel (equal to `rgb` outside the hole) and the model that
/// made it (another than `opts.model` after a GPU failure, see [`gpu_fallback`]). Works with an
/// inpainter that answers later (the browser's AI worker, which no thread of the page may wait
/// for), so `edit.inpaintAi` and the Remove tool's AI mode run everywhere through
/// `jobs::run_async`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn ai_fill_rgb_async(
    svc: Arc<dyn Inpainter>,
    ctx: JobCtx,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    hidden: Option<Vec<f32>>,
    w: usize,
    h: usize,
    opts: AiOptions,
    finish: FillDone,
) {
    let plan = match tiles::plan(&cover, w, h, &opts.tiles) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return finish(Err(bad("nothing to clean: the area is empty"))),
        Err(e) => return finish(Err(bad(e.to_string()))),
    };
    let binary: Vec<f32> = cover.iter().map(|c| if *c > 0.0 { 1.0 } else { 0.0 }).collect();
    let generated = rgb.clone();
    let input = hidden.filter(|m| m.len() == cover.len()).unwrap_or_else(|| cover.clone());
    let state = FillState { svc, ctx, rgb, cover, input, binary, generated, w, h, opts, plan, next: 0, finish };
    fill_step(state);
}

struct FillState {
    svc: Arc<dyn Inpainter>,
    ctx: JobCtx,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    /// The model's input mask: `cover`, or more (hidden from the model, not filled).
    input: Vec<f32>,
    binary: Vec<f32>,
    generated: Vec<f32>,
    w: usize,
    h: usize,
    opts: AiOptions,
    plan: Vec<tiles::Tile>,
    next: usize,
    finish: FillDone,
}

fn fill_step(mut st: FillState) {
    let Some(t) = st.plan.get(st.next).cloned() else {
        let FillState { generated, finish, opts, .. } = st;
        return finish(Ok((generated, opts.model)));
    };
    if let Err(e) = st.ctx.check() {
        return (st.finish)(Err(e));
    }
    let input = match tiles::extract(&st.rgb, &st.input, st.w, st.h, &t) {
        Ok(i) => i,
        Err(e) => return (st.finish)(Err(bad(e.to_string()))),
    };
    let n = st.plan.len() as f32;
    let i = st.next;
    let (a, b) = (i as f32 / n, (i + 1) as f32 / n);
    let status = if st.plan.len() > 1 { format!("{LABEL} ({} of {})", i + 1, st.plan.len()) } else { LABEL.to_string() };
    let hooks = Arc::new(JobHooks { ctx: st.ctx.clone(), lo: a, hi: b, status });
    let svc = st.svc.clone();
    let req = st.opts.request(input);
    svc.run(
        req,
        hooks,
        Box::new(move |r| {
            let out = match r {
                Ok(out) => out,
                Err(LamaError::Gpu(why)) if !st.opts.run.choice.cpu => return gpu_fallback(st, &why),
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

/// The GPU couldn't run the model: the rest of the job runs on the CPU, with the model the
/// user's Quality picks there (Auto: MI-GAN, when it is here or may be downloaded; else the same
/// model). A model change starts the tiles over, so one result never mixes two models.
fn gpu_fallback(mut st: FillState, why: &str) {
    let run = crate::ai_cmds::on_cpu(&st.opts.run);
    let mut model = st.opts.model;
    if !st.opts.pinned {
        let id = if run.choice.lite { catalog::MIGAN_ID } else { catalog::LAMA_ID };
        if let Some(m) = catalog::model(id)
            && (st.opts.allow_download || st.svc.model_state_of(m.id) == ModelState::Ready)
        {
            model = m;
        }
    }
    st.opts.run = run;
    st.ctx.notice(crate::ai_cmds::GPU_FALLBACK_CLASS, crate::ai_cmds::GPU_FALLBACK_NOTICE, why);
    if model.id != st.opts.model.id {
        st.opts.model = model;
        if model.arch == catalog::Arch::Migan {
            st.opts.tiles.max_side = st.opts.tiles.max_side.min(512);
        }
        st.plan = match tiles::plan(&st.cover, st.w, st.h, &st.opts.tiles) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => return (st.finish)(Err(bad("nothing to clean: the area is empty"))),
            Err(e) => return (st.finish)(Err(bad(e.to_string()))),
        };
        st.next = 0;
        st.generated.clone_from(&st.rgb);
    }
    fill_step(st);
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
    let opts = ai_options(s, p, ID)?;
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
    inpaint_ai_labelled(s, p, LABEL)
}

/// [`inpaint_ai`] as one undo step named `label` (Remove with AI on a selection).
pub(crate) fn inpaint_ai_labelled(s: &mut Session, p: &Value, label: &'static str) -> Result<Value> {
    let plan = plan(s, p)?;
    let svc = ready_inpainter(s, &plan.opts)?;
    let device = svc.device();
    crate::jobs::run_async(
        s,
        label,
        true,
        move |ctx, done| {
            ctx.progress(0.0, label);
            let (w, h) = (plan.region.width() as usize, plan.region.height() as usize);
            let (rgb, cover, opts) = (plan.rgb.clone(), plan.cover.clone(), plan.opts.clone());
            let svc2 = svc.clone();
            ai_fill_rgb_async(
                svc,
                ctx.clone(),
                rgb,
                cover,
                None,
                w,
                h,
                opts,
                Box::new(move |r| done.finish(r.map(|(generated, model)| (plan, generated, svc2.device(), model)))),
            );
        },
        move |s, (plan, generated, dev, model)| apply(s, label, plan, &generated, if dev.is_empty() { device } else { dev }, model),
    )
}

fn apply(s: &mut Session, label: &str, plan: Plan, generated: &[f32], device: String, model: &ModelInfo) -> Result<Value> {
    let Plan { layer: id, fmt, region, layer_px, cover, new_layer, .. } = plan;
    let n = fmt.channels();
    let count = cover.iter().filter(|c| **c > 0.0).count();
    let layer = s.edit(label, |doc, active| {
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
        "model": model.id,
        "lite": model.lite,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Clean with AI",
        menu: &["Edit"],
        shortcut: None,
        params: r#"{"output"?: "new" (default: the result on a new layer above, alpha = selection coverage) | "current" (paint it into the layer), "layer"?: layer id (default: the active pixel layer), "maxSide"?: 64..2048, multiple of 8 (default 512: larger crops are downscaled to this working size), "margin"?: 0..1024 px of context around each masked blob (default 64), "allowDownload"?: bool (fetch the model's weights when not cached: Manga LaMa about 100 MB, MI-GAN 13 MB; default false), "quality"?: "auto"|"fast"|"best" (fast: the lite MI-GAN, at most 512 px tiles; auto: Manga LaMa on a capable GPU, MI-GAN on the CPU, phones and weak machines), "device"?: "auto"|"gpu"|"cpu", "model"?: "auto"|"manga-lama"|"migan-512", "threads"?: 0..64 (CPU threads, 0 = one per core), "keepLoaded"?: bool (default true)} -> {"layer", "pixels", "region": [x,y,w,h], "device", "model", "lite"}. Erases what the selection covers (text, SFX) and fills it with Manga LaMa (or MI-GAN). Background job, one undo step."#,
        enabled,
        run: inpaint_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_inpaint_cmds/tests.rs"]
mod tests;
