//! Image › Upscale with AI… (`image.upscaleAi`): MangaJaNai (black-and-white manga) and
//! IllustrationJaNai (colour) super-resolution on the user's own GPU, through the shell's
//! injected [`Upscaler`] (photocraft-ai: in-process natively, the AI Web Worker in the browser).
//!
//! The document's composite is upscaled 2x or 4x. As MangaJaNaiConverterGui does it, a grayscale
//! page (a Grayscale document, or an RGB one that [`mangajanai::is_grayscale`] finds gray) gets
//! auto levels and the MangaJaNai model trained for the nearest page height; a colour page gets
//! IllustrationJaNai. Transparency is upscaled bilinearly beside the model.
//!
//! The result lands like Image Size, as one undo step (`output: "replace"`: the document becomes
//! a single layer at the new size), or opens as a new document (`"newDocument"`, the default for
//! documents with more than one layer, like Photoshop's Super Zoom). It runs as a background job
//! with progress and cancellation. A model that isn't downloaded fails with "… isn't downloaded"
//! (the UI shows its missing-model dialog; models are downloaded with `ai.models.*`, the AI Model
//! Manager); an agent may pass `download: true` to fetch it as part of the run.
//!
//! `quality` (auto, fast, best) and `device` (auto, gpu, cpu) pick between the full models and
//! the lite one (Real-ESRGAN Compact, for phones, weak machines and CPUs) through
//! [`photocraft_ai::profile::choose`] on the shell's device profile; the plan carries the
//! reason, so the dialog says why. `tile`, `overlap`, `threads` and `keepLoaded` are the
//! dialog's Advanced settings.

use std::sync::Arc;

use photocraft_ai::catalog::{self, Content, ModelInfo};
use photocraft_ai::mangajanai;
use photocraft_ai::profile::{self, Choice, DevicePref, DeviceProfile, Quality};
use photocraft_ai::service::{Hooks, ModelState, Request, RunOptions, Upscaler};
use photocraft_ai::tile::TileOptions;
use photocraft_ai::{AiError, Planar};
use photocraft_color::ColorMode;
use photocraft_doc::{Layer, LayerContent, Size};
use photocraft_raster::Surface;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::jobs::{self, JobCtx};
use crate::{EngineError, Result, Session};

pub const COMMAND: &str = "image.upscaleAi";
/// Why the command is disabled without an injected upscaler.
pub const UNAVAILABLE: &str = "AI upscaling is not available in this build";

impl Session {
    /// Injects (or removes) the model runner `image.upscaleAi` uses.
    pub fn set_ai_upscaler(&mut self, upscaler: Option<Arc<dyn Upscaler>>) {
        self.ai_upscaler = upscaler;
    }

    /// The injected model runner, if any.
    pub fn ai_upscaler(&self) -> Option<Arc<dyn Upscaler>> {
        self.ai_upscaler.clone()
    }

    /// What the AI runtime knows about this machine (unknown without an upscaler).
    pub fn ai_profile(&self) -> DeviceProfile {
        self.ai_upscaler.as_ref().map(|u| u.profile()).unwrap_or_default()
    }
}

/// The user's Quality and Device choices and what they resolve to on this machine.
#[derive(Clone, Debug, PartialEq)]
pub struct RunChoice {
    pub quality: Quality,
    pub device: DevicePref,
    pub choice: Choice,
    pub profile: DeviceProfile,
}

/// Reads `quality` and `device` (both default `auto`) for `cmd`.
pub(crate) fn run_choice(s: &Session, p: &Value, cmd: &str) -> Result<RunChoice> {
    let bad = |m: String| EngineError::BadParams { cmd: cmd.into(), msg: m };
    let text = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("auto");
    let quality = Quality::parse(text("quality")).ok_or_else(|| bad("`quality` must be auto, fast or best".into()))?;
    let device = DevicePref::parse(text("device")).ok_or_else(|| bad("`device` must be auto, gpu or cpu".into()))?;
    let profile = s.ai_profile();
    let choice = profile::choose(&profile, quality, device);
    Ok(RunChoice { quality, device, choice, profile })
}

/// The notice when a run fell back to the CPU after the GPU failed ([`crate::jobs::JobCtx::notice`]).
pub const GPU_FALLBACK_NOTICE: &str = "GPU couldn't run this model; used the CPU instead";
/// Its class.
pub const GPU_FALLBACK_CLASS: &str = "ai.gpuFallback";

