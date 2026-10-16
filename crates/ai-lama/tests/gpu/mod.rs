//! A wgpu device whose workgroup memory is capped, as on a browser's WebGPU (16 KiB guaranteed,
//! 32 KiB on most GPUs). 4 KiB too: a software GPU's 8-wide subgroups then hit the budget where a
//! 64-wide GPU hits 32 KiB (burn's matmul sizes its tiles by the subgroup width).

use photocraft_ai_lama::burn::tensor::Device;
use photocraft_ai_lama::burn::tensor::wgpu::{WgpuSetup, wgpu};

/// The capped sizes the tests run at.
pub const LIMITS: [u32; 3] = [4096, 16384, 32768];

/// The first adapter's device with at most `bytes` of workgroup memory; `None` without one.
pub fn limited(bytes: u32) -> Option<Device> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let mut limits = adapter.limits();
    limits.max_compute_workgroup_storage_size = limits.max_compute_workgroup_storage_size.min(bytes);
    let desc = wgpu::DeviceDescriptor {
        label: None,
        required_features: adapter.features().difference(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS | wgpu::Features::all_experimental_mask()),
        required_limits: limits,
        memory_hints: wgpu::MemoryHints::MemoryUsage,
        ..Default::default()
    };
    let (device, queue) = pollster::block_on(adapter.request_device(&desc)).unwrap();
    let backend = adapter.get_info().backend;
    Some(Device::wgpu_options().setup(WgpuSetup { instance, adapter, device, queue, backend }).init().unwrap())
}

/// The largest difference between two outputs.
pub fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}
