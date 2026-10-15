//! Inference with burn: the compute device and tiled execution of any [`Network`].
//!
//! Everything that reads results back is `async`: in the browser WebGPU readback can't block.
//! Native callers use [`block_on`].

use burn::tensor::{Device, Tensor, TensorData};

use crate::tile::{Blender, TileOptions, plan};
use crate::{AiError, Planar, Progress, Result};

/// Which kind of device runs the networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accel {
    /// wgpu: WebGPU in the browser, Vulkan/Metal/DX12 natively.
    Gpu,
    /// burn-flex on the CPU (SIMD; rayon natively).
    Cpu,
}

/// A compute device. Cheap to clone (burn devices are handles).
#[derive(Clone, Debug)]
pub struct Runtime {
    device: Device,
    accel: Accel,
}

impl Runtime {
    /// The CPU device.
    #[cfg(feature = "cpu")]
    pub fn cpu() -> Self {
        Self { device: Device::flex(), accel: Accel::Cpu }
    }

    /// The default GPU (high-performance adapter preferred), initialised asynchronously: the
    /// only way in a browser. Fails when there is no usable adapter (no WebGPU, no driver).
    /// Initialise once per process and clone the runtime.
    #[cfg(feature = "gpu")]
    pub async fn gpu() -> Result<Self> {
        let device = match Device::wgpu_options().init_async().await {
            Ok(d) => d,
            // Another model (the LaMa inpainter) already brought the runtime up: share it.
            Err(burn::tensor::wgpu::WgpuInitError::Registration { .. }) => Device::wgpu(burn::tensor::DeviceKind::DefaultDevice),
            Err(e) => return Err(AiError::Device(e.to_string())),
        };
        Ok(Self { device, accel: Accel::Gpu })
    }

    /// [`Runtime::gpu`], blocking (native only).
    #[cfg(all(feature = "gpu", not(target_family = "wasm")))]
    pub fn gpu_blocking() -> Result<Self> {
        Self::gpu_kind_blocking(burn::tensor::DeviceKind::DefaultDevice)
    }

    /// A specific kind of wgpu adapter, blocking (native only): `DiscreteGpu(0)`,
    /// `IntegratedGpu(0)`… Fails when there is none.
    #[cfg(all(feature = "gpu", not(target_family = "wasm")))]
    pub fn gpu_kind_blocking(kind: burn::tensor::DeviceKind) -> Result<Self> {
        let device = match Device::wgpu_options().device_kind(kind.clone()).init() {
            Ok(d) => d,
            // Already brought up (by another model in this process): share it.
            Err(burn::tensor::wgpu::WgpuInitError::Registration { .. }) => Device::wgpu(kind),
            Err(e) => return Err(AiError::Device(e.to_string())),
        };
        Ok(Self { device, accel: Accel::Gpu })
    }

    pub fn accel(&self) -> Accel {
        self.accel
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    /// A tensor of this shape from row-major values (checked).
    pub fn tensor<const D: usize>(&self, data: Vec<f32>, shape: [usize; D]) -> Result<Tensor<D>> {
        let n = shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d)).ok_or_else(|| AiError::Inference("tensor too large".into()))?;
        if n != data.len() || n == 0 {
            return Err(AiError::Inference(format!("{} values for shape {shape:?}", data.len())));
        }
        Ok(Tensor::from_data(TensorData::new(data, shape), &self.device))
    }

    /// A planar image as a `[1, C, H, W]` tensor.
    pub fn image(&self, img: &Planar) -> Result<Tensor<4>> {
        self.tensor(img.data.clone(), [1, img.channels, img.height, img.width])
    }

    /// Reads a `[1, C, H, W]` tensor back into an image.
    pub async fn read(&self, t: Tensor<4>) -> Result<Planar> {
        let [n, c, h, w] = t.dims();
        if n != 1 {
            return Err(AiError::Inference(format!("batch of {n}, expected 1")));
        }
        let data = t.into_data_async().await.map_err(|e| AiError::Inference(format!("{e:?}")))?;
        let v = data.as_slice::<f32>().map_err(|e| AiError::Inference(format!("{e:?}")))?.to_vec();
        Planar::new(w, h, c, v)
    }
}

