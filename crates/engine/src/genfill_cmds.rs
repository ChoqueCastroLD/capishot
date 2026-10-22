//! Edit › Generative Fill (`edit.generativeFill`): fill the selection with new content from a
//! text prompt (empty: plain background), locally with Stable Diffusion 1.5 inpainting
//! (`photocraft-ai-sd`), as several variants. The result is a new layer above the source with a
//! layer mask from the selection; `edit.generativeFillVariant` swaps which variant the layer
//! shows (each swap one undo step). Variants live in the session (not in the file).
//!
//! The engine plans the crop (selection bounds plus context, resized to the tier's working
//! size), checks and translates the prompt ([`photocraft_ai_sd::prompt`]: refuses sexual content
//! involving minors and clearly illegal requests; Spanish and Portuguese → English), and pastes
//! the result back; the model runs behind the injected [`GenFill`] service
//! ([`Session::set_genfill`]): natively `photocraft_ai_sd::native::NativeGenFill`, in the browser
//! the AI worker. Without a service the command fails with [`UNAVAILABLE`].

use std::sync::Arc;

use photocraft_ai::profile::GpuClass;
use photocraft_ai_sd::crop::{self, Crop};
use photocraft_ai_sd::service::{GenFill, Hooks, ModelState, Request, Tier};
use photocraft_ai_sd::{SdError, prompt};
use photocraft_color::PixelFormat;
use photocraft_doc::{Layer, LayerId, LayerMask};
use photocraft_geom::Rect;
use photocraft_raster::from_rgba_into;
use serde_json::{Value, json};

use crate::ai_models_cmds::not_downloaded;
use crate::commands::CommandSpec;
use crate::jobs::JobCtx;
use crate::{EngineError, Result, Session};

/// The command ids.
pub const ID: &str = "edit.generativeFill";
pub const VARIANT_ID: &str = "edit.generativeFillVariant";
const LABEL: &str = "Generative Fill";
/// Why the command fails in a build without the service.
pub const UNAVAILABLE: &str = "Generative Fill is not available in this build";
/// Why it can't run with little memory.
pub const LOW_MEMORY: &str = "Not enough memory for Generative Fill on this device (the light model needs about 1 GB free); use Edit › Clean with AI to remove things";
/// Why Standard (and up) can't run on a phone or with little memory.
pub const LOW_MEMORY_STANDARD: &str = "Not enough memory for this Generative Fill quality on this device (it needs about 8 GB); choose Quality: Ultra-light";
/// How much an empty-prompt fill re-draws Manga LaMa's result (variants differ, the background
/// stays) and the smooth pre-fill when LaMa isn't downloaded.
const LAMA_STRENGTH: f64 = 0.5;
const PREFILL_STRENGTH: f64 = 0.7;
/// Largest region read for one run.
const MAX_PIXELS: u64 = 64 << 20;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: msg.into() }
}

/// The variants of one Generative Fill layer.
#[derive(Clone, Debug)]
pub struct Variants {
    /// The crop in document pixels.
    pub region: Rect,
    /// Interleaved RGB per variant, `region` sized.
    pub images: Vec<Vec<f32>>,
    pub selected: usize,
    /// The prompt as typed and as the model read it.
    pub prompt: String,
    pub english: String,
    pub seed: u64,
}

impl Session {
    /// Injects (or removes) the service that runs Generative Fill.
    pub fn set_genfill(&mut self, svc: Option<Arc<dyn GenFill>>) {
        self.genfill = svc;
    }

    /// The injected Generative Fill service, if any.
    pub fn genfill(&self) -> Option<&Arc<dyn GenFill>> {
        self.genfill.as_ref()
    }

    /// The variants of Generative Fill layer `layer` in the active document, if it is one.
    pub fn genfill_variants(&self, layer: LayerId) -> Option<&Variants> {
        let doc = self.active()?.doc.id;
        self.genfill_layers.get(&(doc, layer))
    }
}

