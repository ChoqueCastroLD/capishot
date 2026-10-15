//! MI-GAN ("MI-GAN: A Simple Baseline for Image Inpainting on Mobile Devices", Sargsyan et al.,
//! ICCV 2023; Picsart AI Research, code and weights MIT): the lite inpainter, about 7 M
//! parameters (13 MB in fp16) and a fraction of LaMa's work, for phones and machines without a
//! GPU.
//!
//! The generator is a U-Net of depthwise-separable convolutions (`migan_inference.py` upstream):
//! an encoder from the working size down to 4×4 (each block: depthwise 3×3 + bias → act →
//! [FIR downsample] → pointwise 1×1 → act, twice) and a synthesis path back up that adds the
//! encoder features at each level, fixed per-level noise and a StyleGAN2-style skip RGB output.
//! The activation is leaky ReLU 0.2 × √2, clamped to ±256. The [1, 3, 3, 1] FIR filters are
//! constants rebuilt here (the converter drops them). The input is `[known − ½, rgb·known]` with
//! RGB in −1..1, the output RGB in −1..1.
//!
//! The released weights run at 512×512 (their noise maps are that size). A tile of any size up
//! to that is mirror-padded to a multiple of 128 (the encoder halves the size seven times) and
//! uses the top-left part of each noise map.

use burn::tensor::module::{conv2d, interpolate};
use burn::tensor::ops::{ConvOptions, InterpolateMode, InterpolateOptions, PadMode};
use burn::tensor::{Device, Tensor, TensorData};

use crate::weights::Reader;
use crate::{LamaError, Result};

const SLOPE: f64 = 0.2;
const CLAMP: f32 = 256.0;

/// A depthwise-separable conv (`SeparableConv2d` upstream).
#[derive(Debug, Clone)]
struct Sep {
    dw: Tensor<4>,
    dw_b: Tensor<1>,
    pw: Tensor<4>,
    /// Channels of the depthwise part.
    c: usize,
    down: Option<Tensor<4>>,
    up: Option<Tensor<4>>,
    /// Scaled noise map `[1, 1, r, r]`.
    noise: Option<Tensor<4>>,
}

#[derive(Debug, Clone)]
struct EncBlock {
    fromrgb: Option<(Tensor<4>, Tensor<1>)>,
    conv1: Sep,
    conv2: Sep,
}

#[derive(Debug, Clone)]
struct SynBlock {
    conv1: Sep,
    conv2: Sep,
    torgb: (Tensor<4>, Tensor<1>),
    /// The RGB skip's FIR upsampler (absent at 4×4).
    up_rgb: Option<Tensor<4>>,
}

/// A loaded MI-GAN generator on one device.
#[derive(Debug, Clone)]
pub struct Migan {
    device: Device,
    res: usize,
    /// Encoder blocks from `res` down to 8, then the 4×4 one.
    enc: Vec<EncBlock>,
    /// Synthesis blocks from 4 up to `res`.
    syn: Vec<SynBlock>,
}

fn act(x: Tensor<4>) -> Tensor<4> {
    burn::tensor::activation::leaky_relu(x, SLOPE).mul_scalar(std::f32::consts::SQRT_2).clamp(-CLAMP, CLAMP)
}

fn conv1x1(x: Tensor<4>, w: &Tensor<4>, b: Option<&Tensor<1>>) -> Tensor<4> {
    conv2d(x, w.clone(), b.cloned(), ConvOptions::new([1, 1], [0, 0], [1, 1], 1))
}

/// The normalized [1, 3, 3, 1] ⊗ [1, 3, 3, 1] filter times `gain`, for `c` depthwise channels.
fn fir(c: usize, gain: f32, device: &Device) -> Tensor<4> {
    let f = [1.0f32, 3.0, 3.0, 1.0];
    let k: Vec<f32> = (0..c).flat_map(|_| (0..16).map(|i| f[i / 4] * f[i % 4] / 64.0 * gain)).collect();
    Tensor::from_data(TensorData::new(k, [c, 1, 4, 4]), device)
}

