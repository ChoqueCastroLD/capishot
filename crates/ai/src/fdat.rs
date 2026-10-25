//! FDAT (Fast Dual Aggregation Transformer, as spandrel runs it): the network of the
//! IllustrationJaNai V3 models (`4x_IllustrationJaNai_V3detail_FDAT_M_40k`, and its `2x … unshuffle`
//! release, which pixel-unshuffles its input and runs the ×4 network at half resolution).
//!
//! `conv_first` → residual groups of blocks that alternate a spatial block (8×8 window attention
//! with a learned bias, modulated by a channel gate from a depthwise conv) and a channel block
//! (attention between channels over the whole tile, modulating the conv branch spatially), each
//! followed by a gated-depthwise FFN; a 3×3 conv closes every group (residual) → `conv_after`,
//! added to the first features → two stride-2 4×4 transposed convs (GELU between) → a 3×3 conv.
//! Shapes and options are read from the weights; spandrel's hyper-parameters (heads 4, window 8,
//! FFN ×2, gate reduction 8, pattern spatial/channel) are the defaults the releases use.

use burn::tensor::Tensor;
use burn::tensor::activation::{gelu, sigmoid, softmax};
use burn::tensor::module::{conv_transpose2d, conv2d};
use burn::tensor::ops::{ConvOptions, ConvTransposeOptions, PadMode};

use crate::runtime::{Network, Runtime};
use crate::weights::Weights;
use crate::{AiError, Result};

const HEADS: usize = 4;
const WINDOW: usize = 8;
/// `nn.LayerNorm`'s default.
const LN_EPS: f64 = 1e-5;
/// `F.normalize`'s default.
const NORM_EPS: f64 = 1e-12;

/// A convolution's parameters (bias optional), with its padding and groups.
#[derive(Clone, Debug)]
struct Conv {
    weight: Tensor<4>,
    bias: Option<Tensor<1>>,
    pad: usize,
    groups: usize,
}

impl Conv {
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        conv2d(x, self.weight.clone(), self.bias.clone(), ConvOptions::new([1, 1], [self.pad, self.pad], [1, 1], self.groups))
    }
}

/// `nn.Linear`: the weight stored transposed (`[in, out]`) for the matmul.
#[derive(Clone, Debug)]
struct Linear {
    weight: Tensor<2>,
    bias: Option<Tensor<1>>,
}

impl Linear {
    /// `[B, L, in]` → `[B, L, out]`.
    fn forward(&self, x: Tensor<3>) -> Tensor<3> {
        let [b, l, c] = x.dims();
        let out = self.weight.dims()[1];
        let y = x.reshape([b * l, c]).matmul(self.weight.clone());
        let y = match &self.bias {
            Some(bias) => y + bias.clone().reshape([1, out]),
            None => y,
        };
        y.reshape([b, l, out])
    }
}

/// `nn.LayerNorm` over the channels of `[B, L, C]`.
#[derive(Clone, Debug)]
struct Norm {
    weight: Tensor<1>,
    bias: Tensor<1>,
}

impl Norm {
    fn forward(&self, x: Tensor<3>) -> Tensor<3> {
        let c = self.weight.dims()[0];
        let mean = x.clone().mean_dim(2);
        let xc = x - mean;
        let var = xc.clone().square().mean_dim(2);
        let x = xc / (var + LN_EPS).sqrt();
        x * self.weight.clone().reshape([1, 1, c]) + self.bias.clone().reshape([1, 1, c])
    }
}

#[derive(Clone, Debug)]
enum Attention {
    /// Window attention: qkv, proj, the `[heads, 64, 64]` bias.
    Spatial { qkv: Linear, proj: Linear, bias: Tensor<4> },
    /// Channel attention: qkv, proj, the per-head temperature.
    Channel { qkv: Linear, proj: Linear, temp: Tensor<4> },
}