/// The tier these params choose (`quality`: auto|ultra|fast|standard|high), or why none can run.
pub fn tier_for(s: &Session, p: &Value) -> Result<Tier> {
    let q = p.get("quality").and_then(Value::as_str).unwrap_or("auto");
    let device = p.get("device").and_then(Value::as_str).unwrap_or("auto");
    if !["auto", "gpu", "cpu"].contains(&device) {
        return Err(bad("`device` must be auto, gpu or cpu"));
    }
    let profile = s.ai_profile();
    // `navigator.deviceMemory` stops at 8: 8 means "8 GB or more". Standard's model alone is
    // 2.3 GB; Ultra-light runs in about 1 GB.
    let mem = profile.memory_gb;
    let small = profile.mobile || mem.is_some_and(|m| m < 7.5);
    if mem.is_some_and(|m| m < 1.5) {
        return Err(EngineError::Other(LOW_MEMORY.into()));
    }
    let tier = match q {
        // A real GPU: Standard, or Fast (1 GB download) with less memory. No GPU: Fast on a
        // desktop with CPU threads and memory, else Ultra-light.
        "auto" => {
            let gpu = device != "cpu" && !matches!(profile.gpu, GpuClass::None | GpuClass::Software);
            match (gpu, small, profile.mobile) {
                (true, false, _) => Tier::Standard,
                (true, true, false) if mem.is_none_or(|m| m >= 3.5) => Tier::Fast,
                (false, false, false) if profile.cpu_threads => Tier::Fast,
                _ => Tier::Ultra,
            }
        }
        q => Tier::from_id(q).ok_or_else(|| bad("`quality` must be auto, ultra, fast, standard or high"))?,
    };
    if small && !matches!(tier, Tier::Ultra | Tier::Fast) || profile.mobile && tier == Tier::Fast {
        return Err(EngineError::Other(LOW_MEMORY_STANDARD.into()));
    }
    Ok(tier)
}

/// The working size: the tier's, smaller on phones and devices with little memory.
fn default_side(s: &Session, tier: Tier) -> usize {
    let p = s.ai_profile();
    let gpu = !matches!(p.gpu, GpuClass::None | GpuClass::Software);
    match tier {
        Tier::Ultra if p.mobile || p.memory_gb.is_some_and(|m| m <= 4.0) => 256,
        // Fast on the CPU: 384 px.
        Tier::Fast if !gpu => 384,
        t => t.side(),
    }
}

/// The tier these params choose and whether it can run now (the UI's download prompt).
pub fn model_state(s: &Session, p: &Value) -> Option<(Tier, ModelState)> {
    let tier = tier_for(s, p).ok()?;
    Some((tier, s.genfill.as_ref()?.model_state(tier)))
}

fn enabled(s: &Session) -> std::result::Result<(), String> {
    if s.genfill.is_none() {
        return Err(UNAVAILABLE.into());
    }
    crate::edit_menu_cmds::pixel_layer(s)?;
    let d = s.active().ok_or("no document open")?;
    if d.doc.selection.as_ref().is_none_or(|m| m.content_bounds().is_empty()) {
        return Err("select the area to fill".into());
    }
    Ok(())
}

/// Everything the job needs, checked before it starts.
struct Plan {
    layer: LayerId,
    fmt: PixelFormat,
    region: Rect,
    crop: Crop,
    gray: bool,
    req: Request,
    prompt: String,
    english: String,
    /// Pre-fill the hole with Manga LaMa first (empty prompt).
    lama: bool,
    /// The inpainting model of that pre-fill.
    cleaner: Option<&'static str>,
}

fn opt_bool(p: &Value, k: &str, d: bool) -> Result<bool> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(v) => v.as_bool().ok_or_else(|| bad(format!("`{k}` must be true or false"))),
    }
}

fn opt_f64(p: &Value, k: &str, d: f64, lo: f64, hi: f64) -> Result<f64> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(d),
        Some(v) => v.as_f64().filter(|x| x.is_finite() && (lo..=hi).contains(x)).ok_or_else(|| bad(format!("`{k}` must be a number in {lo}..={hi}"))),
    }
}

