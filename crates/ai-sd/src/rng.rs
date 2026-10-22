//! Deterministic standard-normal noise: the same seed gives the same latents on every device and
//! platform (generated on the CPU in f32, uploaded once), so a variant can be regenerated exactly.

/// SplitMix64 with Box–Muller.
#[derive(Debug, Clone)]
pub struct Noise {
    state: u64,
    spare: Option<f32>,
}

impl Noise {
    /// A stream for `seed`; variants use `seed.wrapping_add(i)` or [`Noise::stream`].
    pub fn new(seed: u64) -> Self {
        Self { state: seed ^ 0x5DEE_CE66_D1CE_4E5B, spare: None }
    }

    /// An independent stream `index` of `seed` (e.g. per variant and per step).
    pub fn stream(seed: u64, index: u64) -> Self {
        let mut n = Self::new(seed);
        n.state = n.state.wrapping_add(index.wrapping_mul(0xD1B5_4A32_D192_ED03));
        n.next_u64();
        n
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in (0, 1].
    fn uniform(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 1.0) / (1u64 << 53) as f64
    }

    /// One standard normal sample.
    pub fn normal(&mut self) -> f32 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let (u, v) = (self.uniform(), self.uniform());
        let r = (-2.0 * u.ln()).sqrt();
        let a = std::f64::consts::TAU * v;
        self.spare = Some((r * a.sin()) as f32);
        (r * a.cos()) as f32
    }

    /// `n` samples.
    pub fn fill(&mut self, n: usize) -> Vec<f32> {
        (0..n).map(|_| self.normal()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_standard() {
        let a = Noise::new(42).fill(100_000);
        assert_eq!(a, Noise::new(42).fill(100_000));
        assert_ne!(a[..8], Noise::new(43).fill(8)[..]);
        assert_ne!(Noise::stream(42, 1).fill(8), Noise::stream(42, 2).fill(8));
        let mean = a.iter().map(|&x| f64::from(x)).sum::<f64>() / a.len() as f64;
        let var = a.iter().map(|&x| (f64::from(x) - mean).powi(2)).sum::<f64>() / a.len() as f64;
        assert!(mean.abs() < 0.01 && (var - 1.0).abs() < 0.02, "{mean} {var}");
        assert!(a.iter().all(|x| x.is_finite()));
    }
}
