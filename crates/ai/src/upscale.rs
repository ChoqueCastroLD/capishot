//! The upscale on a [`Runtime`]: load a catalog model (MangaJaNai's ESRGAN or the lite Compact),
//! run it tiled, clamp.

use burn::tensor::Tensor;

use crate::catalog::{Arch, ModelInfo};
use crate::compact::Compact;
use crate::esrgan::Esrgan;
use crate::fdat::Fdat;
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
    Fdat(Fdat),
}

impl Net {
    fn get(&self) -> &dyn Network {
        match self {
            Self::Esrgan(n) => n,
            Self::Compact(n) => n,
            Self::Fdat(n) => n,
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
    fn float_dtype(&self) -> burn::tensor::FloatDType {
        self.get().float_dtype()
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        self.get().forward(x)
    }
}

/// Where a loaded network runs.
enum Backend {
    /// burn, on the runtime it was loaded on (wgpu or the CPU).
    Burn(Box<Net>),
    /// ONNX Runtime on DirectML (Windows; ESRGAN and FDAT).
    #[cfg(all(feature = "directml", windows))]
    DirectMl(crate::directml::DmlUpscaler),
}

/// A network loaded from a catalog model's weight file.
pub struct Loaded {
    pub id: &'static str,
    pub grayscale: bool,
    backend: Backend,
}

impl Loaded {
    /// Parses and uploads `bytes` (already verified) for `model`.
    pub fn new(rt: &Runtime, model: &'static ModelInfo, bytes: &[u8], grayscale: bool) -> Result<Self> {
        let w = Weights::from_safetensors(bytes)?;
        // FDAT always takes RGB: a grayscale page goes in as gray RGB and comes out as RGB.
        let grayscale = grayscale && model.arch != Arch::Fdat;
        let net = match model.arch {
            Arch::Esrgan => Net::Esrgan(Esrgan::load(rt, &w, grayscale)?),
            Arch::Compact => Net::Compact(Compact::load(rt, &w, model.scale as usize, grayscale)?),
            Arch::Fdat => Net::Fdat(Fdat::load(rt, &w)?),
            Arch::Lama | Arch::Migan | Arch::Generative => return Err(crate::AiError::UnknownModel(format!("{} is not an upscaler", model.id))),
        };
        Ok(Self { id: model.id, grayscale, backend: Backend::Burn(Box::new(net)) })
    }

    /// Whether `model` can run on DirectML ([`Self::new_directml`]): ESRGAN (MangaJaNai and
    /// IllustrationJaNai V1) and FDAT (IllustrationJaNai V3).
    pub fn directml_supports(model: &ModelInfo) -> bool {
        matches!(model.arch, Arch::Esrgan | Arch::Fdat)
    }

    /// Parses `bytes` for `model` and builds its ONNX Runtime session on DirectML (Windows).
    #[cfg(all(feature = "directml", windows))]
    pub fn new_directml(model: &'static ModelInfo, bytes: &[u8], grayscale: bool) -> Result<Self> {
        if !Self::directml_supports(model) {
            return Err(crate::AiError::Device(format!("{} doesn't run on DirectML", model.id)));
        }
        let w = Weights::from_safetensors(bytes)?;
        // FDAT always takes RGB, as in [`Self::new`].
        let grayscale = grayscale && model.arch != Arch::Fdat;
        let net = match model.arch {
            Arch::Fdat => crate::directml::DmlUpscaler::fdat(w)?,
            _ => crate::directml::DmlUpscaler::esrgan(&Esrgan::host(&w, grayscale)?)?,
        };
        Ok(Self { id: model.id, grayscale, backend: Backend::DirectMl(net) })
    }

    /// Whether this runs on DirectML (else on burn).
    pub fn is_directml(&self) -> bool {
        match self.backend {
            Backend::Burn(_) => false,
            #[cfg(all(feature = "directml", windows))]
            Backend::DirectMl(_) => true,
        }
    }

    /// Output channels (1 for a grayscale MangaJaNai, else 3).
    pub fn out_channels(&self) -> usize {
        match &self.backend {
            Backend::Burn(n) => n.out_channels(),
            #[cfg(all(feature = "directml", windows))]
            Backend::DirectMl(n) => n.out_channels(),
        }
    }
}

/// Upscales `input` (one gray channel when the model was loaded for grayscale, else RGB; values
/// `0..=1`, preprocessed as [`crate::mangajanai`] says) and clamps the result to `0..=1`. `rt`
/// is the runtime the model was loaded on (unused for a DirectML model).
pub async fn upscale(rt: &Runtime, model: &Loaded, input: &Planar, tiles: TileOptions, progress: &dyn Progress) -> Result<Planar> {
    let input = if model.grayscale && input.channels != 1 { crate::mangajanai::to_gray(input) } else { input.clone() };
    let out = match &model.backend {
        Backend::Burn(net) => run_tiled(rt, net.as_ref(), &input, tiles, progress).await?,
        #[cfg(all(feature = "directml", windows))]
        Backend::DirectMl(_) => return upscale_directml(model, &input, tiles, progress),
    };
    Ok(clamped(out, model))
}

/// [`upscale`] for a model loaded with [`Loaded::new_directml`] (no burn runtime involved).
#[cfg(all(feature = "directml", windows))]
pub fn upscale_directml(model: &Loaded, input: &Planar, tiles: TileOptions, progress: &dyn Progress) -> Result<Planar> {
    let Backend::DirectMl(net) = &model.backend else {
        return Err(crate::AiError::Inference(format!("{} isn't loaded on DirectML", model.id)));
    };
    let input = if model.grayscale && input.channels != 1 { crate::mangajanai::to_gray(input) } else { input.clone() };
    let input = if input.channels == net.in_channels() { input } else { input.with_channels(net.in_channels()) };
    let out = net.run_tiled(&input, tiles, progress)?;
    Ok(clamped(out, model))
}

fn clamped(mut out: Planar, model: &Loaded) -> Planar {
    for v in &mut out.data {
        *v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    }
    debug_assert_eq!(out.channels, model.out_channels());
    out
}