fn plan(s: &mut Session, p: &Value) -> Result<Plan> {
    if !p.is_object() && !p.is_null() {
        return Err(bad("params must be a JSON object"));
    }
    let text = |k: &str| -> Result<String> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(String::new()),
            Some(Value::String(t)) if t.chars().count() <= 1000 => Ok(t.trim().to_string()),
            Some(_) => Err(bad(format!("`{k}` must be text of at most 1000 characters"))),
        }
    };
    let typed = text("prompt")?;
    let negative = text("negative")?;
    prompt::check(&typed).map_err(|e| EngineError::Other(e.into()))?;
    prompt::check(&negative).map_err(|e| EngineError::Other(e.into()))?;
    let tier = tier_for(s, p)?;
    let variants = crate::ai_cmds::uint_param(p, "variants", 1, 4, ID)?.unwrap_or(tier.variants() as u64) as usize;
    let steps = crate::ai_cmds::uint_param(p, "steps", 1, 50, ID)?.unwrap_or(0) as usize;
    let user_strength = match p.get("strength") {
        None | Some(Value::Null) => None,
        Some(_) => Some(opt_f64(p, "strength", 1.0, 0.05, 1.0)?),
    };
    let guidance = opt_f64(p, "guidance", 0.0, 0.0, 30.0)? as f32;
    let side = crate::ai_cmds::uint_param(p, "side", 256, 1024, ID)?.unwrap_or(default_side(s, tier) as u64) as usize;
    if !side.is_multiple_of(64) {
        return Err(bad("`side` must be a multiple of 64"));
    }
    let allow_download = opt_bool(p, "allowDownload", false)?;
    let unload = !crate::ai_cmds::keep_loaded(p, ID)?;
    let f32 = match p.get("precision").and_then(Value::as_str) {
        None | Some("auto") => false,
        Some("f32") => true,
        Some(_) => return Err(bad("`precision` must be auto or f32")),
    };
    let cpu = p.get("device").and_then(Value::as_str) == Some("cpu");
    let threads = crate::ai_cmds::uint_param(p, "threads", 0, 64, ID)?.unwrap_or(0) as usize;
    let seed = match crate::ai_cmds::uint_param(p, "seed", 0, u64::from(u32::MAX), ID)? {
        Some(v) => v,
        None => {
            s.genfill_runs = s.genfill_runs.wrapping_add(1);
            (s.genfill_runs.wrapping_mul(0x9E37_79B9) ^ 0x5EED) & u64::from(u32::MAX)
        }
    };
    let id = match p.get("layer").filter(|v| !v.is_null()) {
        Some(v) => {
            let l = v.as_u64().ok_or_else(|| bad("`layer` must be a layer id"))?;
            crate::edit_menu_cmds::pixel_layer_id(s, LayerId(l))
        }
        None => crate::edit_menu_cmds::pixel_layer(s),
    }
    .map_err(EngineError::Other)?;

    let d = s.active().ok_or(EngineError::NoDocument)?;
    let doc = &d.doc;
    let canvas = doc.bounds();
    let sel = doc.selection.as_ref().ok_or_else(|| bad("select the area to fill"))?;
    let sb = sel.content_bounds().intersect(&canvas);
    if sb.is_empty() {
        return Err(bad("the selection is outside the canvas"));
    }
    let reach = (crop::MIN_CONTEXT as u64).max(u64::from(sb.width().max(sb.height())) / 4).min(1 << 20) as i32;
    let region = sb.inflate(reach).intersect(&canvas);
    let pixels = u64::from(region.width()) * u64::from(region.height());
    if pixels > MAX_PIXELS {
        return Err(bad(format!("the area is too large to fill at once ({pixels} pixels, at most {MAX_PIXELS}); select less")));
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
    let rgb = crate::ai_inpaint_cmds::to_rgb(&fmt, &layer_px);
    let c = crop::plan(&cover, w, h, side).map_err(|e| bad(e.to_string()))?;
    let mut crgb = c.extract(&rgb, w, 3);
    let cmask = c.extract(&cover, w, 1);
    // Empty prompt = background: start from a fill of the hole (Manga LaMa in the job when it is
    // downloaded, else a smooth pull–push fill now) and let the model re-draw it partly, so it
    // continues the background instead of inventing an object.
    let background = typed.is_empty();
    // Manga LaMa, or the lite MI-GAN on the light tier, when downloaded.
    let ready = |id: &str| s.inpainter().is_some_and(|i| i.model_state_of(id) == photocraft_ai_lama::service::ModelState::Ready);
    let cleaner = if !background || user_strength.is_some() {
        None
    } else if ready(photocraft_ai_lama::service::LAMA) {
        Some(photocraft_ai_lama::service::LAMA)
    } else if tier == Tier::Ultra && ready(photocraft_ai_lama::service::MIGAN) {
        Some(photocraft_ai_lama::service::MIGAN)
    } else {
        None
    };
    let lama = cleaner.is_some();
    if background && !lama {
        crgb = crop::prefill(&crgb, &cmask, c.work_w, c.work_h);
    }
    let strength = user_strength.unwrap_or(if !background {
        1.0
    } else if lama {
        LAMA_STRENGTH
    } else {
        PREFILL_STRENGTH
    });
    // A black-and-white page (manga) gets a monochrome prompt and a monochrome result.
    let gray = fmt.channels() <= 2 || crgb.as_chunks::<3>().0.iter().all(|p| (p[0] - p[1]).abs() < 0.02 && (p[1] - p[2]).abs() < 0.02);
    let mut english = prompt::translate(&typed);
    if gray {
        // ("manga" alone makes the model draw characters into an empty fill.)
        english = if english.is_empty() { "monochrome, greyscale".into() } else { format!("{english}, monochrome, greyscale") };
    }
    let region_doc = Rect::new(region.x0 + c.x as i32, region.y0 + c.y as i32, region.x0 + (c.x + c.w) as i32, region.y0 + (c.y + c.h) as i32);
    let req = Request {
        rgb: crgb,
        mask: cmask,
        width: c.work_w,
        height: c.work_h,
        prompt: english.clone(),
        negative: prompt::translate(&negative),
        variants,
        seed,
        steps,
        strength,
        guidance,
        tier,
        allow_download,
        cpu,
        threads,
        unload,
        f32,
    };
    Ok(Plan { layer: id, fmt, region: region_doc, crop: c, gray, req, prompt: typed, english, lama, cleaner })
}

