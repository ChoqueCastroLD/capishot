//! The SD 1.5 UNet (diffusers `UNet2DConditionModel`, 860 M parameters): channels
//! 320/640/1280/1280, two ResNets per level, cross-attention transformers on the first three
//! levels and in the middle, 8 heads, GEGLU feed-forward; also the distilled layouts with fewer
//! levels, ResNets or no middle block (SDXS-512). `in_channels` is read from the
//! weights: 9 for inpainting (`[noisy latent, mask, masked-image latent]`), 4 for plain models.

use burn::tensor::activation::{gelu, silu};
use burn::tensor::module::interpolate;
use burn::tensor::ops::{InterpolateMode, InterpolateOptions};
use burn::tensor::{Tensor, TensorData};

use crate::nn::{Conv, Linear, Norm, Store, attention, merge_heads, split_heads};
use crate::{Result, SdError};

const TEMB: usize = 1280;
const CTX: usize = 768;
const HEADS: usize = 8;
const GROUPS: usize = 32;
const RES_EPS: f64 = 1e-5;
const ATTN_GN_EPS: f64 = 1e-6;
const LN_EPS: f64 = 1e-5;

#[derive(Debug, Clone)]
struct Resnet {
    norm1: Norm,
    conv1: Conv,
    temb: Linear,
    norm2: Norm,
    conv2: Conv,
    shortcut: Option<Conv>,
}

impl Resnet {
    fn load(s: &Store, p: &str, c_in: usize, c_out: usize) -> Result<Self> {
        Ok(Self {
            norm1: s.norm(&format!("{p}.norm1"), c_in)?,
            conv1: s.conv(&format!("{p}.conv1"), c_in, c_out, 3)?,
            temb: s.linear(&format!("{p}.time_emb_proj"), TEMB, c_out, true)?,
            norm2: s.norm(&format!("{p}.norm2"), c_out)?,
            conv2: s.conv(&format!("{p}.conv2"), c_out, c_out, 3)?,
            shortcut: if c_in != c_out { Some(s.conv(&format!("{p}.conv_shortcut"), c_in, c_out, 1)?) } else { None },
        })
    }

    /// `temb` is the already-SiLU'd time embedding `[B, 1280]`.
    fn forward(&self, x: Tensor<4>, temb: &Tensor<2>) -> Tensor<4> {
        let h = self.conv1.forward(silu(self.norm1.group(x.clone(), GROUPS, RES_EPS)));
        let [b, c, _, _] = h.dims();
        let h = h + self.temb.forward(temb.clone()).reshape([b, c, 1, 1]);
        let h = self.conv2.forward(silu(self.norm2.group(h, GROUPS, RES_EPS)));
        let x = match &self.shortcut {
            Some(s) => s.forward(x),
            None => x,
        };
        x + h
    }
}

#[derive(Debug, Clone)]
struct Attn {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
}

impl Attn {
    fn load(s: &Store, p: &str, c: usize, ctx: usize) -> Result<Self> {
        Ok(Self {
            q: s.linear(&format!("{p}.to_q"), c, c, false)?,
            k: s.linear(&format!("{p}.to_k"), ctx, c, false)?,
            v: s.linear(&format!("{p}.to_v"), ctx, c, false)?,
            out: s.linear(&format!("{p}.to_out.0"), c, c, true)?,
        })
    }

    fn forward(&self, x: Tensor<3>, ctx: Option<&Tensor<3>>) -> Tensor<3> {
        let c = ctx.cloned().unwrap_or_else(|| x.clone());
        let q = split_heads(self.q.forward(x), HEADS);
        let k = split_heads(self.k.forward(c.clone()), HEADS);
        let v = split_heads(self.v.forward(c), HEADS);
        self.out.forward(merge_heads(attention(q, k, v, false)))
    }
}

/// `Transformer2DModel` with one `BasicTransformerBlock` (conv projections, SD 1.x).
#[derive(Debug, Clone)]
struct Transformer {
    norm: Norm,
    proj_in: Conv,
    norm1: Norm,
    attn1: Attn,
    norm2: Norm,
    attn2: Attn,
    norm3: Norm,
    ff_in: Linear,
    ff_out: Linear,
    proj_out: Conv,
}

impl Transformer {
    fn load(s: &Store, p: &str, c: usize) -> Result<Self> {
        let b = format!("{p}.transformer_blocks.0");
        Ok(Self {
            norm: s.norm(&format!("{p}.norm"), c)?,
            proj_in: s.conv(&format!("{p}.proj_in"), c, c, 1)?,
            norm1: s.norm(&format!("{b}.norm1"), c)?,
            attn1: Attn::load(s, &format!("{b}.attn1"), c, c)?,
            norm2: s.norm(&format!("{b}.norm2"), c)?,
            attn2: Attn::load(s, &format!("{b}.attn2"), c, CTX)?,
            norm3: s.norm(&format!("{b}.norm3"), c)?,
            ff_in: s.linear(&format!("{b}.ff.net.0.proj"), c, 8 * c, true)?,
            ff_out: s.linear(&format!("{b}.ff.net.2"), 4 * c, c, true)?,
            proj_out: s.conv(&format!("{p}.proj_out"), c, c, 1)?,
        })
    }

