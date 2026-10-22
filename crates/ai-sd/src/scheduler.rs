//! Few-step samplers for distilled SD 1.5 (LCM-LoRA, TCD-LoRA), reduced to per-step scalar
//! coefficients so the device only does `a·x + b·ε + c·z` (no scheduler state on the GPU).
//!
//! Both follow diffusers' `LCMScheduler` / `TCDScheduler` with SD 1.5's training schedule
//! (`scaled_linear` betas 0.00085…0.012, 1000 steps, `set_alpha_to_one = false`) and the
//! distillation's 50-step grid (`k = 20`: timesteps 19, 39, …, 999). `strength < 1` cuts the
//! schedule's head like `StableDiffusionInpaintPipeline::get_timesteps`.

/// Training timesteps of SD 1.5.
pub const TRAIN_STEPS: usize = 1000;
/// The distillation grid of LCM/TCD (`original_inference_steps`).
pub const ORIGINAL_STEPS: usize = 50;
/// Most sampling steps accepted (the grid has 50 points; more is pointless for distilled models).
pub const MAX_STEPS: usize = ORIGINAL_STEPS;

/// The sampler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Sampler {
    /// Latent Consistency Model multistep sampling (boundary-condition scalings, fresh noise
    /// between steps).
    Lcm,
    /// Trajectory Consistency Distillation's strategic stochastic sampling; `gamma` (diffusers'
    /// `eta`) in `0..=1`: 0 is deterministic, the paper's default is 0.3.
    Tcd { gamma: f64 },
}

/// One denoising step: `x_next = a·x + b·ε + c·z`, `ε` the UNet's noise prediction at
/// `timestep`, `z` fresh standard normal noise (only drawn when `c != 0`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Step {
    pub timestep: u32,
    pub a: f64,
    pub b: f64,
    pub c: f64,
    /// The original image's latent `x₀` at this step's output noise level, with the same `z`:
    /// `next_a·x₀ + next_b·z` (blended-latent inpainting with 4-channel UNets keeps the
    /// unmasked area on the original's trajectory).
    pub next_a: f64,
    pub next_b: f64,
}

/// A sampling plan.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub steps: Vec<Step>,
    /// How to make the initial latent from the encoded image `x₀` and noise `z`:
    /// `init_a·x₀ + init_b·z` (for `strength = 1`: `0·x₀ + 1·z`, pure noise).
    pub init_a: f64,
    pub init_b: f64,
    /// `strength == 1`: the initial latent is pure noise (no image latents needed).
    pub from_noise: bool,
}

/// Errors building a schedule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScheduleError {
    #[error("steps must be 1..={MAX_STEPS}, got {0}")]
    Steps(usize),
    #[error("strength must be in (0, 1]")]
    Strength,
    #[error("gamma must be in 0..=1")]
    Gamma,
    #[error("strength too low for this many steps: nothing to denoise")]
    Empty,
}

/// `ᾱ_t` of SD 1.5 (f64, like diffusers' float32 table to ~1e-7).
pub fn alphas_cumprod() -> Vec<f64> {
    let (s, e) = (0.00085f64.sqrt(), 0.012f64.sqrt());
    let n = TRAIN_STEPS;
    let mut acc = 1.0;
    (0..n)
        .map(|i| {
            let b = s + (e - s) * i as f64 / (n - 1) as f64;
            acc *= 1.0 - b * b;
            acc
        })
        .collect()
}

/// The distilled timesteps for `steps` (descending), before any strength cut.
pub fn timesteps(steps: usize) -> Result<Vec<u32>, ScheduleError> {
    if steps == 0 || steps > MAX_STEPS {
        return Err(ScheduleError::Steps(steps));
    }
    let k = TRAIN_STEPS / ORIGINAL_STEPS;
    // 999, 979, …, 19 (descending).
    let origin: Vec<u32> = (1..=ORIGINAL_STEPS).rev().map(|i| u32::try_from(i * k - 1).unwrap_or(0)).collect();
    // floor(linspace(0, 50, steps, endpoint=False)).
    Ok((0..steps).filter_map(|i| origin.get(i * ORIGINAL_STEPS / steps).copied()).collect())
}