/// The service, if it can run `tier` now (else the reason, for the user).
fn ready(s: &Session, tier: Tier, allow_download: bool) -> Result<Arc<dyn GenFill>> {
    let svc = s.genfill.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    match svc.model_state(tier) {
        ModelState::Ready | ModelState::Checking => Ok(svc),
        // Downloads go through the AI Model Manager's queue (one file at a time, cancellable):
        // `allowDownload` queues the tier's missing files there and asks to run again after.
        ModelState::NeedsDownload { bytes } if allow_download => {
            let store = s.ai_models().ok_or_else(|| EngineError::Other(not_downloaded(LABEL, bytes, "allowDownload")))?;
            let files = photocraft_ai_sd::files::tier(tier).map(|f| f.files.clone()).unwrap_or_default();
            for f in files.into_iter().filter(|f| !store.status(f).is_downloaded() && !store.status(f).is_pending()) {
                store.download(f).map_err(|e| EngineError::Other(format!("{LABEL}: {e}")))?;
            }
            Err(EngineError::Other(format!(
                "{LABEL} {} ({:.1} MB): its files are queued in Window › AI Models…; run it again when they are done",
                crate::ai_models_cmds::NOT_DOWNLOADED,
                bytes as f64 / 1e6
            )))
        }
        ModelState::NeedsDownload { bytes } => Err(EngineError::Other(not_downloaded(LABEL, bytes, "allowDownload"))),
        ModelState::Unavailable(why) => Err(EngineError::Other(why)),
    }
}

struct JobHooks {
    ctx: JobCtx,
}

impl Hooks for JobHooks {
    fn progress(&self, fraction: f32, status: &str) {
        let f = if fraction.is_finite() { fraction.clamp(0.0, 1.0) } else { 0.0 };
        self.ctx.progress(f, if status.is_empty() { LABEL } else { status });
    }
    fn cancelled(&self) -> bool {
        self.ctx.cancelled()
    }
}

fn sd_err(e: SdError) -> EngineError {
    match e {
        SdError::Cancelled => EngineError::Cancelled,
        SdError::Unavailable(m) if m.contains(crate::ai_models_cmds::NOT_DOWNLOADED) => EngineError::Other(format!("{LABEL}: {m}")),
        other => EngineError::Other(format!("{LABEL}: {other}")),
    }
}

