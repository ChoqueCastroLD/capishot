//! Inference with burn: the compute device and tiled execution of any [`Network`].
//!
//! Everything that reads results back is `async`: in the browser WebGPU readback can't block.
//! Native callers use [`block_on`].

use burn::tensor::{DType, Device, FloatDType, Tensor, TensorData};

use crate::tile::{Blender, TileOptions, plan, uniform};
use crate::{AiError, Planar, Progress, Result};

/// Which kind of device runs the networks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accel {
    /// wgpu: WebGPU in the browser, Vulkan/Metal/DX12 natively.
    Gpu,
    /// burn-flex on the CPU (SIMD; rayon natively).
    Cpu,
}

/// What a GPU runtime runs on, for logs and the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuInfo {
    /// The adapter's name (`NVIDIA GeForce RTX 3060 Ti`).
    pub name: String,
    /// The graphics API (`Vulkan`, `Dx12`…), when known.
    pub api: String,
    /// A software rasterizer (lavapipe, SwiftShader, WARP): no card behind it.
    pub software: bool,
}

/// A compute device. Cheap to clone (burn devices are handles).
#[derive(Clone, Debug)]
pub struct Runtime {
    device: Device,
    accel: Accel,
    /// Networks loaded on this runtime compute in half precision (f16 weights and activations).
    half: bool,
    info: Option<GpuInfo>,
}

impl Runtime {
    /// The CPU device.
    #[cfg(feature = "cpu")]
    pub fn cpu() -> Self {
        Self { device: Device::flex(), accel: Accel::Cpu, half: false, info: None }
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
        Ok(Self { device, accel: Accel::Gpu, half: false, info: None })
    }

    /// [`Runtime::gpu`], blocking (native only).
    #[cfg(all(feature = "gpu", not(target_family = "wasm")))]
    pub fn gpu_blocking() -> Result<Self> {
        Self::gpu_kind_blocking(burn::tensor::DeviceKind::DefaultDevice)
    }

    /// A specific kind of wgpu adapter, blocking (native only): `DiscreteGpu(0)`,
    /// `IntegratedGpu(0)`… Fails when there is none. The graphics API is wgpu's choice (Vulkan
    /// first) unless `PHOTOCRAFT_AI_GPU_API` (or `WGPU_BACKEND`) names `vulkan`, `dx12`, `metal`
    /// or `gl`. Networks compute in f32: with the `gpu-native` feature `PHOTOCRAFT_AI_FP16=1`
    /// asks for f16 where the adapter has shader-f16 and isn't a software rasterizer
    /// ([`Runtime::half`]). Off by default: on MangaJaNai f16 drifts from f32 in smooth
    /// gradients (about 20 dB PSNR there), which shows.
    #[cfg(all(feature = "gpu", not(target_family = "wasm")))]
    pub fn gpu_kind_blocking(kind: burn::tensor::DeviceKind) -> Result<Self> {
        let api = gpu_api_from_env();
        let mut opts = Device::wgpu_options().device_kind(kind.clone());
        if let Some(api) = api {
            opts = opts.graphics_api(api);
        }
        let (device, api_name) = match opts.init_with_setup() {
            Ok((d, setup)) => (d, format!("{:?}", setup.backend)),
            // Already brought up (by another model in this process, on wgpu's default API): share it.
            Err(burn::tensor::wgpu::WgpuInitError::Registration { .. }) if api.is_none() => (Device::wgpu(kind), String::new()),
            Err(e) => return Err(AiError::Device(e.to_string())),
        };
        let info = device.identity().map(|id| GpuInfo { name: id.name, api: api_name, software: id.physical.is_none() });
        let software = info.as_ref().is_none_or(|i| i.software);
        let half = cfg!(feature = "gpu-native") && !software && fp16_from_env().unwrap_or(false) && device.supports_dtype(DType::F16);
        Ok(Self { device, accel: Accel::Gpu, half, info })
    }

