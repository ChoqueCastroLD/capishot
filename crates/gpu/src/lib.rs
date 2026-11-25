//! GPU compositor (architecture §7.2, milestone M5).
//!
//! Renders a [`Document`] with wgpu, producing the same straight-alpha composite as the CPU
//! reference (`photocraft-compose`) within ~1/255:
//!
//! - **Residency.** Layer and mask surfaces live on the GPU in square *pages* (up to [`PAGE`]²,
//!   never above the device's texture limit) covering their allocated tiles. A page is uploaded
//!   only when a chunk inside it is drawn, and surfaces are copy-on-write `Arc` tiles, so a tile
//!   is re-uploaded only when its `Arc` changed (a brush stroke uploads just the touched 256²
//!   tiles; undo swaps pointers back and uploads only what differs). RGBA8 tiles upload with
//!   zero conversion. Pages over a byte budget are evicted least recently used first.
//! - **Planner** ([`plan`]). The layer tree becomes a linear list of passes over abstract
//!   chunk-sized buffers (blend with every Photoshop mode, opacity × fill, masks, clipping groups,
//!   pass-through vs isolated groups, adjustments, solid / gradient fills, layer effects).
//! - **Layer effects** (`fx.rs`). Each effect layer's maps (shadow, glow, satin, bevel, stroke
//!   bands) are built by fragment passes over its effect region and cached on the GPU per layer
//!   state; a brush stroke rebuilds only the damaged tiles grown by the effect reach. A region
//!   larger than the texture limit is built per page cell, over the cell grown by the effects'
//!   reach, so every map is exact inside its cell.
//! - **Execution.** The canvas is processed page cell by page cell, each in chunks (1024²
//!   RGBA32F accumulators, reused) that bind only that cell's pages, and each finished chunk is
//!   handed to a caller-supplied sink, e.g. to encode it straight into a display texture — no
//!   readback. Documents of any size composite on the GPU; a huge refresh submits as it goes, so
//!   staged uploads and evicted pages stay bounded.
//! - **Stack reuse.** A complete root stack unit can checkpoint its backdrop into bounded chunk
//!   textures. Exact COW snapshots invalidate the lower prefix; edits above it restore those
//!   pixels and execute only the remaining passes. Groups and clipping units stay indivisible.
//!
//! Vector masks (rasterised once per mask state into a combined mask texture), layers clipped to
//! pass-through groups, stroked shapes with clipped layers (fill and stroke split once per shape
//! state), pattern fills and artboards are planned like everything else. What remains
//! (Multichannel documents, patterns larger than the texture limit, uncached gradients with
//! coincident colour or opacity stops) returns [`Unsupported`];
//! callers fall back to the CPU compositor.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod bounds;
mod camera_raw;
mod fx;
pub mod health;
pub mod lut;
pub mod plan;
mod prefix;

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Arc;

use photocraft_color::{PixelFormat, SampleType};
use photocraft_compose::effects::FieldKind;
use photocraft_doc::{DocId, Document, Layer, LayerContent, LayerId, Pattern};
use photocraft_geom::{Rect, TILE_SIZE, TileCoord};
use photocraft_raster::{Surface, Tile};

pub use camera_raw::{CAMERA_RAW_SHADER, CameraRawDevelop};
pub use health::{DeviceHealth, Fault};
pub use plan::{Kernel, Plan, Role, Unsupported, plan};

/// Accumulator format for intermediate buffers. Full float: discontinuous operations
/// (Posterize, Threshold, Hard Mix, Dissolve) must land on the same side of their thresholds as
/// the CPU reference, and Color Burn/Dodge amplify input error.
pub const ACC_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba32Float; // default accumulation format; `preferred_acc_format` downgrades to Rgba16Float where 32-bit float isn't renderable
/// Effect intermediates (distances, blurs) and layer shapes.
const MAP32: wgpu::TextureFormat = wgpu::TextureFormat::R32Float;
/// Final effect coverage maps (0..1; half precision is ~1/4000).
const MAP16: wgpu::TextureFormat = wgpu::TextureFormat::R16Float;
/// Side of the square chunks the canvas is processed in.
pub const CHUNK: u32 = 1024;
const STRIDE: u64 = 256;
const CHUNK_UNIFORM: u64 = 32;
/// Coarsest level of detail [`Compositor::render_lod`] composites at (1/16 of the side).
pub const MAX_LOD: u32 = 4;
const OP_UNIFORM: u64 = 176;
/// The member records of a [`Kernel::Layers`] pass (`array<Op, 4>`) and their uniform stride.
const MEMBERS_UNIFORM: u64 = OP_UNIFORM * plan::MAX_MEMBERS as u64;
const MEMBERS_STRIDE: u64 = MEMBERS_UNIFORM.next_multiple_of(STRIDE);
/// Texture bindings of [`Kernel::Layers`]: the backdrop, then a layer and its mask per member.
const LAYERS_BINDINGS: [u32; 9] = [0, 2, 3, 8, 9, 10, 11, 12, 13];
/// GPU memory the effect-map cache may hold before evicting layers not drawn this frame.
pub const FX_BUDGET: usize = 1536 << 20;
/// Largest side of the square pages layer surfaces are stored in on the GPU (smaller when the
/// device's texture limit is). A chunk only binds the pages of the cell it lies in, so layers of
/// any size composite on the GPU.
pub const PAGE: u32 = 2048;
/// GPU memory resident layer pages may hold before the least recently used are evicted.
pub const RESIDENT_BUDGET: u64 = 4 << 30;
/// Uploads (bytes) [`Compositor::render`] stages before submitting the work recorded so far, so
/// a refresh of a huge document never holds all its uploads in staging memory at once.
const FLUSH_BYTES: u64 = 512 << 20;
/// Pass pixels (chunk pixels × passes) [`Compositor::render`] records before submitting: one
/// command buffer holding a whole huge refresh can run for seconds, long enough for the OS to
/// reset the GPU.
const FLUSH_WORK: u64 = 1 << 30;

/// A finished chunk: straight-alpha RGBA32F pixels of `rect` (document coordinates, divided by
/// 2^`level` for a reduced render) at the texture's origin.
pub struct ChunkOut<'a> {
    pub rect: Rect,
    /// Where `rect`'s first pixel sits in the texture (the chunk may have been rendered larger,
    /// to fill the lower-stack cache).
    pub origin: [u32; 2],
    /// Level of detail: 0 full size, `n` one pixel per 2ⁿ × 2ⁿ document pixels (the display
    /// texture's mip level `n`).
    pub level: u32,
    pub texture: &'a wgpu::Texture,
    pub view: &'a wgpu::TextureView,
}

/// What a render did (for profiling and tests).
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub passes: usize,
    pub chunks: usize,
    pub slots: u32,
    pub tiles_uploaded: usize,
    pub bytes_uploaded: usize,
    /// Effect layers whose shape was (re)rendered, fully or partially.
    pub fx_shapes: usize,
    /// Effect map programs run (one per effect whose maps were rebuilt).
    pub fx_programs: usize,
    /// Effect-map pixels written (all stages).
    pub fx_pixels: u64,
    /// Page cells the region spans (each binds only the layer pages it overlaps).
    pub cells: usize,
    /// Layer pages and paged effect maps evicted to stay within the memory budgets.
    pub evicted: usize,
    /// Intermediate submits of a huge refresh (to bound staging and evicted memory).
    pub flushes: usize,
    /// Chunks restored from an unchanged lower-stack composite.
    pub prefix_hits: usize,
    /// Planned prefix passes skipped, summed over the reused chunks.
    pub prefix_passes_skipped: usize,
    /// Level of detail rendered at (see [`ChunkOut::level`]).
    pub level: u32,
    /// Output pixels composited (at the level of detail).
    pub pixels: u64,
    /// Passes given layer pages and bind groups, summed over the page cells: those that draw in
    /// the chunks recorded there.
    pub bound: usize,
}

/// `r` (document pixels) at level of detail `level`: rounded out to whole reduced pixels.
pub fn level_rect(r: Rect, level: u32) -> Rect {
    if level == 0 || r.is_empty() {
        return r;
    }
    let s = 1i32 << level.min(MAX_LOD);
    Rect::new(r.x0.div_euclid(s), r.y0.div_euclid(s), r.x1.saturating_add(s - 1).div_euclid(s), r.y1.saturating_add(s - 1).div_euclid(s))
}

/// `r` (pixels at level of detail `level`) in document pixels: what its reduced pixels cover.
fn unlevel_rect(r: Rect, level: u32) -> Rect {
    if level == 0 || r.is_empty() {
        return r;
    }
    let s = level.min(MAX_LOD);
    Rect::new(r.x0 << s, r.y0 << s, r.x1.saturating_mul(1 << s), r.y1.saturating_mul(1 << s))
}

/// The pixels of a `canvas` document at level of detail `level` (the size of mip level `level`).
pub fn level_bounds(canvas: Rect, level: u32) -> Rect {
    let l = level.min(MAX_LOD);
    Rect::new(0, 0, (canvas.width() >> l).max(1) as i32, (canvas.height() >> l).max(1) as i32)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TexKind {
    /// RGBA8 tiles copied verbatim.
    Rgba8Direct,
    /// Other 8-bit formats converted to RGBA8.
    Rgba8,
    /// 16/32-bit formats converted to RGBA16F.
    Rgba16F,
    /// GRAY8 masks copied verbatim.
    R8Direct,
    /// Other masks as R32F.
    R32F,
}

impl TexKind {
    fn for_surface(role: Role, f: PixelFormat) -> Self {
        match role {
            Role::Content | Role::Stroke if f == PixelFormat::RGBA8 => TexKind::Rgba8Direct,
            Role::Content | Role::Stroke if f.sample == SampleType::U8 => TexKind::Rgba8,
            Role::Content | Role::Stroke => TexKind::Rgba16F,
            Role::Mask if f == PixelFormat::GRAY8 => TexKind::R8Direct,
            Role::Mask => TexKind::R32F,
        }
    }
    fn format(self) -> wgpu::TextureFormat {
        match self {
            TexKind::Rgba8Direct | TexKind::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
            TexKind::Rgba16F => wgpu::TextureFormat::Rgba16Float,
            TexKind::R8Direct => wgpu::TextureFormat::R8Unorm,
            TexKind::R32F => wgpu::TextureFormat::R32Float,
        }
    }
    fn bytes_per_pixel(self) -> usize {
        match self {
            TexKind::Rgba8Direct | TexKind::Rgba8 => 4,
            TexKind::Rgba16F => 8,
            TexKind::R8Direct => 1,
            TexKind::R32F => 4,
        }
    }
}

/// Index into the render's resident key list, and the texture's region (x, y, w, h).
type ResidentRef = (usize, [i32; 4]);
/// A page cell: the square `[x·page, (x+1)·page) × [y·page, (y+1)·page)` of document pixels.
type Cell = (i32, i32);
/// A resident layer page.
type ResKey = (LayerId, Role, Cell);
/// Effect maps of a layer: over its whole effect region (`None`), or one page cell of a region
/// larger than the texture limit; at a level of detail (one map pixel per 2^level × 2^level
/// document pixels, [`Compositor::map_level`]; page cells only at full size).
type FxKey = (LayerId, Option<Cell>, u32);

struct Resident {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Document-pixel rect covered (tile aligned, inside one page cell).
    region: Rect,
    kind: TexKind,
    format: PixelFormat,
    tiles: HashMap<TileCoord, Arc<Tile>>,
    default_nonzero: bool,
    /// Bit pattern of the surface's default pixel when the texture was filled. Tiles the surface
    /// doesn't store hold it, so a changed default (Invert, Fill) makes the texture stale.
    default_bits: Vec<u32>,
    doc: DocId,
    last_used: u64,
    /// Cell visit that last used it (least recently used pages are evicted first).
    stamp: u64,
    /// CMYK profile the texels were converted with (`CmykSpace::id`, 0 = built-in / not CMYK).
    cmyk: u64,
}

impl Resident {
    fn bytes(&self) -> u64 {
        self.region.width() as u64 * self.region.height() as u64 * self.kind.bytes_per_pixel() as u64
    }
}

/// A texture and its default view.
#[derive(Clone)]
struct Tex {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Tex {
    fn new(device: &wgpu::Device, label: &str, w: u32, h: u32, format: wgpu::TextureFormat, usage: wgpu::TextureUsages) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Tex { texture, view }
    }
    fn map(device: &wgpu::Device, label: &str, r: Rect, format: wgpu::TextureFormat) -> Self {
        Tex::new(
            device,
            label,
            r.width(),
            r.height(),
            format,
            // Copied from too: an effect region that grows keeps the maps it shares ([`FxEntry::resize`]).
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
        )
    }
    /// Upload R32F values (row-major over `r`) into this region-space texture at `r`.
    fn write_r32(&self, queue: &wgpu::Queue, region: Rect, r: Rect, v: &[f32]) {
        if r.is_empty() {
            return;
        }
        FX_WRITTEN.fetch_add(std::mem::size_of_val(v) as u64, std::sync::atomic::Ordering::Relaxed);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: (r.x0 - region.x0) as u32, y: (r.y0 - region.y0) as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            // (R32Float texels are little-endian f32, as every target's.)
            bytemuck::cast_slice(v),
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(r.width() * 4), rows_per_image: Some(r.height()) },
            wgpu::Extent3d { width: r.width(), height: r.height(), depth_or_array_layers: 1 },
        );
    }
    fn bytes(&self) -> usize {
        let s = self.texture.size();
        let bpp = match self.texture.format() {
            wgpu::TextureFormat::R16Float => 2,
            wgpu::TextureFormat::Rgba32Float => 16,
            wgpu::TextureFormat::Rgba16Float => 8,
            _ => 4,
        };
        s.width as usize * s.height as usize * bpp
    }
}

/// Cached maps of one effect (one enabled item of a layer).
struct ProgState {
    key: u64,
    maps: Vec<Option<Tex>>,
}

/// Cached effect state of one layer.
struct FxEntry {
    doc: DocId,
    /// In the pixels of `level` (the key's map level).
    region: Rect,
    level: u32,
    shape_key: u64,
    /// Tiles the shape was rendered from (content, mask): damage is their difference.
    tiles: [Option<fx::Tiles>; 2],
    /// Groups: a clone pinning the tile pointers `shape_key` hashes.
    _pin: Option<Layer>,
    /// The shape on the CPU (distance fields are computed from it) and on the GPU.
    shape_cpu: Vec<f32>,
    shape: Tex,
    /// Distance fields: the reach each is exact to, and its texture.
    fields: HashMap<FieldKind, (i32, Tex)>,
    progs: Vec<ProgState>,
    last_used: u64,
    stamp: u64,
    /// The CPU shape and the distance fields were let go to fit the effect budget
    /// ([`Compositor::evict`]): the maps still draw; a change other than a move builds them
    /// again. `shape_hash` stands in for the shape to tell a move.
    compact: bool,
    shape_hash: u64,
    /// What its shape was made from while the layer was drawn translated (a Move drag,
    /// [`Compositor::translated_from`]); `None` when it wasn't.
    moved_from: Option<(u64, (i32, i32))>,
}

/// A layer's effect shape and distance fields over `region` computed ahead of
/// [`Compositor::sync_fx`] (by [`Compositor::stage_fx`], in bands over several calls, each band
/// written to its texture as it is computed), which then builds the layer's maps from them
/// instead of computing and uploading them in one go.
struct Staged {
    key: FxKey,
    region: Rect,
    /// Region-sized, row-major.
    shape: Vec<f32>,
    /// The shape on the GPU (`pc_fx_shape`).
    shape_tex: Option<Tex>,
    /// Each field read, with the reach it is exact to, on the GPU (`pc_fx_field`).
    fields: HashMap<FieldKind, (i32, Tex)>,
}

impl FxEntry {
    /// Compute the shape over `damage` (or take the one `ready` staged for the whole region) into
    /// the CPU copy and the texture; the bytes uploaded.
    fn update_shape(&mut self, queue: &wgpu::Queue, doc: &Document, layer: &Layer, damage: Rect, ready: Option<&mut Staged>) -> u64 {
        let v = match ready {
            Some(b) => std::mem::take(&mut b.shape),
            None => fx::shape_at(doc, layer, damage, self.level),
        };
        fx::paste(&mut self.shape_cpu, self.region, damage, &v);
        self.shape.write_r32(queue, self.region, damage, &v);
        v.len() as u64 * 4
    }
}

impl Staged {
    /// The shape, on the GPU and the CPU, taken (once).
    fn take_shape(&mut self) -> Option<(Tex, Vec<f32>)> {
        self.shape_tex.take().map(|t| (t, std::mem::take(&mut self.shape)))
    }

    /// The field of `kind` exact to `reach`, taken.
    fn take_field(&mut self, kind: FieldKind, reach: i32) -> Option<Tex> {
        if self.fields.get(&kind).is_some_and(|(r, _)| *r == reach) { self.fields.remove(&kind).map(|(_, t)| t) } else { None }
    }
}

/// A new effect entry's shape over `region`, on the GPU and the CPU: the one `ready` staged
/// (taken, and `true`), else blank.
fn fresh_shape(device: &wgpu::Device, region: Rect, ready: Option<&mut Staged>) -> (Tex, Vec<f32>, bool) {
    match ready.and_then(Staged::take_shape) {
        Some((t, v)) => (t, v, true),
        None => (Tex::map(device, "pc_fx_shape", region, MAP32), vec![0.0; region.width() as usize * region.height() as usize], false),
    }
}

/// Where [`Compositor::stage_fx`] got.
enum Stage {
    Ready(Box<Staged>),
    /// Not done: goes on in the next call.
    Pending,
    /// Built in one go instead (see [`Compositor::stage_fx`]).
    Whole,
}

/// A [`Staged`] build in progress: what it is of (it starts over when that changes) and how far it got.
struct Staging {
    built: Staged,
    doc: DocId,
    shape_key: u64,
    /// [`surface_fingerprint`]s of the layer's content and mask (0: none).
    sources: [u64; 2],
    /// The shape is computed above this row.
    shape_y: i32,
    /// The fields to compute, and the one in progress (`field`), computed above `field_y`.
    kinds: Vec<(FieldKind, i32)>,
    field: usize,
    field_y: i32,
    /// The texture of the field in progress.
    field_tex: Option<Tex>,
}

/// About how many pixels of a layer's shape or of one distance field [`Compositor::stage_fx`]
/// computes before it asks whether there is time for more.
const STAGE_PX: u64 = 128 * 1024;

/// The layer page bytes [`Compositor::prepare`] uploads in a call before it asks whether there
/// is time for more (a few milliseconds of conversion and copying).
const PAGES_AT_ONCE: u64 = 4 << 20;

/// The map programs of `f`'s enabled effects. Filled shapes stroke their outline (the effect
/// shape); others estimate it.
/// The map programs of `f`'s effects at map level `level` ([`fx::at_level`]; one it doesn't
/// scale is left out, which [`Compositor::map_level`] rules out).
fn fx_programs(doc: &Document, f: &plan::FxLayer<'_>, level: u32) -> Vec<fx::MapProgram> {
    let layer = f.layer;
    let vector_shape = matches!(layer.content, LayerContent::Shape(_)) && photocraft_compose::effect_outline(layer).is_none();
    let anchor = layer.effects.reference.unwrap_or((f64::from(f.bounds.x0), f64::from(f.bounds.y0)));
    layer
        .effects
        .items
        .iter()
        .filter(|e| e.enabled())
        .filter_map(|e| fx::at_level(e, level))
        .map(|e| fx::program_with(&e, &doc.global_light, vector_shape, &doc.patterns, anchor, level))
        .collect()
}

