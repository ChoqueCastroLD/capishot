//! The MangaJaNai upscale on a [`Runtime`]: load a catalog model, run it tiled, clamp.

use crate::catalog::{Arch, ModelInfo};
use crate::esrgan::Esrgan;
use crate::runtime::{Network, Runtime, run_tiled};
use crate::tile::TileOptions;
use crate::weights::Weights;
use crate::{Planar, Progress, Result};

/// The default tiling for a model: 128 input pixels for 4x models, 256 for 2x ones (which run
/// at half resolution), so the largest activation (64 channels at the output size) stays at
/// 64 MiB, under WebGPU's default 128 MiB storage binding limit. 16 pixels of context, as
/// MangaJaNaiConverterGui uses.
pub fn default_tiles(model: &ModelInfo) -> TileOptions {
    TileOptions { tile: if model.scale == 2 { 256 } else { 128 }, overlap: 16 }
}

/// A network loaded from a catalog model's weight file.
pub struct Loaded {
    pub id: &'static str,
    pub grayscale: bool,
    pub net: Esrgan,
}

impl Loaded {
    /// Parses and uploads `bytes` (already verified) for `model`.
    pub fn new(rt: &Runtime, model: &'static ModelInfo, bytes: &[u8], grayscale: bool) -> Result<Self> {
        let w = Weights::from_safetensors(bytes)?;
        let net = match model.arch {
            Arch::Esrgan => Esrgan::load(rt, &w, grayscale)?,
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
