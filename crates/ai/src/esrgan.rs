//! ESRGAN (RRDBNet, "old arch" as spandrel runs it): the network of the MangaJaNai and
//! IllustrationJaNai V1 models.
//!
//! `conv_first` → 23 RRDB blocks (three residual dense blocks of five 3×3 convs each, growth 32,
//! residuals scaled by 0.2) → `conv_body`, added to the first features → two nearest ×2
//! upsamplings each followed by a conv → `conv_hr` → `conv_last`; leaky ReLU 0.2 after every conv
//! but `conv_first`, the fifth conv of a dense block, `conv_body` and `conv_last`. Models with
//! `shuffle = 2` (the 2x releases) pixel-unshuffle their input first, so they run the ×4 network
//! at half resolution.

use burn::tensor::Tensor;
use burn::tensor::activation::leaky_relu;
use burn::tensor::module::{conv2d, interpolate};
use burn::tensor::ops::{ConvOptions, InterpolateMode, InterpolateOptions};

use crate::runtime::{Network, Runtime};
use crate::weights::Weights;
use crate::{AiError, Result};

const SLOPE: f64 = 0.2;
const RES: f32 = 0.2;

/// A 3×3 convolution's parameters, on the device.
#[derive(Clone, Debug)]
pub struct Conv {
    weight: Tensor<4>,
    bias: Tensor<1>,
}

impl Conv {
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        conv2d(x, self.weight.clone(), Some(self.bias.clone()), ConvOptions::new([1, 1], [1, 1], [1, 1], 1))
    }
}

/// The hyper-parameters, read from the weight shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub in_nc: usize,
    pub out_nc: usize,
    pub nf: usize,
    pub nb: usize,
    /// Pixel-unshuffle factor of the input (1 = none).
    pub shuffle: usize,
}

impl Config {
    /// The overall scale: the ×4 network divided by the unshuffle factor.
    pub fn scale(&self) -> usize {
        4 / self.shuffle
    }
}

/// A loaded ESRGAN network.
#[derive(Clone, Debug)]
pub struct Esrgan {
    config: Config,
    /// Image channels the network takes (before unshuffling).
    image_in: usize,
    conv_first: Conv,
    body: Vec<[[Conv; 5]; 3]>,
    conv_body: Conv,
    conv_up1: Conv,
    conv_up2: Conv,
    conv_hr: Conv,
    conv_last: Conv,
}

/// Mutable host-side weights before upload, so they can be specialised (grayscale folding).
struct HostConv {
    w: Vec<f32>,
    shape: [usize; 4],
    b: Vec<f32>,
}

impl Esrgan {
    /// Reads the configuration from the weights (new-arch key names, see [`crate::catalog`]).
    pub fn config(w: &Weights) -> Result<Config> {
        let first = w.shape("conv_first.weight").ok_or_else(|| AiError::Weights("missing conv_first.weight".into()))?;
        let last = w.shape("conv_last.weight").ok_or_else(|| AiError::Weights("missing conv_last.weight".into()))?;
        let (nf, in_nc) = match first {
            [nf, inc, 3, 3] => (*nf, *inc),
            s => return Err(AiError::Weights(format!("conv_first.weight has shape {s:?}"))),
        };
        let out_nc = match last {
            [o, n, 3, 3] if *n == nf => *o,
            s => return Err(AiError::Weights(format!("conv_last.weight has shape {s:?}"))),
        };
        let nb = (0..).take_while(|i| w.tensors.contains_key(&format!("body.{i}.rdb1.conv1.weight"))).count();
        if nb == 0 || nb > 64 || nf == 0 || nf > 256 {
            return Err(AiError::Weights(format!("unsupported ESRGAN with {nb} blocks of {nf} filters")));
        }
        let shuffle = match w.metadata.get("shuffle").map(String::as_str) {
            Some("2") => 2,
            Some("1") | None if in_nc == out_nc => 1,
            Some("1") | None if in_nc == out_nc * 4 => 2,
            other => return Err(AiError::Weights(format!("unsupported shuffle {other:?} for {in_nc} input channels"))),
        };
        if in_nc % (shuffle * shuffle) != 0 {
            return Err(AiError::Weights(format!("{in_nc} input channels can't be unshuffled by {shuffle}")));
        }
        Ok(Config { in_nc, out_nc, nf, nb, shuffle })
    }