impl FxEntry {
    /// Move to `region` (the layer's pixels grew or shrank: type, each keystroke), keeping
    /// everything the old and new regions share: the shape, the distance fields and the maps are
    /// copied into textures of the new size. Returns the kept part; the rest of `region` is left
    /// for the caller to compute.
    fn resize(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, region: Rect) -> Rect {
        let (old, kept) = (self.region, self.region.intersect(&region));
        // Its own submission: the shape and fields are then written through the queue, which
        // lands before anything recorded in the frame's encoder; a copy recorded there would
        // overwrite what was written over the kept part.
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_fx_keep") });
        let mut moved = |from: &Tex, label: &str, format: wgpu::TextureFormat| {
            let to = Tex::map(device, label, region, format);
            if !kept.is_empty() {
                encoder.copy_texture_to_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &from.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: (kept.x0 - old.x0) as u32, y: (kept.y0 - old.y0) as u32, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyTextureInfo {
                        texture: &to.texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d { x: (kept.x0 - region.x0) as u32, y: (kept.y0 - region.y0) as u32, z: 0 },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::Extent3d { width: kept.width(), height: kept.height(), depth_or_array_layers: 1 },
                );
            }
            to
        };
        self.shape = moved(&self.shape, "pc_fx_shape", MAP32);
        for (_, t) in self.fields.values_mut() {
            *t = moved(t, "pc_fx_field", MAP32);
        }
        for p in &mut self.progs {
            for m in p.maps.iter_mut().flatten() {
                *m = moved(m, "pc_fx_map", MAP16);
            }
        }
        queue.submit([encoder.finish()]);
        let (w, ow) = (region.width() as usize, old.width() as usize);
        let mut shape = vec![0.0; w * region.height() as usize];
        if !kept.is_empty() {
            let n = kept.width() as usize;
            for y in kept.y0..kept.y1 {
                let from = (y - old.y0) as usize * ow + (kept.x0 - old.x0) as usize;
                let to = (y - region.y0) as usize * w + (kept.x0 - region.x0) as usize;
                if let (Some(src), Some(dst)) = (self.shape_cpu.get(from..from + n), shape.get_mut(to..to + n)) {
                    dst.copy_from_slice(src);
                }
            }
        }
        self.shape_cpu = shape;
        self.region = region;
        kept
    }

    /// Whether [`Self::grow_to`] can keep this entry's maps for the shape sources `now`: it
    /// has its CPU shape and distance fields, and the same sources as when it was made.
    fn can_grow(&self, now: [Option<&photocraft_raster::Surface>; 2]) -> bool {
        !self.compact && self.tiles.iter().zip(now).all(|(o, c)| o.is_some() == c.is_some())
    }

    /// [`Self::resize`] to `region`, and what is left to compute there: the pixels of the shape
    /// sources `now` (content, mask) that changed since the maps were made, and what the new
    /// region adds around the part kept.
    fn grow_to(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, layer: &Layer, now: [Option<&photocraft_raster::Surface>; 2], region: Rect) -> Rect {
        let kept = self.resize(device, queue, region);
        let mut damage = Rect::EMPTY;
        for (old, cur) in self.tiles.iter().zip(now) {
            if let (Some(o), Some(s)) = (old, cur) {
                damage = damage.union(&level_rect(fx::source_damage(layer, o, s), self.level));
            }
        }
        for r in [
            Rect::new(region.x0, region.y0, region.x1, kept.y0),
            Rect::new(region.x0, kept.y1, region.x1, region.y1),
            Rect::new(region.x0, kept.y0, kept.x0, kept.y1),
            Rect::new(kept.x1, kept.y0, region.x1, kept.y1),
        ] {
            if r.x0 < r.x1 && r.y0 < r.y1 {
                damage = damage.union(&r);
            }
        }
        damage.intersect(&region)
    }

    /// Let go of what only building the maps again needs (the CPU shape, the distance fields),
    /// keeping what draws (the shape texture and the maps). Returns the bytes freed.
    fn compact(&mut self) -> usize {
        if self.compact {
            return 0;
        }
        let before = self.bytes();
        self.shape_hash = hash_shape(&self.shape_cpu);
        self.shape_cpu = Vec::new();
        self.fields.clear();
        self.compact = true;
        before.saturating_sub(self.bytes())
    }

    fn bytes(&self) -> usize {
        self.shape.bytes()
            + self.shape_cpu.len() * 4
            + self.fields.values().map(|(_, t)| t.bytes()).sum::<usize>()
            + self.progs.iter().flat_map(|p| p.maps.iter().flatten()).map(Tex::bytes).sum::<usize>()
    }
}

/// Pipelines and layouts, shared by cheap clones (wgpu handles are reference counted).
#[derive(Clone)]
struct Kit {
    /// Built the first time a render needs them ([`Kit::pipeline`]), shared by the clones.
    pipelines: Arc<std::sync::Mutex<HashMap<(Kernel, wgpu::TextureFormat), wgpu::RenderPipeline>>>,
    module: wgpu::ShaderModule,
    layout: wgpu::PipelineLayout,
    /// [`Kernel::Layers`]'s.
    layout_m: wgpu::PipelineLayout,
    bgl0: wgpu::BindGroupLayout,
    bgl1: wgpu::BindGroupLayout,
    /// [`Kernel::Layers`]: uniforms (chunk, op, member ops) and textures ([`LAYERS_BINDINGS`]).
    bgl0m: wgpu::BindGroupLayout,
    bgl1m: wgpu::BindGroupLayout,
    dummy: wgpu::TextureView,
}

/// Builds a compositor's pipelines ahead ([`Compositor::prebuild`]): they land in the cache the
/// compositor reads, so its renders find them built.
pub struct Prebuild {
    kit: Kit,
    acc_format: wgpu::TextureFormat,
}

impl Prebuild {
    /// Build every kernel's pipeline not built yet: the compositing kernels for the accumulation
    /// format, the effect-map kernels for both map formats. One a driver can't build is left for
    /// the render that needs it (which then falls back as before). Returns how many are built.
    pub fn run(&self, device: &wgpu::Device) -> usize {
        let mut built = 0;
        for k in Kernel::DRAWN {
            let formats: &[wgpu::TextureFormat] = if k.is_map() { &[MAP32, MAP16] } else { std::slice::from_ref(&self.acc_format) };
            for &f in formats {
                built += usize::from(self.kit.pipeline(device, k, f).is_ok());
            }
        }
        built
    }
}

/// One draw of an effect-map kernel (outside the chunk loop).
struct MapDraw {
    kernel: Kernel,
    format: wgpu::TextureFormat,
    target: wgpu::TextureView,
    /// Target-local scissor (x, y, w, h).
    scissor: [u32; 4],
    /// Chunk record: origin and size of the target's space.
    space: [i32; 4],
    op: Vec<u8>,
    views: [Option<wgpu::TextureView>; 8],
    lut: Option<Arc<Vec<f32>>>,
}

impl Kit {
    /// The pipeline of kernel `k` drawing to `format`, built the first time a render needs it:
    /// building every kernel's (about fifty) when the compositor was made held the page for most
    /// of a second on a phone's first document (on WebGL each is a shader translation and a
    /// synchronous link), while a page uses a handful. A driver that can't build one is an
    /// `Err`, and the render falls back to the CPU compositor as before.
    fn pipeline(&self, device: &wgpu::Device, k: Kernel, format: wgpu::TextureFormat) -> Result<wgpu::RenderPipeline, Unsupported> {
        if let Some(p) = self.pipelines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(k, format)) {
            return Ok(p.clone());
        }
        let entry = k.entry().ok_or_else(|| Unsupported(format!("kernel {k:?} draws nothing")))?;
        let (p, error) = first_error(device, || {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(if k == Kernel::Layers { &self.layout_m } else { &self.layout }),
                vertex: wgpu::VertexState { module: &self.module, entry_point: Some("vs"), buffers: &[], compilation_options: Default::default() },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.module,
                    entry_point: Some(entry),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    compilation_options: Default::default(),
                }),
                multiview_mask: None,
                cache: None,
            })
        });
        if let Some(e) = error {
            return Err(Unsupported(format!("couldn't build the compositor's pipelines ({entry}): {e}")));
        }
        self.pipelines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((k, format), p.clone());
        Ok(p)
    }

    /// Record `draws` (each its own render pass) into `encoder`.
    fn run(&self, device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, draws: &[MapDraw]) {
        if draws.is_empty() {
            return;
        }
        let mut data = vec![0u8; draws.len() * 2 * STRIDE as usize];
        for (i, d) in draws.iter().enumerate() {
            let w = words(&[I(d.space[0]), I(d.space[1]), I(d.space[2]), I(d.space[3])]);
            data[2 * i * STRIDE as usize..][..w.len()].copy_from_slice(&w);
            data[(2 * i + 1) * STRIDE as usize..][..d.op.len()].copy_from_slice(&d.op);
        }
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_fx_uniforms"),
            size: data.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ubuf, 0, &data);
        let bg0 = uniform_group(device, &self.bgl0, &ubuf);
        for (i, d) in draws.iter().enumerate() {
            let lut = d.lut.as_ref().map(|l| {
                let mut row = [0.0f32; 4096];
                row[..l.len().min(4096)].copy_from_slice(&l[..l.len().min(4096)]);
                lut_texture(device, queue, &[row])
            });
            let mut views: Vec<&wgpu::TextureView> = d.views.iter().map(|v| v.as_ref().unwrap_or(&self.dummy)).collect();
            if let Some((_, lv)) = &lut {
                views[4] = lv;
            }
            let bg1 = texture_group(device, &self.bgl1, &views);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pc_fx_map"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &d.target,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let Ok(pipeline) = self.pipeline(device, d.kernel, d.format) else { continue };
            pass.set_pipeline(&pipeline);
            pass.set_scissor_rect(d.scissor[0], d.scissor[1], d.scissor[2], d.scissor[3]);
            pass.set_bind_group(0, &bg0, &[(2 * i as u64 * STRIDE) as u32, ((2 * i + 1) as u64 * STRIDE) as u32]);
            pass.set_bind_group(1, &bg1, &[]);
            pass.draw(0..3, 0..1);
        }
    }
}

fn uniform_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, ubuf: &wgpu::Buffer) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pc_compose_uniforms"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: ubuf, offset: 0, size: NonZeroU64::new(CHUNK_UNIFORM) }),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: ubuf, offset: 0, size: NonZeroU64::new(OP_UNIFORM) }),
            },
        ],
    })
}

fn layers_uniform_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, ubuf: &wgpu::Buffer) -> wgpu::BindGroup {
    let entry = |binding: u32, size: u64| wgpu::BindGroupEntry {
        binding,
        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: ubuf, offset: 0, size: NonZeroU64::new(size) }),
    };
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("pc_compose_layers_uniforms"),
        layout,
        entries: &[entry(0, CHUNK_UNIFORM), entry(1, OP_UNIFORM), entry(2, MEMBERS_UNIFORM)],
    })
}

fn texture_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, views: &[&wgpu::TextureView]) -> wgpu::BindGroup {
    let e: Vec<wgpu::BindGroupEntry> =
        views.iter().enumerate().map(|(b, v)| wgpu::BindGroupEntry { binding: b as u32, resource: wgpu::BindingResource::TextureView(v) }).collect();
    device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("pc_compose_pass"), layout, entries: &e })
}

/// GPU compositor. Create once per device; call [`Compositor::render`] per refresh.
pub struct Compositor {
    kit: Kit,
    /// Accumulator slots by side: a refresh smaller than a chunk (a brush dab) works in small
    /// textures, so render passes never load and store more than it needs (tiled mobile GPUs
    /// move the whole attachment through tile memory on every pass).
    pools: HashMap<u32, Vec<(wgpu::Texture, wgpu::TextureView)>>,
    residents: HashMap<ResKey, Resident>,
    fx: HashMap<FxKey, FxEntry>,
    prefix: prefix::Prefix,
    /// Effect temporaries (R32F, region sized) and the frame they were last used.
    temps: Vec<(Tex, u64)>,
    patterns: HashMap<String, (u64, Tex)>,
    /// LUT textures by content hash, and the frame that last used them.
    /// The textures of the plans' tables, by the address of their rows (held, so an address is
    /// one table for as long as its texture is kept), with the frame that last used each.
    lut_cache: HashMap<usize, (lut::Lut, (wgpu::Texture, wgpu::TextureView), u64)>,
    /// The colour mode of the document being encoded: patterns paint in it
    /// (`photocraft_compose::pattern::pattern_rgba`).
    pattern_mode: photocraft_color::ColorMode,
    /// Texture side limit the paging is planned for (the device's, or smaller in tests).
    max_dim: u32,
    /// The device's real texture side limit.
    device_max: u32,
    /// Page side (a power of two, multiple of the tile size, at most `max_dim`).
    page: u32,
    /// Chunk side (divides `page`).
    chunk: u32,
    resident_budget: u64,
    /// GPU memory the effect-map cache may hold ([`FX_BUDGET`] or less).
    fx_budget: u64,
    /// What the view shows: a full refresh draws these cells last, so their pages are the
    /// ones still resident for the edits that follow.
    focus: Option<Rect>,
    /// The level of detail the view draws at.
    focus_level: u32,
    /// Render a whole chunk the lower-stack cache lacks so later edits there reuse it (on by
    /// default; see [`Compositor::set_fill_prefix`]).
    fill_prefix: bool,
    frame: u64,
    /// Cell visits so far (LRU clock of pages).
    stamp: u64,
    /// Bytes staged by uploads since the last submit.
    staged: u64,
    /// The document area the render being encoded reads layer pixels in (its cell's part and
    /// the chunks it renders, whose level-of-detail blocks cover it): a layer page uploads only the tiles there,
    /// the rest when a render reaches them. Kernels read layer pixels at the pixel they compute
    /// (`compose.wgsl`), so nothing outside is sampled. A first refresh of a big page in pieces
    /// then uploads a piece's worth of tiles at a time instead of whole pages.
    want: Rect,
    acc_format: wgpu::TextureFormat,
    /// Whether the effect-map pipelines could be built (else documents with layer effects are
    /// [`Unsupported`] and use the CPU compositor).
    effect_maps: bool,
    /// The effect-map pipelines were built (or found impossible): on the first plan with layer
    /// effects ([`Compositor::ensure_pipelines`]).
    maps_built: bool,
    /// `CmykSpace::id` of the document being encoded (0: built-in coated CMYK).
    cmyk: u64,
    /// The device's health (see [`Compositor::set_health`]); `None` = assumed healthy.
    health: Option<DeviceHealth>,
    /// Layers drawn from an already resident surface at an offset ([`Compositor::set_translated`]).
    translated: Vec<Translated>,
    /// The effect maps [`Compositor::prepare`] built since it last found them all current.
    prepared: std::collections::HashSet<FxKey>,
    /// The layer pages [`Compositor::prepare`] uploaded since it last found them all current, by
    /// the band of the cell uploaded (its top row).
    prepared_pages: std::collections::HashSet<(ResKey, i32)>,
    /// A layer's effect shape and fields [`Compositor::stage_fx`] is computing over calls.
    staging: Option<Staging>,
}

/// A layer surface the document shows as another surface moved by a whole-pixel offset (a Move
/// drag's preview): the compositor samples the source's resident pages at the offset instead of
/// uploading the moved copy, whose tiles are all new every frame.
#[derive(Clone, Debug)]
pub struct Translated {
    pub layer: LayerId,
    /// The layer's mask rather than its content.
    pub mask: bool,
    /// [`surface_fingerprint`] of the surface the document holds (the moved copy): the
    /// substitution only applies to exactly that surface.
    pub shown: u64,
    /// The surface before the move, as it is resident.
    pub source: Surface,
    /// Where `shown` is relative to `source`: `shown(x + dx, y + dy) == source(x, y)`.
    pub offset: (i32, i32),
}

impl Translated {
    /// Draw layer `layer`'s content (or mask) — `surface`, as the document holds it, unmoved —
    /// moved by `offset`: a Move drag's preview without a moved copy of the pixels. The plan
    /// places the layer's bounds (and those of the groups around it) at the offset, and the
    /// lower-stack cache isn't used from the layer's unit up. Only for
    /// [`plan::movable_in_place`] layers (rendering refuses others); a CPU fallback draws the
    /// document as it is, so callers keep a moved copy for that path.
    pub fn in_place(layer: LayerId, mask: bool, surface: &Surface, offset: (i32, i32)) -> Self {
        Translated { layer, mask, shown: surface_fingerprint(surface), source: surface.clone(), offset }
    }
}

/// Identity of a surface's pixels: tile `Arc`s and coordinates, default pixel and format.
pub fn surface_fingerprint(s: &Surface) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ s.tile_count() as u64;
    for v in s.default_pixel() {
        h = (h ^ u64::from(v.to_bits())).wrapping_mul(0x100_0000_01b3);
    }
    h = (h ^ u64::from(s.format().bytes_per_pixel() as u32)).wrapping_mul(0x100_0000_01b3);
    let mut tiles: Vec<u64> = s.tiles().map(|(c, t)| (Arc::as_ptr(t) as usize as u64) ^ ((c.tx as u64) << 32 | c.ty as u32 as u64).rotate_left(17)).collect();
    tiles.sort_unstable();
    for t in tiles {
        h = (h ^ t).wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn tex_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            multisampled: false,
            sample_type: wgpu::TextureSampleType::Float { filterable: false },
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

fn uniform_entry(binding: u32, size: u64) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: NonZeroU64::new(size) },
        count: None,
    }
}

/// The compositor's WGSL source (exposed for validation in tests).
pub const SHADER: &str = include_str!("compose.wgsl");

/// Pass-level resources resolved for one page cell: resident pages (texture, mask) and effect
/// maps (view and region) per pass.
struct Bound {
    views: Vec<(Option<ResidentRef>, Option<ResidentRef>)>,
    /// [`Kernel::Layers`] passes: the pages of each member.
    member_views: Vec<Vec<(Option<ResidentRef>, Option<ResidentRef>)>>,
    keys: Vec<ResKey>,
    /// Each pass's effect map: its view, region and level of detail.
    maps: Vec<Option<(wgpu::TextureView, Rect, u32)>>,
}

/// Pass-level resources shared by every cell of a frame.
struct FrameRes {
    /// Tile area of each pass's (texture, mask) surface within the canvas tile grid.
    areas: Vec<(Rect, Rect)>,
    /// The same for the members of [`Kernel::Layers`] passes.
    member_areas: Vec<Vec<(Rect, Rect)>>,
    luts: Vec<Option<(wgpu::Texture, wgpu::TextureView)>>,
    patterns: Vec<Option<wgpu::TextureView>>,
}

/// Document pixels of page cell `c`.
fn cell_rect(c: Cell, page: u32) -> Rect {
    let p = page as i32;
    Rect::new(c.0.saturating_mul(p), c.1.saturating_mul(p), c.0.saturating_add(1).saturating_mul(p), c.1.saturating_add(1).saturating_mul(p))
}

/// `r` cut along the grid of `step`-sized squares (row-major).
fn grid_rects(r: Rect, step: u32) -> Vec<Rect> {
    let s = step.max(1) as i32;
    let mut out = Vec::new();
    let mut y = r.y0.div_euclid(s) * s;
    while y < r.y1 {
        let mut x = r.x0.div_euclid(s) * s;
        while x < r.x1 {
            out.push(Rect::new(x, y, x.saturating_add(s), y.saturating_add(s)).intersect(&r));
            x = x.saturating_add(s);
        }
        y = y.saturating_add(s);
    }
    out
}