    fn forward(&self, x: Tensor<4>, ctx: &Tensor<3>) -> Tensor<4> {
        let [b, c, h, w] = x.dims();
        let res = x.clone();
        let x = self.proj_in.forward(self.norm.group(x, GROUPS, ATTN_GN_EPS));
        let mut t = x.reshape([b, c, h * w]).swap_dims(1, 2);
        t = t.clone() + self.attn1.forward(self.norm1.layer(t, LN_EPS), None);
        t = t.clone() + self.attn2.forward(self.norm2.layer(t, LN_EPS), Some(ctx));
        let f = self.ff_in.forward(self.norm3.layer(t.clone(), LN_EPS));
        let hidden = f.clone().slice([0..b, 0..h * w, 0..4 * c]);
        let gate = f.slice([0..b, 0..h * w, 4 * c..8 * c]);
        t = t + self.ff_out.forward(hidden * gelu(gate));
        let x = t.swap_dims(1, 2).reshape([b, c, h, w]);
        self.proj_out.forward(x) + res
    }
}

/// The UNet.
#[derive(Debug, Clone)]
pub struct Unet {
    in_channels: usize,
    time1: Linear,
    time2: Linear,
    conv_in: Conv,
    down: Vec<(Vec<Resnet>, Vec<Transformer>, Option<Conv>)>,
    mid: Option<(Resnet, Transformer, Resnet)>,
    up: Vec<(Vec<Resnet>, Vec<Transformer>, Option<Conv>)>,
    norm_out: Norm,
    conv_out: Conv,
    /// Total downsampling of the latent (inputs must be multiples of it).
    factor: usize,
}

/// The sinusoidal timestep embedding (`flip_sin_to_cos`, shift 0): `[cos(t·f), sin(t·f)]`.
pub fn timestep_embedding(t: f64, dim: usize) -> Vec<f32> {
    let half = dim / 2;
    let mut out = vec![0f32; dim];
    for i in 0..half {
        let f = (-(10000f64.ln()) * i as f64 / half as f64).exp();
        let a = t * f;
        if let Some(x) = out.get_mut(i) {
            *x = a.cos() as f32;
        }
        if let Some(x) = out.get_mut(half + i) {
            *x = a.sin() as f32;
        }
    }
    out
}

