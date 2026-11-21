//! Move drags on the GPU canvas with the layers above the moving ones cached.
//!
//! While a Move drag goes on, the layers above the moving run don't change: the CPU canvas keeps
//! their composite over opaque black and over opaque white (`move_cpu`), and so does the GPU canvas
//! here. For a run of layers that change an opaque backdrop affinely
//! ([`photocraft_compose::composites_affinely`]: Normal, Multiply and Screen layers, their effects,
//! masks, clipping and groups), a pixel `c` of what lies under them comes out as
//! `k + (w - k) * c`, where `k` and `w` are those layers composited over black and over white.
//! So a drag frame composites only the layers up to the moving run (the lower-stack cache keeps
//! the ones below it), and one pass lays the cached `k` and `w` over it: the cost no longer grows
//! with the layers above.
//!
//! The maps hold what the view shows at the frame's level of detail (screen resolution, not the
//! document's), are counted in the canvas budget (`Resources::canvas_bytes`, the first thing
//! `make_room` lets go of) and are made once per drag (again only when the view moves past them
//! or the layers above change). Where the split can't be exact the drag frame is composited whole,
//! as before:
//!
//! - a layer above that doesn't composite affinely (Overlay, Difference, adjustments, Blend If,
//!   knockout, Bevel and Emboss...), or a run above starting clipped to the moving layer;
//! - what lies under the layers above might not be opaque everywhere (no opaque Background below
//!   the moving layers, a knockout under them): over a transparent pixel the layers composite
//!   differently, which the maps can't tell;
//! - Lab, CMYK, multichannel or 32-bit documents, spot channels, artboards, linear documents the
//!   canvas stores sRGB-encoded;
//! - the maps don't fit the canvas budget, or a texture side.
//!
//! Type layers blend in a gamma space: on their anti-aliased edges the map is an approximation, as
//! on the CPU canvas. The release composites the moved document exactly.

use photocraft_doc::{Document, Layer, LayerId};
use photocraft_geom::Rect;

use super::{CanvasLookup, FORMAT, FORMAT_HIGH, GpuCanvas, Resources, area, texel_bytes, wgpu};

/// One map texture.
pub(super) struct Map {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// The layers above a Move drag's moving run composited over black and over white, and the
/// per-frame composite of what lies under them, for one view at one level (see the module docs).
pub(super) struct AboveMaps {
    /// Document key, level, the rect covered (level pixels) and the texture format.
    key: (u64, u32, Rect, wgpu::TextureFormat),
    /// The layers above, as they were composited, and the document without its layers (global
    /// light, profile, mode...): the maps hold while both are unchanged.
    above: Vec<Layer>,
    shell: Document,
    over_black: Map,
    over_white: Map,
    lower: Map,
}

impl AboveMaps {
    /// The document they belong to.
    pub(super) fn doc(&self) -> u64 {
        self.key.0
    }

