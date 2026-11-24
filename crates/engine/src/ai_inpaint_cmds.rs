//! Edit › Clean Selected Area with AI (`edit.inpaintAi`): erase what the selection covers (text, SFX,
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
//! `quality` (auto, fast, best) and `device` (auto, gpu, cpu; natively also directml, wgpu: the
//! GPU runtime) choose between Manga LaMa and the lite MI-GAN (512 px tiles, a fraction of the
//! work: phones, weak machines, CPUs), as `image.upscaleAi` does; `model` names one explicitly. `threads` and `keepLoaded` are Advanced
//! settings, like `maxSide` and `margin`.

pub mod quick;
pub(crate) mod work;

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
use work::Work;

/// The command id.
pub const ID: &str = "edit.inpaintAi";
const LABEL: &str = "Clean Selected Area with AI";
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
        Request {
            tile,
            allow_download: self.allow_download,
            model: self.model.id,
            cpu: self.run.choice.cpu,
            threads: self.threads,
            unload: self.unload,
            backend: lama_backend(self.run.backend),
        }
    }
}

/// The `device` param's GPU runtime (`directml`, `wgpu`) for LaMa's service.
pub(crate) fn lama_backend(b: photocraft_ai::service::GpuBackend) -> photocraft_ai_lama::service::GpuBackend {
    use photocraft_ai::service::GpuBackend as Ai;
    use photocraft_ai_lama::service::GpuBackend as Lama;
    match b {
        Ai::Auto => Lama::Auto,
        Ai::Wgpu => Lama::Wgpu,
        Ai::DirectMl => Lama::DirectMl,
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

impl AiOptions {
    /// How far around the hole to read: with fixed tile sizes, enough for real pixels to reach the
    /// tile's edge (mirrored ones there make the model draw stray marks).
    pub(crate) fn context_margin(&self) -> usize {
        if self.tiles.fixed_sizes { self.tiles.margin.max(photocraft_ai_lama::tiles::FIXED_SIDES[0]) } else { self.tiles.margin }
    }
}

/// The largest working side of a Manga LaMa run on the GPU: larger crops are downscaled to it.
pub const GPU_MAX_SIDE: u64 = 1024;

/// `full_res_default`: without `maxSide`, crops larger than the working side are split into
/// full-resolution tiles (the Remove tools) rather than downscaled (`edit.inpaintAi`).
pub(crate) fn ai_options(s: &Session, p: &Value, cmd: &str, full_res_default: bool) -> Result<AiOptions> {
    let bad = |m: String| EngineError::BadParams { cmd: cmd.into(), msg: m };
    let (model, run) = model_for(s, p, cmd)?;
    let uint = |k: &str, d: u64, lo: u64, hi: u64| -> Result<u64> { Ok(crate::ai_cmds::uint_param(p, k, lo, hi, cmd)?.unwrap_or(d)) };
    // MI-GAN works on at most 512 × 512; LaMa takes smaller crops on machines with little memory.
    let lite = model.arch == catalog::Arch::Migan;
    let default_side = if lite || !run.choice.small_tiles { 512 } else { 384 };
    // 0: full resolution, larger crops split into overlapping tiles of the default side.
    let (max_side, full_res) = match crate::ai_cmds::uint_param(p, "maxSide", 0, 2048, cmd)? {
        None => (default_side, full_res_default),
        Some(0) => (default_side, true),
        Some(v) if v < 64 => return Err(bad("`maxSide` must be 0 (full resolution) or an integer in 64..=2048".into())),
        Some(v) => (v, false),
    };
    if !max_side.is_multiple_of(8) {
        return Err(bad("`maxSide` must be a multiple of 8".into()));
    }
    // On the GPU one tile is one piece of GPU work, and a long one makes Windows reset the GPU
    // (and the window with it): Manga LaMa at 2048 px is about 6.5 TFLOP, four times 1024's.
    let max_side = if lite { max_side.min(512) } else if run.choice.cpu { max_side } else { max_side.min(GPU_MAX_SIDE) };
    let margin = uint("margin", 64, 0, 1024)?;
    let allow_download = match p.get("allowDownload") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("`allowDownload` must be true or false".into()))?,
    };
    let threads = uint("threads", 0, 0, 64)? as usize;
    let unload = !crate::ai_cmds::keep_loaded(p, cmd)?;
    let pinned = !matches!(p.get("model").and_then(Value::as_str).unwrap_or("auto"), "auto" | "");
    let fixed_sizes = run.backend != photocraft_ai::service::GpuBackend::Wgpu && s.inpainter.as_ref().is_some_and(|i| i.fixed_tile_sizes(model.id, run.choice.cpu));
    Ok(AiOptions { tiles: TileOptions { max_side: max_side as usize, margin: margin as usize, full_res, fixed_sizes, blocks: p.get("byBlocks").and_then(Value::as_bool).unwrap_or(false) }, allow_download, model, run, threads, unload, pinned })
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
    /// This tile and the count, when more than one: said after the model's own status ("Cleaning
    /// with Manga LaMa (WebGPU) (2 of 5)"), so the progress UI can count them.
    tile: Option<(usize, usize)>,
    /// Where the job's runs ended ([`Hooks::fell_back`]).
    trail: Arc<std::sync::Mutex<crate::ai_cmds::Trail>>,
}