impl Unet {
    /// Takes the UNet tensors (diffusers names, no prefix) of `store`. The layout is read from
    /// the tensors: SD 1.5 (channels 320/640/1280/1280, two ResNets per level, attention on the
    /// first three levels, a middle block) or a distilled variant such as SDXS (320/640/1280,
    /// one ResNet per level, attention on the last two, no middle block).
    pub fn from_store(s: &Store) -> Result<Self> {
        let in_channels =
            s.shape("conv_in.weight").and_then(|v| v.get(1).copied()).ok_or_else(|| SdError::Weights("missing tensor `conv_in.weight`".into()))?;
        if in_channels != 9 && in_channels != 4 {
            return Err(SdError::Weights(format!("UNet with {in_channels} input channels")));
        }
        let mut ch = Vec::new();
        while ch.len() < 8 {
            match s.shape(&format!("down_blocks.{}.resnets.0.conv2.weight", ch.len())).and_then(|v| v.first().copied()) {
                Some(c) => ch.push(c),
                None => break,
            }
        }
        let layers = (0..4).take_while(|j| s.has(&format!("down_blocks.0.resnets.{j}.conv1.weight"))).count();
        if ch.is_empty() || layers == 0 || ch.iter().any(|&c| c == 0 || c % GROUPS != 0) {
            return Err(SdError::Weights(format!("unsupported UNet layout (channels {ch:?}, {layers} ResNets per level)")));
        }
        let n = ch.len();
        let attn: Vec<bool> = (0..n).map(|i| s.has(&format!("down_blocks.{i}.attentions.0.proj_in.weight"))).collect();
        let mut down = Vec::new();
        let mut c_prev = ch[0];
        for (i, &c) in ch.iter().enumerate() {
            let mut res = Vec::new();
            let mut tr = Vec::new();
            for j in 0..layers {
                res.push(Resnet::load(s, &format!("down_blocks.{i}.resnets.{j}"), if j == 0 { c_prev } else { c }, c)?);
                if attn[i] {
                    tr.push(Transformer::load(s, &format!("down_blocks.{i}.attentions.{j}"), c)?);
                }
            }
            let ds = if i + 1 < n { Some(s.conv(&format!("down_blocks.{i}.downsamplers.0.conv"), c, c, 3)?) } else { None };
            down.push((res, tr, ds));
            c_prev = c;
        }
        let top = ch[n - 1];
        let mid = if s.has("mid_block.resnets.0.conv1.weight") {
            Some((
                Resnet::load(s, "mid_block.resnets.0", top, top)?,
                Transformer::load(s, "mid_block.attentions.0", top)?,
                Resnet::load(s, "mid_block.resnets.1", top, top)?,
            ))
        } else {
            None
        };
        // Skip channels in pop order (the reverse of the push order).
        let mut skips = vec![ch[0]];
        for (i, &c) in ch.iter().enumerate() {
            skips.extend(std::iter::repeat_n(c, layers));
            if i + 1 < n {
                skips.push(c);
            }
        }
        let mut up = Vec::new();
        let mut c_prev = top;
        for (i, &c) in ch.iter().rev().enumerate() {
            let level = n - 1 - i;
            let mut res = Vec::new();
            let mut tr = Vec::new();
            for j in 0..=layers {
                let skip = skips.pop().ok_or_else(|| SdError::Weights("UNet skip bookkeeping".into()))?;
                let c_in = if j == 0 { c_prev } else { c };
                res.push(Resnet::load(s, &format!("up_blocks.{i}.resnets.{j}"), c_in + skip, c)?);
                if attn[level] {
                    tr.push(Transformer::load(s, &format!("up_blocks.{i}.attentions.{j}"), c)?);
                }
            }
            let us = if i + 1 < n { Some(s.conv(&format!("up_blocks.{i}.upsamplers.0.conv"), c, c, 3)?) } else { None };
            up.push((res, tr, us));
            c_prev = c;
        }
        Ok(Self {
            in_channels,
            time1: s.linear("time_embedding.linear_1", ch[0], TEMB, true)?,
            time2: s.linear("time_embedding.linear_2", TEMB, TEMB, true)?,
            conv_in: s.conv("conv_in", in_channels, ch[0], 3)?,
            down,
            mid,
            up,
            norm_out: s.norm("conv_norm_out", ch[0])?,
            conv_out: s.conv("conv_out", ch[0], 4, 3)?,
            factor: 1 << (n - 1),
        })
    }

    pub fn in_channels(&self) -> usize {
        self.in_channels
    }

    /// Noise prediction for `x` `[B, in_channels, h, w]` (h, w multiples of 8) at `timestep`,
    /// conditioned on `ctx` `[B, 77, 768]`.
    pub fn forward(&self, x: Tensor<4>, timestep: f64, ctx: &Tensor<3>) -> Result<Tensor<4>> {
        let [b, c, h, w] = x.dims();
        if c != self.in_channels || h % self.factor != 0 || w % self.factor != 0 || h == 0 || w == 0 {
            return Err(SdError::Input(format!("UNet input {b}×{c}×{h}×{w}")));
        }
        if ctx.dims()[0] != b {
            return Err(SdError::Input(format!("{} prompt embeddings for a batch of {b}", ctx.dims()[0])));
        }
        let dt = x.dtype();
        let c0 = self.conv_in.w.dims()[0];
        let emb = timestep_embedding(timestep, c0);
        let emb = Tensor::<2>::from_data(TensorData::new(emb, [1, c0]), &x.device()).cast(dt);
        let temb = self.time2.forward(silu(self.time1.forward(emb)));
        let temb = silu(temb).repeat_dim(0, b);

        let mut x = self.conv_in.forward(x);
        let mut skips = vec![x.clone()];
        for (res, tr, ds) in &self.down {
            for (j, r) in res.iter().enumerate() {
                x = r.forward(x, &temb);
                if let Some(t) = tr.get(j) {
                    x = t.forward(x, ctx);
                }
                skips.push(x.clone());
            }
            if let Some(d) = ds {
                x = d.forward_stride2(x, 1);
                skips.push(x.clone());
            }
        }
        if let Some((r0, t, r1)) = &self.mid {
            x = r0.forward(x, &temb);
            x = t.forward(x, ctx);
            x = r1.forward(x, &temb);
        }
        for (res, tr, us) in &self.up {
            for (j, r) in res.iter().enumerate() {
                let skip = skips.pop().ok_or_else(|| SdError::Device("UNet skip underflow".into()))?;
                x = r.forward(Tensor::cat(vec![x, skip], 1), &temb);
                if let Some(t) = tr.get(j) {
                    x = t.forward(x, ctx);
                }
            }
            if let Some(u) = us {
                let [_, _, hh, ww] = x.dims();
                x = interpolate(x, InterpolateOptions::new(InterpolateMode::Nearest).with_output_size([hh * 2, ww * 2]));
                x = u.forward(x);
            }
        }
        Ok(self.conv_out.forward(silu(self.norm_out.group(x, GROUPS, RES_EPS))))
    }
}