    pub(super) fn bytes(&self) -> u64 {
        let (_, _, r, f) = self.key;
        3 * area(r) * texel_bytes(f)
    }
}

/// The document without its layers (and with no id): what the maps depend on besides the layers.
fn shell_of(doc: &Document) -> Document {
    let mut s = doc.clone();
    s.layers = Vec::new();
    s.id = photocraft_doc::DocId(0);
    s
}

fn knockout_free(layers: &[Layer], depth: usize) -> bool {
    depth < 64 && layers.iter().all(|l| l.advanced.knockout == photocraft_doc::Knockout::None && l.children().is_none_or(|c| knockout_free(c, depth + 1)))
}

fn draws(l: &Layer, depth: usize) -> bool {
    l.visible && (depth < 64) && l.children().is_none_or(|c| c.iter().any(|k| draws(k, depth + 1)))
}

fn holds(l: &Layer, ids: &[LayerId], depth: usize) -> bool {
    ids.contains(&l.id) || (depth < 64 && l.children().is_some_and(|c| c.iter().any(|k| holds(k, ids, depth + 1))))
}

/// Whether `bg` (the Background) is opaque everywhere on `canvas` and composites as such: read in
/// bands, so a big page never needs a whole-page buffer.
fn opaque_background(bg: &Layer, canvas: Rect, mode: photocraft_color::ColorMode) -> bool {
    let plain = bg.visible
        && bg.blend == photocraft_color::BlendMode::Normal
        && bg.opacity >= 1.0
        && bg.fill_opacity >= 1.0
        && !bg.clipped
        && !bg.mask.as_ref().is_some_and(|m| m.enabled)
        && !bg.vector_mask.as_ref().is_some_and(|m| m.enabled)
        && !photocraft_compose::effects::has_effects(bg)
        && !photocraft_compose::blend_if_active(bg, mode)
        && bg.excluded_channels == 0
        && bg.advanced.is_default();
    let Some(s) = bg.surface().filter(|_| plain) else { return false };
    let format = s.format();
    if !format.alpha {
        return true;
    }
    let n = s.channels().max(1);
    let mut y = canvas.y0;
    while y < canvas.y1 {
        let band = Rect::new(canvas.x0, y, canvas.x1, y.saturating_add(64).min(canvas.y1));
        let Ok(px) = s.try_read_region(band) else { return false };
        if px.chunks_exact(n).any(|p| p.last().is_none_or(|a| *a < 1.0)) {
            return false;
        }
        y = band.y1;
    }
    true
}

impl GpuCanvas {
    /// Where the layers above the Move drag's moving run start in `doc.layers` (the drag's moving
    /// layers set with [`GpuCanvas::set_moving`]), when they can be laid over the per-frame
    /// composite from cached maps (see the module docs); `None` otherwise.
    fn split_above(&self, res: &mut Resources, doc: &Document, encode_srgb: bool) -> Option<usize> {
        use photocraft_color::{ColorMode, SampleType};
        let moving = res.moving.get(&doc.id.0)?.clone();
        if moving.is_empty()
            || encode_srgb
            || !matches!(doc.mode, ColorMode::Rgb | ColorMode::Grayscale)
            || doc.depth == SampleType::F32
            || !doc.channels.is_empty()
            || doc.has_artboards()
            || !ABOVE_CACHE.load(std::sync::atomic::Ordering::Relaxed)
        {
            return None;
        }
        let root = doc.layers.iter().position(|l| holds(l, &moving, 0))?;
        let mut top = root + 1;
        while doc.layers.get(top).is_some_and(|l| l.clipped) {
            top += 1;
        }
        let above = doc.layers.get(top..)?;
        // Nothing above that draws (an empty group): nothing to cache.
        if !above.iter().any(|l| draws(l, 0)) || !above.iter().all(|l| photocraft_compose::composites_affinely(l, doc.mode, true)) {
            return None;
        }
        // What lies under the layers above is opaque everywhere: an opaque Background under the
        // moving run, and nothing between that can punch through to it.
        let bg = doc.background().filter(|_| root > 0)?;
        if !knockout_free(doc.layers.get(..top)?, 0) {
            return None;
        }
        let print = photocraft_gpu::surface_fingerprint(bg.surface()?);
        let opaque = match res.opaque_bg {
            Some((id, p, o)) if id == bg.id && p == print => o,
            _ => {
                let o = opaque_background(bg, doc.bounds(), doc.mode);
                res.opaque_bg = Some((bg.id, print, o));
                o
            }
        };
        opaque.then_some(top)
    }

