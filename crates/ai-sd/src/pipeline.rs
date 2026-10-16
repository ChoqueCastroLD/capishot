//! The inpainting pipeline (diffusers `StableDiffusionInpaintPipeline` semantics) as an explicit
//! state machine, so a worker can report progress and stop between UNet steps:
//! [`Generation::start`] (text + masked-image latents + initial noise) → [`Generation::step`] ×
//! steps → [`Generation::latents`] → decode with the full VAE or TAESD.
//!
//! All variants run as one batch; variant `i` uses seed `seed + i`, so any variant can be
//! regenerated alone with the same result.

use burn::tensor::Tensor;
use burn::tensor::module::interpolate;
use burn::tensor::ops::{InterpolateMode, InterpolateOptions};

use crate::clip::TextEncoder;
use crate::nn::Store;
use crate::rng::Noise;
use crate::scheduler::{Sampler, Schedule};
use crate::taesd::{TinyDecoder, TinyEncoder};
use crate::tokenizer::CONTEXT;
use crate::unet::Unet;
use crate::vae::{VaeDecoder, VaeEncoder};
use crate::{LATENT_CHANNELS, LATENT_FACTOR, Result, SdError};

/// Most variants per run.
pub const MAX_VARIANTS: usize = 4;
/// Working sides accepted (pixels, multiples of 64).
pub const MIN_SIDE: usize = 128;
pub const MAX_SIDE: usize = 1024;

