//! The seam between the engine and whatever runs Generative Fill: natively
//! `crate::native::NativeGenFill` (wgpu, else the CPU), in the browser a bridge to the AI Web
//! Worker. Shaped like `photocraft_ai_lama::service::Inpainter` (and reusing its
//! [`ModelState`] and [`Hooks`]): [`GenFill::run`] reports through a callback, possibly later
//! and from another thread or a browser event.

use std::sync::Arc;

pub use photocraft_ai_lama::service::{Hooks, ModelState};

use crate::Result;

/// Quality tiers (same Simple choices as the other AI tools).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tier {
    /// SDXS 1–2 steps, 4-bit weights, TAESD, 256–384 px: runs on CPUs and phones.
    Ultra,
    /// The Standard model with 8-bit weights, 384–448 px, TAESD, 1 variant by default.
    Fast,
    /// DreamShaper-8 inpainting + fused TCD/LCM, 512 px, TAESD previews, full VAE result.
    Standard,
    /// The base model with classifier-free guidance, 20–30 steps, full VAE for every variant.
    High,
}

impl Tier {
    pub fn id(self) -> &'static str {
        match self {
            Self::Ultra => "ultra",
            Self::Fast => "fast",
            Self::Standard => "standard",
            Self::High => "high",
        }
    }

    pub fn from_id(s: &str) -> Option<Self> {
        Some(match s {
            "ultra" => Self::Ultra,
            "fast" => Self::Fast,
            "standard" => Self::Standard,
            "high" => Self::High,
            _ => return None,
        })
    }

    /// The working long side.
    pub fn side(self) -> usize {
        match self {
            Self::Ultra => 384,
            Self::Fast => 448,
            Self::Standard | Self::High => 512,
        }
    }

    /// Default number of variants (one on the light tiers: each costs a full run there).
    pub fn variants(self) -> usize {
        match self {
            Self::Ultra | Self::Fast => 1,
            Self::Standard | Self::High => 3,
        }
    }

    /// Default sampling steps.
    pub fn steps(self) -> usize {
        match self {
            Self::Ultra => 1,
            Self::Fast => 4,
            Self::Standard => 4,
            Self::High => 24,
        }
    }
}

/// One generation (the crop already at the working size).
#[derive(Clone, Debug)]
pub struct Request {
    /// Interleaved RGB 0..=1, `width·height·3`.
    pub rgb: Vec<f32>,
    /// Coverage 0..=1, `width·height`.
    pub mask: Vec<f32>,
    pub width: usize,
    pub height: usize,
    /// English prompt (translated and checked by [`crate::prompt`]); empty = fill with
    /// background.
    pub prompt: String,
    pub negative: String,
    pub variants: usize,
    pub seed: u64,
    /// 0 = the tier's default.
    pub steps: usize,
    pub strength: f64,
    /// 0 = the tier's default.
    pub guidance: f32,
    pub tier: Tier,
    pub allow_download: bool,
    pub cpu: bool,
    pub threads: usize,
    pub unload: bool,
    /// Compute in 32-bit floats on the GPU even without `shader-f16` support (Advanced; needs
    /// about 4.5 GB of GPU memory).
    pub f32: bool,
}

/// Receives one interleaved RGB image (`width·height·3`, 0..=1) per variant.
pub type Done = Box<dyn FnOnce(Result<Vec<Vec<f32>>>) + Send>;

/// Runs Generative Fill (see the module docs).
pub trait GenFill: Send + Sync {
    /// Whether `tier` can run now (its files are cached / downloadable / unavailable here).
    fn model_state(&self, tier: Tier) -> ModelState;
    /// Where it runs, for the UI; empty until known.
    fn device(&self) -> String {
        String::new()
    }
    /// Generates and calls `done` exactly once (also on cancellation).
    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done);
}

/// A [`GenFill`] for tests: variant `i` fills the mask with a flat grey `0.2 + 0.2·i`.
#[derive(Debug, Default)]
pub struct FakeGenFill {
    pub fail: Option<String>,
    pub state: Option<ModelState>,
    pub last: std::sync::Mutex<Option<Request>>,
}

impl GenFill for FakeGenFill {
    fn model_state(&self, _tier: Tier) -> ModelState {
        self.state.clone().unwrap_or(ModelState::Ready)
    }

    fn device(&self) -> String {
        "test".into()
    }

    fn run(&self, req: Request, hooks: Arc<dyn Hooks>, done: Done) {
        if hooks.cancelled() {
            return done(Err(crate::SdError::Cancelled));
        }
        if let Some(e) = &self.fail {
            return done(Err(crate::SdError::Device(e.clone())));
        }
        let out = (0..req.variants)
            .map(|i| {
                let g = 0.2 + 0.2 * i as f32;
                let mut o = req.rgb.clone();
                for (px, m) in o.as_chunks_mut::<3>().0.iter_mut().zip(&req.mask) {
                    if *m >= 0.5 {
                        *px = [g; 3];
                    }
                }
                o
            })
            .collect();
        *self.last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(req);
        hooks.progress(1.0, "");
        done(Ok(out));
    }
}