    /// A Move drag frame of document `doc` (its display set on screen, nothing blank) over
    /// `region` (document pixels, what the view shows) at `level`, with the layers above the
    /// moving run laid over from cached maps. `None` when it can't be done that way (the caller
    /// composites the frame whole); nothing is written to the display set then.
    pub(super) fn composite_above(
        &self,
        doc: &Document,
        region: Rect,
        encode_srgb: bool,
        level: u32,
    ) -> Option<photocraft_gpu::Stats> {
        let _s = crate::perf_span::span("gpu.composite_above");
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let key = doc.id.0;
        let mut renderer = self.rs.renderer.write();
        let res = renderer.callback_resources.canvas_mut(self.id)?;
        let Some(top) = self.split_above(res, doc, encode_srgb) else {
            res.above = None;
            return None;
        };
        let d_level = res.docs.get(&key)?.max_level();
        let mut level = level.min(d_level).min(photocraft_gpu::MAX_LOD);
        while level > 0 && res.docs.get(&key)?.tiles.iter().any(|t| t.rect[0] % (1 << level) != 0 || t.rect[1] % (1 << level) != 0) {
            level -= 1;
        }
        let region = region.intersect(&doc.bounds());
        let lrect = photocraft_gpu::level_rect(region, level).intersect(&photocraft_gpu::level_bounds(doc.bounds(), level));
        if lrect.is_empty() {
            return None;
        }
        // Half floats where the canvas has them (the maps' arithmetic stays exact to display
        // precision); 8 bits on devices of 4 GB or less, or without them.
        let small = super::physical_memory().is_some_and(|b| b <= super::LOW_MEMORY);
        let map_format = if res.encode_pipeline_high.is_some() && !small { FORMAT_HIGH } else { FORMAT };
        let above = doc.layers.get(top..)?;
        let fresh = !res.above.as_ref().is_some_and(|m| {
            let (k, l, r, f) = m.key;
            k == key && l == level && f == map_format && r.intersect(&lrect) == lrect && m.above.as_slice() == above && m.shell == shell_of(doc)
        });
        let side = device.limits().max_texture_dimension_2d;
        if fresh {
            res.above = None;
            if lrect.width() > side || lrect.height() > side {
                return None;
            }
            let bytes = 3 * area(lrect) * texel_bytes(map_format);
            if res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
                // Kept view sets give way; never the documents' own sets.
                while res.canvas_bytes().saturating_add(bytes) > res.canvas_budget && res.drop_oldest_view_set() > 0 {}
                if res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
                    return None;
                }
            }
            let make = |label: &str| {
                let t = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d { width: lrect.width(), height: lrect.height(), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: map_format,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = t.create_view(&Default::default());
                Map { _texture: t, view }
            };
            let maps =
                AboveMaps { key: (key, level, lrect, map_format), above: above.to_vec(), shell: shell_of(doc), over_black: make("pc_above_black"), over_white: make("pc_above_white"), lower: make("pc_above_lower") };
            // The layers above over opaque black and over opaque white, each its own document
            // (its own lower-stack cache entry; the layers keep their resident pages).
            let mut comp = self.take_compositor(res, false).ok()?;
            let region_all = Rect::new(lrect.x0 << level, lrect.y0 << level, lrect.x1 << level, lrect.y1 << level).intersect(&doc.bounds());
            let mut ok = true;
            for (target, rgb) in [(&maps.over_black, 0.0), (&maps.over_white, 1.0)] {
                let mut d = doc.clone();
                d.id = photocraft_doc::DocId::fresh();
                let backdrop = Layer::new("backdrop", photocraft_doc::LayerContent::Fill(photocraft_doc::Fill::Solid(photocraft_color::Color::rgb(rgb, rgb, rgb))));
                d.layers = std::iter::once(backdrop).chain(above.iter().cloned()).collect();
                if render_into(device, queue, res, &mut comp, &d, region_all, level, target, lrect, map_format).is_err() {
                    ok = false;
                    break;
                }
                comp.forget_doc(d.id);
            }
            res.compositor = Some(comp);
            if !ok {
                return None;
            }
            res.above = Some(maps);
        }
        // This frame: the layers up to the moving run (the same document, so its lower-stack
        // cache and resident pages carry on), then the maps over it into the display tiles.
        let mut lower = doc.clone();
        lower.layers.truncate(top);
        let mut comp = self.take_compositor(res, true).ok()?;
        let maps = res.above.take()?;
        let rendered = render_into(device, queue, res, &mut comp, &lower, region, level, &maps.lower, maps.key.2, maps.key.3);
        res.compositor = Some(comp);
        let stats = match rendered {
            Ok(s) => s,
            Err(_) => {
                res.above = Some(maps);
                return None;
            }
        };
        let applied = apply_maps(device, queue, res, key, &maps, photocraft_gpu::level_rect(region, level).intersect(&lrect), level, encode_srgb);
        res.above = Some(maps);
        if !applied {
            return None;
        }
        let res: &Resources = res;
        if let Some(d) = res.docs.get(&key) {
            d.regenerate_mips_from(device, queue, res, [region.x0 as u32, region.y0 as u32, region.x1 as u32, region.y1 as u32], level);
        }
        self.pace.submitted(queue);
        Some(photocraft_gpu::Stats { level, ..stats })
    }
}