impl Hooks for JobHooks {
    fn progress(&self, fraction: f32, status: &str) {
        let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
        let message = match (status.is_empty(), self.tile) {
            (true, _) => self.status.clone(),
            (false, Some((i, n))) => format!("{status} ({i} of {n})"),
            (false, None) => status.to_string(),
        };
        self.ctx.progress(self.lo + (self.hi - self.lo) * f, &message);
    }
    fn cancelled(&self) -> bool {
        self.ctx.cancelled()
    }
    fn fell_back(&self, f: &photocraft_ai_lama::service::Fallback) {
        crate::ai_cmds::fell_back(&self.ctx, &self.trail, &f.from, &f.to, f.cpu, &f.why);
    }
}

/// Receives what made the fill (its pixels went to the [`work::Sink`]).
pub(crate) type FillDone = Box<dyn FnOnce(Result<FillRun>) + Send>;

/// What an AI fill ran: the model, whether it ended on the CPU (an automatic run with no usable
/// GPU too), and `fell_back`: it left where it was headed (the GPU failed mid-run, DirectML
/// couldn't run it, or there was no usable GPU).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FillRun {
    pub model: &'static ModelInfo,
    pub cpu: bool,
    pub fell_back: bool,
    /// The run was kept off the GPU ([`crate::ai_cmds::GpuGuard`]).
    pub guard: Option<crate::ai_cmds::GpuGuard>,
}

/// The AI fill of `work`'s region without blocking: each tile's request starts from the previous
/// one's `done`; the filled blocks go to the work's sink as the plan leaves them behind, and
/// `finish` gets the model that made them (another than `opts.model` after a GPU failure, see
/// [`gpu_fallback`]). Works with an inpainter that answers later (the browser's AI worker, which
/// no thread of the page may wait for), so `edit.inpaintAi` and the Remove tool's AI mode run
/// everywhere through `jobs::run_async`.
pub(crate) fn ai_fill_async(svc: Arc<dyn Inpainter>, ctx: JobCtx, mut work: Work, opts: AiOptions, finish: FillDone) {
    let (w, h) = work.size();
    let plan = match tiles::plan_hole(work.hole(), w, h, &opts.tiles) {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => return finish(Err(bad("nothing to clean: the area is empty"))),
        Err(e) => return finish(Err(bad(e.to_string()))),
    };
    let split = match split_state(&plan, w, h, &opts) {
        Ok(sp) => sp,
        Err(e) => return finish(Err(e)),
    };
    work.uses(&plan);
    let state = FillState { svc, ctx, work, opts, plan, split, next: 0, fell_back: false, trail: Arc::default(), finish };
    fill_step(state);
}

/// What the plan's split tiles share ([`tiles::Split`]), when it has any.
fn split_state(plan: &[tiles::Tile], w: usize, h: usize, opts: &AiOptions) -> Result<Option<tiles::Split>> {
    if !plan.iter().any(|t| t.split) {
        return Ok(None);
    }
    tiles::Split::new(w, h, opts.tiles.max_side).map(Some).map_err(|e| bad(e.to_string()))
}

struct FillState {
    svc: Arc<dyn Inpainter>,
    ctx: JobCtx,
    /// The region, filled in place tile by tile: a pasted pixel is inside the hole, which every
    /// later tile's model input masks out, so no tile sees another's output. Held in blocks, only
    /// those the tiles still to run read ([`Work`]).
    work: Work,
    opts: AiOptions,
    plan: Vec<tiles::Tile>,
    /// The fill so far of a large crop split into full-resolution tiles.
    split: Option<tiles::Split>,
    next: usize,
    /// The GPU failed and the rest runs on the CPU ([`gpu_fallback`]).
    fell_back: bool,
    /// The runtime's own fallbacks ([`Hooks::fell_back`]).
    trail: Arc<std::sync::Mutex<crate::ai_cmds::Trail>>,
    finish: FillDone,
}