impl Schedule {
    /// Builds the plan for `steps` sampling steps at `strength` (1 = ignore the original
    /// pixels under the mask entirely, the usual Generative Fill).
    pub fn new(sampler: Sampler, steps: usize, strength: f64) -> Result<Self, ScheduleError> {
        if !(strength > 0.0 && strength <= 1.0) {
            return Err(ScheduleError::Strength);
        }
        if let Sampler::Tcd { gamma } = sampler
            && !(0.0..=1.0).contains(&gamma)
        {
            return Err(ScheduleError::Gamma);
        }
        let all = timesteps(steps)?;
        let ac = alphas_cumprod();
        let alpha = |t: u32| ac.get(t as usize).copied().unwrap_or(1.0);
        // get_timesteps: init = min(floor(n·strength), n), start = n − init.
        let init = ((steps as f64 * strength).floor() as usize).min(steps);
        let start = steps - init;
        if start >= steps {
            return Err(ScheduleError::Empty);
        }
        let mut out = Vec::with_capacity(init);
        for i in start..steps {
            let t = all.get(i).copied().unwrap_or(0);
            let last = i + 1 == steps;
            let at = alpha(t);
            let (sa, sb) = (at.sqrt(), (1.0 - at).sqrt());
            let step = match sampler {
                Sampler::Lcm => {
                    let prev = if last { t } else { all.get(i + 1).copied().unwrap_or(t) };
                    let ap = alpha(prev);
                    let st = f64::from(t) * 10.0;
                    let c_skip = 0.25 / (st * st + 0.25);
                    let c_out = st / (st * st + 0.25).sqrt();
                    // denoised = c_out·x0 + c_skip·x, x0 = (x − sb·ε)/sa
                    let (da, db) = (c_out / sa + c_skip, -c_out * sb / sa);
                    if last {
                        Step { timestep: t, a: da, b: db, c: 0.0, next_a: 1.0, next_b: 0.0 }
                    } else {
                        let s = ap.sqrt();
                        Step { timestep: t, a: s * da, b: s * db, c: (1.0 - ap).sqrt(), next_a: s, next_b: (1.0 - ap).sqrt() }
                    }
                }
                Sampler::Tcd { gamma } => {
                    let prev = if last { 0 } else { all.get(i + 1).copied().unwrap_or(0) };
                    let s = ((1.0 - gamma) * f64::from(prev)).floor() as u32;
                    let (ap, as_) = (alpha(prev), alpha(s));
                    // pns = √ᾱs·x0 + √(1−ᾱs)·ε
                    let (pa, pb) = (as_.sqrt() / sa, (1.0 - as_).sqrt() - as_.sqrt() * sb / sa);
                    if gamma > 0.0 && !last {
                        let r = ap / as_;
                        Step { timestep: t, a: r.sqrt() * pa, b: r.sqrt() * pb, c: (1.0 - r).max(0.0).sqrt(), next_a: ap.sqrt(), next_b: (1.0 - ap).sqrt() }
                    } else {
                        Step { timestep: t, a: pa, b: pb, c: 0.0, next_a: as_.sqrt(), next_b: 0.0 }
                    }
                }
            };
            out.push(step);
        }
        let t0 = out.first().map_or(0, |s| s.timestep);
        let from_noise = strength >= 1.0;
        let (init_a, init_b) = if from_noise { (0.0, 1.0) } else { (alpha(t0).sqrt(), (1.0 - alpha(t0)).sqrt()) };
        Ok(Self { steps: out, init_a, init_b, from_noise })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcm_timesteps_match_diffusers() {
        // LCMScheduler(SD 1.5 config).set_timesteps(n).timesteps
        assert_eq!(timesteps(1).unwrap_or_default(), vec![999]);
        assert_eq!(timesteps(4).unwrap_or_default(), vec![999, 759, 499, 259]);
        assert_eq!(timesteps(8).unwrap_or_default(), vec![999, 879, 759, 639, 499, 379, 259, 139]);
        assert!(timesteps(0).is_err() && timesteps(51).is_err());
    }

    #[test]
    fn alphas_match_diffusers() {
        let a = alphas_cumprod();
        // torch float32 values from DDPMScheduler(scaled_linear 0.00085..0.012)
        assert!((a[0] - 0.99915).abs() < 1e-6);
        assert!((a[999] - 0.0046600).abs() < 1e-6, "{}", a[999]);
    }

    #[test]
    fn coefficients_match_diffusers() {
        // diffusers 0.41 LCMScheduler / TCDScheduler(eta 0.3).step, probed with unit x, ε and noise
        // (scripts/sched_ref.py).
        let lcm =
            [(999, 3.347276, -3.339468, 0.973544), (759, 2.306084, -2.245073, 0.849900), (499, 1.540531, -1.309298, 0.583973), (259, 1.231872, -0.719380, 0.0)];
        let tcd =
            [(999, 3.347276, -2.929013, 0.882788), (759, 2.306084, -1.726657, 0.673480), (499, 1.540531, -0.881513, 0.397524), (259, 1.231348, -0.689919, 0.0)];
        for (sampler, want) in [(Sampler::Lcm, lcm), (Sampler::Tcd { gamma: 0.3 }, tcd)] {
            let s = Schedule::new(sampler, 4, 1.0).unwrap_or_else(|e| panic!("{e}"));
            assert!(s.from_noise);
            for (got, (t, a, b, c)) in s.steps.iter().zip(want) {
                assert_eq!(got.timestep, t);
                for (g, w) in [(got.a, a), (got.b, b), (got.c, c)] {
                    assert!((g - w).abs() < 2e-5, "{sampler:?} t={t}: {g} vs {w}");
                }
            }
        }
    }

    #[test]
    fn strength_cuts_the_head() {
        let s = Schedule::new(Sampler::Lcm, 4, 0.5).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(s.steps.iter().map(|s| s.timestep).collect::<Vec<_>>(), vec![499, 259]);
        assert!(!s.from_noise && s.init_a > 0.0);
        assert_eq!(s.steps.last().map(|s| s.c), Some(0.0));
        assert_eq!(Schedule::new(Sampler::Lcm, 4, 0.1), Err(ScheduleError::Empty));
        assert!(Schedule::new(Sampler::Lcm, 4, 0.0).is_err());
        assert!(Schedule::new(Sampler::Lcm, 4, f64::NAN).is_err());
        assert!(Schedule::new(Sampler::Tcd { gamma: 2.0 }, 4, 1.0).is_err());
    }
}