/// After the GPU failed: the same Quality on the CPU (Auto then means the lite model, as for any
/// CPU run; Best keeps the full one).
pub(crate) fn on_cpu(run: &RunChoice) -> RunChoice {
    let choice = profile::choose(&run.profile, run.quality, DevicePref::Cpu);
    RunChoice { device: DevicePref::Cpu, choice, ..run.clone() }
}

/// An optional integer param in `lo..=hi`.
pub(crate) fn uint_param(p: &Value, k: &str, lo: u64, hi: u64, cmd: &str) -> Result<Option<u64>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|x| (lo..=hi).contains(x))
            .map(Some)
            .ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: format!("`{k}` must be an integer in {lo}..={hi}") }),
    }
}

/// `keepLoaded` (default true).
pub(crate) fn keep_loaded(p: &Value, cmd: &str) -> Result<bool> {
    match p.get("keepLoaded") {
        None | Some(Value::Null) => Ok(true),
        Some(v) => v.as_bool().ok_or_else(|| EngineError::BadParams { cmd: cmd.into(), msg: "`keepLoaded` must be true or false".into() }),
    }
}

/// Where the result goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Replace,
    NewDocument,
}

/// What `image.upscaleAi` would do with these params on the active document (the dialog shows
/// it before running).
#[derive(Clone, Debug)]
pub struct Plan {
    pub model: &'static ModelInfo,
    pub grayscale: bool,
    /// The page was detected (not told) to be grayscale or colour.
    pub detected: bool,
    pub auto_levels: bool,
    pub width: u32,
    pub height: u32,
    pub output: Output,
    pub state: ModelState,
    /// Quality and Device, resolved.
    pub run: RunChoice,
    /// The Advanced settings (device included).
    pub opts: RunOptions,
}

impl Plan {
    pub fn scale(&self) -> u32 {
        self.model.scale
    }
    pub fn new_width(&self) -> u64 {
        u64::from(self.width) * u64::from(self.model.scale)
    }
    pub fn new_height(&self) -> u64 {
        u64::from(self.height) * u64::from(self.model.scale)
    }
}

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: COMMAND.into(), msg: msg.into() }
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document open")?;
    if s.ai_upscaler.is_none() {
        return Err(UNAVAILABLE.into());
    }
    supported_mode(st.doc.mode).map_err(|e| e.to_string())
}

fn supported_mode(mode: ColorMode) -> Result<()> {
    match mode {
        ColorMode::Rgb | ColorMode::Indexed | ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => Ok(()),
        _ => Err(EngineError::Other("AI upscaling works on RGB and Grayscale images; convert the document first (Image › Mode)".into())),
    }
}