/// The reach up to which each distance field must be exact (the largest any program asks for).
fn field_reaches(progs: &[fx::MapProgram]) -> HashMap<FieldKind, i32> {
    let mut want: HashMap<FieldKind, i32> = HashMap::new();
    for (k, r) in progs.iter().flat_map(|p| p.fields.iter()) {
        let w = want.entry(*k).or_insert(0);
        *w = (*w).max(*r);
    }
    want
}

/// Runs `f` inside validation and internal-error scopes and returns the first error raised.
/// Native wgpu reports errors synchronously; where they only arrive asynchronously (WebGPU) none
/// is reported.
/// Profiling: a named span of the compositor's work, timed by the app (`perf_span` in the UI)
/// until the returned guard drops. Installed once with [`set_span_hook`].
pub type SpanFn = fn(&'static str) -> Box<dyn std::any::Any>;

static SPAN: std::sync::OnceLock<SpanFn> = std::sync::OnceLock::new();

/// Install the [`SpanFn`] (once).
pub fn set_span_hook(f: SpanFn) {
    let _ = SPAN.set(f);
}

fn span(name: &'static str) -> Option<Box<dyn std::any::Any>> {
    SPAN.get().map(|f| f(name))
}

/// Layer effect shapes built (or built again) so far, process-wide: a moved layer, or another
/// document composited with the same layers, keeps its maps (tests and diagnostics).
static FX_BUILDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`FX_BUILDS`]: layer effect maps built so far.
pub fn fx_builds() -> u64 {
    FX_BUILDS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Layer effect maps written so far, wholly or in part (built, grown, or a damaged part
/// redrawn), process-wide (tests and diagnostics): each is a layer's shape and distance fields
/// computed on the CPU.
static FX_UPDATES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Pixels of layer effect shapes computed on the CPU so far, process-wide (tests and
/// diagnostics).
pub(crate) static FX_SHAPE_PX: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`FX_SHAPE_PX`]: effect shape pixels computed so far.
pub fn fx_shape_px() -> u64 {
    FX_SHAPE_PX.load(std::sync::atomic::Ordering::Relaxed)
}

/// [`FX_UPDATES`]: layer effect maps written so far.
pub fn fx_updates() -> u64 {
    FX_UPDATES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Bytes of effect shapes and distance fields uploaded so far, process-wide (tests and
/// diagnostics).
static FX_WRITTEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// [`FX_WRITTEN`]: bytes of effect shapes and distance fields uploaded so far.
pub fn fx_bytes_written() -> u64 {
    FX_WRITTEN.load(std::sync::atomic::Ordering::Relaxed)
}

fn first_error<T>(device: &wgpu::Device, f: impl FnOnce() -> T) -> (T, Option<wgpu::Error>) {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
    let out = f();
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut error = None;
    // Scopes pop in reverse order.
    for pop in [internal.pop(), validation.pop()] {
        if let std::task::Poll::Ready(Some(e)) = std::pin::pin!(pop).poll(&mut cx) {
            error.get_or_insert(e);
        }
    }
    (out, error)
}

impl Compositor {
    /// Create a compositor with the default accumulation format ([`ACC_FORMAT`]).
    pub fn new(device: &wgpu::Device) -> Self {
        Self::new_with_format(device, ACC_FORMAT)
    }

    /// The accumulation format to use on `adapter`: `Rgba32Float` when it can be a render target,
    /// else `Rgba16Float`. Some drivers (e.g. Intel Vulkan) don't support rendering to `Rgba32Float`
    /// and would otherwise panic at render-pipeline creation.
    pub fn preferred_acc_format(adapter: &wgpu::Adapter) -> wgpu::TextureFormat {
        let feats = adapter.get_texture_format_features(wgpu::TextureFormat::Rgba32Float);
        if feats.allowed_usages.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) { wgpu::TextureFormat::Rgba32Float } else { wgpu::TextureFormat::Rgba16Float }
    }

    /// [`Compositor::new_with_format`], but a shader or pipeline the driver can't build (e.g. a
    /// D3D12 shader-compiler failure) is an `Err` instead of a panic, so the caller can fall back
    /// to the CPU compositor. Where errors only arrive asynchronously (WebGPU), creation is assumed
    /// to have worked.
    pub fn try_new_with_format(device: &wgpu::Device, acc_format: wgpu::TextureFormat) -> Result<Self, Unsupported> {
        let comp = Self::try_new_lazy(device, acc_format)?;
        for i in 0..Self::FIRST_PIPELINES {
            comp.warm(device, i)?;
        }
        Ok(comp)
    }

    /// How many pipelines every page draws with ([`Self::warm`] builds them one at a time).
    pub const FIRST_PIPELINES: usize = 3;

    /// [`Self::try_new_with_format`] without building any pipeline yet: the caller builds the
    /// first ones with [`Self::warm`], one at a time (on WebGL each is a shader program compiled
    /// and linked on the spot, seconds on a weak GPU: several in one frame made the browser
    /// drop the context).
    pub fn try_new_lazy(device: &wgpu::Device, acc_format: wgpu::TextureFormat) -> Result<Self, Unsupported> {
        match first_error(device, || Self::new_with_format(device, acc_format)) {
            (comp, None) => Ok(comp),
            (_, Some(e)) => Err(Unsupported(format!("couldn't build the compositor's pipelines: {e}"))),
        }
    }

    /// Builds the `i`-th of the pipelines every page draws with (the rest are built as renders
    /// need them, see [`Kit::pipeline`]): a driver that can't build these keeps the canvas on
    /// the CPU. Nothing when it is built already or `i` is past [`Self::FIRST_PIPELINES`].
    pub fn warm(&self, device: &wgpu::Device, i: usize) -> Result<(), Unsupported> {
        let Some(&k) = [Kernel::Content, Kernel::Blend, Kernel::LayerBlend].get(i) else { return Ok(()) };
        self.kit.pipeline(device, k, self.acc_format).map(|_| ())
    }

    /// What builds every other pipeline this compositor (and its clones' renders) may draw with,
    /// ahead of the renders that need them ([`Prebuild::run`], on a worker thread natively). Built
    /// on first use instead, a page's first layer effect or adjustment held its frame while the
    /// driver compiled the shader (D3D12: 0.3 to 2 s each on an integrated GPU).
    pub fn prebuild(&self) -> Prebuild {
        Prebuild { kit: self.kit.clone(), acc_format: self.acc_format }
    }

    /// How many pipelines are built (diagnostics and tests).
    pub fn pipelines_built(&self) -> usize {
        self.kit.pipelines.lock().unwrap_or_else(std::sync::PoisonError::into_inner).len()
    }

    /// Create a compositor whose accumulation/render-target format is `acc_format`. Pass
    /// `Rgba16Float` on adapters that can't render to `Rgba32Float` (e.g. some Intel Vulkan drivers),
    /// which otherwise panics at render-pipeline creation. See `Compositor::preferred_acc_format`.
    pub fn new_with_format(device: &wgpu::Device, acc_format: wgpu::TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("pc_compose"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let bgl0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_compose_uniforms"),
            entries: &[uniform_entry(0, CHUNK_UNIFORM), uniform_entry(1, OP_UNIFORM)],
        });
        let bgl1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_compose_textures"),
            entries: &(0..8).map(tex_entry).collect::<Vec<_>>(),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pc_compose"),
            bind_group_layouts: &[Some(&bgl0), Some(&bgl1)],
            immediate_size: 0,
        });
        let bgl0m = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("pc_compose_layers_uniforms"),
            entries: &[uniform_entry(0, CHUNK_UNIFORM), uniform_entry(1, OP_UNIFORM), uniform_entry(2, MEMBERS_UNIFORM)],
        });
        let bgl1m = device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("pc_compose_layers_textures"), entries: &LAYERS_BINDINGS.map(tex_entry) });
        let layout_m = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("pc_compose_layers"),
            bind_group_layouts: &[Some(&bgl0m), Some(&bgl1m)],
            immediate_size: 0,
        });
        let dummy = Tex::new(device, "pc_compose_dummy", 1, 1, wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureUsages::TEXTURE_BINDING).view;
        let kit = Kit { pipelines: Arc::default(), module, layout, layout_m, bgl0, bgl1, bgl0m, bgl1m, dummy };
        Self {
            kit,
            pools: HashMap::new(),
            residents: HashMap::new(),
            fx: HashMap::new(),
            prefix: prefix::Prefix::default(),
            temps: Vec::new(),
            patterns: HashMap::new(),
            lut_cache: HashMap::new(),
            pattern_mode: photocraft_color::ColorMode::Rgb,
            max_dim: 0,
            device_max: device.limits().max_texture_dimension_2d,
            page: PAGE,
            chunk: CHUNK,
            resident_budget: RESIDENT_BUDGET,
            fx_budget: (FX_BUDGET as u64).saturating_sub(prefix::MAX_BYTES),
            focus: None,
            focus_level: 0,
            fill_prefix: true,
            frame: 0,
            stamp: 0,
            staged: 0,
            want: Rect::EMPTY,
            acc_format,
            effect_maps: true,
            maps_built: false,
            cmyk: 0,
            health: None,
            translated: Vec::new(),
            prepared: std::collections::HashSet::new(),
            prepared_pages: std::collections::HashSet::new(),
            staging: None,
        }
        .with_texture_limit(device.limits().max_texture_dimension_2d)
    }

    fn with_texture_limit(mut self, max: u32) -> Self {
        self.set_texture_limit(max);
        self
    }

    /// Plan paging for a texture side limit of `max` (clamped to the device's own limit and at
    /// least one tile). Tests use a small limit to exercise documents larger than it on any
    /// adapter. Drops every cached texture.
    pub fn set_texture_limit(&mut self, max: u32) {
        let tile = TILE_SIZE as u32;
        self.max_dim = max.min(self.device_max).max(tile);
        let mut page = tile;
        while page * 2 <= self.max_dim.min(PAGE) {
            page *= 2;
        }
        self.page = page;
        self.chunk = CHUNK.min(page);
        self.prefix.set_step(self.chunk);
        self.pools.clear();
        self.residents.clear();
        self.fx.clear();
        self.temps.clear();
        self.prefix.clear();
    }

    /// The page side layer surfaces are stored in.
    pub fn page_size(&self) -> u32 {
        self.page
    }

    /// Set the GPU memory resident layer pages may hold (default [`RESIDENT_BUDGET`]).
    pub fn set_resident_budget(&mut self, bytes: u64) {
        self.resident_budget = bytes;
    }

    /// Set the GPU memory the compositor may hold for layer pages and caches together
    /// (a quarter of it, at most [`FX_BUDGET`], for effect maps and lower-stack chunks, three to one). Uploads and evicted pages in
    /// flight during a refresh stay within about a quarter more.
    pub fn set_memory_budget(&mut self, bytes: u64) {
        let caches = (FX_BUDGET as u64).min(bytes / 4);
        // Three quarters of it for effect maps: they are what a frame can't draw without (a
        // lower-stack chunk only saves recompositing), and a page of styled balloons and type
        // needs every one of the view's at once (at 4 GB half of it held two or three layers').
        self.prefix.set_budget((caches / 4).min(prefix::MAX_BYTES));
        self.fx_budget = caches.saturating_sub(self.prefix.budget());
        self.resident_budget = bytes.saturating_sub(caches);
    }

    /// The budget of layer pages, effect maps and lower-stack chunks (bytes).
    pub fn memory_budget(&self) -> u64 {
        self.resident_budget.saturating_add(self.fx_budget).saturating_add(self.prefix.budget())
    }

    /// Bytes held by reusable lower-stack chunks, within the overall cache allowance.
    pub fn composite_cache_bytes(&self) -> u64 {
        self.prefix.bytes()
    }

    /// Cap lower-stack reuse (default at most 512 MiB); zero disables it. The released allowance
    /// goes back to effect maps, preserving the total budget and layer-page allowance.
    pub fn set_composite_cache_budget(&mut self, bytes: u64) {
        let caches = self.fx_budget.saturating_add(self.prefix.budget());
        self.prefix.set_budget(bytes.min(caches));
        self.fx_budget = caches.saturating_sub(self.prefix.budget());
    }

    /// Bytes of the layer pages a render of `region` needs resident (each layer and mask
    /// surface's tiles within the region's page cells, in their texture format).
    pub fn working_set(&self, doc: &Document, region: Rect) -> u64 {
        let Ok(plan) = plan(doc) else { return 0 };
        let grid = tile_grid(doc.bounds());
        let cells = grid_rects(region.intersect(&doc.bounds()), self.page)
            .into_iter()
            .map(|r| cell_rect((r.x0.div_euclid(self.page as i32), r.y0.div_euclid(self.page as i32)), self.page))
            .fold(Rect::EMPTY, |a, c| if a.is_empty() { c } else { a.union(&c) });
        let mut seen = std::collections::HashSet::new();
        let mut total = 0u64;
        for p in plan.passes.iter().flat_map(|p| std::iter::once(p).chain(&p.members)) {
            let sources = [p.tex.as_ref().map(|t| (t.layer, t.role, t.surface.get())), p.mask.as_ref().map(|m| (m.layer, Role::Mask, m.surface.get()))];
            for (layer, role, s) in sources.into_iter().flatten() {
                if seen.insert((layer, role)) {
                    let a = s.tile_bounds().intersect(&grid).intersect(&cells);
                    total += a.width() as u64 * a.height() as u64 * TexKind::for_surface(role, s.format()).bytes_per_pixel() as u64;
                }
            }
        }
        total
    }

    /// Whether a render of `region` keeps every layer page it needs within the budget; when it
    /// doesn't, each render re-uploads what was evicted.
    pub fn fits_budget(&self, doc: &Document, region: Rect) -> bool {
        self.working_set(doc, region) <= self.resident_budget
    }

    /// The document area the view shows (`None`: unknown) and the level of detail it draws at.
    /// Full refreshes draw its cells last, and the effect cache lets go of maps outside it, and
    /// of maps at other levels, first.
    pub fn set_focus(&mut self, focus: Option<Rect>, level: u32) {
        self.focus = focus;
        self.focus_level = level.min(MAX_LOD);
    }

    /// Whether a render whose region the lower-stack cache lacks draws the region's whole chunks
    /// to fill it (on: brush dabs moving on then skip the lower stack). Off for renders that won't
    /// be repeated there, the canvas's catch-up and refinement: a band of a few rows would
    /// otherwise draw whole 1024² chunks, many times the work asked for. What the cache holds is
    /// reused either way.
    pub fn set_fill_prefix(&mut self, on: bool) {
        self.fill_prefix = on;
    }

    /// GPU memory held by resident layer pages (bytes).
    pub fn resident_bytes(&self) -> u64 {
        self.residents.values().map(Resident::bytes).sum()
    }

    /// Whether `doc` can be composited on the GPU. Documents and layers larger than the texture
    /// limit are fine: surfaces are stored in page-sized textures and effect maps of regions
    /// larger than the limit are built per page cell.
    pub fn supports(&self, doc: &Document) -> Result<(), Unsupported> {
        let p = plan(doc)?;
        self.check_fx(doc, &p)
    }

    /// Whether the maps of an effect region are built per page cell: it exceeds the texture
    /// limit, or a cell's area. Whole, one layer's maps over a region thousands of pixels wide
    /// were hundreds of megabytes (more than a 4 GB device's whole effect budget), the cache could
    /// only let go of all of them, and they were built in one go wherever any of it was drawn
    /// (an 8K strip's styled shapes: 1.3-2.7 s frames, 8.7 GB). Per cell, a render builds and
    /// keeps the maps of the cells it draws (none off the canvas), let go least recently used.
    fn fx_paged(&self, region: Rect) -> bool {
        let cell = u64::from(self.page) * u64::from(self.page);
        region.width() > self.max_dim || region.height() > self.max_dim || u64::from(region.width()) * u64::from(region.height()) > cell
    }

    /// The level of detail `f`'s maps are built at for a render at `level`: the render's, so a
    /// zoomed-out view's maps take a 4^level-th of the memory and the work (an 8K page of 24
    /// styled layers at fit: the full-size maps were more than the effect budget, and were
    /// built again over and over); full size (0) for an effect whose maps don't scale
    /// ([`fx::at_level`]) and where the reduced region still needs page cells.
    fn map_level(&self, f: &plan::FxLayer<'_>, level: u32) -> u32 {
        let scales = || f.layer.effects.items.iter().filter(|e| e.enabled()).all(|e| fx::at_level(e, level).is_some());
        if level == 0 || self.fx_paged(level_rect(f.region, level)) || !scales() { 0 } else { level }
    }

    /// Where a render at `level` reads `f`'s maps from: one set over its whole region (the
    /// key's level and that region in its pixels), or `None` per page cell at full size.
    fn whole_maps(&self, f: &plan::FxLayer<'_>, level: u32) -> Option<(FxKey, Rect)> {
        let ml = self.map_level(f, level);
        (ml > 0 || !self.fx_paged(f.region)).then(|| ((f.layer.id, None, ml), level_rect(f.region, ml)))
    }

    /// How far beyond a page cell the effect maps of `f` must be computed to be exact in it.
    fn fx_apron(doc: &Document, f: &plan::FxLayer<'_>) -> i32 {
        let vector_shape = matches!(f.layer.content, LayerContent::Shape(_)) && photocraft_compose::effect_outline(f.layer).is_none();
        let progs: Vec<fx::MapProgram> = f
            .layer
            .effects
            .items
            .iter()
            .filter(|e| e.enabled())
            .map(|e| fx::program_with(e, &doc.global_light, vector_shape, &doc.patterns, (0.0, 0.0), 0))
            .collect();
        let want = field_reaches(&progs);
        // Distance fields are exact one pixel inside the window they are computed from.
        progs.iter().map(|p| p.reach(&|k| want.get(&k).copied().unwrap_or(0))).max().unwrap_or(0).saturating_add(2)
    }

    /// Builds the pipelines `plan` draws with that aren't built yet: its kernels, and the first
    /// time it has layer effects every effect-map kernel (R32Float and R16Float targets, which
    /// some adapters, e.g. GL without float render targets, can't draw: then layer effects use the
    /// CPU compositor and the rest stays here).
    fn ensure_pipelines(&mut self, device: &wgpu::Device, plan: &Plan<'_>) -> Result<(), Unsupported> {
        let mut kernels: Vec<Kernel> = plan.passes.iter().flat_map(|p| std::iter::once(p.kernel).chain(p.members.iter().map(|m| m.kernel))).collect();
        kernels.sort_by_key(|k| *k as u32);
        kernels.dedup();
        for k in kernels.into_iter().filter(|k| k.entry().is_some()) {
            self.kit.pipeline(device, k, self.acc_format)?;
        }
        if !plan.fx.is_empty() && !self.maps_built {
            self.maps_built = true;
            // One kernel per target format tells whether the adapter can draw effect maps; the
            // others are built when an effect first runs them.
            let probe = Kernel::DRAWN.iter().copied().find(|k| k.is_map());
            for format in [MAP32, MAP16] {
                if let Some(k) = probe
                    && let Err(e) = self.kit.pipeline(device, k, format)
                {
                    log::info!("GPU compositor: no effect-map pipelines ({}); layer effects use the CPU", e.0);
                    self.effect_maps = false;
                    break;
                }
            }
        }
        Ok(())
    }

    /// Effect maps must fit in textures (per page cell when the region exceeds the limit), and
    /// patterns in one texture.
    fn check_fx(&self, doc: &Document, p: &Plan<'_>) -> Result<(), Unsupported> {
        if !self.effect_maps && !p.fx.is_empty() {
            return Err(Unsupported("layer effects need float effect-map render targets, which this GPU lacks".into()));
        }
        for f in &p.fx {
            if self.fx_paged(f.region) {
                let side = i64::from(self.page) + 2 * i64::from(Self::fx_apron(doc, f));
                if side > i64::from(self.device_max) {
                    return Err(Unsupported(format!("effects of `{}` reach too far to build per page", f.layer.name)));
                }
            }
            for e in f.layer.effects.items.iter().filter(|e| e.enabled()) {
                // Effect noise is speckled in the CPU maps; keep CPU and GPU identical by
                // rendering noise-carrying effects on the CPU path.
                let noisy = match e {
                    photocraft_doc::Effect::DropShadow(s) | photocraft_doc::Effect::InnerShadow(s) => s.noise > 0.0,
                    photocraft_doc::Effect::OuterGlow(g) | photocraft_doc::Effect::InnerGlow(g) => g.noise > 0.0,
                    _ => false,
                };
                if noisy {
                    return Err(Unsupported(format!("effect noise on `{}` uses the CPU path", f.layer.name)));
                }
                // The GPU map program casts shadows from the shape alone.
                if matches!(e, photocraft_doc::Effect::DropShadow(s) if s.source == photocraft_doc::ShadowSource::FillAndStroke) {
                    return Err(Unsupported(format!("a drop shadow cast from the strokes of `{}` uses the CPU path", f.layer.name)));
                }
                for s in fx::program_with(e, &doc.global_light, false, &doc.patterns, (0.0, 0.0), 0).stages {
                    if s.lut.as_ref().is_some_and(|l| l.len() > 4096) {
                        return Err(Unsupported(format!("effect blur on `{}` too wide for the GPU path", f.layer.name)));
                    }
                }
            }
        }
        for pass in &p.passes {
            if let Some(pat) = pass.pattern
                && (pat.width > self.device_max || pat.height > self.device_max)
            {
                return Err(Unsupported(format!("pattern `{}` larger than the GPU texture limit", pat.name)));
            }
        }
        Ok(())
    }

    /// Drop GPU textures of a closed document.
    pub fn forget_doc(&mut self, doc: DocId) {
        self.residents.retain(|_, r| r.doc != doc);
        self.fx.retain(|_, e| e.doc != doc);
        self.prefix.forget(doc);
    }

    /// GPU memory held by cached effect maps (bytes).
    pub fn fx_cache_bytes(&self) -> usize {
        self.fx.values().map(FxEntry::bytes).sum()
    }

    /// Watch `health` (see [`DeviceHealth::watch`]): once the device is lost or reported an
    /// error, every render returns [`Unsupported`] without touching the GPU, so callers use the
    /// CPU compositor instead.
    pub fn set_health(&mut self, health: DeviceHealth) {
        self.health = Some(health);
    }

    /// Layer surfaces to draw from their pre-move source at an offset (see [`Translated`]); an
    /// empty list draws every surface as it is. Entries whose `shown` fingerprint doesn't match
    /// the surface being drawn are ignored, so a stale list only costs the uploads it would
    /// have saved.
    pub fn set_translated(&mut self, translated: Vec<Translated>) {
        self.translated = translated;
    }

    /// The layers (and masks) of `doc` that [`Translated::in_place`] entries draw moved from
    /// the surfaces the document holds.
    fn moved_in_place(&self, doc: &Document) -> (plan::Moved, std::collections::HashSet<LayerId>) {
        let mut moved = plan::Moved::new();
        let mut masks = std::collections::HashSet::new();
        for t in self.translated.iter().filter(|t| t.shown == surface_fingerprint(&t.source)) {
            let Some(l) = doc.layer(t.layer) else { continue };
            let held = if t.mask { l.mask.as_ref().map(|m| &m.surface) } else { l.surface() };
            if held.is_some_and(|s| surface_fingerprint(s) == t.shown) {
                if t.mask {
                    masks.insert(t.layer);
                } else {
                    moved.insert(t.layer, t.offset);
                }
            }
        }
        (moved, masks)
    }

    /// Where layer `layer`'s effect shape sources (`content`, its `mask`) are drawn from when it
    /// is drawn translated, both by the same offset: the pre-move surfaces' fingerprint, and the
    /// offset.
    fn translated_from(&self, layer: LayerId, content: Option<&Surface>, mask: Option<&Surface>) -> Option<(u64, (i32, i32))> {
        let from = |surface: &Surface, mask: bool| {
            let t = self.translated.iter().find(|t| t.layer == layer && t.mask == mask)?;
            (surface_fingerprint(surface) == t.shown).then(|| (surface_fingerprint(&t.source), t.offset))
        };
        let (fp, at) = from(content?, false)?;
        match mask {
            None => Some((fp, at)),
            Some(m) => from(m, true).filter(|(_, mat)| *mat == at).map(|(mfp, _)| (fp ^ mfp.rotate_left(1), at)),
        }
    }

    /// The source and offset to draw `surface` (layer `layer`'s content or mask) from, if it is
    /// one of the [`Translated`] surfaces.
    fn translation(&self, layer: LayerId, mask: bool, surface: &Surface) -> Option<(Surface, (i32, i32))> {
        let t = self.translated.iter().find(|t| t.layer == layer && t.mask == mask)?;
        (surface_fingerprint(surface) == t.shown && t.source.format() == surface.format() && t.source.default_pixel() == surface.default_pixel())
            .then(|| (t.source.clone(), t.offset))
    }

    /// [`Self::sync`] of `surface` over `region`, or of its pre-move source over the region moved
    /// back (tile aligned), reported at the offset so the shader samples the moved pixels.
    #[allow(clippy::too_many_arguments)]
    fn sync_shown(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: DocId,
        key: ResKey,
        surface: &Surface,
        region: Rect,
        stats: &mut Stats,
    ) -> Option<(ResKey, [i32; 4])> {
        let Some((source, (dx, dy))) =
            (if self.translated.is_empty() || region.is_empty() { None } else { self.translation(key.0, key.1 == Role::Mask, surface) })
        else {
            return self.sync(device, queue, encoder, doc, key, surface, region, stats);
        };
        let back = tile_grid(Rect::new(region.x0.saturating_sub(dx), region.y0.saturating_sub(dy), region.x1.saturating_sub(dx), region.y1.saturating_sub(dy)));
        let want = self.want;
        if !want.is_empty() {
            self.want = want.translate(-dx, -dy);
        }
        let synced = self.sync(device, queue, encoder, doc, key, &source, back, stats);
        self.want = want;
        let (k, [x, y, w, h]) = synced?;
        Some((k, [x.saturating_add(dx), y.saturating_add(dy), w, h]))
    }

    /// Why the device can't be used (`None` while healthy or unwatched).
    pub fn fault(&self) -> Option<Fault> {
        self.health.as_ref().and_then(DeviceHealth::fault)
    }

    /// Composite `region` of `doc`. Each finished chunk is passed to `sink` while its encoder is
    /// still open; the chunk texture is reused afterwards, so the sink must record any copies
    /// or passes that read it into the given encoder. Submits the work before returning; a
    /// refresh of a huge document also submits in between (the sink may be handed a new
    /// encoder), so its uploads and evicted pages never pile up in memory.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        doc: &Document,
        region: Rect,
        sink: impl FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
    ) -> Result<Stats, Unsupported> {
        self.render_lod(device, queue, doc, region, 0, sink)
    }

    /// [`Self::render`] at a level of detail: `region` (document pixels) is composited at one
    /// pixel per 2^`level` × 2^`level` document pixels (at most [`MAX_LOD`]), for a view that
    /// shows the document that far zoomed out, so a refresh costs about 1/4^`level` of the
    /// full-size one. Layer pixels and masks are box filtered (up to 4 × 4 taps per output
    /// pixel: exact through level 2, a regular subsample above); effect maps, fills and patterns are sampled at the pixel's centre. The result
    /// differs from the mip-mapped full-size composite only by that resampling. The chunks
    /// handed to `sink` are in the level's coordinates ([`level_rect`] of the region, clipped
    /// to [`level_bounds`]). Lower-stack reuse works per level (a change of level starts over).
    pub fn render_lod(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        doc: &Document,
        region: Rect,
        level: u32,
        mut sink: impl FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
    ) -> Result<Stats, Unsupported> {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_compose") });
        let stats = match self.encode_inner(device, queue, &mut encoder, doc, region, level.min(MAX_LOD), &mut sink, true) {
            Ok(stats) => stats,
            Err(error) => {
                self.prefix.clear();
                return Err(error);
            }
        };
        let _s = span("gpu.submit");
        queue.submit([encoder.finish()]);
        Ok(stats)
    }

    /// Build, or bring up to date, what a render of `region` of `doc` at `level` would build or
    /// upload, one unit at a time, asking `more` before each unit after the first that needed work
    /// whether there is time for another: first the effect maps (a layer at a time), then the
    /// layer pages (a pass at a time). Whether they are all current then (a render of `region`
    /// builds and uploads none). Maps larger than a page (built per cell as a render goes) are
    /// left to the render, as are the pages of a region whose layers don't fit the resident
    /// budget together. A caller filling a page in pieces over frames, or making a cache ahead of
    /// a drag, prepares each piece within its frames' work and composites it once it is ready:
    /// one frame built every styled layer's maps (their shapes and distance fields on the CPU)
    /// and uploaded every layer's pages of the piece (his PSD: 40 to 150 ms; a 200-layer page
    /// opened with the Move tool in hand, 0.13.43: gpu.fx 173 ms and gpu.upload 48 ms in one
    /// frame of its drag cache). Also true when a layer's maps built by an earlier call had to be
    /// built again (the effect cache let them go for what was drawn meanwhile: it can't hold them
    /// all, so building ahead can't end, and the render builds them as it goes).
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        doc: &Document,
        region: Rect,
        level: u32,
        more: &mut dyn FnMut() -> bool,
    ) -> Result<bool, Unsupported> {
        if let Some(f) = self.fault() {
            return Err(Unsupported(f.to_string()));
        }
        let space = photocraft_compose::cmyk_space(doc);
        self.cmyk = space.as_ref().map_or(0, |s| s.id);
        photocraft_color::convert::with_cmyk_space(space.as_ref(), || {
            let canvas = doc.bounds();
            let region = region.intersect(&canvas);
            let (moved, moved_masks) = self.moved_in_place(doc);
            let plan = {
                let _s = span("gpu.plan");
                plan::plan_moved(doc, Some(&moved))?
            };
            {
                let _s = span("gpu.pipelines.build");
                self.ensure_pipelines(device, &plan)?;
            }
            self.check_fx(doc, &plan)?;
            let level = level.min(MAX_LOD);
            let drawn = self.drawn_rect(region, level);
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_prepare") });
            let mut stats = Stats::default();
            let mut worked = false;
            let mut done = true;
            // What the render builds: whole regions it draws in, then per page cell of the
            // region the maps of layers larger than a cell (each cell's window, as the render's).
            let mut units: Vec<(&plan::FxLayer<'_>, FxKey, Rect)> = plan
                .fx
                .iter()
                .filter(|f| !f.region.intersect(&drawn).is_empty())
                .filter_map(|f| self.whole_maps(f, level).map(|(key, window)| (f, key, window)))
                .collect();
            for part in grid_rects(region, self.page) {
                let cell = (part.x0.div_euclid(self.page as i32), part.y0.div_euclid(self.page as i32));
                let cr = cell_rect(cell, self.page);
                for f in plan.fx.iter().filter(|f| self.whole_maps(f, level).is_none()) {
                    let pf = cr.intersect(&f.region);
                    if !pf.is_empty() {
                        units.push((f, (f.layer.id, Some(cell), 0), pf.inflate(Self::fx_apron(doc, f)).intersect(&f.region)));
                    }
                }
            }
            for (f, key, window) in units {
                // Maps to build wait for a call with time left: the next frame's. A call made
                // once the frame's work was spent still built one layer's (8.5 ms each at 8K,
                // where the effect cache couldn't hold them all).
                let current = self.fx_current(doc, f, key, window);
                // (A group counts as current for staging, which builds its shape whole.)
                let built = match &f.layer.content {
                    LayerContent::Group(_) => self.fx.get(&key).is_some_and(|e| e.region == window && e.shape_key == fx::group_key(f.layer, &doc.global_light)),
                    _ => current,
                };
                if (worked || !built) && !more() {
                    done = false;
                    break;
                }
                let _s = span("gpu.fx");
                // Maps built anew: the shape and distance fields on the CPU in bands over calls
                // (`stage_fx`), then handed over whole.
                // (A small layer's maps take a part's time: built in one go, as a render would.
                // At a level of detail its shape is computed from all the pixels it stands for.)
                let small = (u64::from(window.width()) * u64::from(window.height())) << (2 * key.2) <= 2 * STAGE_PX;
                let ready = if small || current {
                    None
                } else {
                    match self.stage_fx(device, queue, doc, f, key, window, &mut worked, more) {
                        Stage::Ready(b) => Some(*b),
                        Stage::Whole => None,
                        Stage::Pending => {
                            done = false;
                            break;
                        }
                    }
                };
                let before = (stats.fx_shapes, stats.fx_programs);
                self.sync_fx(device, queue, &mut encoder, doc, f, key, window, &mut stats, ready);
                if (stats.fx_shapes, stats.fx_programs) != before {
                    worked = true;
                    if !self.prepared.insert(key) {
                        // Built again: the cache can't keep what is built ahead.
                        break;
                    }
                    // Within the effect budget, what this preparation built kept (it is what the
                    // render draws): past it, it can't all be kept ahead, and the render builds
                    // the rest a cell at a time.
                    let keep = std::mem::take(&mut self.prepared);
                    stats.evicted += self.evict_keeping(&keep, false).0;
                    let held: usize = keep.iter().filter_map(|k| self.fx.get(k)).map(FxEntry::bytes).sum();
                    self.prepared = keep;
                    if held as u64 > self.fx_budget {
                        break;
                    }
                }
            }
            // The layer pages the render's cells sample, where it draws: the passes past none of
            // the lower-stack cache (the render reuses what that holds), drawing in the region.
            if done && !region.is_empty() && self.fits_budget(doc, region) {
                self.pattern_mode = doc.pixel_format().mode;
                self.stamp += 1;
                let fr = {
                    let _s = span("gpu.frame_res");
                    self.frame_res(device, queue, canvas, &plan, &moved, &moved_masks)
                };
                let none = (Rect::EMPTY, Rect::EMPTY);
                // A cell's pages in bands of whole tile rows of about [`PAGES_AT_ONCE`] each (a
                // 2048 px cell of 8-bit RGBA is 16 MB: one layer's took 19 ms in a call, test
                // build, filling an 8K page).
                let band = ((PAGES_AT_ONCE / (u64::from(self.page) * 4)) as i32 / TILE_SIZE * TILE_SIZE).max(TILE_SIZE);
                let parts = grid_rects(region, self.page).into_iter().flat_map(|part| {
                    let y0 = part.y0.div_euclid(band) * band;
                    (y0..part.y1).step_by(band as usize).map(move |y| Rect::new(part.x0, y.max(part.y0), part.x1, (y + band).min(part.y1)))
                });
                'cells: for part in parts {
                    let cell = (part.x0.div_euclid(self.page as i32), part.y0.div_euclid(self.page as i32));
                    let cr = cell_rect(cell, self.page);
                    self.want = part;
                    let mut keys = Vec::new();
                    for (i, p) in plan.passes.iter().enumerate() {
                        if p.clip.is_some_and(|c| c.intersect(&part).is_empty()) {
                            continue;
                        }
                        let uploaded = stats.tiles_uploaded;
                        // (A light page's pages go in one call: it draws whole in its first frame.)
                        if worked && stats.bytes_uploaded as u64 >= PAGES_AT_ONCE && !more() {
                            done = false;
                            break 'cells;
                        }
                        keys.clear();
                        let _ = self.sync_views(device, queue, &mut encoder, doc.id, p, fr.areas.get(i).copied().unwrap_or(none), cell, cr, &mut keys, &mut stats);
                        for (m, mp) in p.members.iter().enumerate() {
                            let areas = fr.member_areas.get(i).and_then(|a| a.get(m)).copied().unwrap_or(none);
                            let _ = self.sync_views(device, queue, &mut encoder, doc.id, mp, areas, cell, cr, &mut keys, &mut stats);
                        }
                        if stats.tiles_uploaded != uploaded {
                            worked = true;
                            // Uploaded again (pixels made anew for each render, or the budget let
                            // the page go): it can't be kept ahead, the render uploads it.
                            if !keys.iter().all(|k| self.prepared_pages.insert((*k, part.y0))) {
                                break 'cells;
                            }
                        }
                    }
                }
                self.want = Rect::EMPTY;
            }
            queue.submit([encoder.finish()]);
            if done {
                self.prepared.clear();
                self.prepared_pages.clear();
            }
            Ok(done)
        })
    }

    /// Whether `f`'s effect maps under `key` exist for `region` (its whole region, or a cell's
    /// window) and its settings (what changed in its pixels since is updated in place). Groups
    /// count as current: their shape is built whole.
    fn fx_current(&self, doc: &Document, f: &plan::FxLayer<'_>, key: FxKey, region: Rect) -> bool {
        matches!(f.layer.content, LayerContent::Group(_)) || self.fx.get(&key).is_some_and(|e| e.region == region && e.shape_key == fx::shape_key(f.layer, doc.bounds()))
    }

    /// Compute `f`'s effect shape and the distance fields its maps under `key` read, over
    /// `region` (its whole region, or a cell's window), in bands of about [`STAGE_PX`], asking
    /// `more` before each band once `worked`: the build of a page-sized styled layer took 40 to
    /// 150 ms in one frame. Continues where an earlier call left off when the layer is the same.
    /// The result when complete (for [`Self::sync_fx`]). [`Stage::Whole`] when the layer was
    /// being staged for another document or region (a drag cache's sources show it over other
    /// bounds by turns): staged by turns, neither would end.
    #[allow(clippy::too_many_arguments)]
    fn stage_fx(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        doc: &Document,
        f: &plan::FxLayer<'_>,
        key: FxKey,
        region: Rect,
        worked: &mut bool,
        more: &mut dyn FnMut() -> bool,
    ) -> Stage {
        let layer = f.layer;
        let shape_key = fx::shape_key(layer, doc.bounds());
        let (content, mask) = fx::shape_sources(layer);
        let sources = [content.map_or(0, surface_fingerprint), mask.map_or(0, surface_fingerprint)];
        let n = region.width() as usize * region.height() as usize;
        let same = |s: &Staging| s.built.region == region && s.doc == doc.id && s.shape_key == shape_key && s.sources == sources;
        if self.staging.as_ref().is_some_and(|s| s.built.key == key && !same(s)) {
            self.staging = None;
            return Stage::Whole;
        }
        if self.staging.as_ref().is_none_or(|s| s.built.key != key) {
            let kinds = field_reaches(&fx_programs(doc, f, key.2)).into_iter().collect();
            self.staging = Some(Staging {
                built: Staged { key, region, shape: vec![0.0; n], shape_tex: Some(Tex::map(device, "pc_fx_shape", region, MAP32)), fields: HashMap::new() },
                doc: doc.id,
                shape_key,
                sources,
                shape_y: region.y0,
                kinds,
                field: 0,
                field_y: region.y0,
                field_tex: None,
            });
        }
        let Some(st) = self.staging.as_mut() else { return Stage::Whole };
        // (At a level of detail a shape pixel is computed from the 4^level it stands for.)
        let rows = i32::try_from(STAGE_PX / u64::from(region.width().max(1))).unwrap_or(i32::MAX).max(16);
        let shape_rows = i32::try_from((STAGE_PX >> (2 * key.2)) / u64::from(region.width().max(1))).unwrap_or(i32::MAX).max(1);
        while !region.is_empty() {
            if st.shape_y < region.y1 {
                if *worked && !more() {
                    return Stage::Pending;
                }
                let band = Rect::new(region.x0, st.shape_y, region.x1, st.shape_y.saturating_add(shape_rows).min(region.y1));
                let v = fx::shape_at(doc, layer, band, key.2);
                fx::paste(&mut st.built.shape, region, band, &v);
                if let Some(t) = &st.built.shape_tex {
                    t.write_r32(queue, region, band, &v);
                }
                st.shape_y = band.y1;
                *worked = true;
                continue;
            }
            let Some(&(kind, reach)) = st.kinds.get(st.field) else { break };
            if *worked && !more() {
                return Stage::Pending;
            }
            // (A band of a field reads the shape within its radius: not much thinner than that.)
            let band_rows = rows.max(fx::field_radius(kind, reach) + 1);
            let out = Rect::new(region.x0, st.field_y, region.x1, st.field_y.saturating_add(band_rows).min(region.y1));
            let v = fx::field(kind, reach, &st.built.shape, region, out);
            st.field_tex.get_or_insert_with(|| Tex::map(device, "pc_fx_field", region, MAP32)).write_r32(queue, region, out, &v);
            st.field_y = out.y1;
            *worked = true;
            if st.field_y >= region.y1
                && let Some(t) = st.field_tex.take()
            {
                st.built.fields.insert(kind, (reach, t));
                st.field += 1;
                st.field_y = region.y0;
            }
        }
        self.staging.take().map_or(Stage::Whole, |s| Stage::Ready(Box::new(s.built)))
    }

    /// `region` grown to the chunk grid of `level`: what a render of it records.
    fn drawn_rect(&self, region: Rect, level: u32) -> Rect {
        let step = i32::try_from(self.chunk).unwrap_or(i32::MAX).saturating_mul(1 << level.min(MAX_LOD)).max(1);
        let g = |v: i32| v.div_euclid(step).saturating_mul(step);
        Rect::new(g(region.x0), g(region.y0), g(region.x1.saturating_add(step - 1)), g(region.y1.saturating_add(step - 1)))
    }

    /// Like [`Self::render`] but records into `encoder` without submitting.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: &Document,
        region: Rect,
        sink: &mut dyn FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
    ) -> Result<Stats, Unsupported> {
        self.encode_inner(device, queue, encoder, doc, region, 0, sink, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_inner(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: &Document,
        region: Rect,
        level: u32,
        sink: &mut dyn FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
        flush: bool,
    ) -> Result<Stats, Unsupported> {
        if let Some(f) = self.fault() {
            return Err(Unsupported(f.to_string()));
        }
        // CMYK layers convert through the document's own CMYK profile (uploads and plan colours).
        let space = photocraft_compose::cmyk_space(doc);
        self.cmyk = space.as_ref().map_or(0, |s| s.id);
        photocraft_color::convert::with_cmyk_space(space.as_ref(), || self.encode_scoped(device, queue, encoder, doc, region, level, sink, flush))
    }

    #[allow(clippy::too_many_arguments)]
    fn encode_scoped(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: &Document,
        region: Rect,
        level: u32,
        sink: &mut dyn FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
        flush: bool,
    ) -> Result<Stats, Unsupported> {
        let canvas = doc.bounds();
        let region = region.intersect(&canvas);
        let (moved, moved_masks) = self.moved_in_place(doc);
        let plan = {
            let _s = span("gpu.plan");
            plan::plan_moved(doc, Some(&moved))?
        };
        {
            let _s = span("gpu.pipelines.build");
            self.ensure_pipelines(device, &plan)?;
        }
        self.check_fx(doc, &plan)?;
        self.pattern_mode = doc.pixel_format().mode;
        let mut stats = Stats { passes: plan.passes.len(), slots: plan.slots, level, ..Default::default() };
        let lbounds = level_bounds(canvas, level);
        if region.is_empty() {
            return Ok(stats);
        }
        if self.frame == 0 {
            // The first submission after device creation is dropped on some drivers (RADV),
            // which would render the first frame black; land it on an empty buffer instead.
            queue.submit(std::iter::empty::<wgpu::CommandBuffer>());
        }
        self.frame += 1;
        self.stamp += 1;
        self.staged = 0;

        // Only render owns submission. A caller may discard encode's command buffer, so it
        // cannot establish (or consume) a checkpoint whose GPU copy might never execute.
        // Moving layers change what the stack shows without changing the document: nothing at or
        // above their root unit may come from the lower-stack cache.
        let moved_root = (!moved.is_empty()).then(|| doc.layers.iter().position(|l| plan::layer_or_inside(l, &|k| moved.contains_key(&k.id)))).flatten();
        // Per level of detail: a drag composited reduced reuses the reduced lower stack too.
        let checkpoint = if flush { self.prefix.prepare(doc, &plan, moved_root, level) } else { None };

        // Effect maps of whole regions first (they may render group shapes through sub-plans);
        // regions larger than the limit are built per cell below. Only the layers whose effects
        // reach what this render draws: the chunks it records (`region` grown to the chunk grid,
        // as `fill_prefix` grows them). Syncing every effect layer of the document on each render
        // made a piece of a 4000x6000 page of 80 layers build (and, past the effect budget, evict
        // and build again) the maps of layers nowhere near it.
        let drawn = self.drawn_rect(region, level);
        let whole: Vec<Option<(FxKey, Rect)>> = plan.fx.iter().map(|f| self.whole_maps(f, level)).collect();
        let aprons: Vec<i32> = plan.fx.iter().zip(&whole).map(|(f, w)| if w.is_none() { Self::fx_apron(doc, f) } else { 0 }).collect();
        for (f, w) in plan.fx.iter().zip(&whole) {
            if let Some((key, maps)) = *w
                && !f.region.intersect(&drawn).is_empty()
            {
                let _s = span("gpu.fx");
                self.sync_fx(device, queue, encoder, doc, f, key, maps, &mut stats, None);
            }
        }
        // The level of each layer's whole maps (`None`: per page cell).
        let levels: Vec<Option<u32>> = whole.iter().map(|w| w.map(|(key, _)| key.2)).collect();
        let fr = self.frame_res(device, queue, canvas, &plan, &moved, &moved_masks);

        // Page cells; every other frame in reverse, so a repeated full refresh of a document
        // larger than the budget starts with the pages still resident.
        let page = self.page;
        let mut cells = grid_rects(region, page);
        if self.frame.is_multiple_of(2) {
            cells.reverse();
        }
        if let Some(f) = self.focus {
            let p = page as i32;
            let focused = |r: &Rect| !cell_rect((r.x0.div_euclid(p), r.y0.div_euclid(p)), page).intersect(&f).is_empty();
            cells.sort_by_key(focused);
        }
        let flush_bytes = FLUSH_BYTES.min(self.memory_budget() / 8).max(16 << 20);
        stats.cells = cells.len();
        // The previous intermediate submit (waited for before the next is queued; the web has
        // no blocking wait and relies on the browser's own scheduling).
        #[cfg(not(target_arch = "wasm32"))]
        let mut last_submit: Option<wgpu::SubmissionIndex> = None;
        // Work recorded and bytes evicted since the last submit.
        let (mut work, mut freed) = (0u64, 0u64);
        for part in cells {
            self.stamp += 1;
            let cell = (part.x0.div_euclid(page as i32), part.y0.div_euclid(page as i32));
            let cr = cell_rect(cell, page);
            for (i, f) in plan.fx.iter().enumerate() {
                let pf = cr.intersect(&f.region);
                if levels.get(i).is_none_or(Option::is_some) || pf.is_empty() {
                    continue;
                }
                let window = pf.inflate(aprons.get(i).copied().unwrap_or(0)).intersect(&f.region);
                self.sync_fx(device, queue, encoder, doc, f, (f.layer.id, Some(cell), 0), window, &mut stats, None);
            }
            // At a level of detail the cell's part shrinks; pages are a power of two at least
            // 2^MAX_LOD, so cells never share a reduced pixel.
            let lpart = level_rect(part, level).intersect(&lbounds);
            let outs = grid_rects(lpart, self.chunk);
            let mut chunks = outs.clone();
            if checkpoint.is_some() && self.fill_prefix {
                // A damage rect the lower-stack cache doesn't hold yet (a brush dab moving on):
                // render its whole chunk once, so the cache holds all of it and the dabs that
                // follow there skip the lower stack.
                let step = self.chunk as i32;
                let canvas_cell = level_rect(cr.intersect(&canvas), level).intersect(&lbounds);
                for c in &mut chunks {
                    if !self.prefix.contains(*c) {
                        let (x0, y0) = (c.x0.div_euclid(step) * step, c.y0.div_euclid(step) * step);
                        *c = Rect::new(x0, y0, x0 + step, y0 + step).intersect(&canvas_cell);
                    }
                }
            }
            stats.chunks += chunks.len();
            stats.pixels += lpart.width() as u64 * lpart.height() as u64;
            work += lpart.width() as u64 * lpart.height() as u64 * plan.passes.len() as u64;
            // The chunks render whole (`fill_prefix` grows them to chunk squares).
            let read = chunks.iter().fold(Rect::EMPTY, |a, c| {
                let d = unlevel_rect(*c, level);
                if a.is_empty() { d } else { a.union(&d) }
            });
            self.want = read.union(&part);
            let record = span("gpu.record");
            self.run_cell(device, queue, encoder, doc.id, &plan, &levels, cell, &fr, &chunks, &outs, checkpoint, level, &mut stats, sink);
            drop(record);
            self.want = Rect::EMPTY;
            let (evicted, bytes) = self.evict(false);
            stats.evicted += evicted;
            freed += bytes;
            if flush && (self.staged >= flush_bytes || work >= FLUSH_WORK || freed >= flush_bytes) {
                // Submit what's recorded, and wait for the previous submit, so at most two
                // batches of uploads and evicted pages are in flight.
                let done = std::mem::replace(encoder, device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_compose") }));
                let index = queue.submit([done.finish()]);
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(prev) = last_submit.replace(index) {
                    match &self.health {
                        Some(h) => {
                            h.wait(device, Some(prev));
                        }
                        None => {
                            let _ = device.poll(wgpu::PollType::Wait { submission_index: Some(prev), timeout: Some(health::GPU_WAIT_LIMIT) });
                        }
                    }
                }
                #[cfg(target_arch = "wasm32")]
                let _ = index;
                self.staged = 0;
                work = 0;
                freed = 0;
                stats.flushes += 1;
                // The device was lost mid-refresh: issue no more work.
                if let Some(f) = self.fault() {
                    return Err(Unsupported(f.to_string()));
                }
            }
        }

        // Evict this document's textures whose layers are gone (hidden layers stay resident so
        // toggling visibility costs no upload).
        let frame = self.frame;
        let live: std::collections::HashSet<LayerId> = doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
        self.residents.retain(|(id, _, _), r| r.doc != doc.id || r.last_used == frame || live.contains(id));
        self.fx.retain(|(id, ..), e| e.doc != doc.id || e.last_used == frame || live.contains(id));
        stats.evicted += self.evict(true).0;
        self.temps.retain(|(_, used)| frame.saturating_sub(*used) < 240);
        let lut_age = if self.lut_cache.len() > 64 { 1 } else { 240 };
        self.lut_cache.retain(|_, (_, _, used)| frame.saturating_sub(*used) < lut_age);
        Ok(stats)
    }

    /// Keep resident pages within the resident budget and the effect cache within
    /// its budget, least recently used first. Never drops what the current cell uses, nor
    /// whole-region effect maps drawn this frame until the render is `done`, nor while it isn't
    /// the maps of the view ([`Compositor::set_focus`]). Returns how many textures were dropped
    /// and their bytes.
    fn evict(&mut self, done: bool) -> (usize, u64) {
        self.evict_keeping(&std::collections::HashSet::new(), done)
    }

    /// [`Self::evict`], never letting go of the effect maps under `keep` either.
    fn evict_keeping(&mut self, keep: &std::collections::HashSet<FxKey>, done: bool) -> (usize, u64) {
        let (mut n, mut freed) = (0, 0u64);
        let mut total = self.resident_bytes();
        if total > self.resident_budget {
            let mut lru: Vec<(u64, ResKey, u64)> =
                self.residents.iter().filter(|(_, r)| r.stamp != self.stamp).map(|(k, r)| (r.stamp, *k, r.bytes())).collect();
            lru.sort_unstable_by_key(|c| c.0);
            for (_, k, bytes) in lru {
                if total <= self.resident_budget {
                    break;
                }
                self.residents.remove(&k);
                total = total.saturating_sub(bytes);
                n += 1;
                freed += bytes;
            }
        }
        let mut total = self.fx_cache_bytes();
        let fx_budget = usize::try_from(self.fx_budget).unwrap_or(usize::MAX);
        // The view's maps stay: a margin or a drag cache piece drawn after the view let go of
        // the view's maps, least recently used, and its next frame built them again (a phone's
        // 12 MB doesn't hold one 1024 px chunk's maps on a page of balloons, so every piece let
        // go of all it didn't draw). Every frame of the view draws them, so the cache holds them
        // even past its budget; what else was drawn goes once drawn, least recently used first:
        // maps off the view and at another level than the view's (a zoomed-out page refined at
        // full size let go of the view's reduced maps, a 16th of the memory each, built again on
        // its next edit). (The view's chunks are drawn whole: the layers they meet are the
        // view's.)
        let (focus, view_level) = (self.focus.map(|f| self.drawn_rect(f, 0)), self.focus_level);
        let in_view = |e: &FxEntry| focus.is_some_and(|f| !unlevel_rect(e.region, e.level).intersect(&f).is_empty());
        let views = |e: &FxEntry| focus.is_none() || (in_view(e) && e.level == view_level);
        let rank = |e: &FxEntry| (focus.is_none() || in_view(e), views(e), e.last_used, e.stamp);
        if total > fx_budget {
            // First what only building the maps again needs, least recently used first (the
            // maps keep drawing; an effect map set is a fraction of its distance fields and CPU
            // shape): at 4 GB the budget held two or three layers' whole state, and a drag cache
            // build on a page of many styled layers drew hundreds of maps again.
            let mut lru: Vec<((bool, bool, u64, u64), FxKey)> = self.fx.iter().filter(|(_, e)| !e.compact).map(|(k, e)| (rank(e), *k)).collect();
            lru.sort_unstable();
            for (_, k) in lru {
                if total <= fx_budget {
                    break;
                }
                if let Some(e) = self.fx.get_mut(&k) {
                    total = total.saturating_sub(e.compact());
                }
            }
        }
        if total > fx_budget {
            let (frame, stamp) = (self.frame, self.stamp);
            let mut lru: Vec<((bool, bool, u64, u64), FxKey)> = self
                .fx
                .iter()
                .filter(|(k, e)| {
                    let drawn = e.last_used == frame && (k.1.is_none() || e.stamp == stamp);
                    !keep.contains(*k) && if focus.is_none() { !drawn } else { !views(e) && (done || !drawn) }
                })
                .map(|(k, e)| (rank(e), *k))
                .collect();
            lru.sort_unstable_by_key(|c| c.0);
            for (_, k) in lru {
                if total <= fx_budget {
                    break;
                }
                if let Some(e) = self.fx.remove(&k) {
                    total = total.saturating_sub(e.bytes());
                    n += 1;
                    freed += e.bytes() as u64;
                }
            }
        }
        (n, freed)
    }

    /// Per-frame resources of `plan`: the tile area of each pass's surfaces, its LUT and pattern.
    fn frame_res(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        canvas: Rect,
        plan: &Plan<'_>,
        moved: &plan::Moved,
        moved_masks: &std::collections::HashSet<LayerId>,
    ) -> FrameRes {
        let grid = tile_grid(canvas);
        // Where a surface shows: moved in place layers (and their masks) at their offset.
        let area = |s: &Surface, layer: LayerId, mask: bool| {
            let b = s.tile_bounds();
            let b = match moved.get(&layer) {
                Some(&(dx, dy)) if !mask || moved_masks.contains(&layer) => b.translate(dx, dy),
                _ => b,
            };
            b.intersect(&grid)
        };
        let pass_areas = |p: &plan::Pass<'_>| {
            (
                p.tex.as_ref().map_or(Rect::EMPTY, |t| area(t.surface.get(), t.layer, false)),
                p.mask.as_ref().map_or(Rect::EMPTY, |m| area(m.surface.get(), m.layer, true)),
            )
        };
        let areas = plan.passes.iter().map(pass_areas).collect();
        let member_areas = plan.passes.iter().map(|p| p.members.iter().map(pass_areas).collect()).collect();
        let frame = self.frame;
        let luts = plan
            .passes
            .iter()
            .map(|p| {
                let rows = p.lut.as_ref()?;
                // The same table (an unchanged Curves or gradient: the plans share its rows) is
                // uploaded once, not per frame.
                let key = Arc::as_ptr(rows) as usize;
                let entry = self.lut_cache.entry(key).or_insert_with(|| (rows.clone(), lut_texture(device, queue, rows), frame));
                entry.2 = frame;
                Some(entry.1.clone())
            })
            .collect();
        let patterns = plan.passes.iter().map(|p| p.pattern.map(|pat| self.pattern_view(device, queue, pat))).collect();
        FrameRes { areas, member_areas, luts, patterns }
    }

    /// Bring the pages of `cell` that `plan` samples up to date and resolve its effect maps.
    #[allow(clippy::too_many_arguments)]
    fn bind_cell(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: DocId,
        plan: &Plan<'_>,
        whole: &[Option<u32>],
        cell: Cell,
        fr: &FrameRes,
        live: &[bool],
        stats: &mut Stats,
    ) -> Bound {
        let cr = cell_rect(cell, self.page);
        let mut views: Vec<(Option<ResidentRef>, Option<ResidentRef>)> = Vec::with_capacity(plan.passes.len());
        let mut member_views = Vec::with_capacity(plan.passes.len());
        let mut keys: Vec<ResKey> = Vec::new();
        let mut maps = Vec::with_capacity(plan.passes.len());
        let none = (Rect::EMPTY, Rect::EMPTY);
        for (i, p) in plan.passes.iter().enumerate() {
            if !live.get(i).copied().unwrap_or(false) {
                views.push((None, None));
                member_views.push(Vec::new());
                maps.push(None);
                continue;
            }
            let areas = fr.areas.get(i).copied().unwrap_or(none);
            views.push(self.sync_views(device, queue, encoder, doc, p, areas, cell, cr, &mut keys, stats));
            let member_areas = fr.member_areas.get(i);
            let mv = p
                .members
                .iter()
                .enumerate()
                .map(|(m, mp)| {
                    let areas = member_areas.and_then(|a| a.get(m)).copied().unwrap_or(none);
                    self.sync_views(device, queue, encoder, doc, mp, areas, cell, cr, &mut keys, stats)
                })
                .collect();
            member_views.push(mv);
            maps.push(p.map.and_then(|m| {
                let f = plan.fx.get(m.fx)?;
                let key = match whole.get(m.fx).copied().flatten() {
                    Some(level) => (f.layer.id, None, level),
                    None => (f.layer.id, Some(cell), 0),
                };
                let e = self.fx.get(&key)?;
                if m.item == plan::SHAPE_MAP {
                    return Some((e.shape.view.clone(), e.region, e.level));
                }
                let t = e.progs.get(m.item)?.maps.get(m.map)?.as_ref()?;
                Some((t.view.clone(), e.region, e.level))
            }));
        }
        Bound { views, member_views, keys, maps }
    }

    /// The resident pages of pass `p`'s texture and mask in `cell` (uploading what changed),
    /// as indices into `keys`.
    #[allow(clippy::too_many_arguments)]
    fn sync_views(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: DocId,
        p: &plan::Pass<'_>,
        (ta, ma): (Rect, Rect),
        cell: Cell,
        cr: Rect,
        keys: &mut Vec<ResKey>,
        stats: &mut Stats,
    ) -> (Option<ResidentRef>, Option<ResidentRef>) {
        let tex =
            p.tex.as_ref().and_then(|t| self.sync_shown(device, queue, encoder, doc, (t.layer, t.role, cell), t.surface.get(), ta.intersect(&cr), stats)).map(
                |(k, r)| {
                    keys.push(k);
                    (keys.len() - 1, r)
                },
            );
        let mask = p
            .mask
            .as_ref()
            .and_then(|m| self.sync_shown(device, queue, encoder, doc, (m.layer, Role::Mask, cell), m.surface.get(), ma.intersect(&cr), stats))
            .map(|(k, r)| {
                keys.push(k);
                (keys.len() - 1, r)
            });
        (tex, mask)
    }

    /// Run `plan` over the `chunks` of one page cell (document rects, at most a chunk square),
    /// handing each finished chunk to `sink`.
    #[allow(clippy::too_many_arguments)]
    fn run_cell(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: DocId,
        plan: &Plan<'_>,
        whole: &[Option<u32>],
        cell: Cell,
        fr: &FrameRes,
        chunks: &[Rect],
        outs: &[Rect],
        checkpoint: Option<plan::Checkpoint>,
        level: u32,
        stats: &mut Stats,
        sink: &mut dyn FnMut(&mut wgpu::CommandEncoder, ChunkOut<'_>),
    ) {
        if chunks.is_empty() {
            return;
        }
        // Pass clips at the level of detail (rounded out, the same for every pass).
        let clips: Vec<Option<Rect>> = plan.passes.iter().map(|p| p.clip.map(|c| level_rect(c, level))).collect();
        let first_pass = checkpoint.filter(|_| chunks.iter().all(|c| self.prefix.contains(*c))).map_or(0, |c| c.pass_end);
        // The passes these chunks record: past the lower-stack cache, and drawing somewhere in them
        // (a pass clipped away from every chunk records nothing). Only those get pages, uniforms
        // and bind groups: a piece of a page of 200 styled layers bound every layer's passes (two
        // to five per layer) whatever it met, which was most of a catch-up piece's cost.
        let live: Vec<bool> =
            clips.iter().enumerate().map(|(i, clip)| i >= first_pass && clip.is_none_or(|c| chunks.iter().any(|ch| !c.intersect(ch).is_empty()))).collect();
        stats.bound += live.iter().filter(|l| **l).count();
        let bind = span("gpu.bind");
        let bound = self.bind_cell(device, queue, encoder, doc, plan, whole, cell, fr, &live, stats);

        drop(bind);
        // Chunk pool of the smallest side that holds every chunk of this cell.
        let side = chunks.iter().map(|c| c.width().max(c.height())).max().unwrap_or(1).next_power_of_two().clamp(64, self.chunk.max(64));
        let mut pool = self.pools.remove(&side).unwrap_or_default();
        while pool.len() < plan.slots as usize {
            let t = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_compose_chunk"),
                size: wgpu::Extent3d { width: side, height: side, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.acc_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC
                    | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let v = t.create_view(&Default::default());
            pool.push((t, v));
        }

        // Uniforms: chunk records, one record per pass, then the member records of fused passes.
        let op_base = chunks.len() as u64 * STRIDE;
        let members_base = op_base + plan.passes.len() as u64 * STRIDE;
        let mut member_slot = vec![None; plan.passes.len()];
        let mut fused = 0u64;
        for (i, p) in plan.passes.iter().enumerate() {
            if live[i] && p.kernel == Kernel::Layers {
                member_slot[i] = Some(members_base + fused * MEMBERS_STRIDE);
                fused += 1;
            }
        }
        let len = members_base + fused * MEMBERS_STRIDE;
        let mut data = vec![0u8; len.max(if fused > 0 { MEMBERS_STRIDE } else { 0 }) as usize];
        for (i, c) in chunks.iter().enumerate() {
            let w = words(&[I(c.x0), I(c.y0), I(c.width() as i32), I(c.height() as i32), I(level as i32)]);
            data[i * STRIDE as usize..][..w.len()].copy_from_slice(&w);
        }
        for (i, p) in plan.passes.iter().enumerate() {
            if !live[i] {
                continue;
            }
            let (tex, mask) = bound.views[i];
            let map = bound.maps[i].as_ref().map(|(_, r, level)| (*r, *level));
            let w = op_words(p, tex.map(|t| t.1), mask.map(|m| m.1), map);
            let off = (op_base + i as u64 * STRIDE) as usize;
            data[off..][..w.len()].copy_from_slice(&w);
            if let Some(base) = member_slot[i] {
                for (m, mp) in p.members.iter().enumerate() {
                    let (tex, mask) = bound.member_views.get(i).and_then(|v| v.get(m)).copied().unwrap_or((None, None));
                    let w = op_words(mp, tex.map(|t| t.1), mask.map(|m| m.1), None);
                    let off = (base + m as u64 * OP_UNIFORM) as usize;
                    data[off..][..w.len()].copy_from_slice(&w);
                }
            }
        }
        let ubuf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_compose_uniforms"),
            size: data.len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&ubuf, 0, &data);
        let bg0 = uniform_group(device, &self.kit.bgl0, &ubuf);
        let bg0m = (fused > 0).then(|| layers_uniform_group(device, &self.kit.bgl0m, &ubuf));

        // Per-pass texture bind groups (chunk-independent: pool slots are reused per chunk).
        let dummy = &self.kit.dummy;
        let resident_views: Vec<&wgpu::TextureView> = bound.keys.iter().map(|k| self.residents.get(k).map_or(dummy, |r| &r.view)).collect();
        let mut bg1 = Vec::with_capacity(plan.passes.len());
        for (i, p) in plan.passes.iter().enumerate() {
            if !live[i] || p.kernel.entry().is_none() {
                bg1.push(None);
                continue;
            }
            let slot = |s: Option<u32>| s.map_or(dummy, |s| &pool[s as usize].1);
            if p.kernel == Kernel::Layers {
                let mut views = vec![slot(p.a)];
                for m in 0..plan::MAX_MEMBERS {
                    let (tex, mask) = bound.member_views.get(i).and_then(|v| v.get(m)).copied().unwrap_or((None, None));
                    views.push(tex.map_or(dummy, |(k, _)| resident_views[k]));
                    views.push(mask.map_or(dummy, |(k, _)| resident_views[k]));
                }
                let entries: Vec<wgpu::BindGroupEntry> = LAYERS_BINDINGS
                    .iter()
                    .zip(&views)
                    .map(|(&b, v)| wgpu::BindGroupEntry { binding: b, resource: wgpu::BindingResource::TextureView(v) })
                    .collect();
                bg1.push(Some(device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("pc_compose_layers"),
                    layout: &self.kit.bgl1m,
                    entries: &entries,
                })));
                continue;
            }
            let (tex, mask) = &bound.views[i];
            let tv = tex.map_or(slot(p.d), |(k, _)| resident_views[k]);
            let mv = mask.map_or(dummy, |(k, _)| resident_views[k]);
            let lv = fr.luts.get(i).and_then(Option::as_ref).map_or(dummy, |(_, v)| v);
            let map = bound.maps[i].as_ref().map_or(dummy, |(v, ..)| v);
            let pat = fr.patterns.get(i).and_then(Option::as_ref).unwrap_or(dummy);
            bg1.push(Some(texture_group(device, &self.kit.bgl1, &[slot(p.a), slot(p.b), tv, mv, lv, slot(p.c), map, pat])));
        }

        for (ci, c) in chunks.iter().enumerate() {
            let mut resume = 0;
            if let Some(cp) = checkpoint
                && let Some((dst, _)) = pool.get(cp.slot as usize)
                && self.prefix.copy_out(encoder, *c, dst)
            {
                resume = cp.pass_end;
                stats.prefix_hits = stats.prefix_hits.saturating_add(1);
                stats.prefix_passes_skipped = stats.prefix_passes_skipped.saturating_add(resume);
            }
            for (i, p) in plan.passes.iter().enumerate() {
                if i < resume {
                    continue;
                }
                if resume == 0
                    && let Some(cp) = checkpoint.filter(|cp| cp.pass_end == i)
                    && let Some((src, _)) = pool.get(cp.slot as usize)
                {
                    self.prefix.capture(device, encoder, *c, src);
                }
                let scissor = match clips.get(i).copied().flatten() {
                    Some(clip) => {
                        let r = clip.intersect(c);
                        if r.is_empty() {
                            continue;
                        }
                        Rect::new(r.x0 - c.x0, r.y0 - c.y0, r.x1 - c.x0, r.y1 - c.y0)
                    }
                    None => Rect::new(0, 0, c.width() as i32, c.height() as i32),
                };
                match p.kernel {
                    Kernel::CopyRect | Kernel::CopyFull => {
                        // The planner always gives copies a source.
                        let Some(a) = p.a else { continue };
                        let src = &pool[a as usize].0;
                        let dst = &pool[p.dst as usize].0;
                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: src,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: scissor.x0 as u32, y: scissor.y0 as u32, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: dst,
                                mip_level: 0,
                                origin: wgpu::Origin3d { x: scissor.x0 as u32, y: scissor.y0 as u32, z: 0 },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d { width: scissor.width(), height: scissor.height(), depth_or_array_layers: 1 },
                        );
                        continue;
                    }
                    _ => {}
                }
                let target = &pool[p.dst as usize].1;
                let clear = p.kernel == Kernel::Clear;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("pc_compose_pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: target,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: if clear { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load },
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                if clear {
                    continue;
                }
                let Ok(pipeline) = self.kit.pipeline(device, p.kernel, self.acc_format) else { continue };
                pass.set_pipeline(&pipeline);
                pass.set_scissor_rect(scissor.x0 as u32, scissor.y0 as u32, scissor.width(), scissor.height());
                match (member_slot[i], &bg0m) {
                    (Some(members), Some(bg)) => {
                        pass.set_bind_group(0, bg, &[(ci as u64 * STRIDE) as u32, (op_base + i as u64 * STRIDE) as u32, members as u32]);
                    }
                    _ => pass.set_bind_group(0, &bg0, &[(ci as u64 * STRIDE) as u32, (op_base + i as u64 * STRIDE) as u32]),
                }
                pass.set_bind_group(1, bg1[i].as_ref(), &[]);
                pass.draw(0..3, 0..1);
            }
            if resume == 0
                && let Some(cp) = checkpoint.filter(|cp| cp.pass_end == plan.passes.len())
                && let Some((src, _)) = pool.get(cp.slot as usize)
            {
                self.prefix.capture(device, encoder, *c, src);
            }
            let (t, v) = &pool[plan.root as usize];
            let out = outs.get(ci).copied().unwrap_or(*c);
            let origin = [(out.x0 - c.x0).max(0) as u32, (out.y0 - c.y0).max(0) as u32];
            sink(encoder, ChunkOut { rect: out, origin, level, texture: t, view: v });
        }
        self.pools.insert(side, pool);
    }

    /// Upload changed tiles of `surface` within `region` (its tile area inside one page cell);
    /// returns the resident key and its region (x, y, w, h).
    #[allow(clippy::too_many_arguments)]
    fn sync(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: DocId,
        key: ResKey,
        surface: &Surface,
        region: Rect,
        stats: &mut Stats,
    ) -> Option<(ResKey, [i32; 4])> {
        if region.is_empty() {
            return None;
        }
        let _s = span("gpu.upload");
        let format = surface.format();
        let kind = TexKind::for_surface(key.1, format);
        let cmyk = if format.mode == photocraft_color::ColorMode::Cmyk { self.cmyk } else { 0 };
        let default_pixel = surface.default_pixel();
        let default_bits: Vec<u32> = default_pixel.iter().map(|v| v.to_bits()).collect();
        let stale = self
            .residents
            .get(&key)
            .is_none_or(|r| r.region != region || r.kind != kind || r.format != format || r.cmyk != cmyk || r.default_bits != default_bits);
        if stale {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("pc_compose_layer"),
                size: wgpu::Extent3d { width: region.width(), height: region.height(), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: kind.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let default_nonzero = default_pixel.iter().any(|v| *v != 0.0);
            let mut r =
                Resident { texture, view, region, kind, format, tiles: HashMap::new(), default_nonzero, default_bits, doc, last_used: 0, stamp: 0, cmyk };
            // Only the region moved (painting past a layer's tiles grows it): keep the texels the
            // old texture already holds instead of converting and uploading every tile again.
            let kept = self.residents.remove(&key).map_or(Rect::EMPTY, |old| keep_overlap(device, queue, &old, &mut r));
            // A new texture starts zeroed: write the tiles that are present, and the default pixel
            // where they are missing if it isn't zero. Coordinates sharing a tile (a solid fill,
            // the default) take one upload and GPU copies (#1774). Kept tiles are brought up to
            // date below, like those of a resident that didn't move.
            let mut present = Vec::new();
            let mut missing = Vec::new();
            let want = self.want;
            for c in region.tiles().filter(|c| !kept.contains_rect(&c.rect())) {
                match surface.tile(c) {
                    // Outside the render's area: uploaded when a render reaches it (below).
                    Some(_) if !want.is_empty() && want.intersect(&c.rect()).is_empty() => {}
                    Some(t) => present.push((c, t.clone())),
                    None if default_nonzero => missing.push(c),
                    None => {}
                }
            }
            if !missing.is_empty() {
                let bytes = convert_tile(surface, None, kind, TileCoord::new(0, 0));
                self.staged += put_tiles(device, queue, encoder, &r, &missing, &bytes);
            }
            let staged = &mut self.staged;
            convert_tiles(surface, present, kind, |t, cs, bytes| {
                *staged += put_tiles(device, queue, encoder, &r, cs, bytes);
                for &c in cs {
                    stats.tiles_uploaded += 1;
                    stats.bytes_uploaded += bytes.len();
                    r.tiles.insert(c, t.clone());
                }
            });
            self.residents.insert(key, r);
        }
        // Present: inserted above when it was missing or stale.
        let r = self.residents.get_mut(&key)?;
        r.last_used = self.frame;
        r.stamp = self.stamp;
        r.doc = doc;
        let mut changed = Vec::new();
        let mut gone = Vec::new();
        let want = self.want;
        for c in region.tiles().filter(|c| want.is_empty() || !want.intersect(&c.rect()).is_empty()) {
            match surface.tile(c) {
                Some(t) => {
                    if !r.tiles.get(&c).is_some_and(|old| Arc::ptr_eq(old, t)) {
                        changed.push((c, t.clone()));
                    }
                }
                None => {
                    if r.tiles.remove(&c).is_some() {
                        gone.push(c);
                    }
                }
            }
        }
        if !gone.is_empty() {
            // A tile that went away reads as the default pixel again.
            let bytes = if r.default_nonzero {
                convert_tile(surface, None, kind, TileCoord::new(0, 0))
            } else {
                vec![0u8; (TILE_SIZE * TILE_SIZE) as usize * kind.bytes_per_pixel()]
            };
            self.staged += put_tiles(device, queue, encoder, r, &gone, &bytes);
        }
        let staged = &mut self.staged;
        convert_tiles(surface, changed, kind, |t, cs, bytes| {
            *staged += put_tiles(device, queue, encoder, r, cs, bytes);
            for &c in cs {
                stats.tiles_uploaded += 1;
                stats.bytes_uploaded += bytes.len();
                r.tiles.insert(c, t.clone());
            }
        });
        Some((key, [region.x0, region.y0, region.width() as i32, region.height() as i32]))
    }

    /// Premultiplied RGBA32F texture of a pattern as it paints in the document's mode (cached by
    /// pixel identity and mode).
    fn pattern_view(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, pat: &Pattern) -> wgpu::TextureView {
        let fp = pat.surface.tiles().fold((pat.width as u64) << 32 | pat.height as u64, |acc, (c, t)| {
            acc.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (Arc::as_ptr(t) as usize as u64) ^ ((c.tx as u64) << 20) ^ c.ty as u64
        });
        let mode = self.pattern_mode;
        let key = format!("{}\u{0}{}\u{0}{mode:?}", pat.id, pat.name);
        if let Some((f, t)) = self.patterns.get(&key)
            && *f == fp
        {
            return t.view.clone();
        }
        let (w, h) = (pat.width, pat.height);
        let px = photocraft_compose::pattern::pattern_rgba(pat, mode);
        let bytes: Vec<u8> = px.iter().flat_map(|q| [q[0] * q[3], q[1] * q[3], q[2] * q[3], q[3]]).flat_map(f32::to_le_bytes).collect();
        let t = Tex::new(device, "pc_pattern", w, h, wgpu::TextureFormat::Rgba32Float, wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST);
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 16), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        let v = t.view.clone();
        self.patterns.insert(key, (fp, t));
        v
    }

    /// An R32F temporary at least `region`-sized (the smallest that fits) not in `taken`.
    /// Stages only read what they wrote, inside the region, so a larger one is as good; page
    /// windows of different sizes share them.
    fn temp(&mut self, device: &wgpu::Device, region: Rect, taken: &[wgpu::TextureView]) -> wgpu::TextureView {
        let (w, h) = (region.width(), region.height());
        let frame = self.frame;
        let fit = self
            .temps
            .iter_mut()
            .filter(|(t, _)| {
                let s = t.texture.size();
                s.width >= w && s.height >= h && !taken.contains(&t.view)
            })
            .min_by_key(|(t, _)| t.texture.size().width as u64 * t.texture.size().height as u64);
        if let Some((t, used)) = fit {
            *used = frame;
            return t.view.clone();
        }
        let t = Tex::map(device, "pc_fx_temp", region, MAP32);
        let v = t.view.clone();
        self.temps.push((t, frame));
        v
    }

    /// Build or update the effect maps of one layer over `region`: its whole effect region, or
    /// the window of one page cell (`key.1`) grown by the effects' reach.
    #[allow(clippy::too_many_arguments)]
    fn sync_fx(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        doc: &Document,
        f: &plan::FxLayer<'_>,
        key: FxKey,
        region: Rect,
        stats: &mut Stats,
        mut ready: Option<Staged>,
    ) {
        let layer = f.layer;
        let canvas = doc.bounds();
        let is_group = matches!(layer.content, LayerContent::Group(_));
        let shape_key = if is_group { fx::group_key(layer, &doc.global_light) } else { fx::shape_key(layer, canvas) };
        let (content_src, mask_src) = if is_group { (None, None) } else { fx::shape_sources(layer) };
        let frame = self.frame;
        let (rw, rh) = (region.width(), region.height());
        // What `stage_fx` computed ahead for this region (its whole shape and fields).
        ready = ready.filter(|b| b.region == region && b.key == key);

        // Its maps' level of detail: `region` and everything below are in its pixels.
        let level = key.2;
        let progs = fx_programs(doc, f, level);
        let moved_from = if is_group { None } else { self.translated_from(layer.id, content_src, mask_src) };

        // What changed: everything (new settings / size), the position only, some tiles, or
        // nothing.
        let mut damage = Rect::EMPTY;
        let prev = self.fx.get(&key).filter(|e| e.shape_key == shape_key).map(|e| e.region);
        let mut rebuild = true;
        if prev == Some(region)
            && let Some(e) = self.fx.get(&key)
        {
            for (old, cur) in e.tiles.iter().zip([content_src, mask_src]) {
                let d = match (old, cur) {
                    (Some(o), Some(s)) => level_rect(fx::source_damage(layer, o, s), level),
                    (None, None) => Rect::EMPTY,
                    // A source appeared or vanished (the key changes with it too).
                    _ => region,
                };
                damage = if damage.is_empty() { d } else { damage.union(&d) };
            }
            damage = damage.intersect(&region);
            // A compacted entry has nothing to update its maps from: built again when they change.
            let same_progs = e.progs.len() == progs.len() && e.progs.iter().zip(&progs).all(|(a, b)| b.maps == 0 || (a.key == b.key && a.maps.len() == b.maps));
            rebuild = e.compact && (!damage.is_empty() || !same_progs);
        } else if let Some(old) = prev
            && (old.width(), old.height()) == (rw, rh)
            && !region.is_empty()
            && moved_from.is_some_and(|(from, at)| {
                self.fx.get(&key).and_then(|e| e.moved_from).is_some_and(|(was, at0)| {
                    was == from && (level > 0 || (at.0 - at0.0, at.1 - at0.1) == (region.x0 - old.x0, region.y0 - old.y0))
                })
            })
        {
            // Drawn translated from the surfaces its maps were made from, as far as the region
            // moved: the shape moved with it unchanged, so every map is still exact. Checking that
            // by computing the whole shape again ran in every frame of a Move drag of a styled
            // layer (the CPU's share of `gpu.fx` there). At a level of detail the maps move by
            // whole reduced pixels, within one of where the layer is (the frame is reduced too);
            // the drag's release builds them where it landed.
            if let Some(e) = self.fx.get_mut(&key) {
                e.region = region;
                rebuild = false;
            }
        } else if let Some(old) = prev
            && (old.width(), old.height()) == (rw, rh)
            && !region.is_empty()
        {
            // Moved by whole pixels: maps are computed relative to the region, so if the shape
            // moved unchanged with it, every map is still exact.
            let computed;
            let v: &[f32] = match &ready {
                Some(b) => &b.shape,
                None => {
                    computed = fx::shape_at(doc, layer, region, level);
                    &computed
                }
            };
            if let Some(e) = self.fx.get_mut(&key)
                && (if e.compact { hash_shape(v) == e.shape_hash } else { v == e.shape_cpu.as_slice() })
            {
                e.region = region;
                rebuild = false;
            }
        }
        // The region grew or shrank with the layer's own pixels (a keystroke adds a glyph): what
        // both regions share is kept; what changed in it and what the new region adds are
        // computed. Built again whole, every keystroke recomputed the shape and the distance
        // fields of the whole line on the CPU.
        if rebuild
            && ready.is_none()
            && let Some(old) = prev
            && !region.is_empty()
            && !old.intersect(&region).is_empty()
            && let Some(e) = self.fx.get_mut(&key).filter(|e| e.can_grow([content_src, mask_src]))
        {
            damage = e.grow_to(device, queue, layer, [content_src, mask_src], region);
            rebuild = false;
        }
        // The shape staged whole, on the CPU and the GPU: nothing to compute or upload.
        let mut shape_in = false;
        if rebuild {
            FX_BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let (shape, shape_cpu, staged) = fresh_shape(device, region, ready.as_mut());
            shape_in = staged;
            self.fx.insert(
                key,
                FxEntry {
                    doc: doc.id,
                    region,
                    level,
                    shape_key,
                    tiles: [None, None],
                    _pin: is_group.then(|| layer.clone()),
                    shape_cpu,
                    shape,
                    fields: HashMap::new(),
                    progs: Vec::new(),
                    last_used: frame,
                    stamp: 0,
                    compact: false,
                    shape_hash: 0,
                    moved_from: None,
                },
            );
            damage = region;
        }
        // Present: kept or (re)inserted above.
        let stamp = self.stamp;
        let Some(e) = self.fx.get_mut(&key) else { return };
        e.last_used = frame;
        e.stamp = stamp;
        e.doc = doc.id;
        e.tiles = [content_src.map(fx::snapshot), mask_src.map(fx::snapshot)];
        e.moved_from = moved_from;
        if region.is_empty() {
            return;
        }

        let t_trace = web_time_now();
        let mut staged = 0u64;
        // Shape over the damage (compose's own alpha).
        if !damage.is_empty() {
            stats.fx_shapes += 1;
            FX_UPDATES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // (A shape staged whole is there already.)
            staged += if shape_in { 0 } else { e.update_shape(queue, doc, layer, damage, ready.as_mut().filter(|_| damage == region)) };
        }

        if e.compact {
            // Kept or moved: the maps are as they were.
            return;
        }
        let want = field_reaches(&progs);
        e.fields.retain(|k, _| want.contains_key(k));
        for (&kind, &reach) in &want {
            let have = e.fields.get(&kind).map(|(r, _)| *r);
            let out = match have {
                Some(r) if r >= reach => {
                    if damage.is_empty() {
                        continue;
                    }
                    damage.inflate(fx::field_radius(kind, r)).intersect(&region)
                }
                _ => region,
            };
            let reach = have.filter(|r| *r >= reach).unwrap_or(reach);
            stats.fx_pixels += out.width() as u64 * out.height() as u64;
            // Staged whole: its texture is the field's.
            if let Some(t) = ready.as_mut().filter(|_| out == region).and_then(|b| b.take_field(kind, reach)) {
                e.fields.insert(kind, (reach, t));
                continue;
            }
            let v = fx::field(kind, reach, &e.shape_cpu, region, out);
            if have.is_none_or(|r| r < reach) {
                e.fields.insert(kind, (reach, Tex::map(device, "pc_fx_field", region, MAP32)));
            }
            if let Some((_, t)) = e.fields.get(&kind) {
                t.write_r32(queue, region, out, &v);
            }
            staged += v.len() as u64 * 4;
        }

        if let Some(t) = t_trace.filter(|_| !damage.is_empty()) {
            eprintln!("effect maps of `{}`: shape + distance fields over {damage:?} in {:.2} ms (CPU)", layer.name, t.elapsed().as_secs_f64() * 1000.0);
        }
        e.progs.truncate(progs.len());
        while e.progs.len() < progs.len() {
            e.progs.push(ProgState { key: 0, maps: Vec::new() });
        }
        self.staged += staged;
        for (i, prog) in progs.iter().enumerate() {
            if prog.maps == 0 {
                continue;
            }
            let Some(e) = self.fx.get_mut(&key) else { return };
            let same = e.progs[i].key == prog.key && e.progs[i].maps.len() == prog.maps;
            let d = if same { damage } else { region };
            if d.is_empty() {
                continue;
            }
            if !same {
                e.progs[i] = ProgState {
                    key: prog.key,
                    maps: (0..prog.maps).map(|k| prog.stages.iter().any(|s| s.out == Some(k)).then(|| Tex::map(device, "pc_fx_map", region, MAP16))).collect(),
                };
            }
            stats.fx_programs += 1;
            self.run_program(device, queue, encoder, key, i, prog, d, stats);
        }
    }

    /// Record the passes of `prog` (maps of item `item` of `layer`) recomputing damage `d`.
    #[allow(clippy::too_many_arguments)]
    fn run_program(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        key: FxKey,
        item: usize,
        prog: &fx::MapProgram,
        d: Rect,
        stats: &mut Stats,
    ) {
        let pattern_views: Vec<Option<wgpu::TextureView>> =
            prog.stages.iter().map(|s| s.pattern.as_ref().map(|p| self.pattern_view(device, queue, p))).collect();
        let Some(e) = self.fx.get(&key) else { return };
        let region = e.region;
        let shape = e.shape.view.clone();
        let finals: Vec<Option<wgpu::TextureView>> = e.progs[item].maps.iter().map(|m| m.as_ref().map(|t| t.view.clone())).collect();
        let fields: HashMap<FieldKind, (i32, wgpu::TextureView)> = e.fields.iter().map(|(k, (r, t))| (*k, (*r, t.view.clone()))).collect();
        let windows = prog.windows(d, region, &|k| fields.get(&k).map_or(0, |f| f.0));
        let (assign, count) = prog.temps();
        let mut temps: Vec<wgpu::TextureView> = Vec::with_capacity(count);
        for _ in 0..count {
            let v = self.temp(device, region, &temps);
            temps.push(v);
        }
        // `None` only if a program's map or temporary wasn't allocated, which `sync_fx` and
        // `MapProgram::temps` rule out.
        let target_of = |i: usize| -> Option<(wgpu::TextureView, wgpu::TextureFormat)> {
            match prog.stages[i].out {
                Some(k) => Some((finals.get(k)?.clone()?, MAP16)),
                None => Some((temps.get((*assign.get(i)?)?)?.clone(), MAP32)),
            }
        };
        let view_of = |inp: Option<fx::In>| -> Option<wgpu::TextureView> {
            match inp? {
                fx::In::Shape => Some(shape.clone()),
                fx::In::Field(k) => fields.get(&k).map(|f| f.1.clone()),
                fx::In::Val(k) => target_of(k).map(|t| t.0),
            }
        };
        let mut draws = Vec::with_capacity(prog.stages.len());
        for (i, s) in prog.stages.iter().enumerate() {
            let w = windows[i];
            if w.is_empty() {
                continue;
            }
            stats.fx_pixels += w.width() as u64 * w.height() as u64;
            let Some((target, format)) = target_of(i) else { continue };
            let mut p = plan::Pass::new(s.kernel, 0);
            p.params[0] = s.p0;
            p.params[1] = s.p1;
            p.params[3] = s.p3;
            p.extra = s.p4;
            draws.push(MapDraw {
                kernel: s.kernel,
                format,
                target,
                scissor: [(w.x0 - region.x0) as u32, (w.y0 - region.y0) as u32, w.width(), w.height()],
                space: [region.x0, region.y0, region.width() as i32, region.height() as i32],
                op: op_words(&p, None, None, None),
                views: [view_of(s.a), view_of(s.b), view_of(s.s), None, None, None, None, pattern_views[i].clone()],
                lut: s.lut.clone(),
            });
        }
        self.kit.run(device, queue, encoder, &draws);
    }
}