#[derive(Clone, Debug)]
struct Block {
    n1: Norm,
    n2: Norm,
    attn: Attention,
    /// Depthwise 3×3 of the conv branch (GELU after).
    conv: Conv,
    /// Spatial gate (channel blocks): 1×1 to one channel.
    sg: Conv,
    /// Channel gate (spatial blocks): 1×1 down, GELU, 1×1 up.
    cg1: Conv,
    cg2: Conv,
    fc1: Linear,
    fc2: Linear,
    /// Depthwise 3×3 inside the FFN.
    smix: Conv,
}

#[derive(Clone, Debug)]
struct Group {
    blocks: Vec<Block>,
    conv: Conv,
}

/// The hyper-parameters, read from the weight shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub dim: usize,
    pub groups: usize,
    pub blocks: usize,
    /// Pixel-unshuffle factor of the input (1 = none); the network is ×4 on the unshuffled input.
    pub shuffle: usize,
}

impl Config {
    pub fn scale(&self) -> usize {
        4 / self.shuffle
    }
}

/// A loaded FDAT network.
#[derive(Clone, Debug)]
pub struct Fdat {
    config: Config,
    conv_first: Conv,
    groups: Vec<Group>,
    conv_after: Conv,
    up1: (Tensor<4>, Tensor<1>),
    up2: (Tensor<4>, Tensor<1>),
    conv_last: Conv,
}

impl Fdat {
    /// Reads the configuration from the weights (spandrel's key names).
    pub fn config(w: &Weights) -> Result<Config> {
        let (first, shuffle) = match (w.shape("conv_first.weight"), w.shape("conv_first.1.weight")) {
            (Some(s), _) => (s, 1),
            (None, Some(s)) => (s, 2),
            _ => return Err(AiError::Weights("missing conv_first weights".into())),
        };
        let dim = match first {
            [d, c, 3, 3] if *c == 3 * shuffle * shuffle => *d,
            s => return Err(AiError::Weights(format!("conv_first has shape {s:?}"))),
        };
        let groups = (0..).take_while(|i| w.tensors.contains_key(&format!("groups.{i}.conv.weight"))).count();
        let blocks = (0..).take_while(|i| w.tensors.contains_key(&format!("groups.0.blocks.{i}.n1.weight"))).count();
        if groups == 0 || blocks == 0 || dim % HEADS != 0 || dim == 0 || dim > 512 {
            return Err(AiError::Weights(format!("unsupported FDAT: {groups} groups of {blocks} blocks, dim {dim}")));
        }
        Ok(Config { dim, groups, blocks, shuffle })
    }