/// Reads the params against the active document (without running anything).
pub fn plan(s: &Session, p: &Value) -> Result<Plan> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let svc = s.ai_upscaler.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    supported_mode(st.doc.mode)?;
    let doc = &st.doc;
    let run = run_choice(s, p, COMMAND)?;
    let model_param = p.get("model").and_then(Value::as_str).unwrap_or("auto");
    let manual = match model_param {
        "auto" | "" => None,
        id => Some(catalog::model(id).filter(|m| is_upscaler(m)).ok_or_else(|| bad(format!("unknown model `{id}`; the models are {}", ids())))?),
    };
    let scale = match p.get("scale") {
        None | Some(Value::Null) => manual.map_or(2, |m| m.scale),
        Some(v) => match v.as_u64() {
            Some(n @ (2 | 4)) => n as u32,
            _ => return Err(bad("`scale` must be 2 or 4")),
        },
    };
    if let Some(m) = manual
        && m.scale != scale
    {
        return Err(bad(format!("{} upscales {}x, not {scale}x", m.title, m.scale)));
    }
    let gray_doc = doc.pixel_format().mode == ColorMode::Grayscale;
    let (grayscale, detected) = match (manual, p.get("content").and_then(Value::as_str).unwrap_or("auto")) {
        (Some(m), _) if m.content != Content::Any => (m.content == Content::Grayscale, false),
        (Some(_), "grayscale") => (true, false),
        (Some(_), "color") => (false, false),
        (Some(_), _) => (gray_doc || mangajanai::is_grayscale(&composite_rgb(doc), mangajanai::GRAYSCALE_THRESHOLD), true),
        (None, "grayscale") => (true, false),
        (None, "color") => (false, false),
        (None, "auto") => (gray_doc || mangajanai::is_grayscale(&composite_rgb(doc), mangajanai::GRAYSCALE_THRESHOLD), true),
        (None, other) => return Err(bad(format!("`content` must be auto, grayscale or color, not `{other}`"))),
    };
    let model = match manual {
        Some(m) => m,
        None if run.choice.lite => lite(scale)?,
        None => mangajanai::select(doc.size.height as usize, scale, if grayscale { Content::Grayscale } else { Content::Color })
            .map_err(|e| EngineError::Other(e.to_string()))?,
    };
    let output = match p.get("output").and_then(Value::as_str).unwrap_or("auto") {
        "replace" => Output::Replace,
        "newDocument" => Output::NewDocument,
        "auto" => {
            if doc.layers.len() <= 1 {
                Output::Replace
            } else {
                Output::NewDocument
            }
        }
        other => return Err(bad(format!("`output` must be auto, replace or newDocument, not `{other}`"))),
    };
    let auto_levels = grayscale && p.get("autoLevels").and_then(Value::as_bool).unwrap_or(true);
    let opts = run_options(p, &run, model)?;
    Ok(Plan { model, grayscale, detected, auto_levels, width: doc.size.width, height: doc.size.height, output, state: svc.model_state(model), run, opts })
}

fn is_upscaler(m: &ModelInfo) -> bool {
    matches!(m.arch, catalog::Arch::Esrgan | catalog::Arch::Compact)
}

/// The lite upscaler for `scale`.
pub fn lite(scale: u32) -> Result<&'static ModelInfo> {
    let id = if scale <= 2 { catalog::COMPACT_2X_ID } else { catalog::COMPACT_4X_ID };
    catalog::model(id).ok_or_else(|| EngineError::Other(format!("{id} isn't in the catalog")))
}

/// The Advanced settings: `tile` (32..=1024 input pixels; default the model's, halved on
/// machines with very little memory), `overlap` (0..=128, default 16), `threads` (0..=64, 0 =
/// one per core) and `keepLoaded`.
fn run_options(p: &Value, run: &RunChoice, model: &ModelInfo) -> Result<RunOptions> {
    let d = photocraft_ai::tile::default_tiles(model);
    let default_tile = if run.choice.small_tiles { (d.tile / 2).max(64) } else { d.tile };
    let tile = uint_param(p, "tile", 32, 1024, COMMAND)?.map_or(default_tile, |t| t as usize);
    let overlap = uint_param(p, "overlap", 0, 128, COMMAND)?.map_or(d.overlap, |o| o as usize);
    if overlap * 2 >= tile {
        return Err(bad(format!("`overlap` {overlap} must be less than half the tile ({tile})")));
    }
    let threads = uint_param(p, "threads", 0, 64, COMMAND)?.unwrap_or(0) as usize;
    let tiles = (tile != d.tile || overlap != d.overlap).then_some(TileOptions { tile, overlap });
    Ok(RunOptions { cpu: run.choice.cpu, tiles, threads, unload: !keep_loaded(p, COMMAND)? })
}

fn ids() -> String {
    catalog::MODELS.iter().filter(|m| is_upscaler(m)).map(|m| m.id).collect::<Vec<_>>().join(", ")
}

/// The composite flattened over white, as RGB.
fn composite_rgb(doc: &photocraft_doc::Document) -> Planar {
    let (rgb, _) = composite(doc);
    rgb
}

/// The composite as RGB over white plus its alpha (None when opaque).
fn composite(doc: &photocraft_doc::Document) -> (Planar, Option<Vec<f32>>) {
    let buf = photocraft_compose::flatten(doc);
    let (w, h) = (doc.size.width as usize, doc.size.height as usize);
    let n = w * h;
    let mut data = vec![0.0f32; n * 3];
    let mut alpha = Vec::with_capacity(n);
    let mut opaque = true;
    for (i, q) in buf.px.iter().enumerate().take(n) {
        let a = q[3].clamp(0.0, 1.0);
        opaque &= a >= 1.0;
        alpha.push(a);
        for (c, v) in q.iter().take(3).enumerate() {
            if let Some(d) = data.get_mut(c * n + i) {
                *d = v.clamp(0.0, 1.0) * a + (1.0 - a);
            }
        }
    }
    let rgb = Planar { width: w, height: h, channels: 3, data };
    (rgb, (!opaque).then_some(alpha))
}

