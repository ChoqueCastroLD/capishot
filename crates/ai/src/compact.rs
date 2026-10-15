//! Real-ESRGAN Compact (SRVGGNetCompact): the lite upscaler (`realesr-animevideov3`, BSD-3,
//! about 0.6 M parameters, 1.2 MB in fp16): fast enough for phones and machines without a GPU.
//!
//! `body.0` 3×3 conv → PReLU → `num_conv` × (3×3 conv → PReLU) → 3×3 conv to `out·r²` channels →
//! pixel shuffle ×r, plus the nearest-upsampled input (the network learns the residual). The
//! weight file keeps the release's key names (`body.N.weight`/`.bias`, PReLU slopes at the odd
//! indices). The ×2 variant runs the ×4 network and box-downsamples by 2, as Real-ESRGAN's
//! `--outscale 2` does.

use burn::tensor::Tensor;
use burn::tensor::activation::prelu;
use burn::tensor::module::{conv2d, interpolate};
use burn::tensor::ops::{ConvOptions, InterpolateMode, InterpolateOptions};

use crate::runtime::{Network, Runtime};
use crate::weights::Weights;
use crate::{AiError, Result};

/// The hyper-parameters, read from the weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub in_nc: usize,
    pub nf: usize,
    pub num_conv: usize,
    /// The network's own scale (pixel shuffle factor).
    pub upscale: usize,
}

#[derive(Clone, Debug)]
struct Layer {
    weight: Tensor<4>,
    bias: Tensor<1>,
    /// PReLU slopes (absent after the last conv).
    alpha: Option<Tensor<1>>,
}

/// A loaded Compact network.
#[derive(Clone, Debug)]
pub struct Compact {
    config: Config,
    /// Image channels in and out (1 when loaded for grayscale).
    channels: usize,
    /// Output scale: [`Config::upscale`], or half of it for the ×2 variant.
    scale: usize,
    layers: Vec<Layer>,
}

impl Compact {
    /// Reads the configuration from the weight shapes.
    pub fn config(w: &Weights) -> Result<Config> {
        let first = w.shape("body.0.weight").ok_or_else(|| AiError::Weights("missing body.0.weight".into()))?;
        let (nf, in_nc) = match first {
            [nf, inc, 3, 3] => (*nf, *inc),
            s => return Err(AiError::Weights(format!("body.0.weight has shape {s:?}"))),
        };
        // Convs at 0, 2, 4, …; the last one has no PReLU after it.
        let convs = (0..).take_while(|i| w.tensors.contains_key(&format!("body.{}.weight", 2 * i))).count();
        if !(2..=64).contains(&convs) || !(1..=256).contains(&nf) || !(1..=4).contains(&in_nc) {
            return Err(AiError::Weights(format!("unsupported Compact network: {convs} convs of {nf} filters, {in_nc} channels")));
        }
        let last = w.shape(&format!("body.{}.weight", 2 * (convs - 1))).ok_or_else(|| AiError::Weights("missing last conv".into()))?;
        let out = match last {
            [o, n, 3, 3] if *n == nf => *o,
            s => return Err(AiError::Weights(format!("last conv has shape {s:?}"))),
        };
        let upscale = (1..=8).find(|r| r * r * in_nc == out).ok_or_else(|| AiError::Weights(format!("{out} output channels for {in_nc} input channels")))?;
        Ok(Config { in_nc, nf, num_conv: convs - 2, upscale })
    }

    /// Uploads the network for an output `scale` (the network's own, or half of it). With
    /// `grayscale` it takes and returns one channel (the first conv's input weights summed, the
    /// last conv's first channel group kept): the same arithmetic as a gray RGB image, minus work.
    pub fn load(rt: &Runtime, w: &Weights, scale: usize, grayscale: bool) -> Result<Self> {
        let c = Self::config(w)?;
        if scale != c.upscale && scale * 2 != c.upscale {
            return Err(AiError::Weights(format!("a ×{} network can't upscale ×{scale}", c.upscale)));
        }
        let gray = grayscale && c.in_nc > 1;
        let channels = if gray { 1 } else { c.in_nc };
        let convs = c.num_conv + 2;
        let r2 = c.upscale * c.upscale;
        let mut layers = Vec::with_capacity(convs);
        for i in 0..convs {
            let (o, inp) = match i {
                0 => (c.nf, c.in_nc),
                i if i == convs - 1 => (c.in_nc * r2, c.nf),
                _ => (c.nf, c.nf),
            };
            let wt = &w.get(&format!("body.{}.weight", 2 * i), &[o, inp, 3, 3])?.data;
            let bt = &w.get(&format!("body.{}.bias", 2 * i), &[o])?.data;
            let (wv, bv, shape) = if gray && i == 0 {
                // Sum over the input channels.
                let mut f = vec![0.0; o * 9];
                for oc in 0..o {
                    for ic in 0..inp {
                        for t in 0..9 {
                            if let (Some(d), Some(s)) = (f.get_mut(oc * 9 + t), wt.get((oc * inp + ic) * 9 + t)) {
                                *d += *s;
                            }
                        }
                    }
                }
                (f, bt.clone(), [o, 1, 3, 3])
            } else if gray && i == convs - 1 {
                // Pixel shuffle reads channel group 0 for output channel 0.
                (wt.get(..r2 * inp * 9).unwrap_or(&[]).to_vec(), bt.get(..r2).unwrap_or(&[]).to_vec(), [r2, inp, 3, 3])
            } else {
                (wt.clone(), bt.clone(), [o, inp, 3, 3])
            };
            let alpha = if i < convs - 1 { Some(rt.tensor(w.get(&format!("body.{}.weight", 2 * i + 1), &[c.nf])?.data.clone(), [c.nf])?) } else { None };
            let n = shape[0];
            layers.push(Layer { weight: rt.tensor(wv, shape)?, bias: rt.tensor(bv, [n])?, alpha });
        }
        Ok(Self { config: c, channels, scale, layers })
    }

