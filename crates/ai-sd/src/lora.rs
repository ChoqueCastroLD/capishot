//! LoRA fusion on the device: `W += up·down` for every adapted layer (linear `[out, in]` or
//! convolution `[out, in, k, k]`, the down projection carrying the kernel). The converter folds
//! `alpha / rank` into `up` and names the pair `<layer>.lora_down` / `<layer>.lora_up` after
//! the UNet's own tensor names, so fusion is one matmul per layer, computed in f32.

use burn::tensor::DType;

use crate::nn::Store;
use crate::{Result, SdError};

/// Fuses every LoRA pair of `lora` into `base` with an extra `scale` (1 = as trained). Returns
/// the number of layers changed. Fails (leaving `base` partly fused) if a pair doesn't match its
/// layer, so callers reload the base on error.
pub fn fuse(base: &mut Store, lora: &Store, scale: f64) -> Result<usize> {
    let mut names: Vec<String> = lora.names().filter_map(|n| n.strip_suffix(".lora_down")).map(str::to_string).collect();
    names.sort();
    let dt = base.dtype();
    let mut fused = 0;
    for layer in names {
        let target = format!("{layer}.weight");
        let shape = base.shape(&target).ok_or_else(|| SdError::Weights(format!("LoRA layer `{layer}` is not in the model")))?.to_vec();
        let down_shape = lora.shape(&format!("{layer}.lora_down")).map(<[usize]>::to_vec).unwrap_or_default();
        let up_shape = lora.shape(&format!("{layer}.lora_up")).map(<[usize]>::to_vec).unwrap_or_default();
        let (Some(&r), Some(&out)) = (down_shape.first(), shape.first()) else {
            return Err(SdError::Weights(format!("LoRA `{layer}`: bad shapes")));
        };
        let fan_in: usize = shape.iter().skip(1).product();
        if down_shape.iter().skip(1).product::<usize>() != fan_in || up_shape.first() != Some(&out) || up_shape.iter().skip(1).product::<usize>() != r {
            return Err(SdError::Weights(format!("LoRA `{layer}`: {down_shape:?}/{up_shape:?} don't fit {shape:?}")));
        }
        let down = lora.get_flat(&format!("{layer}.lora_down"))?.reshape([r, fan_in]).cast(DType::F32);
        let up = lora.get_flat(&format!("{layer}.lora_up"))?.reshape([out, r]).cast(DType::F32);
        let w = base.get_flat(&target)?.reshape([out, fan_in]).cast(DType::F32);
        let w = (w + up.matmul(down) * scale).cast(dt);
        base.replace_flat(&target, w.reshape([out * fan_in]))?;
        fused += 1;
    }
    Ok(fused)
}
