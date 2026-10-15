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
//! with progress and cancellation. A model that isn't cached yet needs `download: true`: the
//! dialog asks the user first, showing its size and licence.

use std::sync::Arc;

use photocraft_ai::catalog::{self, Content, ModelInfo};
use photocraft_ai::mangajanai;
use photocraft_ai::service::{Hooks, ModelState, Request, Upscaler};
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
    let model_param = p.get("model").and_then(Value::as_str).unwrap_or("auto");
    let manual = match model_param {
        "auto" | "" => None,
        id => Some(catalog::model(id).filter(|m| m.arch == catalog::Arch::Esrgan).ok_or_else(|| bad(format!("unknown model `{id}`; the models are {}", ids())))?),
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
        (Some(m), _) => (m.content == Content::Grayscale, false),
        (None, "grayscale") => (true, false),
        (None, "color") => (false, false),
        (None, "auto") => (gray_doc || mangajanai::is_grayscale(&composite_rgb(doc), mangajanai::GRAYSCALE_THRESHOLD), true),
        (None, other) => return Err(bad(format!("`content` must be auto, grayscale or color, not `{other}`"))),
    };
    let model = match manual {
        Some(m) => m,
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
    Ok(Plan { model, grayscale, detected, auto_levels, width: doc.size.width, height: doc.size.height, output, state: svc.model_state(model) })
}

fn ids() -> String {
    catalog::MODELS.iter().filter(|m| m.arch == catalog::Arch::Esrgan).map(|m| m.id).collect::<Vec<_>>().join(", ")
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
    image: Planar,
    alpha: Option<Vec<f32>>,
}

fn upscale_ai(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = plan(s, p)?;
    let download = p.get("download").and_then(Value::as_bool).unwrap_or(false);
    let m = plan.model;
    match &plan.state {
        ModelState::Ready => {}
        ModelState::NeedsDownload if download => {}
        ModelState::NeedsDownload => {
            return Err(EngineError::Other(format!(
                "{} needs a one-time download of {:.1} MB ({} licence, by {}); run again with \"download\": true to agree",
                m.title,
                m.bytes as f64 / 1e6,
                m.license,
                m.author
            )));
        }
        // The browser's cache answers asynchronously; with consent the worker finds out itself.
        ModelState::Checking if download => {}
        ModelState::Checking => return Err(EngineError::Other("still checking whether the model is downloaded; try again in a moment".into())),
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
    jobs::run_async(
        s,
        &label,
        output == Output::Replace,
        move |ctx, done| {
            ctx.progress(0.0, &format!("Starting {}", m.title));
            let req = Request { model: m, input, grayscale, allow_download: download };
            svc.run(
                req,
                Arc::new(JobHooks(ctx.clone())),
                Box::new(move |r| {
                    done.finish(r.map(|image| Upscaled { image, alpha: alpha.map(|a| upscale_plane(&a, w, h, scale)) }).map_err(|e| match e {
                        AiError::Cancelled => EngineError::Cancelled,
                        e => EngineError::Other(e.to_string()),
                    }))
                }),
            );
        },
        move |s, up: Upscaled| apply(s, up, m, output, content),
    )
}

/// Puts the upscaled image in the document (one undo step) or a new document.
fn apply(s: &mut Session, up: Upscaled, m: &'static ModelInfo, output: Output, content: &str) -> Result<Value> {
    let img = &up.image;
    let (w, h) = (img.width, img.height);
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (ow, oh) = (st.doc.size.width, st.doc.size.height);
    let fmt = st.doc.pixel_format();
    if w != ow as usize * m.scale as usize || h != oh as usize * m.scale as usize {
        return Err(EngineError::Other(format!("the model returned {w} x {h} px for a {ow} x {oh} px image")));
    }
    let n = w * h;
    let mut vals = Vec::with_capacity(n * fmt.channels());
    let mut px = vec![0.0f32; fmt.channels()];
    for i in 0..n {
        let v = |c: usize| img.data.get(c.min(img.channels.saturating_sub(1)) * n + i).copied().unwrap_or(0.0);
        let a = up.alpha.as_ref().and_then(|a| a.get(i)).copied().unwrap_or(1.0).clamp(0.0, 1.0);
        // Un-flatten from white where transparent.
        let un = |x: f32| if a > 1e-4 { ((x - (1.0 - a)) / a).clamp(0.0, 1.0) } else { 0.0 };
        let k = photocraft_raster::from_rgba_into(&fmt, [un(v(0)), un(v(1)), un(v(2)), a], &mut px);
        vals.extend_from_slice(px.get(..k).unwrap_or(&[]));
    }
    let (nw, nh) = (w as u32, h as u32);
    let rect = photocraft_geom::Rect::from_xywh(0, 0, nw, nh);
    let mut surf = Surface::new(fmt);
    surf.write_region(rect, &vals);
    surf.prune();
    let k = f64::from(m.scale);
    let affine = photocraft_geom::Affine { m: [k, 0.0, 0.0, k, 0.0, 0.0] };
    let result = json!({"width": nw, "height": nh, "model": m.id, "scale": m.scale, "content": content});
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
        params: r##"{"scale":2|4=2,"model":"auto|<id from photocraft_ai::catalog>"="auto","content":"auto|grayscale|color"="auto","autoLevels":bool=true (grayscale only, as MangaJaNai),"output":"auto|replace|newDocument"="auto" (auto: replace a single-layer document, else open a new one),"download":bool=false (agree to download the model's weights when they aren't cached)} → {width,height,model,scale,content,document?} (a background job: MangaJaNai for grayscale pages by page height, IllustrationJaNai for colour)"##,
        enabled,
        run: upscale_ai,
        journal: true,
    }]
}

#[cfg(test)]
#[path = "ai_cmds_tests.rs"]
mod tests;
