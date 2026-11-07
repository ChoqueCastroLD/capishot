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
    let max_side = if lite { max_side.min(512) } else { max_side };
    let margin = uint("margin", 64, 0, 1024)?;
    let allow_download = match p.get("allowDownload") {
        None | Some(Value::Null) => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("`allowDownload` must be true or false".into()))?,
    };
    let threads = uint("threads", 0, 0, 64)? as usize;
    let unload = !crate::ai_cmds::keep_loaded(p, cmd)?;
    let pinned = !matches!(p.get("model").and_then(Value::as_str).unwrap_or("auto"), "auto" | "");
    let fixed_sizes = run.backend != photocraft_ai::service::GpuBackend::Wgpu && s.inpainter.as_ref().is_some_and(|i| i.fixed_tile_sizes(model.id, run.choice.cpu));
    Ok(AiOptions { tiles: TileOptions { max_side: max_side as usize, margin: margin as usize, full_res, fixed_sizes }, allow_download, model, run, threads, unload, pinned })
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

/// Receives the generated RGB and what made it.
pub(crate) type FillDone = Box<dyn FnOnce(Result<(Vec<f32>, FillRun)>) + Send>;

/// What an AI fill ran: the model, whether it ended on the CPU (an automatic run with no usable
/// GPU too), and `fell_back`: it left where it was headed (the GPU failed mid-run, DirectML
/// couldn't run it, or there was no usable GPU).
#[derive(Clone, Copy, Debug)]
pub(crate) struct FillRun {
    pub model: &'static ModelInfo,
    pub cpu: bool,
    pub fell_back: bool,
}

/// The AI fill of a `w×h` region (`rgb` interleaved RGB 0..=1, `cover` the hole's coverage),
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
    let split = match split_state(&plan, w, h, &opts) {
        Ok(sp) => sp,
        Err(e) => return finish(Err(e)),
    };
    let state = FillState { svc, ctx, rgb, cover, binary, w, h, opts, plan, split, next: 0, fell_back: false, trail: Arc::default(), finish };
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
    /// later tile's model input masks out, so no tile sees another's output (and no second
    /// region-sized copy is kept: phones ran out of memory on large selections).
    rgb: Vec<f32>,
    cover: Vec<f32>,
    binary: Vec<f32>,
    w: usize,
    h: usize,
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