/// A tile's crop while the model runs on it: its RGB and coverage (buffers of just `t.rect`), the
/// split's window over it and the paste weights of a split tile.
struct Crop {
    rgb: Vec<f32>,
    cover: Vec<f32>,
    split: Option<(tiles::Split, Vec<f32>)>,
}

/// Whose turn it is to start the next tile, between [`fill_step`] and the answer to its request.
enum Turn {
    /// The request is being made: an answer now leaves the state here for [`fill_step`]'s loop.
    Asking,
    /// The answer came inside the request, with the state to go on from.
    Answered(Box<FillState>),
    /// The request returned without an answer: the answer starts the next tile itself.
    Later,
}

/// Runs the tiles from `st.next` on, one request after another. An inpainter may answer inside
/// `run` (the native ones) or later (the browser's worker): an answer inside `run` comes back to
/// this loop instead of starting the next tile from within the answer, which nested one model
/// run per tile and overflowed the stack on a clean of many tiles (1 MB on the main thread of
/// Windows, 2 MB on a job's).
fn fill_step(mut st: FillState) {
    loop {
        // The next tile with something to fill (a split tile whose hole the ones before it filled has none).
        let (t, input, crop) = loop {
            let Some(t) = st.plan.get(st.next).cloned() else {
                let FillState { work, finish, opts, fell_back, trail, .. } = st;
                if let Err(e) = work.finish() {
                    return finish(Err(e));
                }
                let t = trail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let run = FillRun { model: opts.model, cpu: opts.run.choice.cpu || t.cpu, fell_back: fell_back || t.fell_back, guard: opts.run.guard };
                drop(t);
                return finish(Ok(run));
            };
            if let Err(e) = st.ctx.check() {
                return (st.finish)(Err(e));
            }
            let (rgb, cover) = st.work.cut(t.rect);
            let local = t.local();
            let cut = match (t.split, st.split.as_ref()) {
                (true, Some(sp)) => {
                    let win = sp.window(t.rect);
                    win.extract(&rgb, &cover, &local).map(|r| r.map(|(i, wts)| (i, Some((win, wts)))))
                }
                (true, None) => Err(LamaError::Input("a split tile without its state".into())),
                (false, _) => tiles::extract(&rgb, &cover, t.rect.w, t.rect.h, &local).map(|i| Some((i, None))),
            };
            match cut {
                Ok(Some((input, split))) => break (t, input, Crop { rgb, cover, split }),
                Ok(None) => {
                    if let Err(e) = st.work.retire(st.next) {
                        return (st.finish)(Err(e));
                    }
                    st.next += 1;
                    st.ctx.progress(st.next as f32 / st.plan.len() as f32, "");
                }
                Err(e) => return (st.finish)(Err(bad(e.to_string()))),
            }
        };
        let n = st.plan.len() as f32;
        let i = st.next;
        let (a, b) = (i as f32 / n, (i + 1) as f32 / n);
        let status = if st.plan.len() > 1 { format!("{LABEL} ({} of {})", i + 1, st.plan.len()) } else { LABEL.to_string() };
        let tile = (st.plan.len() > 1).then_some((i + 1, st.plan.len()));
        let hooks = Arc::new(JobHooks { ctx: st.ctx.clone(), lo: a, hi: b, status, tile, trail: st.trail.clone() });
        let svc = st.svc.clone();
        let req = st.opts.request(input);
        let turn = Arc::new(std::sync::Mutex::new(Turn::Asking));
        let answer = turn.clone();
        svc.run(
            req,
            hooks,
            Box::new(move |r| {
                let Some(next) = after_tile(st, &t, crop, b, r) else { return };
                let mut turn = answer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if matches!(*turn, Turn::Asking) {
                    *turn = Turn::Answered(Box::new(next));
                } else {
                    drop(turn);
                    fill_step(next);
                }
            }),
        );
        let mut turn = turn.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::mem::replace(&mut *turn, Turn::Later) {
            Turn::Answered(next) => st = *next,
            Turn::Asking | Turn::Later => return,
        }
    }
}