    pub fn configuration(&self) -> Config {
        self.config
    }
}

/// `[1, C·r², H, W]` → `[1, C, H·r, W·r]` in PyTorch's `pixel_shuffle` order.
fn pixel_shuffle(x: Tensor<4>, r: usize) -> Tensor<4> {
    let [n, c, h, w] = x.dims();
    let c = c / (r * r);
    x.reshape([n, c, r, r, h, w]).permute([0, 1, 4, 2, 5, 3]).reshape([n, c, h * r, w * r])
}

impl Network for Compact {
    fn scale(&self) -> usize {
        self.scale
    }
    fn in_channels(&self) -> usize {
        self.channels
    }
    fn out_channels(&self) -> usize {
        self.channels
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let r = self.config.upscale;
        let base = interpolate(x.clone(), InterpolateOptions::new(InterpolateMode::Nearest).with_scale_factor([r as f32, r as f32]));
        let mut t = x;
        for l in &self.layers {
            t = conv2d(t, l.weight.clone(), Some(l.bias.clone()), ConvOptions::new([1, 1], [1, 1], [1, 1], 1));
            if let Some(a) = &l.alpha {
                t = prelu(t, a.clone());
            }
        }
        let y = pixel_shuffle(t, r) + base;
        if self.scale == r {
            return y;
        }
        // ×2: the ×4 result box-downsampled by 2.
        let [n, c, h, w] = y.dims();
        y.reshape([n, c, h / 2, 2, w / 2, 2]).mean_dim(5).mean_dim(3).reshape([n, c, h / 2, w / 2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weights::to_safetensors;

    /// A tiny random Compact (2 middle convs, 8 filters, ×4) as safetensors.
    pub(crate) fn tiny() -> Weights {
        let nf = 8;
        let mut seed = 7u32;
        let mut rnd = |n: usize, s: f32| -> Vec<f32> {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * s
                })
                .collect()
        };
        let mut t: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::new();
        let shapes = [(nf, 3), (nf, nf), (nf, nf), (48, nf)];
        for (i, (o, inp)) in shapes.iter().enumerate() {
            t.push((format!("body.{}.weight", 2 * i), vec![*o, *inp, 3, 3], rnd(o * inp * 9, 0.4)));
            t.push((format!("body.{}.bias", 2 * i), vec![*o], rnd(*o, 0.2)));
            if i < 3 {
                t.push((format!("body.{}.weight", 2 * i + 1), vec![nf], rnd(nf, 0.5)));
            }
        }
        let refs: Vec<(&str, &[usize], &[f32])> = t.iter().map(|(n, s, d)| (n.as_str(), s.as_slice(), d.as_slice())).collect();
        Weights::from_safetensors(&to_safetensors(&refs, false, &[("arch", "compact")])).unwrap()
    }

    #[test]
    fn config_from_shapes() {
        let c = Compact::config(&tiny()).unwrap();
        assert_eq!((c.in_nc, c.nf, c.num_conv, c.upscale), (3, 8, 2, 4));
        assert!(Compact::config(&Weights::default()).is_err());
        assert!(Compact::load(&Runtime::cpu(), &tiny(), 3, false).is_err());
    }

    #[test]
    fn pixel_shuffle_matches_torch_order() {
        let rt = Runtime::cpu();
        // 4 channels, 1×1, r = 2: channel k lands at (k / 2, k % 2).
        let x = rt.tensor(vec![1.0, 2.0, 3.0, 4.0], [1, 4, 1, 1]).unwrap();
        let y = crate::runtime::block_on(rt.read(pixel_shuffle(x, 2))).unwrap();
        assert_eq!(y.data, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!((y.width, y.height), (2, 2));
    }

    #[test]
    fn grayscale_and_half_scale_match_the_full_network() {
        let rt = Runtime::cpu();
        let w = tiny();
        let img = crate::Planar::new(6, 5, 1, (0..30).map(|i| (i as f32 * 0.37).sin() * 0.5 + 0.5).collect()).unwrap();
        let rgb = Compact::load(&rt, &w, 4, false).unwrap();
        let gray = Compact::load(&rt, &w, 4, true).unwrap();
        let a = crate::runtime::block_on(rt.read(rgb.forward(rt.image(&img.with_channels(3)).unwrap()))).unwrap();
        let b = crate::runtime::block_on(rt.read(gray.forward(rt.image(&img).unwrap()))).unwrap();
        assert_eq!((b.width, b.height, b.channels, a.channels), (24, 20, 1, 3));
        for (x, y) in a.plane(0).iter().zip(b.plane(0)) {
            assert!((x - y).abs() < 1e-4, "{x} vs {y}");
        }
        let half = Compact::load(&rt, &w, 2, true).unwrap();
        let h = crate::runtime::block_on(rt.read(half.forward(rt.image(&img).unwrap()))).unwrap();
        assert_eq!((h.width, h.height), (12, 10));
        let p = b.plane(0);
        let mean = (p[0] + p[1] + p[24] + p[25]) / 4.0;
        assert!((h.data[0] - mean).abs() < 1e-5);
    }
}
