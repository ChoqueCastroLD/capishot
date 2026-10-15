//! The LaMa generator on burn.
//!
//! Layout (big-lama, `ngf = 64`): reflect-pad 3 + 7×7 conv → two stride-2 3×3 convs → a stride-2
//! FFC splitting into local (25 %) and global (75 %) channels → 18 FFC ResNet blocks → three
//! stride-2 transposed convs → reflect-pad 3 + 7×7 conv + sigmoid. Batch norms are folded into
//! the convolutions by the converter, so every layer here is conv (+ bias) (+ ReLU).

use burn::tensor::activation::{relu, sigmoid};
use burn::tensor::module::{conv_transpose2d, conv2d};
use burn::tensor::ops::{ConvOptions, ConvTransposeOptions, PadMode};
use burn::tensor::{Device, Tensor, TensorData};

use crate::dft::Dft2;
use crate::weights::{self, Arch, Conv, Ffc, Weights};
use crate::{LamaError, MAX_WORK_SIDE, MIN_WORK_SIDE, Result, SIZE_MULTIPLE};

/// A loaded generator on one device.
#[derive(Debug, Clone)]
pub struct Lama {
    w: Weights,
    device: Device,
}

/// Device-side DFT matrices for one bottleneck size.
struct DftT {
    fw: Tensor<2>,
    fh: Tensor<2>,
    ih: Tensor<2>,
    iw: Tensor<2>,
}

fn conv(x: Tensor<4>, c: &Conv, stride: usize, pad: usize) -> Tensor<4> {
    let x = if pad > 0 { x.pad((pad, pad, pad, pad), PadMode::Reflect) } else { x };
    conv2d(x, c.w.clone(), c.b.clone(), ConvOptions::new([stride; 2], [0, 0], [1, 1], 1))
}

impl Lama {
    /// Loads converted weights (see the crate docs) onto `device`.
    pub fn from_safetensors(bytes: &[u8], device: &Device) -> Result<Self> {
        Ok(Self { w: weights::load(bytes, device)?, device: device.clone() })
    }

    /// The architecture read from the weights.
    pub fn arch(&self) -> Arch {
        self.w.arch
    }

    /// The device the weights live on.
    pub fn device(&self) -> &Device {
        &self.device
    }

    /// Checks a working size: multiples of 8 between [`MIN_WORK_SIDE`] and [`MAX_WORK_SIDE`].
    pub fn check_size(w: usize, h: usize) -> Result<()> {
        for (name, v) in [("width", w), ("height", h)] {
            if !v.is_multiple_of(SIZE_MULTIPLE) || !(MIN_WORK_SIDE..=MAX_WORK_SIDE).contains(&v) {
                return Err(LamaError::Input(format!("{name} {v} must be a multiple of {SIZE_MULTIPLE} in {MIN_WORK_SIDE}..={MAX_WORK_SIDE}")));
            }
        }
        Ok(())
    }