/// A tile's answer pasted into the fill: the state to go on from, or `None` when the fill ended
/// here (its `finish` was called with the error).
fn after_tile(mut st: FillState, t: &tiles::Tile, crop: Crop, done: f32, r: std::result::Result<Vec<f32>, LamaError>) -> Option<FillState> {
    let out = match r {
        Ok(out) => out,
        // Once more on the CPU; a CPU run that failed this way only got there on threads.
        Err(LamaError::Gpu(why)) if !st.opts.run.choice.cpu || !st.fell_back => return gpu_fallback(st, &why),
        Err(e) => {
            (st.finish)(Err(lama_err(e)));
            return None;
        }
    };
    let Crop { mut rgb, cover, split } = crop;
    let local = t.local();
    let pasted = match (split, st.split.as_mut()) {
        (Some((mut win, wts)), Some(sp)) => win.paste(&mut rgb, &local, &out, &wts).map(|_| sp.put_window(t.rect, &win)),
        _ => {
            let binary: Vec<f32> = cover.iter().map(|c| if *c > 0.0 { 1.0 } else { 0.0 }).collect();
            tiles::paste(&mut rgb, &binary, t.rect.w, t.rect.h, &local, &out).map(|_| ())
        }
    };
    if let Err(e) = pasted {
        (st.finish)(Err(lama_err(e)));
        return None;
    }
    st.work.put_back(t.rect, &rgb);
    if let Err(e) = st.work.retire(st.next) {
        (st.finish)(Err(e));
        return None;
    }
    st.ctx.progress(done, "");
    st.next += 1;
    Some(st)
}

/// The GPU couldn't run the model: the rest of the job runs on the CPU, with the model the
/// user's Quality picks there (Auto: MI-GAN, when it is here or may be downloaded; else the same
/// model). A model change starts the tiles over, so one result never mixes two models (the
/// pixels filled so far are in the hole: the new model doesn't see them, and overwrites them).
fn gpu_fallback(mut st: FillState, why: &str) -> Option<FillState> {
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
    st.fell_back = true;
    st.ctx.notice(crate::ai_cmds::GPU_FALLBACK_CLASS, crate::ai_cmds::GPU_FALLBACK_NOTICE, why);
    st.ctx.progress(st.ctx.fraction(), crate::ai_cmds::SWITCHING_TO_CPU);
    if model.id != st.opts.model.id {
        st.opts.model = model;
        if model.arch == catalog::Arch::Migan {
            st.opts.tiles.max_side = st.opts.tiles.max_side.min(512);
        }
        let (w, h) = st.work.size();
        st.plan = match tiles::plan_hole(st.work.hole(), w, h, &st.opts.tiles) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => {
                (st.finish)(Err(bad("nothing to clean: the area is empty")));
                return None;
            }
            Err(e) => {
                (st.finish)(Err(bad(e.to_string())));
                return None;
            }
        };
        st.work.uses(&st.plan);
        st.split = match split_state(&st.plan, w, h, &st.opts) {
            Ok(sp) => sp,
            Err(e) => {
                (st.finish)(Err(e));
                return None;
            }
        };
        st.next = 0;
    }
    Some(st)
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

/// Where a clean's result goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Output {
    /// Into the layer.
    Current,
    /// On a new layer above it, named after this, for each clean. Only what was generated.
    New(String),
    /// On one layer above it with this name, all the cleans together.
    Single(String),
}

/// What a layer of cleans is called when the caller doesn't say.
pub(crate) const CLEAN_LAYER: &str = "AI Clean";

impl Output {
    /// `output` and `layerName`; `default` without `output`.
    pub(crate) fn parse(p: &Value, default: Output) -> Result<Output> {
        let name = p.get("layerName").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or(CLEAN_LAYER).to_string();
        match p.get("output") {
            None | Some(Value::Null) => Ok(default),
            Some(Value::String(o)) => match o.as_str() {
                "new" => Ok(Output::New(name)),
                "single" => Ok(Output::Single(name)),
                "current" => Ok(Output::Current),
                o => Err(bad(format!("unknown `output` {o:?} (new|single|current)"))),
            },
            Some(_) => Err(bad("`output` must be \"new\", \"single\" or \"current\"")),
        }
    }

    fn name(&self) -> Option<&str> {
        match self {
            Output::Current => None,
            Output::New(n) | Output::Single(n) => Some(n),
        }
    }
}

fn find_layer(list: &[Layer], wanted: &dyn Fn(&Layer) -> bool) -> Option<LayerId> {
    list.iter().rev().find_map(|l| if wanted(l) { Some(l.id) } else { l.children().and_then(|c| find_layer(c, wanted)) })
}

/// The layer of cleans called `name`, if the document has one.
pub(crate) fn clean_layer(doc: &photocraft_doc::Document, name: &str) -> Option<LayerId> {
    find_layer(&doc.layers, &|l| l.name == name && l.surface().is_some())
}