    pub fn load(rt: &Runtime, w: &Weights) -> Result<Self> {
        let c = Self::config(w)?;
        let d = c.dim;
        let t4 = |name: &str, shape: [usize; 4]| -> Result<Tensor<4>> { rt.tensor(w.get(name, &shape)?.data.clone(), shape) };
        let t1 = |name: &str, n: usize| -> Result<Tensor<1>> { rt.tensor(w.get(name, &[n])?.data.clone(), [n]) };
        let conv = |name: &str, o: usize, i: usize, k: usize, bias: bool, groups: usize| -> Result<Conv> {
            Ok(Conv { weight: t4(&format!("{name}.weight"), [o, i, k, k])?, bias: if bias { Some(t1(&format!("{name}.bias"), o)?) } else { None }, pad: k / 2, groups })
        };
        let linear = |name: &str, o: usize, i: usize, bias: bool| -> Result<Linear> {
            let wt = w.get(&format!("{name}.weight"), &[o, i])?;
            // [out, in] → [in, out]
            let mut tr = vec![0.0f32; o * i];
            for r in 0..o {
                for col in 0..i {
                    if let (Some(dst), Some(src)) = (tr.get_mut(col * o + r), wt.data.get(r * i + col)) {
                        *dst = *src;
                    }
                }
            }
            Ok(Linear { weight: rt.tensor(tr, [i, o])?, bias: if bias { Some(t1(&format!("{name}.bias"), o)?) } else { None } })
        };
        let norm = |name: &str| -> Result<Norm> { Ok(Norm { weight: t1(&format!("{name}.weight"), d)?, bias: t1(&format!("{name}.bias"), d)? }) };
        let hidden = w.shape("groups.0.blocks.0.ffn.fc1.weight").and_then(|s| s.first().copied()).unwrap_or(2 * d);
        let red = w.shape("groups.0.blocks.0.inter.cg.1.weight").and_then(|s| s.first().copied()).unwrap_or(d / 8);
        let conv_first = if c.shuffle == 1 { conv("conv_first", d, 3, 3, true, 1)? } else { conv("conv_first.1", d, 3 * c.shuffle * c.shuffle, 3, true, 1)? };
        let mut groups = Vec::with_capacity(c.groups);
        for g in 0..c.groups {
            let mut blocks = Vec::with_capacity(c.blocks);
            for b in 0..c.blocks {
                let p = format!("groups.{g}.blocks.{b}");
                let qkv = linear(&format!("{p}.attn.qkv"), 3 * d, d, w.tensors.contains_key(&format!("{p}.attn.qkv.bias")))?;
                let proj = linear(&format!("{p}.attn.proj"), d, d, true)?;
                let attn = if w.tensors.contains_key(&format!("{p}.attn.temp")) {
                    Attention::Channel { qkv, proj, temp: rt.tensor(w.get(&format!("{p}.attn.temp"), &[HEADS, 1, 1])?.data.clone(), [1, HEADS, 1, 1])? }
                } else {
                    let n = WINDOW * WINDOW;
                    Attention::Spatial { qkv, proj, bias: rt.tensor(w.get(&format!("{p}.attn.bias"), &[HEADS, n, n])?.data.clone(), [1, HEADS, n, n])? }
                };
                blocks.push(Block {
                    n1: norm(&format!("{p}.n1"))?,
                    n2: norm(&format!("{p}.n2"))?,
                    attn,
                    conv: conv(&format!("{p}.conv.0"), d, 1, 3, false, d)?,
                    sg: conv(&format!("{p}.inter.sg.0"), 1, d, 1, false, 1)?,
                    cg1: conv(&format!("{p}.inter.cg.1"), red, d, 1, false, 1)?,
                    cg2: conv(&format!("{p}.inter.cg.3"), d, red, 1, false, 1)?,
                    fc1: linear(&format!("{p}.ffn.fc1"), hidden, d, false)?,
                    fc2: linear(&format!("{p}.ffn.fc2"), d, hidden, false)?,
                    smix: conv(&format!("{p}.ffn.smix"), hidden, 1, 3, false, hidden)?,
                });
            }
            groups.push(Group { blocks, conv: conv(&format!("groups.{g}.conv"), d, d, 3, false, 1)? });
        }
        let out = w.shape("upsampler.2.weight").and_then(|s| s.get(1).copied()).unwrap_or(3);
        Ok(Self {
            config: c,
            conv_first,
            groups,
            conv_after: conv("conv_after", d, d, 3, false, 1)?,
            up1: (t4("upsampler.0.weight", [d, d, 4, 4])?, t1("upsampler.0.bias", d)?),
            up2: (t4("upsampler.2.weight", [d, out, 4, 4])?, t1("upsampler.2.bias", out)?),
            conv_last: conv("upsampler.3", out, out, 3, true, 1)?,
        })
    }

    pub fn configuration(&self) -> Config {
        self.config
    }
}

/// `[B, L, C]` (row-major tokens) → `[B, C, H, W]`.
fn to_img(x: Tensor<3>, h: usize, w: usize) -> Tensor<4> {
    let [b, _, c] = x.dims();
    x.swap_dims(1, 2).reshape([b, c, h, w])
}

/// `[B, C, H, W]` → `[B, L, C]`.
fn to_seq(x: Tensor<4>) -> Tensor<3> {
    let [b, c, h, w] = x.dims();
    x.reshape([b, c, h * w]).swap_dims(1, 2)
}

