use super::{DocId, Document, PhotocraftApp, mask_thumbs_ui};
use crate::perf_span;

impl PhotocraftApp {
    pub(super) fn prune_thumbs(&mut self) {
        let documents = self.session.documents();
        if self.thumb_documents.len() == documents.len()
            && self.thumb_documents.iter().zip(documents).all(|((id, snapshot), st)| *id == st.doc.id && snapshot.ptr_eq(&std::sync::Arc::downgrade(&st.doc)))
        {
            return;
        }
        // Check snapshots every sync, but walk the layer tree only after it changes. A Weak
        // tracks replacement (including undo and preserved-ID reopen) without retaining pixels.
        if !self.thumbs.is_empty() {
            let mut live = std::collections::HashSet::new();
            let mut pending: Vec<_> = documents.iter().flat_map(|st| &st.doc.layers).collect();
            while let Some(layer) = pending.pop() {
                live.insert((layer.id, mask_thumbs_ui::THUMB_LAYER));
                if layer.mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_MASK));
                }
                if layer.vector_mask.is_some() {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_VECTOR));
                }
                if matches!(&layer.content, photocraft_doc::LayerContent::Smart(sm) if sm.filter_mask.is_some()) {
                    live.insert((layer.id, mask_thumbs_ui::THUMB_SMART_MASK));
                }
                if let Some(children) = layer.children() {
                    pending.extend(children);
                }
            }
            // Native files can restore IDs: retain the union across all open documents, rather
            // than deleting an ID just because one document containing it was closed.
            self.thumbs.retain(|key, _| live.contains(key));
            self.thumb_frames.retain(|id, _| live.contains(&(*id, mask_thumbs_ui::THUMB_LAYER)));
        }
        self.thumb_documents = documents.iter().map(|st| (st.doc.id, std::sync::Arc::downgrade(&st.doc))).collect();
    }

    /// Cached 64px thumbnail of a pixel-ish layer, letterboxed in a square, and the document
    /// area it frames: the canvas, or with Clip Thumbnails to Layer Bounds the layer's pixels.
    pub fn layer_thumb(&mut self, ctx: &egui::Context, doc: &Document, layer: &photocraft_doc::Layer) -> (egui::TextureId, photocraft_geom::Rect) {
        let _s = perf_span::span("layers.thumb");
        let clip = self.session.prefs().interface.clip_thumbnails_to_layer_bounds;
        // Key by content, not document revision: COW tiles change pointer only when their pixels
        // change, so unrelated edits (e.g. painting another layer) don't rebuild this thumbnail.
        let rev = layer.surface().map_or(0, surface_fingerprint) ^ (doc.size.width as u64) << 40 ^ u64::from(clip) << 63;
        let key = (layer.id, mask_thumbs_ui::THUMB_LAYER);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
            && let Some(frame) = self.thumb_frames.get(&layer.id)
        {
            return (tex.id(), *frame);
        }
        let canvas = doc.bounds();
        let frame = layer_thumb_frame(layer, canvas, clip);
        if !self.thumb_budget(ctx) {
            return match self.thumbs.get(&key).map(|(_, tex)| tex.id()) {
                Some(id) => (id, self.thumb_frames.get(&layer.id).copied().unwrap_or(frame)),
                None => (self.blank_thumb(ctx), frame),
            };
        }
        self.perf_counters.thumbs += 1;
        let mut px = [0.0f32; 8];
        let img = thumb_image(frame, 64, |x, y| {
            let Some(s) = layer.surface() else { return [0.0; 4] };
            let n = s.channels();
            s.read_pixel(x, y, &mut px[..n]);
            photocraft_raster::to_rgba(&s.format(), &px[..n])
        });
        self.thumb_frames.insert(layer.id, frame);
        (self.store_thumb(ctx, key, rev, img), frame)
    }

    /// The History panel thumbnail of snapshot `snap` of document `doc` (#2966): its composite,
    /// centred in a square. Thumbnails of `doc`'s snapshots not in `live` are dropped.
    pub fn snapshot_thumb(&mut self, ctx: &egui::Context, doc: DocId, snap: &photocraft_engine::history_cmds::Snapshot, live: &[u64]) -> egui::TextureId {
        self.snapshot_thumbs.retain(|(d, id), _| *d != doc || live.contains(id));
        self.snapshot_bands.retain(|(d, id), _| *d != doc || live.contains(id));
        if let Some(tex) = self.snapshot_thumbs.get(&(doc, snap.id)) {
            return tex.id();
        }
        // Composited off the frame: on a worker thread where there is one, else a band of whole
        // thumbnail rows per frame within a few milliseconds. A whole-document composite in one
        // frame held a new document's first frame for a quarter of a second on a large page; the
        // row shows a blank square until the image is done.
        let _s = perf_span::span("history.thumb");
        let (w, h) = photocraft_compose::thumbnail_size(&snap.doc, SNAPSHOT_SIDE);
        let side = w.max(h) as usize;
        let bands = self.snapshot_bands.entry((doc, snap.id)).or_insert_with(|| SnapshotBands {
            image: egui::ColorImage::new([side, side], vec![egui::Color32::TRANSPARENT; side * side]),
            next: 0,
            rows: 1,
            worker: spawn_snapshot_thumb(ctx, snap.doc.clone(), w, h),
        });
        if let Some(rx) = &bands.worker {
            match rx.try_recv() {
                Ok(buf) => {
                    place(&mut bands.image, side, (w, h), &buf);
                    bands.next = h as i32;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // The native worker wakes the window; a browser one can't.
                    ctx.request_repaint_after(std::time::Duration::from_millis(30));
                    return self.blank_thumb(ctx);
                }
                // The worker died: composite here, in bands.
                Err(std::sync::mpsc::TryRecvError::Disconnected) => bands.worker = None,
            }
        }
        let (height, doc_h) = (h as i32, snap.doc.size.height);
        let t0 = crate::gpu_canvas::now_ms();
        while bands.next < height && crate::gpu_canvas::now_ms() - t0 < SNAPSHOT_BAND_MS {
            let b0 = crate::gpu_canvas::now_ms();
            // Whole thumbnail rows: a band inside one row would composite all of its document
            // rows again for each band.
            let start = bands.next;
            let end = (start + bands.rows).min(height);
            let buf = photocraft_compose::render_reduced_damage(&snap.doc, w, h, thumb_rows(snap.doc.size.width, doc_h, height, start, end));
            place(&mut bands.image, side, (w, h), &buf);
            bands.next = end;
            let took = (crate::gpu_canvas::now_ms() - b0).max(0.1);
            bands.rows = ((f64::from(end - start) * (SNAPSHOT_BAND_MS / 2.0) / took) as i32).clamp(1, height.max(1));
        }
        if bands.next < height {
            ctx.request_repaint();
            return self.blank_thumb(ctx);
        }
        let Some(done) = self.snapshot_bands.remove(&(doc, snap.id)) else { return self.blank_thumb(ctx) };
        let tex = ctx.load_texture(format!("snapshot-{}", snap.id), done.image, egui::TextureOptions::LINEAR);
        let id = tex.id();
        self.snapshot_thumbs.insert((doc, snap.id), tex);
        id
    }

    /// The cached thumbnail of `mask`, the layer mask of layer `id`.
    pub fn mask_thumb(&mut self, ctx: &egui::Context, doc: &Document, id: photocraft_doc::LayerId, mask: &photocraft_doc::LayerMask) -> egui::TextureId {
        self.mask_thumb_kind(ctx, doc, id, mask, mask_thumbs_ui::THUMB_MASK)
    }

    /// The thumbnail of `mask` under cache `kind`, re-rendered only when the mask's pixels or the document size change; Smart Filter and layer masks are cached apart.
    pub(crate) fn mask_thumb_kind(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        id: photocraft_doc::LayerId,
        mask: &photocraft_doc::LayerMask,
        kind: u8,
    ) -> egui::TextureId {
        let rev = surface_fingerprint(&mask.surface) ^ (doc.size.width as u64) << 40 ^ (doc.size.height as u64) << 16;
        let key = (id, kind);
        if let Some((r, tex)) = self.thumbs.get(&key)
            && *r == rev
        {
            return tex.id();
        }
        if !self.thumb_budget(ctx) {
            return match self.thumbs.get(&key).map(|(_, tex)| tex.id()) {
                Some(id) => id,
                None => self.blank_thumb(ctx),
            };
        }
        self.perf_counters.mask_thumbs += 1;
        let mut v = [0.0f32; 1];
        let img = thumb_image(doc.bounds(), 64, |x, y| {
            mask.surface.read_pixel(x, y, &mut v);
            [v[0], v[0], v[0], 1.0]
        });
        self.store_thumb(ctx, key, rev, img)
    }

    /// Whether another thumbnail may be rebuilt this frame: at most [`THUMBS_PER_FRAME`], so
    /// scrolling a long Layers panel or an edit touching many layers spreads the work over frames
    /// (the rest keep their previous image, or none yet, and the next frame continues).
    fn thumb_budget(&mut self, ctx: &egui::Context) -> bool {
        if self.thumb_frame.0 != self.frame {
            self.thumb_frame = (self.frame, 0);
        }
        if self.thumb_frame.1 >= THUMBS_PER_FRAME {
            self.perf_counters.thumbs_deferred += 1;
            ctx.request_repaint();
            return false;
        }
        self.thumb_frame.1 += 1;
        true
    }

    fn blank_thumb(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.blank_thumb
            .get_or_insert_with(|| {
                ctx.load_texture("thumb-blank", egui::ColorImage::new([1, 1], vec![egui::Color32::TRANSPARENT]), egui::TextureOptions::LINEAR)
            })
            .id()
    }

    pub(crate) fn store_thumb(&mut self, ctx: &egui::Context, key: (photocraft_doc::LayerId, u8), rev: u64, img: egui::ColorImage) -> egui::TextureId {
        match self.thumbs.get_mut(&key) {
            Some((r, tex)) => {
                tex.set(img, egui::TextureOptions::LINEAR);
                *r = rev;
                tex.id()
            }
            None => {
                let tex = ctx.load_texture(format!("thumb-{}-{}", key.0.0, key.1), img, egui::TextureOptions::LINEAR);
                let id = tex.id();
                self.thumbs.insert(key, (rev, tex));
                id
            }
        }
    }
}