/// The layer under `id` among its siblings.
fn below(list: &[Layer], id: LayerId) -> Option<LayerId> {
    if let Some(at) = list.iter().position(|l| l.id == id) {
        return at.checked_sub(1).map(|k| list[k].id);
    }
    list.iter().find_map(|l| l.children().and_then(|c| below(c, id)))
}

/// What a clean reads: with the layer of cleans active, the page under it. A layer of cleans holds
/// only what was generated, so there'd be nothing to fill from.
pub(crate) fn source_layer(doc: &photocraft_doc::Document, id: LayerId, output: &Output) -> LayerId {
    let Some(name) = output.name() else { return id };
    let is_clean = doc.layer(id).is_some_and(|l| l.name.starts_with(name));
    if is_clean { below(&doc.layers, id).unwrap_or(id) } else { id }
}

/// `rgb` (interleaved, of `region`) under the layer of cleans, so a clean sees the ones before it.
pub(crate) fn under_cleans(rgb: &mut [f32], doc: &photocraft_doc::Document, region: Rect, output: &Output) {
    let Output::Single(name) = output else { return };
    let Some(surf) = clean_layer(doc, name).and_then(|id| doc.layer(id)).and_then(|l| l.surface()) else { return };
    let fmt = surf.format();
    let px = surf.read_region(region);
    for (dst, p) in rgb.chunks_exact_mut(3).zip(px.chunks_exact(fmt.channels().max(1))) {
        let c = to_rgba(&fmt, p);
        if c[3] > 0.0 {
            for k in 0..3 {
                dst[k] += (c[k] - dst[k]) * c[3];
            }
        }
    }
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

fn plan(s: &Session, p: &Value) -> Result<Head> {
    if !p.is_object() && !p.is_null() {
        return Err(bad("params must be a JSON object"));
    }
    let opts = ai_options(s, p, ID, false)?;
    head_of(s, p, Output::New(CLEAN_LAYER.to_string()), opts.context_margin(), opts)
}

/// What a fill holds for each pixel of its region for the whole run: the hole's flag, the split
/// tiles' "filled" flag and the plan's search for blobs ([`work`]: the pixels themselves are in
/// blocks, a band at a time).
const WORK_BYTES_PER_PIXEL: u64 = 3;

/// The largest region a clean reads on this device: [`MAX_PIXELS`], less where a quarter of
/// Memory Usage (what an AI run may take beside the pages and the model) can't hold its flags.
fn max_pixels(s: &Session) -> u64 {
    let budget = s.prefs().performance.history_budget_bytes() as u64 / 4;
    MAX_PIXELS.min(budget / WORK_BYTES_PER_PIXEL)
}

/// The layer and the zone a clean works on, with what it reads around the selection.
fn head_of<O>(s: &Session, p: &Value, default_output: Output, margin: usize, opts: O) -> Result<Head<O>> {
    let id = match p.get("layer").filter(|v| !v.is_null()) {
        Some(v) => {
            let l = v.as_u64().ok_or_else(|| bad("`layer` must be a layer id"))?;
            crate::edit_menu_cmds::pixel_layer_id(s, LayerId(l))
        }
        None => crate::edit_menu_cmds::pixel_layer(s),
    }
    .map_err(EngineError::Other)?;
    let output = Output::parse(p, default_output)?;
    let keep_active = p.get("keepActive").and_then(Value::as_bool).unwrap_or(false);
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &d.doc;
    let id = source_layer(doc, id, &output);
    let canvas = doc.bounds();
    let sel = doc.selection.as_ref().ok_or_else(|| bad("make a selection around what to clean"))?;
    let sb = sel.content_bounds().intersect(&canvas);
    if sb.is_empty() {
        return Err(bad("the selection is outside the canvas"));
    }
    let region = context_region(sb, canvas, margin);
    let pixels = u64::from(region.width()) * u64::from(region.height());
    let most = max_pixels(s);
    if pixels > most {
        return Err(bad(format!("the area is too large to clean at once on this device ({pixels} pixels, at most {most}); select less")));
    }
    doc.layer(id).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(id))?;
    Ok(Head { layer: id, region, output, keep_active, opts, doc: doc.clone() })
}

/// What [`plan`] checked on the page's thread.
struct Head<O = AiOptions> {
    layer: LayerId,
    region: Rect,
    output: Output,
    keep_active: bool,
    opts: O,
    doc: std::sync::Arc<photocraft_doc::Document>,
}