/// A network: a burn forward pass from `[1, in_channels, H, W]` to
/// `[1, out_channels, H·scale, W·scale]`.
pub trait Network {
    /// Output pixels per input pixel along each side.
    fn scale(&self) -> usize;
    fn in_channels(&self) -> usize;
    fn out_channels(&self) -> usize;
    /// Input sides must be multiples of this (they are reflect-padded, the output cropped).
    fn multiple_of(&self) -> usize {
        1
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4>;
}

/// Runs `net` over `input` tile by tile ([`crate::tile`]), reporting each tile to `progress`
/// and stopping with [`AiError::Cancelled`] between tiles once cancelled. One-channel input is
/// repeated to the network's channel count.
pub async fn run_tiled(rt: &Runtime, net: &dyn Network, input: &Planar, opts: TileOptions, progress: &dyn Progress) -> Result<Planar> {
    let input = if input.channels == net.in_channels() {
        std::borrow::Cow::Borrowed(input)
    } else if input.channels == 1 {
        std::borrow::Cow::Owned(input.with_channels(net.in_channels()))
    } else {
        return Err(AiError::Image(format!("{} channels for a {}-channel network", input.channels, net.in_channels())));
    };
    let s = net.scale();
    let p = plan(input.width, input.height, opts)?;
    let mut blend = Blender::new(input.width, input.height, net.out_channels(), s, &p)?;
    let total = p.tiles.len() as u64;
    progress.report(0, total, "");
    for (i, t) in p.tiles.iter().enumerate() {
        if progress.cancelled() {
            return Err(AiError::Cancelled);
        }
        let crop = input.crop(t.x0, t.y0, t.width(), t.height());
        let m = net.multiple_of().max(1);
        let padded = reflect_pad(&crop, crop.width.div_ceil(m) * m, crop.height.div_ceil(m) * m);
        let y = net.forward(rt.image(&padded)?);
        let mut out = rt.read(y).await?;
        if out.width != padded.width * s || out.height != padded.height * s {
            return Err(AiError::Inference(format!("network gave {}×{}, expected {}×{}", out.width, out.height, padded.width * s, padded.height * s)));
        }
        if padded.width != crop.width || padded.height != crop.height {
            out = out.crop(0, 0, crop.width * s, crop.height * s);
        }
        blend.add(t, &out)?;
        progress.report(i as u64 + 1, total, "");
    }
    Ok(blend.finish())
}

/// `img` grown to `w × h` by mirroring (without repeating the edge pixel, like PyTorch's
/// `reflect`; repeating it when the image is a single pixel wide).
pub fn reflect_pad(img: &Planar, w: usize, h: usize) -> Planar {
    if w <= img.width && h <= img.height {
        return img.clone();
    }
    let idx = |i: usize, n: usize| -> usize {
        if n <= 1 {
            return 0;
        }
        let period = 2 * (n - 1);
        let m = i % period;
        if m < n { m } else { period - m }
    };
    let mut data = Vec::with_capacity(w * h * img.channels);
    for c in 0..img.channels {
        let p = img.plane(c);
        for y in 0..h {
            let sy = idx(y, img.height);
            for x in 0..w {
                data.push(p.get(sy * img.width + idx(x, img.width)).copied().unwrap_or(0.0));
            }
        }
    }
    Planar { width: w, height: h, channels: img.channels, data }
}

/// Runs a future to completion on this thread (native callers of the async API).
#[cfg(not(target_family = "wasm"))]
pub fn block_on<F: std::future::Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflect_pad_matches_torch() {
        let img = Planar::new(3, 1, 1, vec![1.0, 2.0, 3.0]).unwrap();
        assert_eq!(reflect_pad(&img, 6, 2).data, vec![1.0, 2.0, 3.0, 2.0, 1.0, 2.0, 1.0, 2.0, 3.0, 2.0, 1.0, 2.0]);
        let one = Planar::new(1, 1, 1, vec![5.0]).unwrap();
        assert_eq!(reflect_pad(&one, 2, 2).data, vec![5.0; 4]);
    }
}