/// The History panel's snapshot thumbnails: their side (pixels), and the time a frame may spend
/// compositing one (ms).
const SNAPSHOT_SIDE: u32 = 56;
const SNAPSHOT_BAND_MS: f64 = 4.0;

/// A History snapshot thumbnail being composited on a worker, or a band of rows per frame.
pub(crate) struct SnapshotBands {
    image: egui::ColorImage,
    /// The next thumbnail row to draw.
    next: i32,
    /// Thumbnail rows per band, sized by the last band's time.
    rows: i32,
    /// The worker compositing the whole thumbnail, if one could start.
    worker: Option<std::sync::mpsc::Receiver<photocraft_compose::Buffer>>,
}

/// Composites `doc`'s `w`×`h` thumbnail on a worker thread; `None` where none can start (the
/// single-threaded web build), and the caller composites it in bands.
fn spawn_snapshot_thumb(ctx: &egui::Context, doc: std::sync::Arc<Document>, w: u32, h: u32) -> Option<std::sync::mpsc::Receiver<photocraft_compose::Buffer>> {
    let (tx, rx) = std::sync::mpsc::channel();
    #[cfg(not(target_arch = "wasm32"))]
    let spawned = {
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("snapshot-thumb".into())
            .spawn(move || {
                let _ = tx.send(photocraft_compose::render_reduced(&doc, w, h));
                ctx.request_repaint();
            })
            .is_ok()
    };
    // The browser threads build: on the rayon pool (a worker can't wake egui; the panel repaints
    // while it waits).
    #[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
    let spawned = {
        let _ = ctx;
        rayon::spawn(move || {
            let _ = tx.send(photocraft_compose::render_reduced(&doc, w, h));
        });
        true
    };
    #[cfg(all(target_arch = "wasm32", not(target_feature = "atomics")))]
    let spawned = {
        let _ = (ctx, doc, w, h, tx);
        false
    };
    spawned.then_some(rx)
}