/// A hash of an effect shape's coverage (a compacted entry's stand-in for it).
fn hash_shape(v: &[f32]) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_usize(v.len());
    for x in v {
        h.write_u32(x.to_bits());
    }
    h.finish()
}

/// A start time when `PHOTOCRAFT_FX_TRACE=1` (it prints the CPU time of effect shapes and
/// distance fields); never on wasm.
fn web_time_now() -> Option<std::time::Instant> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        T.get_or_init(|| std::env::var_os("PHOTOCRAFT_FX_TRACE").is_some()).then(std::time::Instant::now)
    }
    #[cfg(target_arch = "wasm32")]
    None
}

/// The uniform record of a pass.
/// `map`: the effect map's region and level of detail (in whose pixels the region is).
fn op_words(p: &plan::Pass<'_>, tex: Option<[i32; 4]>, mask: Option<[i32; 4]>, map: Option<(Rect, u32)>) -> Vec<u8> {
    let mut flags = p.flags;
    let (mut to, mut ts, mut mo, mut ms) = ([0; 2], [0; 2], [0; 2], [0; 2]);
    if let Some(r) = tex {
        flags |= 4;
        to = [r[0], r[1]];
        ts = [r[2], r[3]];
    }
    let (mut density, mut mdefault) = (0.0, 1.0);
    if let Some(m) = &p.mask {
        flags |= 1;
        density = m.density;
        mdefault = m.default;
        if let Some(r) = mask {
            flags |= 2;
            mo = [r[0], r[1]];
            ms = [r[2], r[3]];
        }
    }
    if p.gradient {
        flags |= 8;
    }
    let (mpo, mps) = map.map_or(([0, 0], [0, 0]), |(r, _)| ([r.x0, r.y0], [r.width() as i32, r.height() as i32]));
    let mut v = vec![
        I(plan::mode_index(p.mode)),
        I(p.adjust_kind),
        U(flags),
        U(map.map_or(0, |(_, level)| level)),
        F(p.opacity),
        F(density),
        F(mdefault),
        F(p.fill),
        I(to[0]),
        I(to[1]),
        I(ts[0]),
        I(ts[1]),
        I(mo[0]),
        I(mo[1]),
        I(ms[0]),
        I(ms[1]),
    ];
    v.extend(p.color.iter().map(|f| F(*f)));
    for row in &p.params {
        v.extend(row.iter().map(|f| F(*f)));
    }
    v.extend([I(mpo[0]), I(mpo[1]), I(mps[0]), I(mps[1])]);
    v.extend(p.extra.iter().map(|f| F(*f)));
    words(&v)
}