/// `F.normalize(x, dim=-1)`.
fn l2_normalize(x: Tensor<4>) -> Tensor<4> {
    let n = x.clone().square().sum_dim(3).sqrt().clamp_min(NORM_EPS);
    x / n
}

/// q, k, v of `[B, L, 3C]` as `[B, heads, L, C/heads]` each.
fn split_qkv(qkv: Tensor<3>) -> [Tensor<4>; 3] {
    let [b, l, c3] = qkv.dims();
    let hd = c3 / 3 / HEADS;
    let t = qkv.reshape([b, l, 3, HEADS, hd]);
    let part = |i: usize| t.clone().narrow(2, i, 1).reshape([b, l, HEADS, hd]).swap_dims(1, 2);
    [part(0), part(1), part(2)]
}

impl Attention {
    fn forward(&self, x: Tensor<3>, h: usize, w: usize) -> Tensor<3> {
        match self {
            Self::Spatial { qkv, proj, bias } => {
                let [b, _, c] = x.dims();
                let ws = WINDOW;
                let (pad_r, pad_b) = ((ws - w % ws) % ws, (ws - h % ws) % ws);
                let mut img = to_img(x, h, w);
                if pad_r > 0 || pad_b > 0 {
                    img = img.pad((0, pad_r, 0, pad_b), PadMode::Constant(0.0));
                }
                let (hp, wp) = (h + pad_b, w + pad_r);
                let (nh, nw) = (hp / ws, wp / ws);
                let n = ws * ws;
                let win = img.permute([0, 2, 3, 1]).reshape([b, nh, ws, nw, ws, c]).permute([0, 1, 3, 2, 4, 5]).reshape([b * nh * nw, n, c]);
                let [q, k, v] = split_qkv(qkv.forward(win));
                let hd = c / HEADS;
                let scale = (hd as f64).powf(-0.5);
                let attn = q.mul_scalar(scale).matmul(k.swap_dims(2, 3)) + bias.clone();
                let out = softmax(attn, 3).matmul(v).swap_dims(1, 2).reshape([b * nh * nw, n, c]);
                let out = proj.forward(out).reshape([b, nh, nw, ws, ws, c]).permute([0, 1, 3, 2, 4, 5]).reshape([b, hp, wp, c]);
                let out = if pad_r > 0 || pad_b > 0 { out.narrow(1, 0, h).narrow(2, 0, w) } else { out };
                out.reshape([b, h * w, c])
            }
            Self::Channel { qkv, proj, temp } => {
                let [b, l, c] = x.dims();
                let [q, k, v] = split_qkv(qkv.forward(x));
                let q = l2_normalize(q.swap_dims(2, 3));
                let k = l2_normalize(k.swap_dims(2, 3));
                let attn = softmax(q.matmul(k.swap_dims(2, 3)) * temp.clone(), 3);
                let out = attn.matmul(v.swap_dims(2, 3)).permute([0, 3, 1, 2]).reshape([b, l, c]);
                proj.forward(out)
            }
        }
    }
}

impl Block {
    fn forward(&self, x: Tensor<3>, h: usize, w: usize) -> Tensor<3> {
        let [b, l, c] = x.dims();
        let n1 = self.n1.forward(x.clone());
        let a = self.attn.forward(n1.clone(), h, w);
        let cf = to_seq(gelu(self.conv.forward(to_img(n1, h, w))));
        let fused = match self.attn {
            // channel_modulates_spatial: a channel gate from the conv branch scales the attention.
            Attention::Spatial { .. } => {
                let pooled = to_img(cf.clone(), h, w).reshape([b, c, l]).mean_dim(2).reshape([b, c, 1, 1]);
                let cm = sigmoid(self.cg2.forward(gelu(self.cg1.forward(pooled)))).reshape([b, 1, c]);
                a * cm + cf
            }
            // spatial_modulates_channel: a spatial gate from the attention scales the conv branch.
            Attention::Channel { .. } => {
                let sm = sigmoid(self.sg.forward(to_img(a.clone(), h, w))).reshape([b, l, 1]);
                a + cf * sm
            }
        };
        let x = x + fused;
        let f = gelu(self.fc1.forward(self.n2.forward(x.clone())));
        let f = self.fc2.forward(to_seq(self.smix.forward(to_img(f, h, w))));
        x + f
    }
}