/// Bilinear (pixel-centre aligned) upscaling of one plane by an integer factor.
fn upscale_plane(p: &[f32], w: usize, h: usize, s: usize) -> Vec<f32> {
    let (ow, oh) = (w * s, h * s);
    let at = |x: usize, y: usize| p.get(y.min(h.saturating_sub(1)) * w + x.min(w.saturating_sub(1))).copied().unwrap_or(0.0);
    let mut out = Vec::with_capacity(ow * oh);
    for oy in 0..oh {
        let fy = ((oy as f32 + 0.5) / s as f32 - 0.5).max(0.0);
        let (y0, ty) = (fy.floor() as usize, fy.fract());
        for ox in 0..ow {
            let fx = ((ox as f32 + 0.5) / s as f32 - 0.5).max(0.0);
            let (x0, tx) = (fx.floor() as usize, fx.fract());
            let top = at(x0, y0) * (1.0 - tx) + at(x0 + 1, y0) * tx;
            let bottom = at(x0, y0 + 1) * (1.0 - tx) + at(x0 + 1, y0 + 1) * tx;
            out.push(top * (1.0 - ty) + bottom * ty);
        }
    }
    out
}

/// Progress and cancellation of the job, for the upscaler.
struct JobHooks(JobCtx);

impl Hooks for JobHooks {
    fn progress(&self, fraction: f32, status: &str) {
        self.0.progress(fraction, status);
    }
    fn cancelled(&self) -> bool {
        self.0.cancelled()
    }
}

/// What the job hands to `apply`.
struct Upscaled {
    /// The upscaled image's size.
    width: usize,
    height: usize,
    /// Its pixels, in the document's format ([`to_surface`]).
    surface: Surface,
    /// The model that ran (the lite one after a GPU failure, see [`run_upscale`]).
    model: &'static ModelInfo,
}