/// The result a clean writes into as it goes: a copy of the layer it lands on.
pub(crate) type Out = std::sync::Arc<std::sync::Mutex<photocraft_raster::Surface>>;

impl<O> Head<O> {
    /// What a clean reads (the document as it was when the clean started) and where its result
    /// goes: a copy of the layer (or of the layer of cleans, or a new one) that blocks of the
    /// result are written into, in that layer's format.
    fn parts(&self) -> Result<(DocSource, DocSink, Out)> {
        let doc = &self.doc;
        let surf = doc.layer(self.layer).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(self.layer))?;
        let source = DocSource { doc: doc.clone(), layer: self.layer, at: self.region, output: self.output.clone() };
        let (sink, out) = match &self.output {
            Output::Current => {
                let out = std::sync::Arc::new(std::sync::Mutex::new(surf.clone()));
                (DocSink { at: self.region, base: surf.clone(), out: out.clone(), over: false }, out)
            }
            output => clean_sink(doc, output, self.region),
        };
        Ok((source, sink, out))
    }

    /// [`Head::parts`] as the AI fill's paged region, its hole read now.
    fn work(&self) -> Result<(Work, Out)> {
        let (source, sink, out) = self.parts()?;
        let r = self.region;
        let (w, h) = (r.width() as usize, r.height() as usize);
        let tile = photocraft_geom::TILE_SIZE;
        let phase = (r.x0.rem_euclid(tile) as usize, r.y0.rem_euclid(tile) as usize);
        Ok((Work::new(w, h, phase, Box::new(source), Box::new(sink)), out))
    }
}

/// Where a clean of `at` on a layer of cleans (`output` new or single) writes: over the layer of
/// cleans `doc` has (single), or a new layer's pixels.
pub(crate) fn clean_sink(doc: &photocraft_doc::Document, output: &Output, at: Rect) -> (DocSink, Out) {
    let existing = match output {
        Output::Single(name) => clean_layer(doc, name).and_then(|id| doc.layer(id)).and_then(|l| l.surface()).cloned(),
        _ => None,
    };
    let base = existing.unwrap_or_else(|| photocraft_raster::Surface::new(PixelFormat { alpha: true, ..doc.pixel_format() }));
    let out = std::sync::Arc::new(std::sync::Mutex::new(base.clone()));
    (DocSink { at, base, out: out.clone(), over: true }, out)
}

/// The surface a clean wrote, once nothing else holds it.
pub(crate) fn take_out(out: Out) -> Result<photocraft_raster::Surface> {
    std::sync::Arc::try_unwrap(out)
        .map(|m| m.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner))
        .map_err(|_| EngineError::Other("the clean's result is still in use".into()))
}

/// A region-local rectangle of the fill on the document.
fn doc_rect(at: Rect, r: tiles::Rect) -> Rect {
    Rect::from_xywh(at.x0 + r.x as i32, at.y0 + r.y as i32, r.w as u32, r.h as u32)
}

/// What a clean reads: the layer (under the cleans before it, with `output: single`) and the
/// selection's coverage.
pub(crate) struct DocSource {
    doc: std::sync::Arc<photocraft_doc::Document>,
    layer: LayerId,
    at: Rect,
    output: Output,
}

impl work::Source for DocSource {
    fn cover(&mut self, r: tiles::Rect, out: &mut Vec<f32>) {
        out.clear();
        let Some(sel) = self.doc.selection.as_ref() else { return };
        let sc = sel.channels().max(1);
        out.extend(sel.read_region(doc_rect(self.at, r)).chunks_exact(sc).map(|p| if p[0].is_finite() { p[0].clamp(0.0, 1.0) } else { 0.0 }));
    }

    fn rgb(&mut self, r: tiles::Rect, out: &mut Vec<f32>) {
        out.clear();
        let Some(surf) = self.doc.layer(self.layer).and_then(|l| l.surface()) else { return };
        let a = doc_rect(self.at, r);
        *out = to_rgb(&surf.format(), &surf.read_region(a));
        under_cleans(out, &self.doc, a, &self.output);
    }
}

/// Where a clean's blocks go: `out`, a copy of `base` (the layer as the clean started, or the
/// layer of cleans) with the fill blended in where the selection covers. Each block is blended
/// from `base`, so a block handed over again replaces the first.
pub(crate) struct DocSink {
    at: Rect,
    base: photocraft_raster::Surface,
    out: std::sync::Arc<std::sync::Mutex<photocraft_raster::Surface>>,
    /// Over what the layer of cleans has (alpha = coverage), not into the layer's own pixels.
    over: bool,
}