/// The document rows of thumbnail rows `start..end` of a `thumb_h`-row thumbnail.
fn thumb_rows(doc_w: u32, doc_h: u32, thumb_h: i32, start: i32, end: i32) -> photocraft_geom::Rect {
    let row = |t: i32| (i64::from(t) * i64::from(doc_h) / i64::from(thumb_h.max(1))) as i32;
    photocraft_geom::Rect::new(0, row(start), doc_w as i32, row(end))
}

/// Copies `buf` (thumbnail pixels at its rect) into the centre of the `side`-square `image`.
fn place(image: &mut egui::ColorImage, side: usize, (w, h): (u32, u32), buf: &photocraft_compose::Buffer) {
    let (ox, oy) = ((side.saturating_sub(w as usize)) / 2, (side.saturating_sub(h as usize)) / 2);
    let part = crate::canvas::display_image(None, buf);
    let (bx, by) = (buf.rect.x0.max(0) as usize, buf.rect.y0.max(0) as usize);
    let pw = part.size[0].max(1);
    for (i, px) in part.pixels.iter().enumerate() {
        let (x, y) = (ox + bx + i % pw, oy + by + i / pw);
        if let Some(out) = image.pixels.get_mut(y * side + x).filter(|_| x < side) {
            *out = *px;
        }
    }
}