/// Canvas rounded out to whole tiles.
fn tile_grid(canvas: Rect) -> Rect {
    let t = TILE_SIZE;
    Rect::new(canvas.x0.div_euclid(t) * t, canvas.y0.div_euclid(t) * t, (canvas.x1 + t - 1).div_euclid(t) * t, (canvas.y1 + t - 1).div_euclid(t) * t)
}

fn write_tile(queue: &wgpu::Queue, r: &Resident, c: TileCoord, bytes: &[u8]) {
    let tr = c.rect();
    let bpp = r.kind.bytes_per_pixel() as u32;
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &r.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: (tr.x0 - r.region.x0) as u32, y: (tr.y0 - r.region.y0) as u32, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(TILE_SIZE as u32 * bpp), rows_per_image: Some(TILE_SIZE as u32) },
        wgpu::Extent3d { width: TILE_SIZE as u32, height: TILE_SIZE as u32, depth_or_array_layers: 1 },
    );
}

/// Distinct tiles converted per parallel batch: bounds the converted bytes held at once (64 tiles
/// of a 16-bit page are 32 MiB).
const CONVERT_BATCH: usize = 64;

/// Write the same tile `bytes` at every coordinate in `cs` of resident `r`; returns the bytes
/// staged. Several coordinates (a solid fill or a cleared area shares one tile) get one upload to
/// a scratch tile and GPU copies from it: writing each one from the CPU sent 387 MB per refresh
/// of a filled 50 MP 16-bit page (#1774).
fn put_tiles(device: &wgpu::Device, queue: &wgpu::Queue, encoder: &mut wgpu::CommandEncoder, r: &Resident, cs: &[TileCoord], bytes: &[u8]) -> u64 {
    if let [c] = cs {
        write_tile(queue, r, *c, bytes);
        return bytes.len() as u64;
    }
    if cs.is_empty() {
        return 0;
    }
    let extent = wgpu::Extent3d { width: TILE_SIZE as u32, height: TILE_SIZE as u32, depth_or_array_layers: 1 };
    // Its own texture per call: queued writes land before the encoder runs, so a shared scratch
    // would hold only the last tile by then.
    let scratch = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pc_tile_scratch"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: r.kind.format(),
        usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &scratch, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(TILE_SIZE as u32 * r.kind.bytes_per_pixel() as u32),
            rows_per_image: Some(TILE_SIZE as u32),
        },
        extent,
    );
    for c in cs {
        let tr = c.rect();
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo { texture: &scratch, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyTextureInfo {
                texture: &r.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: (tr.x0 - r.region.x0) as u32, y: (tr.y0 - r.region.y0) as u32, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            extent,
        );
    }
    bytes.len() as u64
}

