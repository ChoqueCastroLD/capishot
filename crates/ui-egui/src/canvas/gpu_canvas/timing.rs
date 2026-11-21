//! GPU time of the canvas's own passes, from timestamp queries where the device has them
//! (`wgpu::Features::TIMESTAMP_QUERY`): on a real GPU the CPU time of a frame says little of
//! what the GPU spends on it (the work is queued), and a software adapter runs it on the CPU,
//! so neither measures it. A measurement is read back a frame or more later (never waited for);
//! one is in flight at a time.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use super::wgpu;

const IDLE: u8 = 0;
const IN_FLIGHT: u8 = 1;
const MAPPED: u8 = 2;

pub(super) struct GpuTimer {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Nanoseconds per timestamp tick.
    period: f32,
    state: Arc<AtomicU8>,
}

impl GpuTimer {
    /// A timer, when the device takes timestamp queries.
    pub(super) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let set = device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("pc_gpu_timer"), ty: wgpu::QueryType::Timestamp, count: 2 });
        let buffer = |label: &str, usage: wgpu::BufferUsages| device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: 16, usage, mapped_at_creation: false });
        Some(Self {
            set,
            resolve: buffer("pc_gpu_timer_resolve", wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC),
            read: buffer("pc_gpu_timer_read", wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST),
            period: queue.get_timestamp_period(),
            state: Arc::new(AtomicU8::new(IDLE)),
        })
    }

    /// Whether a measurement can start (none in flight).
    pub(super) fn free(&self) -> bool {
        self.state.load(Ordering::Acquire) == IDLE
    }

    /// The timestamp writes of a pass: the start of the measured work at `begin`, its end at
    /// `end`.
    pub(super) fn writes(&self, begin: bool, end: bool) -> Option<wgpu::RenderPassTimestampWrites<'_>> {
        (begin || end).then(|| wgpu::RenderPassTimestampWrites {
            query_set: &self.set,
            beginning_of_pass_write_index: begin.then_some(0),
            end_of_pass_write_index: end.then_some(1),
        })
    }

    /// Record the read-back of the two timestamps into `enc` (after the measured passes).
    pub(super) fn resolve(&self, enc: &mut wgpu::CommandEncoder) {
        enc.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
    }

    /// After the submission holding [`GpuTimer::resolve`]: map the result when the GPU is done.
    pub(super) fn submitted(&self) {
        self.state.store(IN_FLIGHT, Ordering::Release);
        let state = self.state.clone();
        self.read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            state.store(if r.is_ok() { MAPPED } else { IDLE }, Ordering::Release);
        });
    }

    /// The last measurement in milliseconds, once it has come back.
    pub(super) fn take(&self) -> Option<f64> {
        if self.state.load(Ordering::Acquire) != MAPPED {
            return None;
        }
        let ms = self.read.slice(..).get_mapped_range().ok().map(|view| {
            let t = |i: usize| view.get(i * 8..i * 8 + 8).and_then(|b| <[u8; 8]>::try_from(b).ok()).map(u64::from_le_bytes).unwrap_or(0);
            t(1).saturating_sub(t(0)) as f64 * f64::from(self.period) / 1e6
        });
        self.read.unmap();
        self.state.store(IDLE, Ordering::Release);
        ms
    }
}