/// Composite `region` (document pixels) of `doc` at `level` with `comp` into `map`, which covers
/// `rect` (level pixels), stored as the compositor's straight values (the frame's arithmetic is on
/// opaque pixels, where they equal the premultiplied ones).
#[allow(clippy::too_many_arguments)]
fn render_into(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    res: &Resources,
    comp: &mut photocraft_gpu::Compositor,
    doc: &Document,
    region: Rect,
    level: u32,
    map: &Map,
    rect: Rect,
    format: wgpu::TextureFormat,
) -> Result<photocraft_gpu::Stats, photocraft_gpu::Unsupported> {
    let pipe = res.encode_pipeline(format).ok_or_else(|| photocraft_gpu::Unsupported("no map pipeline".into()))?;
    let bgl = &res.encode_bgl;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        comp.render_lod(device, queue, doc, region, level, |enc, out| {
            let r = out.rect.intersect(&rect);
            if r.is_empty() {
                return;
            }
            // Values kept as composited (no clamp to 1, no sRGB encoding).
            let offset = [out.rect.x0 - rect.x0 - out.origin[0] as i32, out.rect.y0 - rect.y0 - out.origin[1] as i32, 0, 1];
            let ubuf = super::init_buffer(device, "pc_above_store", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
            let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("pc_above_store"),
                layout: bgl,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(out.view) },
                    wgpu::BindGroupEntry { binding: 1, resource: ubuf.as_entire_binding() },
                ],
            });
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("pc_above_store"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &map.view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &bg, &[]);
            pass.set_scissor_rect((r.x0 - rect.x0) as u32, (r.y0 - rect.y0) as u32, r.width(), r.height());
            pass.draw(0..3, 0..1);
        })
    }));
    result.unwrap_or_else(|_| Err(photocraft_gpu::Unsupported("the GPU compositor stopped (internal error)".into())))
}

/// Lay `maps` over their per-frame composite into document `key`'s display tiles over `lrect`
/// (level pixels) at `level`. Whether it was written.
#[allow(clippy::too_many_arguments)]
fn apply_maps(device: &wgpu::Device, queue: &wgpu::Queue, res: &Resources, key: u64, maps: &AboveMaps, lrect: Rect, level: u32, encode_srgb: bool) -> bool {
    let Some(d) = res.docs.get(&key) else { return false };
    let Some(pipe) = res.apply_pipeline(d.format) else { return false };
    let (_, _, rect, _) = maps.key;
    let keep_hdr = i32::from(d.format == FORMAT_HIGH);
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_above_apply") });
    for t in &d.tiles {
        let l = level;
        let Some(target) = t.levels.get(l as usize) else { continue };
        let [tx, ty, tw, th] = [t.rect[0] >> l, t.rect[1] >> l, (t.rect[2] >> l).max(1), (t.rect[3] >> l).max(1)].map(|v| v as i32);
        let r = lrect.intersect(&Rect::from_xywh(tx, ty, tw as u32, th as u32));
        if r.is_empty() {
            continue;
        }
        let offset = [rect.x0 - tx, rect.y0 - ty, i32::from(encode_srgb), keep_hdr];
        let ubuf = super::init_buffer(device, "pc_above_apply", &offset.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>(), wgpu::BufferUsages::UNIFORM);
        let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("pc_above_apply"),
            layout: &res.apply_bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&maps.lower.view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&maps.over_black.view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&maps.over_white.view) },
                wgpu::BindGroupEntry { binding: 3, resource: ubuf.as_entire_binding() },
            ],
        });
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("pc_above_apply"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                depth_slice: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &bg, &[]);
        pass.set_scissor_rect((r.x0 - tx) as u32, (r.y0 - ty) as u32, r.width(), r.height());
        pass.draw(0..3, 0..1);
    }
    queue.submit([enc.finish()]);
    true
}

static ABOVE_CACHE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Turn the cached layers above a Move drag off (or back on, the default): measurements compare
/// both in one build.
pub fn set_above_cache(on: bool) {
    ABOVE_CACHE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The maps laid over a drag frame: `k + (w - k) * c` on opaque pixels (the per-frame composite
/// and both maps are opaque where this runs), then stored as the encode pass stores a composite.
pub(super) const APPLY_WGSL: &str = r#"
@group(0) @binding(0) var lower: texture_2d<f32>;
@group(0) @binding(1) var over_black: texture_2d<f32>;
@group(0) @binding(2) var over_white: texture_2d<f32>;
@group(0) @binding(3) var<uniform> offset: vec4<i32>; // tile to map, encode sRGB, keep values above 1.0

@vertex
fn vs(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2(f32((vi << 1u) & 2u), f32(vi & 2u));
    return vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

fn srgb_encode(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3(0.0031308));
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let p = vec2<i32>(floor(pos.xy)) - offset.xy;
    let c = textureLoad(lower, p, 0);
    let k = textureLoad(over_black, p, 0);
    let w = textureLoad(over_white, p, 0);
    var rgb = clamp(k.rgb + (w.rgb - k.rgb) * c.rgb, vec3(0.0), vec3(1.0));
    if (offset.w != 0) {
        rgb = max(k.rgb + (w.rgb - k.rgb) * c.rgb, vec3(0.0));
    }
    if (offset.z != 0) {
        rgb = srgb_encode(rgb);
    }
    let a = clamp(c.a, 0.0, 1.0);
    return vec4(rgb * a, a);
}
"#;