    /// Builds the model input on the device: `[rgb·(1−m), m]`, with the mask binarized (> 0).
    /// `rgb` is interleaved RGB in 0..=1 (`w·h·3`), `mask` is `w·h` coverage.
    fn input(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Tensor<4>> {
        Self::check_size(w, h)?;
        let n = w * h;
        if rgb.len() != n * 3 || mask.len() != n {
            return Err(LamaError::Input(format!("expected {} RGB and {n} mask samples, got {} and {}", n * 3, rgb.len(), mask.len())));
        }
        let mut planar = vec![0f32; n * 4];
        for i in 0..n {
            let m = if mask[i] > 0.0 { 1.0 } else { 0.0 };
            for c in 0..3 {
                let v = rgb[i * 3 + c];
                let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
                planar[c * n + i] = v * (1.0 - m);
            }
            planar[3 * n + i] = m;
        }
        Ok(Tensor::from_data(TensorData::new(planar, [1, 4, h, w]), &self.device))
    }

    fn dft(&self, h: usize, w: usize) -> Result<DftT> {
        let d = Dft2::new(h, w).ok_or_else(|| LamaError::Input(format!("bottleneck {w}x{h} out of range")))?;
        let wc = d.wc();
        let t = |v: Vec<f32>, r: usize, c: usize| Tensor::<2>::from_data(TensorData::new(v, [r, c]), &self.device);
        Ok(DftT { fw: t(d.fw, w, 2 * wc), fh: t(d.fh, 2 * h, 2 * h), ih: t(d.ih, 2 * h, 2 * h), iw: t(d.iw, 2 * wc, w) })
    }

    /// Fourier unit: rfft2 (ortho) → 1×1 conv + ReLU on `[re, im]` channels → irfft2.
    fn fourier(z: Tensor<4>, fu: &Conv, d: &DftT) -> Tensor<4> {
        let [b, c, h, w] = z.dims();
        let wc = w / 2 + 1;
        // Rows: (b·c·h, w) · (w, 2wc) → [A | B]
        let y = z.reshape([b * c * h, w]).matmul(d.fw.clone());
        // → (b, c, wc, 2, h) → (b·c·wc, 2h) = [Aᵀ | Bᵀ]; columns: · fh → [Reᵀ | Imᵀ]
        let y: Tensor<5> = y.reshape([b, c, h, 2, wc]).permute([0, 1, 4, 3, 2]);
        let y = y.reshape([b * c * wc, 2 * h]).matmul(d.fh.clone());
        // Channels as [re(c) …, im(c) …] (the converter reordered the conv to match).
        let y: Tensor<5> = y.reshape([b, c, wc, 2, h]).permute([0, 3, 1, 2, 4]);
        let y = y.reshape([b, 2 * c, wc, h]);
        let y = relu(conv2d(y, fu.w.clone(), fu.b.clone(), ConvOptions::new([1, 1], [0, 0], [1, 1], 1)));
        let y: Tensor<5> = y.reshape([b, 2, c, wc, h]).permute([0, 2, 3, 1, 4]);
        let y = y.reshape([b * c * wc, 2 * h]).matmul(d.ih.clone());
        let y: Tensor<5> = y.reshape([b, c, wc, 2, h]).permute([0, 1, 4, 3, 2]);
        y.reshape([b * c * h, 2 * wc]).matmul(d.iw.clone()).reshape([b, c, h, w])
    }

    fn ffc(xl: Tensor<4>, xg: Tensor<4>, f: &Ffc, d: &DftT) -> (Tensor<4>, Tensor<4>) {
        let ol = conv(xl.clone(), &f.l2l, 1, 1) + conv(xg.clone(), &f.g2l, 1, 1);
        let z = relu(conv(xg, &f.st1, 1, 0));
        let st = conv(z.clone() + Self::fourier(z, &f.fu, d), &f.st2, 1, 0);
        let og = conv(xl, &f.l2g, 1, 1) + st;
        (relu(ol), relu(og))
    }

    /// The generator on a prepared `(1, 4, h, w)` input; returns `(1, 3, h, w)` in 0..=1.
    fn generator(&self, x: Tensor<4>) -> Result<Tensor<4>> {
        let [_, _, h, w] = x.dims();
        let d = self.dft(h / 8, w / 8)?;
        let x = relu(conv(x, &self.w.stem, 1, 3));
        let x = relu(conv(x, &self.w.down1, 2, 1));
        let x = relu(conv(x, &self.w.down2, 2, 1));
        let mut xl = relu(conv(x.clone(), &self.w.down3_l, 2, 1));
        let mut xg = relu(conv(x, &self.w.down3_g, 2, 1));
        for [c1, c2] in &self.w.blocks {
            let (al, ag) = Self::ffc(xl.clone(), xg.clone(), c1, &d);
            let (al, ag) = Self::ffc(al, ag, c2, &d);
            xl = xl + al;
            xg = xg + ag;
        }
        let mut x = Tensor::cat(vec![xl, xg], 1);
        let up = ConvTransposeOptions::new([2, 2], [1, 1], [1, 1], [1, 1], 1);
        for u in &self.w.up {
            x = relu(conv_transpose2d(x, u.w.clone(), u.b.clone(), up.clone()));
        }
        Ok(sigmoid(conv(x, &self.w.out, 1, 3)))
    }

    /// Runs the model and leaves the interleaved `(h, w, 3)` result on the device.
    fn run(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Tensor<3>> {
        let x = self.input(rgb, mask, w, h)?;
        let y = self.generator(x)?;
        Ok(y.reshape([3, h, w]).permute([1, 2, 0]))
    }

    /// Inpaints one working tile and returns the raw generator output (interleaved RGB,
    /// `w·h·3`, 0..=1) for every pixel; callers keep the original outside the mask
    /// ([`crate::tiles::paste`]). Blocks until the device is done: native only (the browser
    /// must use [`Lama::generate_async`]).
    #[cfg(not(target_family = "wasm"))]
    pub fn generate(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        let t = self.run(rgb, mask, w, h)?;
        let data = t.try_into_data().map_err(|e| LamaError::Device(format!("{e:?}")))?;
        to_vec(data, w * h * 3)
    }

    /// [`Lama::generate`] with an asynchronous read-back (WebGPU in the browser).
    pub async fn generate_async(&self, rgb: &[f32], mask: &[f32], w: usize, h: usize) -> Result<Vec<f32>> {
        let t = self.run(rgb, mask, w, h)?;
        let data = t.into_data_async().await.map_err(|e| LamaError::Device(format!("{e:?}")))?;
        to_vec(data, w * h * 3)
    }
}

pub(crate) fn to_vec(data: TensorData, n: usize) -> Result<Vec<f32>> {
    let v = data.try_into_vec::<f32>().map_err(|e| LamaError::Device(format!("read-back: {e:?}")))?;
    if v.len() != n {
        return Err(LamaError::Device(format!("read back {} values, expected {n}", v.len())));
    }
    Ok(v)
}