    /// Uploads the network. With `grayscale`, the network takes one channel (the image's gray
    /// value, which the model would see repeated in R, G and B: `conv_first` sums its input
    /// weights accordingly) and outputs only its first channel (as MangaJaNaiConverterGui keeps
    /// for grayscale pages). The result is the same arithmetic, minus work.
    pub fn load(rt: &Runtime, w: &Weights, grayscale: bool) -> Result<Self> {
        let c = Self::config(w)?;
        let host = |name: &str, o: usize, i: usize| -> Result<HostConv> {
            let wt = w.get(&format!("{name}.weight"), &[o, i, 3, 3])?;
            let bt = w.get(&format!("{name}.bias"), &[o])?;
            Ok(HostConv { w: wt.data.clone(), shape: [o, i, 3, 3], b: bt.data.clone() })
        };
        let up = |h: HostConv| -> Result<Conv> { Ok(Conv { weight: rt.tensor(h.w, h.shape)?, bias: rt.tensor(h.b, [h.shape[0]])? }) };
        let nf = c.nf;
        let gc = 32;
        let mut first = host("conv_first", nf, c.in_nc)?;
        let mut last = host("conv_last", c.out_nc, nf)?;
        let image_channels = c.in_nc / (c.shuffle * c.shuffle);
        let mut image_in = image_channels;
        if grayscale && image_channels > 1 {
            first = fold_input(first, c.shuffle * c.shuffle);
            image_in = 1;
            last = HostConv { w: last.w.get(..nf * 9).unwrap_or(&[]).to_vec(), shape: [1, nf, 3, 3], b: last.b.get(..1).unwrap_or(&[]).to_vec() };
        }
        let mut body = Vec::with_capacity(c.nb);
        for i in 0..c.nb {
            let rdb = |j: usize| -> Result<[Conv; 5]> {
                let n = |k: usize| format!("body.{i}.rdb{j}.conv{k}");
                Ok([
                    up(host(&n(1), gc, nf)?)?,
                    up(host(&n(2), gc, nf + gc)?)?,
                    up(host(&n(3), gc, nf + 2 * gc)?)?,
                    up(host(&n(4), gc, nf + 3 * gc)?)?,
                    up(host(&n(5), nf, nf + 4 * gc)?)?,
                ])
            };
            body.push([rdb(1)?, rdb(2)?, rdb(3)?]);
        }
        let config = Config { in_nc: image_in * c.shuffle * c.shuffle, out_nc: last.shape[0], ..c };
        Ok(Self {
            config,
            image_in,
            conv_first: up(first)?,
            body,
            conv_body: up(host("conv_body", nf, nf)?)?,
            conv_up1: up(host("conv_up1", nf, nf)?)?,
            conv_up2: up(host("conv_up2", nf, nf)?)?,
            conv_hr: up(host("conv_hr", nf, nf)?)?,
            conv_last: up(last)?,
        })
    }

    pub fn configuration(&self) -> Config {
        self.config
    }
}

/// `conv_first` for an input whose image channels are all equal: for each of the `positions`
/// unshuffle positions, the weights of every image channel are summed into one.
fn fold_input(h: HostConv, positions: usize) -> HostConv {
    let [o, i, kh, kw] = h.shape;
    let k = kh * kw;
    let mut w = vec![0.0; o * positions * k];
    for oc in 0..o {
        for ic in 0..i {
            // Unshuffled channel ic = image channel * positions + position.
            let pos = ic % positions;
            for t in 0..k {
                if let (Some(d), Some(s)) = (w.get_mut((oc * positions + pos) * k + t), h.w.get((oc * i + ic) * k + t)) {
                    *d += *s;
                }
            }
        }
    }
    HostConv { w, shape: [o, positions, kh, kw], b: h.b }
}

fn lrelu(x: Tensor<4>) -> Tensor<4> {
    leaky_relu(x, SLOPE)
}

fn rdb(c: &[Conv; 5], x: Tensor<4>) -> Tensor<4> {
    let x1 = lrelu(c[0].forward(x.clone()));
    let x2 = lrelu(c[1].forward(Tensor::cat(vec![x.clone(), x1.clone()], 1)));
    let x3 = lrelu(c[2].forward(Tensor::cat(vec![x.clone(), x1.clone(), x2.clone()], 1)));
    let x4 = lrelu(c[3].forward(Tensor::cat(vec![x.clone(), x1.clone(), x2.clone(), x3.clone()], 1)));
    let x5 = c[4].forward(Tensor::cat(vec![x.clone(), x1, x2, x3, x4], 1));
    x5.mul_scalar(RES).add(x)
}

fn up2(x: Tensor<4>) -> Tensor<4> {
    interpolate(x, InterpolateOptions::new(InterpolateMode::Nearest).with_scale_factor([2.0, 2.0]))
}

/// `[1, C, H, W]` → `[1, C·r², H/r, W/r]` in PyTorch's `pixel_unshuffle` channel order.
fn pixel_unshuffle(x: Tensor<4>, r: usize) -> Tensor<4> {
    let [n, c, h, w] = x.dims();
    x.reshape([n, c, h / r, r, w / r, r]).permute([0, 1, 3, 5, 2, 4]).reshape([n, c * r * r, h / r, w / r])
}