/// Copy the texels `old` holds inside the region of its replacement `new` (same texel kind,
/// format and default pixel; only the region moved) and carry over the tiles they show, so only
/// tiles outside the overlap or changed since need uploading. Returns the kept (tile-aligned)
/// rect; empty when nothing can be kept.
fn keep_overlap(device: &wgpu::Device, queue: &wgpu::Queue, old: &Resident, new: &mut Resident) -> Rect {
    let same = old.kind == new.kind && old.format == new.format && old.cmyk == new.cmyk && old.default_bits == new.default_bits;
    let k = old.region.intersect(&new.region);
    if !same || k.is_empty() {
        return Rect::EMPTY;
    }
    // Its own submission: tile writes go through the queue and land at the start of the next
    // submit, before anything in the frame's encoder, so a copy recorded there would overwrite
    // the tiles written after it with the old texels.
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_compose_keep") });
    encoder.copy_texture_to_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &old.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: (k.x0 - old.region.x0) as u32, y: (k.y0 - old.region.y0) as u32, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyTextureInfo {
            texture: &new.texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: (k.x0 - new.region.x0) as u32, y: (k.y0 - new.region.y0) as u32, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d { width: k.width(), height: k.height(), depth_or_array_layers: 1 },
    );
    queue.submit([encoder.finish()]);
    new.tiles = old.tiles.iter().filter(|(c, _)| k.contains_rect(&c.rect())).map(|(c, t)| (*c, t.clone())).collect();
    k
}