/// `[1, C, H, W]` → `[1, C·r², H/r, W/r]` in PyTorch's `pixel_unshuffle` channel order.
fn pixel_unshuffle(x: Tensor<4>, r: usize) -> Tensor<4> {
    let [n, c, h, w] = x.dims();
    x.reshape([n, c, h / r, r, w / r, r]).permute([0, 1, 3, 5, 2, 4]).reshape([n, c * r * r, h / r, w / r])
}

impl Network for Fdat {
    fn scale(&self) -> usize {
        self.config.scale()
    }
    fn in_channels(&self) -> usize {
        3
    }
    fn out_channels(&self) -> usize {
        self.conv_last.weight.dims()[0]
    }
    fn multiple_of(&self) -> usize {
        self.config.shuffle
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let [_, _, h0, w0] = x.dims();
        let x = if self.config.shuffle > 1 { pixel_unshuffle(x, self.config.shuffle) } else { x };
        let shallow = self.conv_first.forward(x);
        let [_, _, h, w] = shallow.dims();
        let mut deep = shallow.clone();
        for g in &self.groups {
            let mut seq = to_seq(deep.clone());
            for blk in &g.blocks {
                seq = blk.forward(seq, h, w);
            }
            deep = g.conv.forward(to_img(seq, h, w)) + deep;
        }
        let x = self.conv_after.forward(deep) + shallow;
        let opts = ConvTransposeOptions::new([2, 2], [1, 1], [0, 0], [1, 1], 1);
        let x = gelu(conv_transpose2d(x, self.up1.0.clone(), Some(self.up1.1.clone()), opts.clone()));
        let x = conv_transpose2d(x, self.up2.0.clone(), Some(self.up2.1.clone()), opts);
        let x = self.conv_last.forward(x);
        let s = self.scale();
        let [_, _, ho, wo] = x.dims();
        if ho == h0 * s && wo == w0 * s { x } else { x.narrow(2, 0, h0 * s).narrow(3, 0, w0 * s) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_weights() {
        assert!(Fdat::config(&Weights::default()).is_err());
    }

    /// The released model against spandrel's output (`CAPISHOT_FDAT_REF=<model.safetensors>:<in.f32>:<out.f32>`,
    /// raw little-endian planar f32 `[3, H, W]` / `[3, 4H, 4W]` with the size in the file names'
    /// `…_<W>x<H>.f32`): skipped when unset.
    #[test]
    fn matches_spandrel() {
        let Ok(spec) = std::env::var("CAPISHOT_FDAT_REF") else { return };
        let parts: Vec<&str> = spec.split(':').collect();
        let [model, input, output] = parts.as_slice() else { return };
        let read = |p: &str| -> Vec<f32> { std::fs::read(p).unwrap_or_default().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect() };
        let size = |p: &str| -> (usize, usize) {
            let s = p.rsplit('_').next().unwrap_or("").trim_end_matches(".f32");
            let mut it = s.split('x').map(|v| v.parse::<usize>().unwrap_or(0));
            (it.next().unwrap_or(0), it.next().unwrap_or(0))
        };
        let (w, h) = size(input);
        let rt = Runtime::cpu();
        let weights = Weights::from_safetensors(&std::fs::read(model).unwrap()).unwrap();
        let net = Fdat::load(&rt, &weights).unwrap();
        let img = crate::Planar::new(w, h, 3, read(input)).unwrap();
        let out = crate::runtime::block_on(rt.read(net.forward(rt.image(&img).unwrap()))).unwrap();
        let want = read(output);
        assert_eq!(out.data.len(), want.len());
        let max = out.data.iter().zip(&want).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(max < 2e-3, "off by {max}");
    }
}