/// Layer and mask thumbnails rebuilt per frame at most (see `PhotocraftApp::thumb_budget`).
pub const THUMBS_PER_FRAME: u32 = 8;

/// Cheap identity of a surface's pixels: its default (untouched) pixel, tile coordinates and
/// `Arc` pointers. The default pixel matters: inverting or filling a tile-less mask only changes
/// it (#2117).
pub fn surface_fingerprint(s: &photocraft_raster::Surface) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ s.tile_count() as u64;
    for &b in s.default_bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3);
    }
    for (c, t) in s.tiles() {
        let p = std::sync::Arc::as_ptr(t) as usize as u64;
        h = (h ^ p ^ ((c.tx as u64) << 32 | c.ty as u32 as u64)).wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// The document area a layer thumbnail frames: the canvas, or with `clip` the layer's pixels on
/// it (the canvas again when it has none there, so an empty layer still shows the page shape).
pub(crate) fn layer_thumb_frame(layer: &photocraft_doc::Layer, canvas: photocraft_geom::Rect, clip: bool) -> photocraft_geom::Rect {
    if !clip {
        return canvas;
    }
    let bounds = layer.surface().map_or(photocraft_geom::Rect::EMPTY, photocraft_compose::bounds::content_bounds).intersect(&canvas);
    if bounds.is_empty() { canvas } else { bounds }
}

/// Square thumbnail of the document area `frame`, letterboxed, sampling `f(x, y)` in document
/// space.
fn thumb_image(frame: photocraft_geom::Rect, side: usize, mut f: impl FnMut(i32, i32) -> [f32; 4]) -> egui::ColorImage {
    let (w, h) = (frame.width().max(1) as f32, frame.height().max(1) as f32);
    let scale = w.max(h) / side as f32;
    let (ox, oy) = ((side as f32 - w / scale) / 2.0, (side as f32 - h / scale) / 2.0);
    let mut px = vec![egui::Color32::TRANSPARENT; side * side];
    for ty in 0..side {
        for tx in 0..side {
            let dx = (tx as f32 - ox + 0.5) * scale;
            let dy = (ty as f32 - oy + 0.5) * scale;
            if dx < 0.0 || dy < 0.0 || dx >= w || dy >= h {
                continue;
            }
            let c = f(frame.x0.saturating_add(dx as i32), frame.y0.saturating_add(dy as i32));
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            px[ty * side + tx] = egui::Color32::from_rgba_unmultiplied(q(c[0]), q(c[1]), q(c[2]), q(c[3]));
        }
    }
    egui::ColorImage::new([side, side], px)
}

impl PhotocraftApp {
    /// Cached `Surface::content_bounds` keyed by an id and the surface's tile identity.
    pub fn cached_bounds(&mut self, key: u64, surface: &photocraft_raster::Surface) -> photocraft_geom::Rect {
        let rev = surface_fingerprint(surface);
        if let Some((r, b)) = self.bounds_cache.get(&key)
            && *r == rev
        {
            return *b;
        }
        // Per-tile cached: after an edit only the tiles it changed are scanned again.
        let b = photocraft_compose::bounds::content_bounds(surface);
        if self.bounds_cache.len() > 256 {
            self.bounds_cache.clear();
        }
        self.bounds_cache.insert(key, (rev, b));
        b
    }

    /// The box Properties › Transform shows for `layer` (X, Y, W, H): its pixels' bounds, but a
    /// soft shape's path and stroke, whose Density and Feather render past them (#5846), as its
    /// transform frame does.
    pub fn layer_frame_bounds(&mut self, layer: &photocraft_doc::Layer, surface: &photocraft_raster::Surface) -> photocraft_geom::Rect {
        match &layer.content {
            photocraft_doc::LayerContent::Shape(sh) if sh.has_soft_mask() => {
                // Cached per rendering, like the pixel bounds: the frame renders strips of the shape.
                let (key, rev) = (layer.id.0 | 1 << 63, surface_fingerprint(surface));
                if let Some((r, b)) = self.bounds_cache.get(&key)
                    && *r == rev
                {
                    return *b;
                }
                let canvas = self.session.active().map_or_else(|| surface.content_bounds(), |st| st.doc.bounds());
                let b = photocraft_compose::soft_shape_frame(sh, surface.format(), canvas).unwrap_or(photocraft_geom::Rect::EMPTY);
                if self.bounds_cache.len() > 256 {
                    self.bounds_cache.clear();
                }
                self.bounds_cache.insert(key, (rev, b));
                b
            }
            _ => self.cached_bounds(layer.id.0, surface),
        }
    }
}

impl PhotocraftApp {
    /// Channels panel thumbnails of the active document snapshot: the composite,
    /// each colour channel (when there is more than one), each alpha channel, then the Quick Mask.
    pub fn channel_thumbs(&mut self, ctx: &egui::Context) -> Vec<egui::TextureId> {
        let Some(st) = self.session.active() else {
            self.channel_thumbs = None;
            return Vec::new();
        };
        let (id, doc) = (st.doc.id, st.doc.clone());
        // View-only commands bump revision without changing pixels. Keeping a Weak pins allocation
        // identity against address reuse without retaining the document's pixel data.
        let snapshot = std::sync::Arc::downgrade(&doc);
        let show_color = self.session.prefs().interface.show_channels_in_color;
        // A step that changes only the selection (a marquee, Deselect) makes a new snapshot with
        // the same pixels: keep the thumbnails, a full composite at 56 px costs ~0.7 s on a
        // layered 20 MP document (#4518).
        let current = match &mut self.channel_thumbs {
            Some((d, old, in_color, _)) if *d == id && *in_color == show_color => {
                let same = old.ptr_eq(&snapshot) || old.upgrade().is_some_and(|prev| same_but_selection(&prev, &doc));
                if same {
                    *old = snapshot.clone();
                }
                same
            }
            _ => false,
        };
        if !current {
            let comp = photocraft_compose::thumbnail(&doc, 56);
            let (w, h) = (comp.width as usize, comp.height as usize);
            let side = w.max(h);
            let make = |f: &dyn Fn(&[u8]) -> egui::Color32, name: &str| {
                let mut px = vec![egui::Color32::TRANSPARENT; side * side];
                let (ox, oy) = ((side - w) / 2, (side - h) / 2);
                for y in 0..h {
                    for x in 0..w {
                        let o = (y * w + x) * 4;
                        px[(y + oy) * side + x + ox] = f(&comp.pixels[o..o + 4]);
                    }
                }
                (format!("chan-{name}"), egui::ColorImage::new([side, side], px))
            };
            let fmt = doc.pixel_format();
            let colors = fmt.mode.color_channels();
            let mut texs = vec![make(&|p| egui::Color32::from_rgb(p[0], p[1], p[2]), "composite")];
            if colors > 1 {
                for k in 0..colors {
                    let cmyk = fmt.mode == photocraft_doc::ColorMode::Cmyk;
                    texs.push(make(
                        &|p| {
                            let rgba = [p[0], p[1], p[2], 255].map(|v| f32::from(v) / 255.0);
                            let x = photocraft_raster::from_rgba(&fmt, rgba)[k];
                            let g = if cmyk { 1.0 - x } else { x };
                            let byte = (g.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                            channel_tint(fmt.mode, k, byte, show_color)
                        },
                        &format!("c{k}"),
                    ));
                }
            }
            // Alpha channels and the Quick Mask, sampled at thumbnail resolution.
            let (dw, dh) = (doc.size.width.max(1) as f32, doc.size.height.max(1) as f32);
            for (i, ch) in doc.channels.iter().chain(doc.quick_mask.as_ref()).enumerate() {
                let mut px = vec![egui::Color32::TRANSPARENT; side * side];
                let (ox, oy) = ((side - w) / 2, (side - h) / 2);
                for y in 0..h {
                    for x in 0..w {
                        let sx = ((x as f32 + 0.5) * dw / w as f32) as i32;
                        let sy = ((y as f32 + 0.5) * dh / h as f32) as i32;
                        let v = ch.surface.sample_channel(sx, sy, 0);
                        px[(y + oy) * side + x + ox] = egui::Color32::from_gray((v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8);
                    }
                }
                texs.push((format!("chan-a{i}"), egui::ColorImage::new([side, side], px)));
            }
            // Update the textures already uploaded rather than allocating new ones.
            let mut handles = match self.channel_thumbs.take() {
                Some((d, _, _, h)) if d == id => h,
                _ => Vec::new(),
            };
            handles.truncate(texs.len());
            for (i, (name, image)) in texs.into_iter().enumerate() {
                match handles.get_mut(i) {
                    Some(h) => h.set(image, egui::TextureOptions::LINEAR),
                    None => handles.push(ctx.load_texture(name, image, egui::TextureOptions::LINEAR)),
                }
            }
            self.channel_thumbs = Some((id, snapshot, show_color, handles));
        }
        self.channel_thumbs.as_ref().map(|(_, _, _, t)| t.iter().map(|t| t.id()).collect()).unwrap_or_default()
    }
}

/// Whether `new` differs from `old` at most in its selection, so every composite and channel
/// thumbnail of it is unchanged. Tiles compare by pointer first, so this costs little.
fn same_but_selection(old: &Document, new: &Document) -> bool {
    if old.selection == new.selection {
        return old == new;
    }
    let mut o = old.clone();
    o.selection = new.selection.clone();
    o == *new
}

/// A channel thumbnail pixel whose lightness is `byte` (255 = white: full light, no ink).
/// Preferences ▸ Interface ▸ Show Channels in Color tints RGB channels from black to their
/// primary and CMYK channels from white to their ink, as Photoshop does; otherwise grey.
fn channel_tint(mode: photocraft_doc::ColorMode, k: usize, byte: u8, in_color: bool) -> egui::Color32 {
    use photocraft_doc::ColorMode::{Cmyk, Rgb};
    if !in_color {
        return egui::Color32::from_gray(byte);
    }
    match (mode, k) {
        (Rgb, 0) => egui::Color32::from_rgb(byte, 0, 0),
        (Rgb, 1) => egui::Color32::from_rgb(0, byte, 0),
        (Rgb, 2) => egui::Color32::from_rgb(0, 0, byte),
        (Cmyk, 0) => egui::Color32::from_rgb(byte, 255, 255),
        (Cmyk, 1) => egui::Color32::from_rgb(255, byte, 255),
        (Cmyk, 2) => egui::Color32::from_rgb(255, 255, byte),
        _ => egui::Color32::from_gray(byte),
    }
}

#[cfg(test)]
mod channel_tint_tests {
    use super::channel_tint;
    use egui::Color32;
    use photocraft_doc::ColorMode::{Cmyk, Grayscale, Rgb};

    #[test]
    fn channels_in_color_tint_rgb_from_black_and_cmyk_inks_from_white() {
        assert_eq!(channel_tint(Rgb, 0, 255, true), Color32::from_rgb(255, 0, 0));
        assert_eq!(channel_tint(Rgb, 2, 0, true), Color32::BLACK);
        // CMYK: no ink is white, full ink is the ink's colour.
        assert_eq!(channel_tint(Cmyk, 0, 255, true), Color32::WHITE);
        assert_eq!(channel_tint(Cmyk, 0, 0, true), Color32::from_rgb(0, 255, 255));
        assert_eq!(channel_tint(Cmyk, 1, 0, true), Color32::from_rgb(255, 0, 255));
        assert_eq!(channel_tint(Cmyk, 2, 0, true), Color32::from_rgb(255, 255, 0));
        assert_eq!(channel_tint(Cmyk, 3, 0, true), Color32::BLACK);
        assert_eq!(channel_tint(Grayscale, 0, 77, true), Color32::from_gray(77));
        assert_eq!(channel_tint(Rgb, 0, 77, false), Color32::from_gray(77));
    }
}

#[cfg(test)]
mod snapshot_band_tests {
    use super::*;

    /// Bands of whole thumbnail rows (the single-threaded web build's) make the worker's image.
    #[test]
    fn row_bands_make_the_whole_thumbnail() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", serde_json::json!({"width": 300, "height": 700})).unwrap();
        s.execute("filter.render.clouds", serde_json::json!({"seed": 4})).unwrap();
        let doc = s.active().unwrap().doc.clone();
        let (w, h) = photocraft_compose::thumbnail_size(&doc, SNAPSHOT_SIDE);
        let side = w.max(h) as usize;
        let blank = || egui::ColorImage::new([side, side], vec![egui::Color32::TRANSPARENT; side * side]);
        let mut whole = blank();
        place(&mut whole, side, (w, h), &photocraft_compose::render_reduced(&doc, w, h));
        let mut banded = blank();
        let mut start = 0;
        for n in [1, 2, 5, 3, 100] {
            let end = (start + n).min(h as i32);
            place(&mut banded, side, (w, h), &photocraft_compose::render_reduced_damage(&doc, w, h, thumb_rows(300, 700, h as i32, start, end)));
            start = end;
        }
        assert_eq!(start, h as i32);
        assert!(whole.pixels.iter().zip(&banded.pixels).all(|(a, b)| a.to_array().iter().zip(b.to_array()).all(|(x, y)| x.abs_diff(y) <= 1)));
    }
}