/// Convert `tiles` of `surface` to `kind` texels and hand each distinct tile, the coordinates
/// sharing it and its bytes to `put`. Each distinct tile is converted once (a solid fill shares
/// one `Arc` across every coordinate it covers), in parallel batches. Converting every tile one
/// after another on the UI thread froze a 50 MP 16-bit document for about 1.4 s after a
/// full-canvas fill (#1774). Returns how many tiles were converted.
fn convert_tiles(surface: &Surface, tiles: Vec<(TileCoord, Arc<Tile>)>, kind: TexKind, mut put: impl FnMut(&Arc<Tile>, &[TileCoord], &[u8])) -> usize {
    let mut groups: Vec<(Arc<Tile>, Vec<TileCoord>)> = Vec::new();
    let mut index: HashMap<*const Tile, usize> = HashMap::new();
    for (c, t) in tiles {
        match index.get(&Arc::as_ptr(&t)).and_then(|&i| groups.get_mut(i)) {
            Some(g) => g.1.push(c),
            None => {
                index.insert(Arc::as_ptr(&t), groups.len());
                groups.push((t, vec![c]));
            }
        }
    }
    for batch in groups.chunks(CONVERT_BATCH) {
        // A tile's content is the same at every coordinate sharing it: convert it at the first.
        let jobs: Vec<_> = batch.iter().filter_map(|(t, cs)| cs.first().map(|c| (t, *c))).collect();
        let converted = fx::par_map(jobs, |(t, c)| convert_tile(surface, Some(t), kind, c));
        for ((t, cs), bytes) in batch.iter().zip(&converted) {
            put(t, cs, bytes);
        }
    }
    groups.len()
}

