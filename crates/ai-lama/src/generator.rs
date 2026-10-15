//! [`Generator`]: any of this crate's inpainting networks behind one interface, chosen by the
//! weight file's `arch` metadata (`lama-ffc` → [`Lama`], `migan` → [`Migan`]).

use burn::tensor::Device;
use safetensors::SafeTensors;

use crate::{Lama, LamaError, Migan, Result};

/// A loaded inpainting generator (one at a time: the size difference doesn't matter).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum Generator {
    /// Manga LaMa (full quality).
    Lama(Lama),
    /// MI-GAN (lite: phones, CPUs).
    Migan(Migan),
}

impl Generator {
    /// Loads a weight file, picking the network from its metadata.
    pub fn from_safetensors(bytes: &[u8], device: &Device) -> Result<Self> {
        let (_, header) = SafeTensors::read_metadata(bytes).map_err(|e| LamaError::Weights(format!("not a safetensors file: {e}")))?;
        match header.metadata().as_ref().and_then(|m| m.get("arch")).map(String::as_str) {
            Some("migan") => Ok(Self::Migan(Migan::from_safetensors(bytes, device)?)),
            Some("lama-ffc") | None => Ok(Self::Lama(Lama::from_safetensors(bytes, device)?)),
            Some(other) => Err(LamaError::Weights(format!("unknown inpainting architecture `{other}`"))),
        }
    }

    /// The largest working side the network takes (tiles must be planned within it).
    pub fn max_side(&self) -> usize {
        match self {
            Self::Lama(_) => crate::MAX_WORK_SIDE,
            Self::Migan(m) => m.resolution(),
        }
    }

    /// See [`Lama::generate`].
    #[cfg(not(target_family = "wasm"))]
    pub fn generate(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        match self {
            Self::Lama(m) => m.generate(rgb, mask, w, h),
            Self::Migan(m) => m.generate(rgb, mask, w, h),
        }
    }

    /// See [`Lama::generate_async`].
    pub async fn generate_async(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        match self {
            Self::Lama(m) => m.generate_async(rgb, mask, w, h).await,
            Self::Migan(m) => m.generate_async(rgb, mask, w, h).await,
        }
    }
}