/// What encodes images to latents.
pub enum Encoder<'a> {
    Full(&'a VaeEncoder),
    Tiny(&'a TinyEncoder),
}

/// What decodes latents to images.
pub enum Decoder<'a> {
    Full(&'a VaeDecoder),
    Tiny(&'a TinyDecoder),
}

/// One generation request at the working size.
#[derive(Debug, Clone)]
pub struct Request {
    /// Prompt tokens ([`crate::tokenizer::Tokenizer::encode_padded`]).
    pub prompt: [u32; CONTEXT],
    /// Negative prompt tokens (only used with `guidance > 1`; empty prompt when `None`).
    pub negative: Option<[u32; CONTEXT]>,
    /// Classifier-free guidance; `<= 1` runs the conditional branch only (distilled models).
    pub guidance: f32,
    /// Interleaved RGB, 0..1, `width·height·3`.
    pub rgb: Vec<f32>,
    /// Coverage 0..1, `width·height` (binarised at 0.5 for the model).
    pub mask: Vec<f32>,
    pub width: usize,
    pub height: usize,
    pub variants: usize,
    pub seed: u64,
    pub steps: usize,
    pub strength: f64,
    pub sampler: Sampler,
}

/// A generation in progress.
pub struct Generation {
    ctx: Tensor<3>,
    cond: Tensor<4>,
    /// 4-channel UNets (blended latents): the mask at latent size and the original's latent.
    blend: Option<(Tensor<4>, Tensor<4>)>,
    latents: Tensor<4>,
    schedule: Schedule,
    next: usize,
    guidance: f32,
    variants: usize,
    seed: u64,
    lat: [usize; 2],
}

fn check(req: &Request) -> Result<()> {
    for (n, v) in [("width", req.width), ("height", req.height)] {
        if !v.is_multiple_of(64) || !(MIN_SIDE..=MAX_SIDE).contains(&v) {
            return Err(SdError::Input(format!("{n} {v} must be a multiple of 64 in {MIN_SIDE}..={MAX_SIDE}")));
        }
    }
    let n = req.width * req.height;
    if req.rgb.len() != n * 3 || req.mask.len() != n {
        return Err(SdError::Input(format!("expected {} RGB and {n} mask samples, got {} and {}", n * 3, req.rgb.len(), req.mask.len())));
    }
    if req.variants == 0 || req.variants > MAX_VARIANTS {
        return Err(SdError::Input(format!("variants must be 1..={MAX_VARIANTS}")));
    }
    if !req.guidance.is_finite() || !(0.0..=30.0).contains(&req.guidance) {
        return Err(SdError::Input("guidance must be in 0..=30".into()));
    }
    Ok(())
}

impl Generation {
    /// Encodes the prompt and the masked image and draws the initial latents.
    pub fn start(store: &Store, text: &TextEncoder, unet: &Unet, encoder: Encoder<'_>, req: &Request) -> Result<Self> {
        check(req)?;
        let schedule = Schedule::new(req.sampler, req.steps, req.strength).map_err(|e| SdError::Input(e.to_string()))?;
        let (w, h, b) = (req.width, req.height, req.variants);
        let (lw, lh) = (w / LATENT_FACTOR, h / LATENT_FACTOR);
        let n = w * h;
        let cfg = req.guidance > 1.0;

        // Text: [uncond×B, cond×B] with guidance, else [cond×B].
        let mut ids = Vec::with_capacity(2);
        if cfg {
            ids.push(req.negative.unwrap_or_else(|| {
                let mut e = [crate::tokenizer::EOS; CONTEXT];
                e[0] = crate::tokenizer::BOS;
                e
            }));
        }
        ids.push(req.prompt);
        let emb = text.forward(&ids)?;
        let ctx = if cfg {
            let [_, t, c] = emb.dims();
            let u = emb.clone().slice([0..1, 0..t, 0..c]).repeat_dim(0, b);
            let k = emb.slice([1..2, 0..t, 0..c]).repeat_dim(0, b);
            Tensor::cat(vec![u, k], 0)
        } else {
            emb.repeat_dim(0, b)
        };

        // Planar image in −1..1 (full VAE) or 0..1 (TAESD); masked pixels zeroed like diffusers
        // (`image · (mask < 0.5)` in −1..1, i.e. mid-grey).
        let tiny = matches!(encoder, Encoder::Tiny(_));
        let mut masked = vec![0f32; 3 * n];
        let mut full = vec![0f32; 3 * n];
        let mut m = vec![0f32; n];
        for i in 0..n {
            let keep = req.mask.get(i).copied().unwrap_or(0.0) < 0.5;
            if let Some(x) = m.get_mut(i) {
                *x = if keep { 0.0 } else { 1.0 };
            }
            for c in 0..3 {
                let v = req.rgb.get(i * 3 + c).copied().unwrap_or(0.0);
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                let s = v * 2.0 - 1.0;
                let mv = if keep { s } else { 0.0 };
                let (s, mv) = if tiny { ((s + 1.0) / 2.0, (mv + 1.0) / 2.0) } else { (s, mv) };
                if let Some(x) = masked.get_mut(c * n + i) {
                    *x = mv;
                }
                if let Some(x) = full.get_mut(c * n + i) {
                    *x = s;
                }
            }
        }
        let blended = unet.in_channels() == 4;
        let mut images = vec![store.tensor(masked, [1, 3, h, w])?];
        if !schedule.from_noise || blended {
            images.push(store.tensor(full, [1, 3, h, w])?);
        }
        let imgs = Tensor::cat(images, 0);
        let lat_all = match encoder {
            Encoder::Full(e) => e.forward(imgs)?,
            Encoder::Tiny(e) => e.forward(imgs)?,
        };
        let masked_lat = lat_all.clone().slice([0..1, 0..LATENT_CHANNELS, 0..lh, 0..lw]);
        let mask_t = store.tensor(m, [1, 1, h, w])?;
        let mask_lat = interpolate(mask_t, InterpolateOptions::new(InterpolateMode::Nearest).with_output_size([lh, lw]));
        let x0_orig = if images_len(&lat_all) > 1 { Some(lat_all.clone().slice([1..2, 0..LATENT_CHANNELS, 0..lh, 0..lw]).repeat_dim(0, b)) } else { None };
        let blend = match (&x0_orig, blended) {
            (Some(x0), true) => Some((mask_lat.clone().repeat_dim(0, b), x0.clone())),
            _ => None,
        };
        let cond = if unet.in_channels() == 9 { Tensor::cat(vec![mask_lat, masked_lat], 1).repeat_dim(0, b) } else { masked_lat.repeat_dim(0, b) };

        // Initial latents per variant.
        let ln = LATENT_CHANNELS * lh * lw;
        let mut noise = Vec::with_capacity(b * ln);
        for v in 0..b {
            noise.extend(Noise::stream(req.seed.wrapping_add(v as u64), 0).fill(ln));
        }
        let z = store.tensor(noise, [b, LATENT_CHANNELS, lh, lw])?;
        let mut latents = match (&x0_orig, schedule.from_noise) {
            (Some(x0), false) => x0.clone() * schedule.init_a + z.clone() * schedule.init_b,
            _ => z.clone() * schedule.init_b,
        };
        if let Some((m, x0)) = &blend {
            // Outside the mask the original, noised to the first step's level.
            let (a, bb) = match schedule.steps.first() {
                Some(st) => {
                    let ac = crate::scheduler::alphas_cumprod();
                    let at = ac.get(st.timestep as usize).copied().unwrap_or(0.0);
                    (at.sqrt(), (1.0 - at).sqrt())
                }
                None => (1.0, 0.0),
            };
            let orig = x0.clone() * a + z * bb;
            latents = latents * m.clone() + orig * (m.clone().neg() + 1.0);
        }
        Ok(Self { ctx, cond, blend, latents, schedule, next: 0, guidance: req.guidance, variants: b, seed: req.seed, lat: [lh, lw] })
    }

    /// Steps done and total.
    pub fn progress(&self) -> (usize, usize) {
        (self.next, self.schedule.steps.len())
    }

    pub fn done(&self) -> bool {
        self.next >= self.schedule.steps.len()
    }

    /// Runs one UNet step (all variants). Does nothing once done.
    pub fn step(&mut self, store: &Store, unet: &Unet) -> Result<()> {
        let Some(st) = self.schedule.steps.get(self.next).copied() else { return Ok(()) };
        let b = self.variants;
        let x = self.latents.clone();
        let cfg = self.guidance > 1.0;
        let input = if unet.in_channels() == 9 { Tensor::cat(vec![x.clone(), self.cond.clone()], 1) } else { x.clone() };
        let eps = if cfg {
            let out = unet.forward(Tensor::cat(vec![input.clone(), input], 0), f64::from(st.timestep), &self.ctx)?;
            let [_, c, h, w] = out.dims();
            let u = out.clone().slice([0..b, 0..c, 0..h, 0..w]);
            let k = out.slice([b..2 * b, 0..c, 0..h, 0..w]);
            u.clone() + (k - u) * f64::from(self.guidance)
        } else {
            unet.forward(input, f64::from(st.timestep), &self.ctx)?
        };
        let mut next = x * st.a + eps * st.b;
        let mut zt = None;
        if st.c != 0.0 {
            let [lh, lw] = self.lat;
            let ln = LATENT_CHANNELS * lh * lw;
            let mut z = Vec::with_capacity(b * ln);
            for v in 0..b {
                z.extend(Noise::stream(self.seed.wrapping_add(v as u64), self.next as u64 + 1).fill(ln));
            }
            let z = store.tensor(z, [b, LATENT_CHANNELS, lh, lw])?;
            next = next + z.clone() * st.c;
            zt = Some(z);
        }
        if let Some((m, x0)) = &self.blend {
            let orig = match zt {
                Some(z) => x0.clone() * st.next_a + z * st.next_b,
                None => x0.clone() * st.next_a,
            };
            next = next * m.clone() + orig * (m.clone().neg() + 1.0);
        }
        self.latents = next;
        self.next += 1;
        Ok(())
    }

    /// The current latents `[variants, 4, h/8, w/8]`.
    pub fn latents(&self) -> Tensor<4> {
        self.latents.clone()
    }
}

fn images_len(t: &Tensor<4>) -> usize {
    t.dims()[0]
}

/// Decodes latents to `[B, 3, H, W]` images in 0..1 (clamped).
pub fn decode(decoder: Decoder<'_>, latents: Tensor<4>) -> Result<Tensor<4>> {
    let img = match decoder {
        Decoder::Full(d) => (d.forward(latents)? + 1.0) / 2.0,
        Decoder::Tiny(d) => d.forward(latents)?,
    };
    Ok(img.clamp(0.0, 1.0))
}