fn fill_step(mut st: FillState) {
    // The next tile with something to fill (a split tile whose hole the ones before it filled has none).
    let (t, input, weights) = loop {
        let Some(t) = st.plan.get(st.next).cloned() else {
            let FillState { rgb, finish, opts, fell_back, trail, .. } = st;
            let t = trail.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let run = FillRun { model: opts.model, cpu: opts.run.choice.cpu || t.cpu, fell_back: fell_back || t.fell_back };
            drop(t);
            return finish(Ok((rgb, run)));
        };
        if let Err(e) = st.ctx.check() {
            return (st.finish)(Err(e));
        }
        let cut = match (t.split, st.split.as_ref()) {
            (true, Some(sp)) => sp.extract(&st.rgb, &st.cover, &t).map(|r| r.map(|(i, w)| (i, Some(w)))),
            (true, None) => Err(LamaError::Input("a split tile without its state".into())),
            (false, _) => tiles::extract(&st.rgb, &st.cover, st.w, st.h, &t).map(|i| Some((i, None))),
        };
        match cut {
            Ok(Some((input, weights))) => break (t, input, weights),
            Ok(None) => {
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
    svc.run(
        req,
        hooks,
        Box::new(move |r| {
            let out = match r {
                Ok(out) => out,
                // Once more on the CPU; a CPU run that failed this way only got there on threads.
                Err(LamaError::Gpu(why)) if !st.opts.run.choice.cpu || !st.fell_back => return gpu_fallback(st, &why),
                Err(e) => return (st.finish)(Err(lama_err(e))),
            };
            let pasted = match (weights, st.split.as_mut()) {
                (Some(wts), Some(sp)) => sp.paste(&mut st.rgb, &t, &out, &wts),
                _ => tiles::paste(&mut st.rgb, &st.binary, st.w, st.h, &t, &out),
            };
            if let Err(e) = pasted {
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
/// model). A model change starts the tiles over, so one result never mixes two models (the
/// pixels filled so far are in the hole: the new model doesn't see them, and overwrites them).
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
    st.fell_back = true;
    st.ctx.notice(crate::ai_cmds::GPU_FALLBACK_CLASS, crate::ai_cmds::GPU_FALLBACK_NOTICE, why);
    st.ctx.progress(st.ctx.fraction(), crate::ai_cmds::SWITCHING_TO_CPU);
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
        st.split = match split_state(&st.plan, st.w, st.h, &st.opts) {
            Ok(sp) => sp,
            Err(e) => return (st.finish)(Err(e)),
        };
        st.next = 0;
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

/// `generated` (interleaved RGB of `region`) where `cover` says, on a layer of its own above `id`
/// or over what the layer of cleans already has. The layer.
pub(crate) fn write_clean(doc: &mut photocraft_doc::Document, id: LayerId, output: &Output, region: Rect, generated: &[f32], cover: &[f32]) -> Result<LayerId> {
    let name = output.name().unwrap_or(CLEAN_LAYER).to_string();
    let existing = if matches!(output, Output::Single(_)) { clean_layer(doc, &name) } else { None };
    let base = doc.pixel_format();
    let nid = match existing {
        Some(l) => l,
        None => {
            let layer_name = if matches!(output, Output::Single(_)) { name } else { doc.next_layer_name(&name) };
            doc.insert_above(Some(id), Layer::raster(layer_name, PixelFormat { alpha: true, ..base }))
        }
    };
    let surf = crate::edit_menu_cmds::writable_surface(doc, nid)?;
    let lf = surf.format();
    let n = lf.channels();
    let mut px = surf.read_region(region);
    px.resize(cover.len() * n, 0.0);
    let mut enc = [0.0f32; 8];
    for (i, &c) in cover.iter().enumerate() {
        if c > 0.0 {
            let o = to_rgba(&lf, &px[i * n..(i + 1) * n]);
            let g = &generated[i * 3..i * 3 + 3];
            // Over what's there: a soft edge over an earlier clean.
            let a = c + o[3] * (1.0 - c);
            let mix = |k: usize| if a > 0.0 { (g[k] * c + o[k] * o[3] * (1.0 - c)) / a } else { g[k] };
            from_rgba_into(&lf, [mix(0), mix(1), mix(2), a], &mut enc);
            px[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
        }
    }
    surf.write_region(region, &px);
    surf.prune();
    Ok(nid)
}

/// Everything the job needs, checked before it starts.
struct Plan {
    layer: LayerId,
    fmt: PixelFormat,
    region: Rect,
    layer_px: Vec<f32>,
    rgb: Vec<f32>,
    cover: Vec<f32>,
    output: Output,
    keep_active: bool,
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

fn plan(s: &Session, p: &Value) -> Result<Head> {
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
    let output = Output::parse(p, Output::New(CLEAN_LAYER.to_string()))?;
    let keep_active = p.get("keepActive").and_then(Value::as_bool).unwrap_or(false);
    let opts = ai_options(s, p, ID, false)?;
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &d.doc;
    let id = source_layer(doc, id, &output);
    let canvas = doc.bounds();
    let sel = doc.selection.as_ref().ok_or_else(|| bad("make a selection around what to clean"))?;
    let sb = sel.content_bounds().intersect(&canvas);
    if sb.is_empty() {
        return Err(bad("the selection is outside the canvas"));
    }
    let region = context_region(sb, canvas, opts.context_margin());
    let pixels = u64::from(region.width()) * u64::from(region.height());
    if pixels > MAX_PIXELS {
        return Err(bad(format!("the area is too large to clean at once ({pixels} pixels, at most {MAX_PIXELS}); select less")));
    }
    doc.layer(id).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(id))?;
    Ok(Head { layer: id, region, output, keep_active, opts, doc: doc.clone() })
}

/// What [`plan`] checked on the page's thread; [`Head::read`] reads the pixels.
struct Head {
    layer: LayerId,
    region: Rect,
    output: Output,
    keep_active: bool,
    opts: AiOptions,
    doc: std::sync::Arc<photocraft_doc::Document>,
}

impl Head {
    /// The region's layer pixels, their RGB and the selection's coverage (on the browser threads
    /// build's pool, in two parts between frames in the single-threaded one: a large selection
    /// takes a while on a phone).
    fn read(self) -> Result<crate::jobs::Prep<Plan>> {
        let Head { layer: id, region, output, keep_active, opts, doc } = self;
        let (w, h) = (region.width() as usize, region.height() as usize);
        let surf = doc.layer(id).and_then(|l| l.surface()).ok_or(EngineError::NoLayer(id))?;
        let sel = doc.selection.as_ref().ok_or_else(|| bad("make a selection around what to clean"))?;
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
        Ok(crate::jobs::Prep::then(move |_| {
            let mut rgb = to_rgb(&fmt, &layer_px);
            under_cleans(&mut rgb, &doc, region, &output);
            Ok(crate::jobs::Prep::Done(Plan { layer: id, fmt, region, layer_px, rgb, cover, output, keep_active, opts }))
        }))
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

/// [`inpaint_ai`] as one undo step named `label` (Remove with AI on a selection).
pub(crate) fn inpaint_ai_labelled(s: &mut Session, p: &Value, label: &'static str) -> Result<Value> {
    let head = plan(s, p)?;
    let svc = ready_inpainter(s, &head.opts)?;
    let device = svc.device();
    crate::jobs::run_async_prepared(
        s,
        label,
        true,
        move |_ctx| head.read(),
        move |ctx, mut plan: Plan, done| {
            ctx.progress(0.0, label);
            let (w, h) = (plan.region.width() as usize, plan.region.height() as usize);
            let (rgb, cover, opts) = (std::mem::take(&mut plan.rgb), plan.cover.clone(), plan.opts.clone());
            let svc2 = svc.clone();
            let started = web_time::Instant::now();
            ai_fill_rgb_async(
                svc,
                ctx.clone(),
                rgb,
                cover,
                w,
                h,
                opts,
                Box::new(move |r| {
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    done.finish(r.map(|(generated, run)| (plan, generated, svc2.device(), run, ms)))
                }),
            );
        },
        move |s, (plan, generated, dev, run, ms)| apply(s, label, plan, &generated, if dev.is_empty() { device } else { dev }, run, ms),
    )
}

fn apply(s: &mut Session, label: &str, plan: Plan, generated: &[f32], device: String, run: FillRun, ms: u64) -> Result<Value> {
    let model = run.model;
    let Plan { layer: id, fmt, region, layer_px, cover, output, keep_active, .. } = plan;
    let n = fmt.channels();
    let count = cover.iter().filter(|c| **c > 0.0).count();
    let layer = s.edit(label, |doc, active| {
        if output != Output::Current {
            let nid = write_clean(doc, id, &output, region, generated, &cover)?;
            if !keep_active {
                *active = Some(nid);
            }
            return Ok(nid);
        }
        let surf = crate::edit_menu_cmds::writable_surface(doc, id)?;
        // In place: each pixel is read before it is written.
        let mut out = layer_px;
        let mut enc = [0.0f32; 8];
        for (i, &c) in cover.iter().enumerate() {
            if c > 0.0 {
                let o = to_rgba(&fmt, &out[i * n..(i + 1) * n]);
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
        "model": model.id,
        "lite": model.lite,
        // `device` too: `CAPISHOT_AI_DEVICE=cpu` forces the CPU without the request knowing.
        "cpu": run.cpu || device == "CPU",
        "device": device,
        "fellBack": run.fell_back,
        "ms": ms,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Clean Selected Area with AI",
        menu: &["Edit"],
        shortcut: None,
        params: r#"{"output"?: "new" (default: the result on a new layer above, alpha = selection coverage) | "single" (on one layer above that gathers every clean, which it also reads) | "current" (paint it into the layer), "layerName"?: str (what those layers are called), "keepActive"?: bool (the active layer stays), "layer"?: layer id (default: the active pixel layer), "maxSide"?: 0 or 64..2048, multiple of 8 (default 512: larger crops are downscaled to this working size; 0: full resolution, larger crops split into overlapping 512 px tiles feathered at their seams), "margin"?: 0..1024 px of context around each masked blob (default 64), "allowDownload"?: bool (fetch the model's weights when not cached: Manga LaMa about 100 MB, MI-GAN 13 MB; default false), "quality"?: "auto"|"fast"|"best" (fast: the lite MI-GAN, at most 512 px tiles; auto: Manga LaMa on a capable GPU, MI-GAN on the CPU, phones and weak machines), "device"?: "auto"|"gpu"|"cpu"|"directml"|"wgpu" (directml, wgpu: natively, that GPU runtime; auto picks DirectML on Windows for Manga LaMa when ONNX Runtime is installed), "model"?: "auto"|"manga-lama"|"migan-512", "threads"?: 0..64 (CPU threads, 0 = one per core), "keepLoaded"?: bool (default true)} -> {"layer", "pixels", "region": [x,y,w,h], "device", "model", "lite", "cpu" (it ran on the CPU, also when an automatic run found no usable GPU), "fellBack" (it left where it was headed: the GPU failed mid-run and the CPU finished it, DirectML couldn't run it, or there was no usable GPU; then "notices" say which: ai.gpuFallback, ai.directmlFallback, ai.cpuFallback), "ms" (the run's wall time)}. Erases what the selection covers (text, SFX) and fills it with Manga LaMa (or MI-GAN). Background job, one undo step."#,
        enabled,
        run: inpaint_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_inpaint_cmds/tests.rs"]
mod tests;
