//! The SD 1.x VAE (diffusers `AutoencoderKL`: channels 128/256/512/512, two ResNets per level
//! in the encoder, three in the decoder, one single-head attention in each middle block), split
//! into [`VaeEncoder`] and [`VaeDecoder`] so each half is its own download and can be dropped.
//! The encoder returns the latent distribution's mean (deterministic), already scaled by
//! [`crate::VAE_SCALE`]; the decoder takes scaled latents.

use burn::tensor::Tensor;
use burn::tensor::activation::silu;
use burn::tensor::module::interpolate;
use burn::tensor::ops::{InterpolateMode, InterpolateOptions, PadMode};

use crate::nn::{Conv, Linear, Norm, Store, attention, merge_heads, split_heads};
use crate::{LATENT_CHANNELS, Result, SdError, VAE_SCALE};

const CH: [usize; 4] = [128, 256, 512, 512];
const GROUPS: usize = 32;
const EPS: f64 = 1e-6;

#[derive(Debug, Clone)]
struct Resnet {
    norm1: Norm,
    conv1: Conv,
    norm2: Norm,
    conv2: Conv,
    shortcut: Option<Conv>,
}

impl Resnet {
    fn load(s: &Store, p: &str, c_in: usize, c_out: usize) -> Result<Self> {
        Ok(Self {
            norm1: s.norm(&format!("{p}.norm1"), c_in)?,
            conv1: s.conv(&format!("{p}.conv1"), c_in, c_out, 3)?,
            norm2: s.norm(&format!("{p}.norm2"), c_out)?,
            conv2: s.conv(&format!("{p}.conv2"), c_out, c_out, 3)?,
            shortcut: if c_in != c_out { Some(s.conv(&format!("{p}.conv_shortcut"), c_in, c_out, 1)?) } else { None },
        })
    }

    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let h = self.conv1.forward(silu(self.norm1.group(x.clone(), GROUPS, EPS)));
        let h = self.conv2.forward(silu(self.norm2.group(h, GROUPS, EPS)));
        let x = match &self.shortcut {
            Some(s) => s.forward(x),
            None => x,
        };
        x + h
    }
}

/// Single-head self-attention over the pixels, with a residual.
#[derive(Debug, Clone)]
struct MidAttn {
    norm: Norm,
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
}

impl MidAttn {
    fn load(s: &Store, p: &str, c: usize) -> Result<Self> {
        Ok(Self {
            norm: s.norm(&format!("{p}.group_norm"), c)?,
            q: s.linear(&format!("{p}.to_q"), c, c, true)?,
            k: s.linear(&format!("{p}.to_k"), c, c, true)?,
            v: s.linear(&format!("{p}.to_v"), c, c, true)?,
            out: s.linear(&format!("{p}.to_out.0"), c, c, true)?,
        })
    }

    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let [b, c, h, w] = x.dims();
        let t = self.norm.group(x.clone(), GROUPS, EPS).reshape([b, c, h * w]).swap_dims(1, 2);
        let q = split_heads(self.q.forward(t.clone()), 1);
        let k = split_heads(self.k.forward(t.clone()), 1);
        let v = split_heads(self.v.forward(t), 1);
        let o = self.out.forward(merge_heads(attention(q, k, v, false)));
        o.swap_dims(1, 2).reshape([b, c, h, w]) + x
    }
}

/// Image → latent.
#[derive(Debug, Clone)]
pub struct VaeEncoder {
    conv_in: Conv,
    down: Vec<(Vec<Resnet>, Option<Conv>)>,
    mid: (Resnet, MidAttn, Resnet),
    norm_out: Norm,
    conv_out: Conv,
    quant: Conv,
}

impl VaeEncoder {
    /// Takes the `encoder.*` and `quant_conv.*` tensors.
    pub fn from_store(s: &Store) -> Result<Self> {
        let mut down = Vec::new();
        let mut c_prev = CH[0];
        for (i, &c) in CH.iter().enumerate() {
            let res = vec![
                Resnet::load(s, &format!("encoder.down_blocks.{i}.resnets.0"), c_prev, c)?,
                Resnet::load(s, &format!("encoder.down_blocks.{i}.resnets.1"), c, c)?,
            ];
            let ds = if i < 3 { Some(s.conv(&format!("encoder.down_blocks.{i}.downsamplers.0.conv"), c, c, 3)?) } else { None };
            down.push((res, ds));
            c_prev = c;
        }
        Ok(Self {
            conv_in: s.conv("encoder.conv_in", 3, CH[0], 3)?,
            down,
            mid: (
                Resnet::load(s, "encoder.mid_block.resnets.0", 512, 512)?,
                MidAttn::load(s, "encoder.mid_block.attentions.0", 512)?,
                Resnet::load(s, "encoder.mid_block.resnets.1", 512, 512)?,
            ),
            norm_out: s.norm("encoder.conv_norm_out", 512)?,
            conv_out: s.conv("encoder.conv_out", 512, 2 * LATENT_CHANNELS, 3)?,
            quant: s.conv("quant_conv", 2 * LATENT_CHANNELS, 2 * LATENT_CHANNELS, 1)?,
        })
    }