    /// A GPU device (the first adapter wgpu finds, software ones included) whose workgroup
    /// memory is capped at `workgroup_bytes`, as on a browser's WebGPU (16 KiB guaranteed, 32 KiB
    /// on most GPUs): for tests that every model runs within such limits. `Ok(None)` when there
    /// is no adapter. Native only.
    #[cfg(all(feature = "gpu", not(target_family = "wasm")))]
    pub fn gpu_limited_blocking(workgroup_bytes: u32) -> Result<Option<Self>> {
        use burn::tensor::wgpu::{WgpuSetup, wgpu};
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let Ok(adapter) = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())) else { return Ok(None) };
        let mut limits = adapter.limits();
        limits.max_compute_workgroup_storage_size = limits.max_compute_workgroup_storage_size.min(workgroup_bytes);
        let desc = wgpu::DeviceDescriptor {
            label: None,
            required_features: adapter.features().difference(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS | wgpu::Features::all_experimental_mask()),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&desc)).map_err(|e| AiError::Device(e.to_string()))?;
        let backend = adapter.get_info().backend;
        let setup = WgpuSetup { instance, adapter, device, queue, backend };
        let device = Device::wgpu_options().setup(setup).init().map_err(|e| AiError::Device(e.to_string()))?;
        Ok(Some(Self { device, accel: Accel::Gpu, half: false, info: None }))
    }

    pub fn accel(&self) -> Accel {
        self.accel
    }

    /// Whether networks loaded on this runtime compute in f16.
    pub fn half(&self) -> bool {
        self.half
    }

    /// This runtime with f16 compute on or off (on only where the device supports f16).
    pub fn with_half(mut self, half: bool) -> Self {
        self.half = half && self.accel == Accel::Gpu && self.device.supports_dtype(DType::F16);
        self
    }

    /// The float type networks loaded on this runtime compute in.
    pub fn float_dtype(&self) -> FloatDType {
        if self.half { FloatDType::F16 } else { FloatDType::F32 }
    }

    /// The adapter, for a GPU runtime brought up natively.
    pub fn info(&self) -> Option<&GpuInfo> {
        self.info.as_ref()
    }

    /// A network parameter: [`Runtime::tensor`] in [`Runtime::float_dtype`].
    pub fn param<const D: usize>(&self, data: Vec<f32>, shape: [usize; D]) -> Result<Tensor<D>> {
        Ok(self.tensor(data, shape)?.cast(self.float_dtype()))
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
        let data = t.cast(FloatDType::F32).into_data_async().await.map_err(|e| AiError::Inference(format!("{e:?}")))?;
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
    /// The float type `forward` takes and computes in (its weights' type).
    fn float_dtype(&self) -> FloatDType {
        FloatDType::F32
    }
    fn forward(&self, x: Tensor<4>) -> Tensor<4>;
}

/// Tiles no smaller than this are split in four when they fail (out of device memory); a
/// smaller one failing fails the run.
const MIN_SPLIT: usize = 64;

/// Runs `net` over `input` tile by tile ([`crate::tile`]), reporting each tile to `progress`
/// and stopping with [`AiError::Cancelled`] between tiles once cancelled. One-channel input is
/// repeated to the network's channel count.
///
/// One tile is queued ahead: tile `i + 1` is submitted before tile `i` is read back, so the
/// device never waits on the readback and blending. A tile that fails (the device ran out of
/// memory) is run again as four smaller ones, and the run goes on.
pub async fn run_tiled(rt: &Runtime, net: &dyn Network, input: &Planar, opts: TileOptions, progress: &dyn Progress) -> Result<Planar> {
    let input = if input.channels == net.in_channels() {
        std::borrow::Cow::Borrowed(input)
    } else if input.channels == 1 {
        std::borrow::Cow::Owned(input.with_channels(net.in_channels()))
    } else {
        return Err(AiError::Image(format!("{} channels for a {}-channel network", input.channels, net.in_channels())));
    };
    run_plan(rt, net, &input, opts, progress, true).await
}