impl Network for Esrgan {
    fn scale(&self) -> usize {
        self.config.scale()
    }
    fn in_channels(&self) -> usize {
        self.image_in
    }
    fn out_channels(&self) -> usize {
        self.config.out_nc
    }
    fn multiple_of(&self) -> usize {
        self.config.shuffle
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let x = if self.config.shuffle > 1 { pixel_unshuffle(x, self.config.shuffle) } else { x };
        let fea = self.conv_first.forward(x);
        let mut t = fea.clone();
        for b in &self.body {
            let o = rdb(&b[2], rdb(&b[1], rdb(&b[0], t.clone())));
            t = o.mul_scalar(RES).add(t);
        }
        let fea = fea.add(self.conv_body.forward(t));
        let fea = lrelu(self.conv_up1.forward(up2(fea)));
        let fea = lrelu(self.conv_up2.forward(up2(fea)));
        self.conv_last.forward(lrelu(self.conv_hr.forward(fea)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::weights::to_safetensors;

    /// A tiny random ESRGAN (2 blocks, 8 filters) as safetensors.
    pub(crate) fn tiny(in_nc: usize, shuffle: &str) -> Weights {
        let nf = 8;
        let gc = 32;
        let mut seed = 1u32;
        let mut rnd = |n: usize| -> Vec<f32> {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.2
                })
                .collect()
        };
        let mut t: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::new();
        let mut conv = |name: String, o: usize, i: usize| {
            t.push((format!("{name}.weight"), vec![o, i, 3, 3], rnd(o * i * 9)));
            t.push((format!("{name}.bias"), vec![o], rnd(o)));
        };
        conv("conv_first".into(), nf, in_nc);
        for b in 0..2 {
            for j in 1..=3 {
                for k in 1..=5 {
                    let o = if k == 5 { nf } else { gc };
                    conv(format!("body.{b}.rdb{j}.conv{k}"), o, nf + (k - 1) * gc);
                }
            }
        }
        for n in ["conv_body", "conv_up1", "conv_up2", "conv_hr"] {
            conv(n.into(), nf, nf);
        }
        conv("conv_last".into(), 3, nf);
        let refs: Vec<(&str, &[usize], &[f32])> = t.iter().map(|(n, s, d)| (n.as_str(), s.as_slice(), d.as_slice())).collect();
        Weights::from_safetensors(&to_safetensors(&refs, false, &[("shuffle", shuffle)])).unwrap()
    }

    /// Within WebGPU's workgroup memory (16 KiB guaranteed, 32 KiB on most GPUs; 4 KiB too,
    /// so a 64-wide subgroup's tiling would show up on a software GPU's 8-wide one): the same
    /// output as the CPU. Skipped without a GPU adapter (lavapipe: `VK_ICD_FILENAMES`).
    #[cfg(feature = "gpu")]
    #[test]
    fn runs_on_a_gpu_with_little_workgroup_memory() {
        let cpu = Runtime::cpu();
        let img = crate::Planar::new(16, 12, 3, (0..16 * 12 * 3).map(|i| (i % 23) as f32 / 23.0).collect()).unwrap();
        for bytes in [4096, 16384, 32768] {
            let Some(gpu) = Runtime::gpu_limited_blocking(bytes).unwrap() else { return eprintln!("no GPU adapter: skipped") };
            for (inc, sh) in [(3, "1"), (12, "2")] {
                let w = tiny(inc, sh);
                let run = |rt: &Runtime| {
                    let net = Esrgan::load(rt, &w, false).unwrap();
                    crate::runtime::block_on(rt.read(net.forward(rt.image(&img).unwrap()))).unwrap()
                };
                let (a, b) = (run(&cpu), run(&gpu));
                assert_eq!((a.width, a.height), (b.width, b.height));
                let max = a.data.iter().zip(&b.data).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
                assert!(max < 1e-3, "{bytes} B, shuffle {sh}: off by {max}");
            }
        }
    }

    #[test]
    fn config_from_shapes() {
        let c = Esrgan::config(&tiny(3, "1")).unwrap();
        assert_eq!((c.in_nc, c.out_nc, c.nf, c.nb, c.shuffle, c.scale()), (3, 3, 8, 2, 1, 4));
        let c = Esrgan::config(&tiny(12, "2")).unwrap();
        assert_eq!((c.shuffle, c.scale()), (2, 2));
        assert!(Esrgan::config(&Weights::default()).is_err());
    }

    #[test]
    fn grayscale_folding_matches_repeated_input() {
        let rt = Runtime::cpu();
        for (inc, sh) in [(3, "1"), (12, "2")] {
            let w = tiny(inc, sh);
            let rgb = Esrgan::load(&rt, &w, false).unwrap();
            let gray = Esrgan::load(&rt, &w, true).unwrap();
            assert_eq!((gray.in_channels(), gray.out_channels()), (1, 1));
            let img = crate::Planar::new(6, 4, 1, (0..24).map(|i| i as f32 / 24.0).collect()).unwrap();
            let a = crate::runtime::block_on(rt.read(rgb.forward(rt.image(&img.with_channels(3)).unwrap()))).unwrap();
            let b = crate::runtime::block_on(rt.read(gray.forward(rt.image(&img).unwrap()))).unwrap();
            let s = rgb.scale();
            assert_eq!((b.width, b.height, a.channels), (6 * s, 4 * s, 3));
            for (x, y) in a.plane(0).iter().zip(b.plane(0)) {
                assert!((x - y).abs() < 1e-4, "{x} vs {y}");
            }
        }
    }
}