/// Tile pixels in the texture's format. `tile = None` gives the default pixel everywhere.
fn convert_tile(surface: &Surface, tile: Option<&Arc<Tile>>, kind: TexKind, c: TileCoord) -> Vec<u8> {
    if let (Some(t), TexKind::Rgba8Direct | TexKind::R8Direct) = (tile, kind) {
        return t.bytes().to_vec();
    }
    let n = (TILE_SIZE * TILE_SIZE) as usize;
    let fmt = surface.format();
    let ch = fmt.channels();
    if let (Some(t), TexKind::Rgba16F) = (tile, kind)
        && let Some(out) = rgb_gray16_to_f16(&fmt, t.bytes()).or_else(|| rgb_gray32_to_f16(&fmt, t.bytes()))
    {
        return out;
    }
    let raw: Vec<f32> = match tile {
        Some(_) => surface.read_region(c.rect()),
        None => surface.default_pixel().repeat(n),
    };
    let mut out = Vec::with_capacity(n * kind.bytes_per_pixel());
    for px in raw.chunks_exact(ch) {
        match kind {
            TexKind::Rgba8Direct | TexKind::Rgba8 => {
                let v = photocraft_raster::to_rgba(&fmt, px);
                out.extend(v.map(|x| (x.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
            }
            TexKind::Rgba16F => {
                let v = photocraft_raster::to_rgba(&fmt, px);
                for x in v {
                    out.extend(f32_to_f16(x).to_le_bytes());
                }
            }
            TexKind::R8Direct => out.push((px[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8),
            TexKind::R32F => out.extend(px[0].to_le_bytes()),
        }
    }
    out
}

/// RGBA16F texels of a 16-bit RGB or grayscale tile, through a table of every 16-bit sample's
/// half: the same bytes as the general path at a fraction of its cost (a 50 MP 16-bit refresh
/// converts dozens of tiles). `None` for other formats.
fn rgb_gray16_to_f16(fmt: &PixelFormat, bytes: &[u8]) -> Option<Vec<u8>> {
    use photocraft_color::ColorMode;
    static HALF: std::sync::OnceLock<Box<[u16]>> = std::sync::OnceLock::new();
    if fmt.sample != SampleType::U16 {
        return None;
    }
    let gray = match fmt.mode {
        ColorMode::Rgb => false,
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => true,
        _ => return None,
    };
    let half = HALF.get_or_init(|| (0..=u16::MAX).map(|v| f32_to_f16(v as f32 / 65535.0)).collect());
    let one = f32_to_f16(1.0).to_le_bytes();
    let ch = fmt.channels();
    let mut out = Vec::with_capacity((TILE_SIZE * TILE_SIZE) as usize * 8);
    for px in bytes.chunks_exact(ch * 2) {
        let h = |i: usize| -> [u8; 2] {
            let v = px.get(i * 2..i * 2 + 2).map_or(0, |b| u16::from_ne_bytes([b[0], b[1]]));
            half.get(v as usize).copied().unwrap_or(0).to_le_bytes()
        };
        let (r, g, b) = if gray { (h(0), h(0), h(0)) } else { (h(0), h(1), h(2)) };
        let a = if fmt.alpha { h(ch - 1) } else { one };
        out.extend_from_slice(&r);
        out.extend_from_slice(&g);
        out.extend_from_slice(&b);
        out.extend_from_slice(&a);
    }
    Some(out)
}

/// RGBA16F texels of a 32-bit float RGB or grayscale tile, straight from its samples: the same
/// bytes as the general path without decoding the tile into a float buffer and converting it
/// pixel by pixel (a brush dab on a 32-bit layer re-uploads the tiles it touched). `None` for
/// other formats.
fn rgb_gray32_to_f16(fmt: &PixelFormat, bytes: &[u8]) -> Option<Vec<u8>> {
    use photocraft_color::ColorMode;
    if fmt.sample != SampleType::F32 {
        return None;
    }
    let gray = match fmt.mode {
        ColorMode::Rgb => false,
        ColorMode::Grayscale | ColorMode::Bitmap | ColorMode::Duotone => true,
        _ => return None,
    };
    let one = f32_to_f16(1.0).to_le_bytes();
    let ch = fmt.channels();
    let mut out = Vec::with_capacity((TILE_SIZE * TILE_SIZE) as usize * 8);
    for px in bytes.chunks_exact(ch * 4) {
        let h = |i: usize| -> [u8; 2] {
            let v = px.get(i * 4..i * 4 + 4).map_or(0.0, |b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]));
            f32_to_f16(v).to_le_bytes()
        };
        let (r, g, b) = if gray {
            let v = h(0);
            (v, v, v)
        } else {
            (h(0), h(1), h(2))
        };
        let a = if fmt.alpha { h(ch - 1) } else { one };
        out.extend_from_slice(&r);
        out.extend_from_slice(&g);
        out.extend_from_slice(&b);
        out.extend_from_slice(&a);
    }
    Some(out)
}

fn lut_texture(device: &wgpu::Device, queue: &wgpu::Queue, rows: &[[f32; 4096]]) -> (wgpu::Texture, wgpu::TextureView) {
    let t = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("pc_compose_lut"),
        size: wgpu::Extent3d { width: 4096, height: rows.len() as u32, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let bytes: Vec<u8> = rows.iter().flat_map(|r| r.iter().flat_map(|v| v.to_le_bytes())).collect();
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        &bytes,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4096 * 4), rows_per_image: Some(rows.len() as u32) },
        wgpu::Extent3d { width: 4096, height: rows.len() as u32, depth_or_array_layers: 1 },
    );
    let v = t.create_view(&Default::default());
    (t, v)
}

#[derive(Clone, Copy)]
enum W {
    I(i32),
    U(u32),
    F(f32),
}
use W::{F, I, U};

fn words(v: &[W]) -> Vec<u8> {
    v.iter()
        .flat_map(|w| match w {
            I(i) => i.to_le_bytes(),
            U(u) => u.to_le_bytes(),
            F(f) => f.to_le_bytes(),
        })
        .collect()
}

/// IEEE half from f32 (round to nearest even; overflow → inf).
pub fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xff) as i32;
    let man = b & 0x7f_ffff;
    if exp == 0xff {
        return sign | 0x7c00 | if man != 0 { 0x200 } else { 0 };
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = 1u32 << (shift - 1);
        let rounded = (m + half - 1 + ((m >> shift) & 1)) >> shift;
        return sign | rounded as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rest = man & 0x1fff;
    if rest > 0x1000 || (rest == 0x1000 && (h & 1) == 1) {
        h += 1;
    }
    sign | h as u16
}

/// f32 from IEEE half.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let man = (h & 0x3ff) as f32;
    match exp {
        0 => sign * man * 2f32.powi(-24),
        0x1f => {
            if man == 0.0 {
                sign * f32::INFINITY
            } else {
                f32::NAN
            }
        }
        e => sign * (1.0 + man / 1024.0) * 2f32.powi(e - 15),
    }
}

/// Blocking readback of a composite (native tests and tools).
#[cfg(not(target_arch = "wasm32"))]
pub fn render_to_vec(comp: &mut Compositor, device: &wgpu::Device, queue: &wgpu::Queue, doc: &Document, rect: Rect) -> Result<Vec<[f32; 4]>, Unsupported> {
    render_to_vec_stats(comp, device, queue, doc, rect).map(|(v, _)| v)
}

/// [`render_to_vec`] plus the render's [`Stats`].
#[cfg(not(target_arch = "wasm32"))]
pub fn render_to_vec_stats(
    comp: &mut Compositor,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    doc: &Document,
    rect: Rect,
) -> Result<(Vec<[f32; 4]>, Stats), Unsupported> {
    render_to_vec_lod(comp, device, queue, doc, rect, 0)
}

/// [`render_to_vec_stats`] at a level of detail ([`Compositor::render_lod`]): the pixels of
/// [`level_rect`] of `rect`, within [`level_bounds`], row-major.
#[cfg(not(target_arch = "wasm32"))]
pub fn render_to_vec_lod(
    comp: &mut Compositor,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    doc: &Document,
    rect: Rect,
    level: u32,
) -> Result<(Vec<[f32; 4]>, Stats), Unsupported> {
    let level = level.min(MAX_LOD);
    let doc_rect = rect.intersect(&doc.bounds());
    let rect = level_rect(doc_rect, level).intersect(&level_bounds(doc.bounds(), level));
    let mut staging: Vec<(Rect, wgpu::Buffer, u32)> = Vec::new();
    let mut bpp = 16u32;
    let stats = comp.render_lod(device, queue, doc, doc_rect, level, |enc, out| {
        // Bytes per pixel of the accumulation format (Rgba32Float = 16, Rgba16Float = 8).
        bpp = if out.texture.format() == wgpu::TextureFormat::Rgba32Float { 16 } else { 8 };
        let row = (out.rect.width() * bpp).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("pc_readback"),
            size: (row * out.rect.height()) as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: out.texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x: out.origin[0], y: out.origin[1], z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buf,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(out.rect.height()) },
            },
            wgpu::Extent3d { width: out.rect.width(), height: out.rect.height(), depth_or_array_layers: 1 },
        );
        staging.push((out.rect, buf, row));
    })?;
    for (_, b, _) in &staging {
        b.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    }
    match &comp.health {
        Some(h) if !h.wait(device, None) => return Err(Unsupported(h.fault().map_or_else(|| "GPU device lost".into(), |f| f.to_string()))),
        Some(_) => {}
        None => {
            // Not mapped unless the wait finished: reading them then would panic.
            device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(health::GPU_WAIT_LIMIT) }).map_err(|e| Unsupported(format!("GPU wait failed: {e}")))?;
        }
    }
    let w = rect.width() as usize;
    let mut out = vec![[0.0f32; 4]; w * rect.height() as usize];
    for (r, b, row) in &staging {
        let data = b.slice(..).get_mapped_range().map_err(|e| Unsupported(format!("GPU readback failed: {e:?}")))?;
        for y in 0..r.height() as usize {
            for x in 0..r.width() as usize {
                let o = y * *row as usize + x * bpp as usize;
                let px: [f32; 4] = if bpp == 16 {
                    std::array::from_fn(|i| f32::from_le_bytes([data[o + i * 4], data[o + i * 4 + 1], data[o + i * 4 + 2], data[o + i * 4 + 3]]))
                } else {
                    std::array::from_fn(|i| half::f16::from_le_bytes([data[o + i * 2], data[o + i * 2 + 1]]).to_f32())
                };
                let (dx, dy) = ((r.x0 - rect.x0) as usize + x, (r.y0 - rect.y0) as usize + y);
                out[dy * w + dx] = px;
            }
        }
    }
    Ok((out, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_roundtrip() {
        for v in [0.0f32, 1.0, 0.5, 0.25, 1.0 / 255.0, 0.333, 65504.0, -2.5, 1e-6] {
            let r = f16_to_f32(f32_to_f16(v));
            assert!((r - v).abs() <= v.abs() / 1024.0 + 1e-7, "{v} -> {r}");
        }
    }

    #[test]
    fn shader_validates() {
        let module = wgpu::naga::front::wgsl::parse_str(SHADER).unwrap_or_else(|e| panic!("{}", e.emit_to_string(SHADER)));
        wgpu::naga::valid::Validator::new(wgpu::naga::valid::ValidationFlags::all(), wgpu::naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
        for k in Kernel::DRAWN {
            let e = k.entry().unwrap();
            assert!(module.entry_points.iter().any(|ep| ep.name == e), "missing entry point {e}");
        }
    }

    /// The decimal float literals in `src` (comments skipped; hex literals are exact, so skipped),
    /// with their line numbers.
    fn decimal_float_literals(src: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        for (n, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("").as_bytes();
            let mut i = 0;
            while i < code.len() {
                let digit_or_dot = code[i].is_ascii_digit() || (code[i] == b'.' && code.get(i + 1).is_some_and(u8::is_ascii_digit));
                let word_before = i > 0 && (code[i - 1].is_ascii_alphanumeric() || code[i - 1] == b'_' || code[i - 1] == b'.');
                if !digit_or_dot || word_before {
                    i += 1;
                    continue;
                }
                if code[i] == b'0' && matches!(code.get(i + 1), Some(b'x' | b'X')) {
                    i += 2;
                    while i < code.len() && (code[i].is_ascii_hexdigit() || matches!(code[i], b'.' | b'p' | b'P' | b'+' | b'-')) {
                        i += 1;
                    }
                    continue;
                }
                let start = i;
                while i < code.len() && (code[i].is_ascii_digit() || code[i] == b'.') {
                    i += 1;
                }
                if i < code.len() && matches!(code[i], b'e' | b'E') {
                    let mut j = i + 1;
                    if j < code.len() && matches!(code[j], b'+' | b'-') {
                        j += 1;
                    }
                    if j < code.len() && code[j].is_ascii_digit() {
                        i = j;
                        while i < code.len() && code[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                out.push((n + 1, String::from_utf8_lossy(&code[start..i]).into_owned()));
            }
        }
        out
    }

    /// Browsers' WebGPU shader compiler (Tint) rejects a float literal beyond f32's finite range
    /// even when it would round to `f32::MAX` (naga, used natively, rounds it), and then no
    /// compositor pipeline builds and the web app falls back to the CPU renderer. `3.40282347e38`
    /// was such a literal.
    #[test]
    fn shader_float_literals_fit_f32() {
        let lits = decimal_float_literals(SHADER);
        assert!(lits.iter().any(|(_, l)| l == "1.0"), "the scanner finds literals: {lits:?}");
        let bad: Vec<_> = lits.iter().filter(|(_, l)| l.parse::<f64>().map_or(true, |v| !v.is_finite() || v.abs() > f64::from(f32::MAX))).collect();
        assert!(bad.is_empty(), "float literals outside f32's finite range (line, literal): {bad:?}");
    }

    #[test]
    fn op_record_fits_the_uniform() {
        let p = plan::Pass::new(Kernel::FxPaint, 0);
        assert_eq!(op_words(&p, None, None, None).len() as u64, OP_UNIFORM);
    }

    /// #1774: a solid fill shares one tile across every coordinate; it is converted once, and
    /// every coordinate (shared or not) still gets exactly the bytes `convert_tile` gives it, at
    /// every depth and texture kind.
    #[test]
    fn convert_tiles_converts_each_distinct_tile_once() {
        let area = Rect::new(0, 0, TILE_SIZE * 9, TILE_SIZE * 9);
        for (fmt, kind) in [
            (PixelFormat::RGBA8, TexKind::Rgba8Direct),
            (PixelFormat { alpha: false, ..PixelFormat::RGBA8 }, TexKind::Rgba8),
            (PixelFormat::RGBA16, TexKind::Rgba16F),
            (PixelFormat::RGBA32F, TexKind::Rgba16F),
            (PixelFormat::GRAY8, TexKind::R8Direct),
            (PixelFormat { sample: SampleType::U16, ..PixelFormat::GRAY8 }, TexKind::R32F),
        ] {
            let mut s = Surface::new(fmt);
            let px = [0.2, 0.4, 0.6, 1.0];
            let n = fmt.channels();
            // Fully covered tiles share one `Arc`; then two tiles get their own pixels.
            s.fill_rect(area, &px[..n]);
            s.fill_rect(Rect::new(3, 5, 40, 60), &[0.9, 0.1, 0.3, 0.5][..n]);
            s.fill_rect(Rect::new(TILE_SIZE * 4 + 7, TILE_SIZE * 2, TILE_SIZE * 4 + 9, TILE_SIZE * 2 + 3), &[0.0; 4][..n]);
            let tiles: Vec<_> = area.tiles().filter_map(|c| s.tile(c).map(|t| (c, t.clone()))).collect();
            assert_eq!(tiles.len(), 81, "{fmt:?}");
            let mut seen = Vec::new();
            let converted = convert_tiles(&s, tiles, kind, |t, cs, bytes| {
                for &c in cs {
                    assert!(s.tile(c).is_some_and(|own| Arc::ptr_eq(own, t)), "{fmt:?} {c:?}");
                    assert_eq!(bytes, convert_tile(&s, Some(t), kind, c).as_slice(), "{fmt:?} {c:?}");
                    seen.push(c);
                }
            });
            assert_eq!(converted, 3, "{fmt:?}: the shared tile once, plus the two edited ones");
            seen.sort_by_key(|c| (c.ty, c.tx));
            let mut want: Vec<_> = area.tiles().collect();
            want.sort_by_key(|c| (c.ty, c.tx));
            assert_eq!(seen, want, "{fmt:?}: every coordinate once");
        }
        // More distinct tiles than one batch: all converted, each coordinate once.
        let mut s = Surface::new(PixelFormat::RGBA16);
        let wide = Rect::new(0, 0, TILE_SIZE * (CONVERT_BATCH as i32 + 5), TILE_SIZE);
        for c in wide.tiles() {
            s.fill_rect(Rect::new(c.rect().x0, 0, c.rect().x0 + 1, 1), &[c.tx as f32 / 100.0, 0.0, 0.0, 1.0]);
        }
        let tiles: Vec<_> = wide.tiles().filter_map(|c| s.tile(c).map(|t| (c, t.clone()))).collect();
        let mut n = 0;
        let converted = convert_tiles(&s, tiles, TexKind::Rgba16F, |t, cs, bytes| {
            for &c in cs {
                assert_eq!(bytes, convert_tile(&s, Some(t), TexKind::Rgba16F, c).as_slice());
                n += 1;
            }
        });
        assert_eq!((converted, n), (CONVERT_BATCH + 5, CONVERT_BATCH + 5));
    }

    /// The 16-bit and 32-bit float RGB/gray fast paths give exactly the general path's half
    /// floats (32-bit samples above 1 and below 0 included).
    #[test]
    fn sixteen_and_thirty_two_bit_fast_paths_match_the_general_one() {
        for fmt in [
            PixelFormat::RGBA16,
            PixelFormat { alpha: false, ..PixelFormat::RGBA16 },
            PixelFormat { sample: SampleType::U16, ..PixelFormat::GRAY8 },
            PixelFormat { sample: SampleType::U16, alpha: true, ..PixelFormat::GRAY8 },
            PixelFormat::RGBA32F,
            PixelFormat { alpha: false, ..PixelFormat::RGBA32F },
            PixelFormat { sample: SampleType::F32, ..PixelFormat::GRAY8 },
            PixelFormat { sample: SampleType::F32, alpha: true, ..PixelFormat::GRAY8 },
        ] {
            let mut s = Surface::new(fmt);
            let r = Rect::new(0, 0, TILE_SIZE, TILE_SIZE);
            let n = (TILE_SIZE * TILE_SIZE) as usize * fmt.channels();
            let hdr = if fmt.sample == SampleType::F32 { 4.0 } else { 1.0 };
            let data: Vec<f32> = (0..n).map(|i| (i as u32).wrapping_mul(2_654_435_761) as f32 / u32::MAX as f32 * hdr - (hdr - 1.0) / 4.0).collect();
            s.write_region(r, &data);
            let t = s.tile(TileCoord::new(0, 0)).expect("written").clone();
            let fast = rgb_gray16_to_f16(&fmt, t.bytes()).or_else(|| rgb_gray32_to_f16(&fmt, t.bytes())).expect("fast path");
            let raw = s.read_region(r);
            let general: Vec<u8> =
                raw.chunks_exact(fmt.channels()).flat_map(|px| photocraft_raster::to_rgba(&fmt, px).map(|x| f32_to_f16(x).to_le_bytes())).flatten().collect();
            assert_eq!(fast, general, "{fmt:?}");
            assert_eq!(convert_tile(&s, Some(&t), TexKind::Rgba16F, TileCoord::new(0, 0)), general, "{fmt:?}");
        }
        assert!(rgb_gray16_to_f16(&PixelFormat::RGBA8, &[]).is_none());
        assert!(rgb_gray16_to_f16(&PixelFormat { sample: SampleType::U16, ..PixelFormat::CMYKA8 }, &[]).is_none());
        assert!(rgb_gray32_to_f16(&PixelFormat::RGBA16, &[]).is_none());
        assert!(rgb_gray32_to_f16(&PixelFormat { sample: SampleType::F32, ..PixelFormat::CMYKA8 }, &[]).is_none());
    }

    #[test]
    fn grid_rounds_out() {
        assert_eq!(tile_grid(Rect::new(0, 0, 300, 256)), Rect::new(0, 0, 512, 256));
    }
}