/// `Upsample2d` upstream: zero-insertion ×2 (nearest × a [[1, 0], [0, 0]] pattern), zero pad
/// (2, 1, 2, 1), depthwise 4×4 FIR (gain 4).
fn upsample(x: Tensor<4>, f: &Tensor<4>) -> Tensor<4> {
    let [n, c, h, w] = x.dims();
    let up = interpolate(x, InterpolateOptions::new(InterpolateMode::Nearest).with_scale_factor([2.0, 2.0]));
    let pattern: Vec<f32> = (0..4 * h * w).map(|i| if (i / (2 * w)) % 2 == 0 && (i % (2 * w)) % 2 == 0 { 1.0 } else { 0.0 }).collect();
    let pattern = Tensor::<4>::from_data(TensorData::new(pattern, [1, 1, 2 * h, 2 * w]), &up.device());
    let z = (up * pattern).pad((2, 1, 2, 1), PadMode::Constant(0.0));
    let y = conv2d(z, f.clone(), None, ConvOptions::new([1, 1], [0, 0], [1, 1], c));
    debug_assert_eq!(y.dims(), [n, c, 2 * h, 2 * w]);
    y
}

impl Sep {
    fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let x = act(conv2d(x, self.dw.clone(), Some(self.dw_b.clone()), ConvOptions::new([1, 1], [1, 1], [1, 1], self.c)));
        let x = match &self.down {
            Some(f) => conv2d(x, f.clone(), None, ConvOptions::new([2, 2], [1, 1], [1, 1], self.c)),
            None => x,
        };
        let x = conv1x1(x, &self.pw, None);
        let x = match &self.up {
            Some(f) => upsample(x, f),
            None => x,
        };
        let x = match &self.noise {
            Some(n) => {
                let [_, _, h, w] = x.dims();
                x + n.clone().slice([0..1, 0..1, 0..h, 0..w])
            }
            None => x,
        };
        act(x)
    }
}

/// Largest supported model resolution (the release is 512).
const MAX_RES: usize = 1024;