fn generative_fill(s: &mut Session, p: &Value) -> Result<Value> {
    let plan = plan(s, p)?;
    let svc = ready(s, plan.req.tier, plan.req.allow_download)?;
    let inpainter = if plan.lama { s.inpainter().cloned() } else { None };
    crate::jobs::run_async(
        s,
        LABEL,
        true,
        move |ctx, done| {
            ctx.progress(0.0, LABEL);
            let hooks = Arc::new(JobHooks { ctx: ctx.clone() });
            let generate = move |plan: Plan, done: crate::jobs::Completion<(Plan, Vec<Vec<f32>>, String)>, hooks: Arc<JobHooks>| {
                let req = plan.req.clone();
                let svc2 = svc.clone();
                svc.run(
                    req,
                    hooks,
                    Box::new(move |r| {
                        done.finish(r.map_err(sd_err).and_then(|outs| {
                            if outs.is_empty() {
                                return Err(EngineError::Other(format!("{LABEL}: no result")));
                            }
                            let images: Vec<Vec<f32>> = outs.iter().map(|o| finish_image(&plan.crop, o, plan.gray)).collect();
                            Ok((plan, images, svc2.device()))
                        }))
                    }),
                );
            };
            let Some(lama) = inpainter else { return generate(plan, done, hooks) };
            let cleaner_device = lama.device();
            // Manga LaMa first (the background), then the model re-draws it partly.
            ctx.progress(0.0, "Clean with AI");
            let binary: Vec<f32> = plan.req.mask.iter().map(|m| if *m >= 0.5 { 1.0 } else { 0.0 }).collect();
            let tile = photocraft_ai_lama::tiles::TileInput { rgb: plan.req.rgb.clone(), mask: binary, w: plan.req.width, h: plan.req.height };
            let lreq = photocraft_ai_lama::service::Request {
                tile,
                allow_download: false,
                model: plan.cleaner.unwrap_or(photocraft_ai_lama::service::LAMA),
                cpu: plan.req.cpu,
                threads: plan.req.threads,
                unload: false,
            };
            lama.run(
                lreq,
                hooks.clone(),
                Box::new(move |r| {
                    let mut plan = plan;
                    match r {
                        // The light tier keeps the cleaner's background as is: on a CPU a re-draw
                        // costs more than it adds.
                        Ok(filled) if filled.len() == plan.req.rgb.len() && plan.req.tier == Tier::Ultra => {
                            let image = finish_image(&plan.crop, &filled, plan.gray);
                            return done.finish(Ok((plan, vec![image], cleaner_device)));
                        }
                        Ok(filled) if filled.len() == plan.req.rgb.len() => plan.req.rgb = filled,
                        // LaMa failed: the smooth pre-fill, with the stronger re-draw.
                        _ => {
                            plan.req.rgb = crop::prefill(&plan.req.rgb, &plan.req.mask, plan.req.width, plan.req.height);
                            plan.req.strength = PREFILL_STRENGTH;
                        }
                    }
                    generate(plan, done, hooks);
                }),
            );
        },
        apply,
    )
}

/// A working-size result back at the crop's size (monochrome on grayscale pages).
fn finish_image(c: &Crop, out: &[f32], gray: bool) -> Vec<f32> {
    let mut img = c.restore(out);
    if gray {
        for px in img.as_chunks_mut::<3>().0 {
            let y = 0.2126 * px[0] + 0.7152 * px[1] + 0.0722 * px[2];
            px.fill(y);
        }
    }
    img
}

/// Pixels of a raster layer in `fmt` (with alpha) from interleaved RGB.
fn encode(fmt: &PixelFormat, rgb: &[f32]) -> Vec<f32> {
    let n = fmt.channels();
    let mut out = vec![0.0f32; rgb.len() / 3 * n];
    let mut enc = [0.0f32; 8];
    for (i, px) in rgb.as_chunks::<3>().0.iter().enumerate() {
        from_rgba_into(fmt, [px[0], px[1], px[2], 1.0], &mut enc);
        out[i * n..(i + 1) * n].copy_from_slice(&enc[..n]);
    }
    out
}

fn layer_name(prompt: &str) -> String {
    if prompt.is_empty() {
        return LABEL.to_string();
    }
    let short: String = prompt.chars().take(40).collect();
    if short.len() < prompt.len() { format!("{short}…") } else { short }
}

fn apply(s: &mut Session, (plan, images, device): (Plan, Vec<Vec<f32>>, String)) -> Result<Value> {
    let Plan { layer: id, fmt, region, req, prompt, english, .. } = plan;
    let name = layer_name(&prompt);
    let first = images.first().cloned().unwrap_or_default();
    let layer = s.edit(LABEL, |doc, active| {
        let sel = doc.selection.clone().ok_or_else(|| EngineError::Other("the selection is gone".into()))?;
        let mut l = Layer::raster(doc.next_layer_name(&name), PixelFormat { alpha: true, ..fmt });
        let lf = l.surface().map(|s| s.format()).unwrap_or(PixelFormat { alpha: true, ..fmt });
        if let Some(surf) = l.surface_mut() {
            surf.write_region(region, &encode(&lf, &first));
        }
        l.mask = Some(LayerMask { surface: sel, ..LayerMask::reveal_all() });
        let nid = doc.insert_above(Some(id), l);
        *active = Some(nid);
        Ok(nid)
    })?;
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(region);
    }
    let doc = s.active().map(|st| st.doc.id).ok_or(EngineError::NoDocument)?;
    let n = images.len();
    s.genfill_layers.insert((doc, layer), Variants { region, images, selected: 0, prompt: prompt.clone(), english: english.clone(), seed: req.seed });
    Ok(json!({
        "layer": layer.0,
        "variants": n,
        "selected": 0,
        "region": [region.x0, region.y0, region.width(), region.height()],
        "prompt": prompt,
        "english": english,
        "seed": req.seed,
        "tier": req.tier.id(),
        "device": device,
    }))
}