/// Receives the upscaled image and the model that made it.
type UpscaleDone = Box<dyn FnOnce(photocraft_ai::Result<(Planar, &'static ModelInfo)>) + Send>;

/// What a GPU run needs to run again on the CPU.
struct CpuRetry {
    input: Planar,
    run: RunChoice,
    /// The params named the model: keep it.
    pinned: bool,
}

/// Runs `req`; when the GPU couldn't run it and `retry` is given, runs it again on the CPU with
/// the model the user's Quality picks there (Auto: the lite one, when it is here or may be
/// downloaded; else the same model), and says so ([`GPU_FALLBACK_NOTICE`]).
fn run_upscale(svc: Arc<dyn Upscaler>, ctx: JobCtx, req: Request, retry: Option<CpuRetry>, finish: UpscaleDone) {
    let model = req.model;
    let (grayscale, allow_download, opts) = (req.grayscale, req.allow_download, req.opts);
    let again = svc.clone();
    let hooks = Arc::new(JobHooks(ctx.clone()));
    svc.run(
        req,
        hooks,
        Box::new(move |r| match (r, retry) {
            (Err(AiError::Gpu(why)), Some(retry)) => {
                let run = on_cpu(&retry.run);
                let mut m = model;
                if !retry.pinned
                    && run.choice.lite
                    && let Ok(l) = lite(model.scale)
                    && (allow_download || again.model_state(l) == ModelState::Ready)
                {
                    m = l;
                }
                ctx.notice(GPU_FALLBACK_CLASS, GPU_FALLBACK_NOTICE, &why);
                let req = Request { model: m, input: retry.input, grayscale, allow_download, opts: RunOptions { cpu: true, ..opts } };
                run_upscale(again, ctx, req, None, finish);
            }
            (r, _) => finish(r.map(|image| (image, model))),
        }),
    );
}

fn upscale_ai(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = plan(s, p)?;
    let download = p.get("download").and_then(Value::as_bool).unwrap_or(false);
    let m = plan.model;
    match &plan.state {
        ModelState::Ready => {}
        ModelState::NeedsDownload if download => {}
        ModelState::NeedsDownload => return Err(EngineError::Other(crate::ai_models_cmds::not_downloaded(m.title, m.bytes, "download"))),
        // The browser's cache answers asynchronously: run, and a missing model fails the run
        // with the same "isn't downloaded" error.
        ModelState::Checking => {}
        ModelState::Unavailable(why) => return Err(EngineError::Other(why.clone())),
    }
    let (nw, nh) = (plan.new_width(), plan.new_height());
    if nw > photocraft_ai::image::MAX_SIDE as u64 || nh > photocraft_ai::image::MAX_SIDE as u64 {
        return Err(bad(format!("{nw} x {nh} px is too large; AI upscaling allows up to {} px a side", photocraft_ai::image::MAX_SIDE)));
    }
    let samples = nw.saturating_mul(nh).saturating_mul(if plan.grayscale { 1 } else { 3 });
    if samples > photocraft_ai::image::MAX_SAMPLES as u64 {
        return Err(bad(format!("{nw} x {nh} px is too large for AI upscaling; upscale a smaller image or a crop")));
    }
    {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        crate::image_cmds::check_resample_budget(COMMAND, &st.doc, nw as u32, nh as u32, "")?;
    }
    let svc = s.ai_upscaler.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    let doc = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let fmt = doc.pixel_format();
    let (rgb, alpha) = composite(&doc);
    let input = if plan.grayscale {
        let g = mangajanai::to_gray(&rgb);
        if plan.auto_levels { mangajanai::auto_levels(&g) } else { g }
    } else {
        rgb
    };
    let scale = m.scale as usize;
    let (w, h) = (input.width, input.height);
    let grayscale = plan.grayscale;
    let output = plan.output;
    let label = format!("Upscale with AI ({}x)", m.scale);
    let content = if grayscale { "grayscale" } else { "color" };
    let opts = plan.opts;
    let run = plan.run.clone();
    let pinned = !matches!(p.get("model").and_then(Value::as_str).unwrap_or("auto"), "auto" | "");
    jobs::run_async(
        s,
        &label,
        output == Output::Replace,
        move |ctx, done| {
            ctx.progress(0.0, &format!("Starting {}", m.title));
            // A GPU run keeps the input for a CPU retry.
            let retry = (!opts.cpu).then(|| CpuRetry { input: input.clone(), run, pinned });
            let req = Request { model: m, input, grayscale, allow_download: download, opts };
            let finish: UpscaleDone = Box::new(move |r| {
                // The result arrives on the page's thread (an AI worker message): the layer's
                // pixels are made on a worker, so a big upscale doesn't hold the page.
                off_thread(move || {
                    done.finish(
                        r.map(|(image, model)| {
                            let alpha = alpha.map(|a| upscale_plane(&a, w, h, scale));
                            Upscaled { width: image.width, height: image.height, surface: to_surface(&image, alpha.as_deref(), fmt), model }
                        })
                        .map_err(|e| match e {
                            AiError::Cancelled => EngineError::Cancelled,
                            e => EngineError::Other(e.to_string()),
                        }),
                    );
                });
            });
            run_upscale(svc, ctx.clone(), req, retry, finish);
        },
        move |s, up: Upscaled| apply(s, up, output, content),
    )
}

/// Puts the upscaled image in the document (one undo step) or a new document.
fn apply(s: &mut Session, up: Upscaled, output: Output, content: &str) -> Result<Value> {
    let m = up.model;
    let (w, h) = (up.width, up.height);
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (ow, oh) = (st.doc.size.width, st.doc.size.height);
    if w != ow as usize * m.scale as usize || h != oh as usize * m.scale as usize {
        return Err(EngineError::Other(format!("the model returned {w} x {h} px for a {ow} x {oh} px image")));
    }
    if up.surface.format() != st.doc.pixel_format() {
        return Err(EngineError::Other("the document's mode changed while it was upscaled".into()));
    }
    let surf = up.surface;
    let (nw, nh) = (w as u32, h as u32);
    let k = f64::from(m.scale);
    let affine = photocraft_geom::Affine { m: [k, 0.0, 0.0, k, 0.0, 0.0] };
    let result = json!({"width": nw, "height": nh, "model": m.id, "scale": m.scale, "content": content, "lite": m.lite});
    match output {
        Output::Replace => {
            s.set_sampler_map(affine);
            let label = format!("Upscale with AI ({}x)", m.scale);
            s.edit(&label, move |doc, active| {
                replace_with(doc, surf, nw, nh, &affine);
                *active = doc.layers.first().map(|l| l.id);
                Ok(())
            })?;
            Ok(result)
        }
        Output::NewDocument => {
            let mut doc = (*st.doc).clone();
            doc.id = photocraft_doc::DocId::fresh();
            let stem = doc.name.rsplit_once('.').map_or(doc.name.as_str(), |(a, _)| a).to_string();
            doc.name = format!("{stem} ({}x AI)", m.scale);
            replace_with(&mut doc, surf, nw, nh, &affine);
            let index = s.add_document(doc, None);
            let mut r = result;
            r["document"] = json!(index);
            Ok(r)
        }
    }
}

/// The upscaled `img` (planar) as a surface in `fmt`, un-flattened from white where `alpha` makes
/// it transparent.
fn to_surface(img: &Planar, alpha: Option<&[f32]>, fmt: photocraft_color::PixelFormat) -> Surface {
    let (w, h) = (img.width, img.height);
    let n = w * h;
    let mut vals = Vec::with_capacity(n * fmt.channels());
    let mut px = vec![0.0f32; fmt.channels()];
    for i in 0..n {
        let v = |c: usize| img.data.get(c.min(img.channels.saturating_sub(1)) * n + i).copied().unwrap_or(0.0);
        let a = alpha.and_then(|a| a.get(i)).copied().unwrap_or(1.0).clamp(0.0, 1.0);
        // Un-flatten from white where transparent.
        let un = |x: f32| if a > 1e-4 { ((x - (1.0 - a)) / a).clamp(0.0, 1.0) } else { 0.0 };
        let k = photocraft_raster::from_rgba_into(&fmt, [un(v(0)), un(v(1)), un(v(2)), a], &mut px);
        vals.extend_from_slice(px.get(..k).unwrap_or(&[]));
    }
    let mut surf = Surface::new(fmt);
    surf.write_region(photocraft_geom::Rect::from_xywh(0, 0, w as u32, h as u32), &vals);
    surf.prune();
    surf
}

/// Runs `f` on a worker where there are workers (natively, the browser threads build), else here.
pub(crate) fn off_thread(f: impl FnOnce() + Send + 'static) {
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    rayon::spawn(f);
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    f();
}

/// `doc` as one layer holding `surf`, at `nw × nh`, its guides and other geometry scaled.
fn replace_with(doc: &mut photocraft_doc::Document, surf: Surface, nw: u32, nh: u32, affine: &photocraft_geom::Affine) {
    doc.layers = vec![Layer::new("Background", LayerContent::Raster(surf))];
    doc.layer_comps.clear();
    doc.last_applied_comp = None;
    doc.last_document_state = None;
    doc.selection = None;
    doc.channels.clear();
    doc.quick_mask = None;
    crate::canvas_geom::transform_geometry(doc, affine);
    doc.size = Size::new(nw, nh);
    crate::canvas_geom::refresh(doc, crate::canvas_geom::Refresh::All);
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: COMMAND,
        label: "Upscale with AI…",
        menu: &["Image"],
        shortcut: None,
        params: r##"{"scale":2|4=2,"model":"auto|<id from photocraft_ai::catalog>"="auto","quality":"auto|fast|best"="auto" (fast: the lite Real-ESRGAN Compact; auto: full models on a capable GPU, lite on the CPU, phones and weak machines),"device":"auto|gpu|cpu"="auto","tile":32..1024 (input pixels per tile; default the model's),"overlap":0..128=16,"threads":0..64=0 (CPU threads, 0 = one per core),"keepLoaded":bool=true,"content":"auto|grayscale|color"="auto","autoLevels":bool=true (grayscale only, as MangaJaNai),"output":"auto|replace|newDocument"="auto" (auto: replace a single-layer document, else open a new one),"download":bool=false (agents: fetch the model's weights when they aren't downloaded; the UI downloads with ai.models.download)} → {width,height,model,scale,content,document?} (a background job: MangaJaNai for grayscale pages by page height, IllustrationJaNai for colour)"##,
        enabled,
        run: upscale_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_cmds_tests.rs"]
mod tests;