impl Migan {
    /// Loads converted weights (`arch = migan`, see the module docs) onto `device`.
    pub fn from_safetensors(bytes: &[u8], device: &Device) -> Result<Self> {
        let r = Reader::new(bytes, device)?;
        // The resolution: the largest synthesis block.
        let mut res = 4;
        while res < MAX_RES && r.shape(&format!("synthesis.b{}.torgb.weight", res * 2)).is_some() {
            res *= 2;
        }
        if res < 16 {
            return Err(LamaError::Weights("not an MI-GAN generator (no synthesis blocks)".into()));
        }
        let ch = |name: &str, dim: usize| -> Result<usize> {
            r.shape(name).and_then(|s| s.get(dim).copied()).filter(|c| (1..=4096).contains(c)).ok_or_else(|| LamaError::Weights(format!("missing or bad tensor `{name}`")))
        };
        let scalar = |name: &str| -> Result<f32> {
            let t = r.st.tensor(name).map_err(|_| LamaError::Weights(format!("missing tensor `{name}`")))?;
            let n: usize = t.shape().iter().product();
            if n != 1 {
                return Err(LamaError::Weights(format!("`{name}` is not a scalar")));
            }
            let shape = t.shape().to_vec();
            r.floats(name, &shape)?.first().copied().ok_or_else(|| LamaError::Weights(format!("`{name}` is empty")))
        };
        // A separable conv `p` with `ci` → `co` channels at map size `size` (for its noise).
        let sep = |p: &str, ci: usize, co: usize, down: bool, up: bool, size: Option<usize>| -> Result<Sep> {
            let noise = match size {
                Some(s) => {
                    let n = r.floats(&format!("{p}.noise_const"), &[s, s])?;
                    let k = scalar(&format!("{p}.noise_strength"))?;
                    Some(Tensor::from_data(TensorData::new(n.into_iter().map(|v| v * k).collect::<Vec<f32>>(), [1, 1, s, s]), device))
                }
                None => None,
            };
            Ok(Sep {
                dw: r.t4(&format!("{p}.conv1.weight"), [ci, 1, 3, 3])?,
                dw_b: r.t1(&format!("{p}.conv1.bias"), ci)?,
                pw: r.t4(&format!("{p}.conv2.weight"), [co, ci, 1, 1])?,
                c: ci,
                down: down.then(|| fir(ci, 1.0, device)),
                up: up.then(|| fir(co, 4.0, device)),
                noise,
            })
        };
        let mut enc = Vec::new();
        let mut s = res;
        while s >= 8 {
            let p = format!("encoder.b{s}");
            let ic = ch(&format!("{p}.conv1.conv1.weight"), 0)?;
            let oc = ch(&format!("{p}.conv2.conv2.weight"), 0)?;
            let fromrgb = if s == res { Some((r.t4(&format!("{p}.fromrgb.weight"), [ic, 4, 1, 1])?, r.t1(&format!("{p}.fromrgb.bias"), ic)?)) } else { None };
            enc.push(EncBlock { fromrgb, conv1: sep(&format!("{p}.conv1"), ic, ic, false, false, None)?, conv2: sep(&format!("{p}.conv2"), ic, oc, true, false, None)? });
            s /= 2;
        }
        let c4 = ch("encoder.b4.conv1.conv1.weight", 0)?;
        enc.push(EncBlock { fromrgb: None, conv1: sep("encoder.b4.conv1", c4, c4, false, false, None)?, conv2: sep("encoder.b4.conv2", c4, c4, false, false, None)? });
        let torgb = |p: &str, c: usize| -> Result<(Tensor<4>, Tensor<1>)> { Ok((r.t4(&format!("{p}.torgb.weight"), [3, c, 1, 1])?, r.t1(&format!("{p}.torgb.bias"), 3)?)) };
        let s4 = ch("synthesis.b4.conv1.conv1.weight", 0)?;
        let mut syn = vec![SynBlock {
            conv1: sep("synthesis.b4.conv1", s4, s4, false, false, None)?,
            conv2: sep("synthesis.b4.conv2", s4, s4, false, false, None)?,
            torgb: torgb("synthesis.b4", s4)?,
            up_rgb: None,
        }];
        let mut s = 8;
        while s <= res {
            let p = format!("synthesis.b{s}");
            let ic = ch(&format!("{p}.conv1.conv1.weight"), 0)?;
            let oc = ch(&format!("{p}.conv1.conv2.weight"), 0)?;
            syn.push(SynBlock {
                conv1: sep(&format!("{p}.conv1"), ic, oc, false, true, Some(s))?,
                conv2: sep(&format!("{p}.conv2"), oc, oc, false, false, Some(s))?,
                torgb: torgb(&p, oc)?,
                up_rgb: Some(fir(3, 4.0, device)),
            });
            s *= 2;
        }
        Ok(Self { device: device.clone(), res, enc, syn })
    }

    /// The resolution the weights were trained at (the largest working side).
    pub fn resolution(&self) -> usize {
        self.res
    }

    /// Working sides are padded to a multiple of this.
    pub fn multiple(&self) -> usize {
        self.res / 4
    }

    /// Checks a tile size: at most [`Migan::resolution`] per side.
    pub fn check_size(&self, w: usize, h: usize) -> Result<()> {
        if w == 0 || h == 0 || w > self.res || h > self.res {
            return Err(LamaError::Input(format!("MI-GAN tiles are 1..={} pixels per side, got {w}x{h}", self.res)));
        }
        Ok(())
    }