/// [`run_tiled`] on an input that already has the network's channels. `report`: whether tiles
/// count towards `progress` (not for the pieces of a split tile).
fn run_plan<'a>(
    rt: &'a Runtime,
    net: &'a dyn Network,
    input: &'a Planar,
    opts: TileOptions,
    progress: &'a dyn Progress,
    report: bool,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Planar>> + 'a>> {
    Box::pin(async move {
        let s = net.scale();
        let p = plan(input.width, input.height, opts)?;
        // With autotune (the native GPU path) every new input shape is benchmarked again: one
        // shape for the whole image instead of up to nine.
        let p = if cfg!(feature = "gpu-native") && rt.accel() == Accel::Gpu { uniform(&p, input.width, input.height) } else { p };
        let mut blend = Blender::new(input.width, input.height, net.out_channels(), s, &p)?;
        let total = p.tiles.len() as u64;
        if report {
            progress.report(0, total, "");
        }
        // The tile submitted last, not read yet: its index, input crop and output on the device.
        let mut queued: Option<(usize, Planar, Result<Tensor<4>>)> = None;
        for i in 0..=p.tiles.len() {
            let next = match p.tiles.get(i) {
                Some(t) => {
                    if progress.cancelled() {
                        return Err(AiError::Cancelled);
                    }
                    let crop = input.crop(t.x0, t.y0, t.width(), t.height());
                    let y = submit(rt, net, &crop);
                    Some((i, crop, y))
                }
                None => None,
            };
            if let Some((j, crop, y)) = std::mem::replace(&mut queued, next) {
                let out = match y {
                    Ok(y) => finish(rt, net, &crop, y).await,
                    Err(e) => Err(e),
                };
                let out = match out {
                    Ok(o) => o,
                    Err(AiError::Cancelled) => return Err(AiError::Cancelled),
                    Err(e) if crop.width.max(crop.height) > MIN_SPLIT => {
                        // Out of memory, most likely: free what the pool holds, run it in quarters.
                        rt.device.memory_cleanup();
                        let half = crop.width.max(crop.height).div_ceil(2);
                        let sub = TileOptions { tile: half, overlap: opts.overlap.min(half / 2) };
                        if report {
                            progress.report(j as u64, total, &format!("a tile failed on the device ({e}): splitting it"));
                        }
                        run_plan(rt, net, &crop, sub, progress, false).await?
                    }
                    Err(e) => return Err(e),
                };
                if let Some(t) = p.tiles.get(j) {
                    blend.add(t, &out)?;
                }
                if report {
                    progress.report(j as u64 + 1, total, "");
                }
            }
        }
        Ok(blend.finish())
    })
}

/// Queues `crop` (reflect-padded to the network's multiple) through `net`. Panics inside the
/// backend (a failed allocation) come back as errors.
fn submit(rt: &Runtime, net: &dyn Network, crop: &Planar) -> Result<Tensor<4>> {
    let m = net.multiple_of().max(1);
    let padded = reflect_pad(crop, crop.width.div_ceil(m) * m, crop.height.div_ceil(m) * m);
    let x = rt.image(&padded)?.cast(net.float_dtype());
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| net.forward(x))).map_err(|_| AiError::Inference("the GPU failed running a tile".into()))
}

/// Reads a tile's output back and crops off the padding [`submit`] added.
async fn finish(rt: &Runtime, net: &dyn Network, crop: &Planar, y: Tensor<4>) -> Result<Planar> {
    let s = net.scale();
    let m = net.multiple_of().max(1);
    let (pw, ph) = (crop.width.div_ceil(m) * m, crop.height.div_ceil(m) * m);
    let mut out = rt.read(y).await?;
    if out.width != pw * s || out.height != ph * s {
        return Err(AiError::Inference(format!("network gave {}×{}, expected {}×{}", out.width, out.height, pw * s, ph * s)));
    }
    if pw != crop.width || ph != crop.height {
        out = out.crop(0, 0, crop.width * s, crop.height * s);
    }
    Ok(out)
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

/// The graphics API `PHOTOCRAFT_AI_GPU_API` (else `WGPU_BACKEND`) names, if any: `vulkan`,
/// `dx12`, `metal` or `gl` (`auto`, unset or anything else: wgpu's choice).
#[cfg(all(feature = "gpu", not(target_family = "wasm")))]
fn gpu_api_from_env() -> Option<burn::tensor::wgpu::WgpuBackend> {
    use burn::tensor::wgpu::WgpuBackend;
    let v = std::env::var("PHOTOCRAFT_AI_GPU_API").or_else(|_| std::env::var("WGPU_BACKEND")).ok()?;
    match v.trim().to_ascii_lowercase().as_str() {
        "vulkan" | "vk" => Some(WgpuBackend::Vulkan),
        "dx12" | "d3d12" => Some(WgpuBackend::Dx12),
        "metal" | "mtl" => Some(WgpuBackend::Metal),
        "gl" | "opengl" => Some(WgpuBackend::Gl),
        _ => None,
    }
}

/// `PHOTOCRAFT_AI_FP16`: `1`/`on` asks for f16 on the GPU, `0`/`off` keeps f32; unset: `None`.
#[cfg(all(feature = "gpu", not(target_family = "wasm")))]
fn fp16_from_env() -> Option<bool> {
    match std::env::var("PHOTOCRAFT_AI_FP16").ok()?.trim().to_ascii_lowercase().as_str() {
        "0" | "off" | "false" | "no" => Some(false),
        "1" | "on" | "true" | "yes" => Some(true),
        _ => None,
    }
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
