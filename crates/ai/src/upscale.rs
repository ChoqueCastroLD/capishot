//! The upscale on a [`Runtime`]: load a catalog model (MangaJaNai's ESRGAN or the lite Compact),
//! run it tiled, clamp.

use burn::tensor::Tensor;

use crate::catalog::{Arch, ModelInfo};
use crate::compact::Compact;
use crate::esrgan::Esrgan;
use crate::runtime::{Network, Runtime, run_tiled};
use crate::tile::TileOptions;
use crate::weights::Weights;
use crate::{Planar, Progress, Result};

pub use crate::tile::default_tiles;

/// A loaded upscaling network (one per worker: the size difference doesn't matter).
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum Net {
    Esrgan(Esrgan),
    Compact(Compact),
}

impl Net {
    fn get(&self) -> &dyn Network {
        match self {
            Self::Esrgan(n) => n,
            Self::Compact(n) => n,
        }
    }
}

impl Network for Net {
    fn scale(&self) -> usize {
        self.get().scale()
    }
    fn in_channels(&self) -> usize {
        self.get().in_channels()
    }
    fn out_channels(&self) -> usize {
        self.get().out_channels()
    }
    fn multiple_of(&self) -> usize {
        self.get().multiple_of()
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        self.get().forward(x)
    }
}

/// A network loaded from a catalog model's weight file.
pub struct Loaded {
    pub id: &'static str,
    pub grayscale: bool,
    pub net: Net,
}

impl Loaded {
    /// Parses and uploads `bytes` (already verified) for `model`.
    pub fn new(rt: &Runtime, model: &'static ModelInfo, bytes: &[u8], grayscale: bool) -> Result<Self> {
        let w = Weights::from_safetensors(bytes)?;
        let net = match model.arch {
            Arch::Esrgan => Net::Esrgan(Esrgan::load(rt, &w, grayscale)?),
            Arch::Compact => Net::Compact(Compact::load(rt, &w, model.scale as usize, grayscale)?),
            Arch::Lama | Arch::Migan => return Err(crate::AiError::UnknownModel(format!("{} is not an upscaler", model.id))),
        };
        Ok(Self { id: model.id, grayscale, net })
    }
}

/// Upscales `input` (one gray channel when the model was loaded for grayscale, else RGB; values
/// `0..=1`, preprocessed as [`crate::mangajanai`] says) and clamps the result to `0..=1`.
pub async fn upscale(rt: &Runtime, model: &Loaded, input: &Planar, tiles: TileOptions, progress: &dyn Progress) -> Result<Planar> {
    let input = if model.grayscale && input.channels != 1 { crate::mangajanai::to_gray(input) } else { input.clone() };
    let mut out = run_tiled(rt, &model.net, &input, tiles, progress).await?;
    for v in &mut out.data {
        *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    }
    debug_assert_eq!(out.channels, model.net.out_channels());
    Ok(out)
}