// ------------------------------------------------------------------ variants

fn variant_enabled(s: &Session) -> std::result::Result<(), String> {
    let st = s.active().ok_or("no document open")?;
    let l = st.active_layer.ok_or("no active layer")?;
    if s.genfill_layers.contains_key(&(st.doc.id, l)) { Ok(()) } else { Err("the active layer isn't a Generative Fill result".into()) }
}

fn variant(s: &mut Session, p: &Value) -> Result<Value> {
    let vbad = |m: &str| EngineError::BadParams { cmd: VARIANT_ID.into(), msg: m.into() };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.id;
    let layer = match p.get("layer").filter(|v| !v.is_null()) {
        Some(v) => LayerId(v.as_u64().ok_or_else(|| vbad("`layer` must be a layer id"))?),
        None => st.active_layer.ok_or_else(|| vbad("no active layer"))?,
    };
    let index = p.get("index").and_then(Value::as_u64).ok_or_else(|| vbad("`index` must be a variant number (0-based)"))? as usize;
    let v = s.genfill_layers.get(&(doc, layer)).ok_or_else(|| vbad("that layer isn't a Generative Fill result"))?;
    let img = v.images.get(index).cloned().ok_or_else(|| vbad(&format!("there are {} variants", v.images.len())))?;
    let region = v.region;
    s.edit("Generative Fill Variant", |doc, _| {
        let surf = crate::edit_menu_cmds::writable_surface(doc, layer)?;
        let fmt = surf.format();
        surf.write_region(region, &encode(&fmt, &img));
        Ok(())
    })?;
    if let Some(v) = s.genfill_layers.get_mut(&(doc, layer)) {
        v.selected = index;
    }
    if let Some(st) = s.active_mut() {
        st.last_damage = Some(region);
    }
    Ok(json!({"layer": layer.0, "selected": index}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: ID,
            label: "Generative Fill…",
            menu: &["Edit"],
            shortcut: None,
            params: r#"{"prompt"?: text (English, Spanish or Portuguese; empty = fill with background), "negative"?: text, "variants"?: 1..4 (default 3), "seed"?: 0..4294967295 (default: new each run), "steps"?: 1..50 (default: the tier's), "strength"?: 0.05..1 (1 = ignore what is under the selection), "guidance"?: 0..30 (0 = the tier's), "quality"?: "auto"|"ultra"|"fast"|"standard"|"high", "device"?: "auto"|"gpu"|"cpu", "side"?: 256..1024 working size (multiple of 64), "layer"?: layer id, "allowDownload"?: bool (queue the missing model files, about 2.3 GB, in the AI Model Manager; run again when done), "keepLoaded"?: bool, "threads"?: 0..64, "precision"?: "auto"|"f32" (f32: run without GPU 16-bit float support, about 4.5 GB of GPU memory)} -> {"layer", "variants", "selected", "region": [x,y,w,h], "prompt", "english", "seed", "tier", "device"}. Fills the selection with new content on a new masked layer. Background job, one undo step. Refuses sexual content involving minors and clearly illegal requests."#,
            enabled,
            run: generative_fill,
            journal: true,
        },
        CommandSpec {
            id: VARIANT_ID,
            label: "Generative Fill Variant",
            menu: &[],
            shortcut: None,
            params: r#"{"index": 0-based variant, "layer"?: layer id (default: the active layer)} -> {"layer", "selected"}. Shows another variant of a Generative Fill layer (one undo step)."#,
            enabled: variant_enabled,
            run: variant,
            journal: true,
        },
    ]
}

#[cfg(test)]
#[path = "genfill_cmds/tests.rs"]
mod tests;