    fn generator(&self, x: Tensor<4>) -> Tensor<4> {
        let mut feats = Vec::with_capacity(self.enc.len());
        let mut x = x;
        let img = x.clone();
        for b in &self.enc {
            // Only the first block reads the image (upstream: `x = fromrgb(img)` when x is None).
            if let Some((w, bias)) = &b.fromrgb {
                x = act(conv1x1(img.clone(), w, Some(bias)));
            }
            let feat = b.conv1.forward(x);
            x = b.conv2.forward(feat.clone());
            feats.push(feat);
        }
        // Synthesis from 4×4 up, adding the encoder features from the deepest.
        let mut img: Option<Tensor<4>> = None;
        for (b, feat) in self.syn.iter().zip(feats.into_iter().rev()) {
            x = b.conv2.forward(b.conv1.forward(x) + feat);
            let y = conv1x1(x.clone(), &b.torgb.0, Some(&b.torgb.1));
            img = Some(match (img, &b.up_rgb) {
                (Some(i), Some(f)) => upsample(i, f) + y,
                _ => y,
            });
        }
        img.unwrap_or(x)
    }

    /// Builds the padded input: `[known − ½, (2·rgb − 1)·known]` at `pw × ph`, mirroring the
    /// tile into the padding (mask included).
    fn input(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize, pw: usize, ph: usize) -> Result<Tensor<4>> {
        let n = w * h;
        if rgb.len() != n * 3 || mask.len() != n {
            return Err(LamaError::Input(format!("expected {} RGB and {n} mask samples, got {} and {}", n * 3, rgb.len(), mask.len())));
        }
        let idx = |i: usize, len: usize| -> usize {
            if len <= 1 {
                return 0;
            }
            // Symmetric (edge-repeating) mirror, periodic for any amount of padding.
            let m = i % (2 * len);
            if m < len { m } else { 2 * len - 1 - m }
        };
        let pn = pw * ph;
        let mut planar = vec![0f32; pn * 4];
        for y in 0..ph {
            let sy = idx(y, h);
            for x in 0..pw {
                let si = sy * w + idx(x, w);
                let o = y * pw + x;
                let known = if mask.get(si).copied().unwrap_or(0.0) > 0.0 { 0.0 } else { 1.0 };
                planar[o] = known - 0.5;
                for c in 0..3 {
                    let v = rgb.get(si * 3 + c).copied().unwrap_or(0.0);
                    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                    planar[(c + 1) * pn + o] = (v * 2.0 - 1.0) * known;
                }
            }
        }
        Ok(Tensor::from_data(TensorData::new(planar, [1, 4, ph, pw]), &self.device))
    }

    fn run(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Tensor<3>> {
        self.check_size(w, h)?;
        let m = self.multiple();
        let (pw, ph) = (w.div_ceil(m) * m, h.div_ceil(m) * m);
        let y = self.generator(self.input(rgb, mask, w, h, pw, ph)?);
        let y = y.slice([0..1, 0..3, 0..h, 0..w]).mul_scalar(0.5).add_scalar(0.5).clamp(0.0, 1.0);
        Ok(y.reshape([3, h, w]).permute([1, 2, 0]))
    }

    /// Inpaints one tile (`mask` 1 = fill) and returns the generator output for every pixel
    /// (interleaved RGB, `w·h·3`, 0..=1). Native only (blocks on the device).
    #[cfg(not(target_family = "wasm"))]
    pub fn generate(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        let t = self.run(rgb, mask, w, h)?;
        crate::model::to_vec(t.try_into_data().map_err(|e| LamaError::Device(format!("{e:?}")))?, w * h * 3)
    }

    /// [`Migan::generate`] with an asynchronous read-back (WebGPU in the browser).
    pub async fn generate_async(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        let t = self.run(rgb, mask, w, h)?;
        crate::model::to_vec(t.into_data_async().await.map_err(|e| LamaError::Device(format!("{e:?}")))?, w * h * 3)
    }
}