    /// `[B, 3, H, W]` in −1..1 (H, W multiples of 8) → scaled latent mean `[B, 4, H/8, W/8]`.
    pub fn forward(&self, x: Tensor<4>) -> Result<Tensor<4>> {
        let [b, c, h, w] = x.dims();
        if c != 3 || h % 8 != 0 || w % 8 != 0 || h == 0 || w == 0 {
            return Err(SdError::Input(format!("VAE encoder input {b}×{c}×{h}×{w}")));
        }
        let mut x = self.conv_in.forward(x);
        for (res, ds) in &self.down {
            for r in res {
                x = r.forward(x);
            }
            if let Some(d) = ds {
                // diffusers pads (0, 1, 0, 1) and convolves with stride 2, padding 0.
                x = d.forward_stride2(x.pad((0, 1, 0, 1), PadMode::Constant(0.0)), 0);
            }
        }
        x = self.mid.0.forward(x);
        x = self.mid.1.forward(x);
        x = self.mid.2.forward(x);
        let x = self.quant.forward(self.conv_out.forward(silu(self.norm_out.group(x, GROUPS, EPS))));
        let [b, _, h, w] = x.dims();
        Ok(x.slice([0..b, 0..LATENT_CHANNELS, 0..h, 0..w]) * f64::from(VAE_SCALE))
    }
}

/// Latent → image.
#[derive(Debug, Clone)]
pub struct VaeDecoder {
    post_quant: Conv,
    conv_in: Conv,
    mid: (Resnet, MidAttn, Resnet),
    up: Vec<(Vec<Resnet>, Option<Conv>)>,
    norm_out: Norm,
    conv_out: Conv,
}

impl VaeDecoder {
    /// Takes the `decoder.*` and `post_quant_conv.*` tensors.
    pub fn from_store(s: &Store) -> Result<Self> {
        let rev: Vec<usize> = CH.iter().rev().copied().collect();
        let mut up = Vec::new();
        let mut c_prev = 512;
        for (i, &c) in rev.iter().enumerate() {
            let mut res = Vec::new();
            for j in 0..3 {
                res.push(Resnet::load(s, &format!("decoder.up_blocks.{i}.resnets.{j}"), if j == 0 { c_prev } else { c }, c)?);
            }
            let us = if i < 3 { Some(s.conv(&format!("decoder.up_blocks.{i}.upsamplers.0.conv"), c, c, 3)?) } else { None };
            up.push((res, us));
            c_prev = c;
        }
        Ok(Self {
            post_quant: s.conv("post_quant_conv", LATENT_CHANNELS, LATENT_CHANNELS, 1)?,
            conv_in: s.conv("decoder.conv_in", LATENT_CHANNELS, 512, 3)?,
            mid: (
                Resnet::load(s, "decoder.mid_block.resnets.0", 512, 512)?,
                MidAttn::load(s, "decoder.mid_block.attentions.0", 512)?,
                Resnet::load(s, "decoder.mid_block.resnets.1", 512, 512)?,
            ),
            up,
            norm_out: s.norm("decoder.conv_norm_out", CH[0])?,
            conv_out: s.conv("decoder.conv_out", CH[0], 3, 3)?,
        })
    }

    /// Scaled latent `[B, 4, h, w]` → image `[B, 3, 8h, 8w]` in −1..1 (not clamped).
    pub fn forward(&self, z: Tensor<4>) -> Result<Tensor<4>> {
        let [b, c, h, w] = z.dims();
        if c != LATENT_CHANNELS || h == 0 || w == 0 {
            return Err(SdError::Input(format!("VAE decoder input {b}×{c}×{h}×{w}")));
        }
        let mut x = self.conv_in.forward(self.post_quant.forward(z / f64::from(VAE_SCALE)));
        x = self.mid.0.forward(x);
        x = self.mid.1.forward(x);
        x = self.mid.2.forward(x);
        for (res, us) in &self.up {
            for r in res {
                x = r.forward(x);
            }
            if let Some(u) = us {
                let [_, _, hh, ww] = x.dims();
                x = u.forward(interpolate(x, InterpolateOptions::new(InterpolateMode::Nearest).with_output_size([hh * 2, ww * 2])));
            }
        }
        Ok(self.conv_out.forward(silu(self.norm_out.group(x, GROUPS, EPS))))
    }
}
