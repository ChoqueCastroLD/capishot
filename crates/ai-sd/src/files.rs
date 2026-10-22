//! Which catalog files (`photocraft_ai::catalog`) each Generative Fill tier loads.

use photocraft_ai::catalog::{self, ModelInfo};

use crate::scheduler::Sampler;
use crate::service::Tier;

/// The few-step sampler matching the fused LoRA ([`catalog::GENFILL_FAST_LORA_ID`]).
pub const SAMPLER: Sampler = Sampler::Tcd { gamma: 0.3 };

/// The sampler of a tier: TCD for the DreamShaper tiers, LCM-style for SDXS (one step from pure
/// noise predicts the image directly).
pub fn sampler(tier: Tier) -> Sampler {
    match tier {
        Tier::Ultra => Sampler::Lcm,
        Tier::High => Sampler::Ddim,
        _ => SAMPLER,
    }
}

/// A tier's files.
#[derive(Debug)]
pub struct TierFiles {
    pub files: Vec<&'static ModelInfo>,
}

impl TierFiles {
    /// Total download size.
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|f| f.bytes).sum()
    }
}

/// The files of `tier`, or `None` while the tier isn't available.
pub fn tier(tier: Tier) -> Option<&'static TierFiles> {
    use std::sync::OnceLock;
    static STANDARD: OnceLock<TierFiles> = OnceLock::new();
    static ULTRA: OnceLock<TierFiles> = OnceLock::new();
    static FAST: OnceLock<TierFiles> = OnceLock::new();
    static HIGH: OnceLock<TierFiles> = OnceLock::new();
    let list = |ids: &[&str]| TierFiles { files: ids.iter().filter_map(|id| catalog::model(id)).collect() };
    match tier {
        Tier::Standard => Some(STANDARD.get_or_init(|| list(catalog::GENFILL_STANDARD))),
        Tier::Ultra => Some(ULTRA.get_or_init(|| list(catalog::GENFILL_ULTRA))),
        Tier::Fast => Some(FAST.get_or_init(|| list(catalog::GENFILL_FAST))),
        // The Standard files without the few-step LoRA: the base model, many steps, guidance.
        Tier::High => Some(HIGH.get_or_init(|| list(catalog::GENFILL_HIGH))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ultra_is_small() {
        let t = tier(Tier::Ultra).unwrap_or_else(|| panic!("no ultra tier"));
        assert_eq!(t.files.len(), catalog::GENFILL_ULTRA.len());
        assert!(t.bytes() < 300_000_000, "{}", t.bytes());
    }

    #[test]
    fn standard_files_are_in_the_catalog() {
        let t = tier(Tier::Standard).unwrap_or_else(|| panic!("no standard tier"));
        assert_eq!(t.files.len(), catalog::GENFILL_STANDARD.len());
        assert!(t.bytes() > 2_000_000_000 && t.bytes() < 2_500_000_000, "{}", t.bytes());
    }
}