impl work::Sink for DocSink {
    fn put(&mut self, r: tiles::Rect, rgb: &[f32], cover: &[f32]) -> Result<()> {
        if !cover.iter().any(|c| *c > 0.0) {
            return Ok(());
        }
        let a = doc_rect(self.at, r);
        let fmt = self.base.format();
        let n = fmt.channels();
        let mut px = self.base.read_region(a);
        px.resize(cover.len() * n, 0.0);
        let mut enc = [0.0f32; 8];
        for (i, &c) in cover.iter().enumerate() {
            if c <= 0.0 {
                continue;
            }
            let o = to_rgba(&fmt, &px[i * n..(i + 1) * n]);
            let g = &rgb[i * 3..i * 3 + 3];
            let v = if self.over {
                // Over what's there: a soft edge over an earlier clean.
                let al = c + o[3] * (1.0 - c);
                let mix = |k: usize| if al > 0.0 { (g[k] * c + o[k] * o[3] * (1.0 - c)) / al } else { g[k] };
                [mix(0), mix(1), mix(2), al]
            } else {
                let mix = |k: usize| o[k] + (g[k] - o[k]) * c;
                [mix(0), mix(1), mix(2), o[3]]
            };
            from_rgba_into(&fmt, v, &mut enc);
            px[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
        }
        self.out.lock().unwrap_or_else(std::sync::PoisonError::into_inner).write_region(a, &px);
        Ok(())
    }
}

/// What a fill of a hole within `sb` reads on `canvas`: every blob's tile can reach that far
/// (`tiles::plan` grows each blob by max(`margin`, its long side / 4)). The Remove tools' AI fill
/// reads the same, so a stroke's hole is filled exactly as a selection of that shape.
pub(crate) fn context_region(sb: Rect, canvas: Rect, margin: usize) -> Rect {
    let reach = (margin as u64).max(u64::from(sb.width().max(sb.height())) / 4).min(1 << 20) as i32;
    sb.inflate(reach).intersect(&canvas)
}

fn inpaint_ai(s: &mut Session, p: &Value) -> Result<Value> {
    inpaint_ai_labelled(s, p, LABEL)
}

/// The clean's result: where it goes, what it wrote and how many pixels the selection covered.
struct Cleaned<O = AiOptions> {
    head: Head<O>,
    out: photocraft_raster::Surface,
    pixels: usize,
}

/// [`inpaint_ai`] as one undo step named `label` (Remove with AI on a selection).
pub(crate) fn inpaint_ai_labelled(s: &mut Session, p: &Value, label: &'static str) -> Result<Value> {
    let head = plan(s, p)?;
    let svc = ready_inpainter(s, &head.opts)?;
    let device = svc.device();
    crate::jobs::run_async_prepared(
        s,
        label,
        true,
        // The hole, read a band at a time (on the browser threads build's pool; the pixels come
        // in blocks as the tiles need them).
        move |_ctx| {
            let (work, out) = head.work()?;
            let pixels = work.hole().iter().filter(|h| **h).count();
            if pixels == 0 {
                return Err(bad("the selection doesn't cover any pixel"));
            }
            Ok(crate::jobs::Prep::Done((head, work, out, pixels)))
        },
        move |ctx, (head, work, out, pixels), done| {
            ctx.progress(0.0, label);
            let opts = head.opts.clone();
            let svc2 = svc.clone();
            let started = web_time::Instant::now();
            ai_fill_async(
                svc,
                ctx.clone(),
                work,
                opts,
                Box::new(move |r| {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    // The work and its sink are gone: the result is `out`'s alone.
                    done.finish(r.and_then(|run| Ok((Cleaned { head, out: take_out(out)?, pixels }, svc2.device(), run, ms))))
                }),
            );
        },
        move |s, (c, dev, run, ms)| apply(s, label, c, if dev.is_empty() { device } else { dev }, run, ms),
    )
}

/// The tiles of `from` over `region` into `to` (both copies of one layer: the others are the same).
pub(crate) fn put_region(to: &mut photocraft_raster::Surface, from: &photocraft_raster::Surface, region: Rect) {
    to.put_tiles(region.tiles().filter_map(|c| from.tile(c).map(|t| (c, t.clone()))));
}

/// What a clean onto a layer of cleans wrote (`out`, from [`clean_sink`]) over `region`, into the
/// layer of cleans (`output: single`, when `doc` has it) or on a new one above `id`. That layer.
pub(crate) fn land_clean(doc: &mut photocraft_doc::Document, id: LayerId, output: &Output, out: photocraft_raster::Surface, region: Rect) -> Result<LayerId> {
    let name = output.name().unwrap_or(CLEAN_LAYER).to_string();
    if let Output::Single(_) = output
        && let Some(l) = clean_layer(doc, &name)
    {
        let surf = crate::edit_menu_cmds::writable_surface(doc, l)?;
        put_region(surf, &out, region);
        surf.prune();
        return Ok(l);
    }
    let layer_name = if matches!(output, Output::Single(_)) { name } else { doc.next_layer_name(&name) };
    let mut l = Layer::raster(layer_name, out.format());
    let surf = crate::pixels_mut(&mut l)?;
    *surf = out;
    surf.prune();
    Ok(doc.insert_above(Some(id), l))
}

/// The clean as one history step: the layer it went into.
fn write_result<O>(s: &mut Session, label: &str, c: Cleaned<O>) -> Result<LayerId> {
    let Cleaned { head, out, .. } = c;
    let Head { layer: id, region, output, keep_active, .. } = head;
    let layer = s.edit(label, |doc, active| {
        let nid = match &output {
            Output::Current => {
                put_region(crate::edit_menu_cmds::writable_surface(doc, id)?, &out, region);
                return Ok(id);
            }
            output => land_clean(doc, id, output, out, region)?,
        };
        if !keep_active {
            *active = Some(nid);
        }
        Ok(nid)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(region);
    }
    Ok(layer)
}

fn apply(s: &mut Session, label: &str, c: Cleaned, device: String, run: FillRun, ms: u64) -> Result<Value> {
    let model = run.model;
    let (count, region) = (c.pixels, c.head.region);
    let layer = write_result(s, label, c)?;
    Ok(json!({
        "layer": layer.0,
        "pixels": count,
        "region": [region.x0, region.y0, region.width(), region.height()],
        "model": model.id,
        "lite": model.lite,
        // `device` too: `CAPISHOT_AI_DEVICE=cpu` forces the CPU without the request knowing.
        "cpu": run.cpu || device == "CPU",
        "device": device,
        "fellBack": run.fell_back,
        "gpuGuard": run.guard.map(crate::ai_cmds::GpuGuard::id),
        "ms": ms,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![quick::spec(), CommandSpec {
        id: ID,
        label: "Clean Selected Area with AI",
        menu: &["Edit"],
        shortcut: None,
        params: r#"{"output"?: "new" (default: the result on a new layer above, alpha = selection coverage) | "single" (on one layer above that gathers every clean, which it also reads) | "current" (paint it into the layer), "layerName"?: str (what those layers are called), "keepActive"?: bool (the active layer stays), "byBlocks"?: bool (a run for each block of the selection, not one for neighbours whose crops overlap), "layer"?: layer id (default: the active pixel layer), "maxSide"?: 0 or 64..2048, multiple of 8 (default 512: larger crops are downscaled to this working size, at most 1024 on the GPU; 0: full resolution, larger crops split into overlapping 512 px tiles feathered at their seams), "margin"?: 0..1024 px of context around each masked blob (default 64), "allowDownload"?: bool (fetch the model's weights when not cached: Manga LaMa about 100 MB, MI-GAN 13 MB; default false), "quality"?: "auto"|"fast"|"best" (fast: the lite MI-GAN, at most 512 px tiles; auto: Manga LaMa on a capable GPU, MI-GAN on the CPU, phones and weak machines), "device"?: "auto"|"gpu"|"cpu"|"directml"|"wgpu" (directml, wgpu: natively, that GPU runtime; auto picks DirectML on Windows for Manga LaMa when ONNX Runtime is installed), "model"?: "auto"|"manga-lama"|"migan-512", "threads"?: 0..64 (CPU threads, 0 = one per core), "keepLoaded"?: bool (default true)} -> {"layer", "pixels", "region": [x,y,w,h], "device", "model", "lite", "cpu" (it ran on the CPU, also when an automatic run found no usable GPU), "fellBack" (it left where it was headed: the GPU failed mid-run and the CPU finished it, DirectML couldn't run it, or there was no usable GPU; then "notices" say which: ai.gpuFallback, ai.directmlFallback, ai.cpuFallback), "ms" (the run's wall time)}. Erases what the selection covers (text, SFX) and fills it with Manga LaMa (or MI-GAN). Background job, one undo step."#,
        enabled,
        run: inpaint_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_inpaint_cmds/tests.rs"]
mod tests;
