//! Which catalog files (`photocraft_ai::catalog`) each Generative Fill tier loads.

use photocraft_ai::catalog::{self, ModelInfo};

use crate::scheduler::Sampler;
use crate::service::Tier;

/// The few-step sampler matching the fused LoRA ([`catalog::GENFILL_FAST_LORA_ID`]).
pub const SAMPLER: Sampler = Sampler::Tcd { gamma: 0.3 };

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
    match tier {
        Tier::Standard => Some(STANDARD.get_or_init(|| TierFiles { files: catalog::GENFILL_STANDARD.iter().filter_map(|id| catalog::model(id)).collect() })),
        Tier::Ultra | Tier::Fast | Tier::High => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_files_are_in_the_catalog() {
        let t = tier(Tier::Standard).unwrap_or_else(|| panic!("no standard tier"));
        assert_eq!(t.files.len(), catalog::GENFILL_STANDARD.len());
        assert!(t.bytes() > 2_000_000_000 && t.bytes() < 2_500_000_000, "{}", t.bytes());
    }
}
