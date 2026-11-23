//! Display texture sets kept per layer visibility (Capikoma request 17).
//!
//! A host that flips a page between named views (`document.views.apply`: one language group
//! shown, the others hidden) used to redraw the whole changed area in the frame after the switch,
//! 0.2 to 0.9 s on a 12 MP page with fifty outlined texts, and as much again when it came back to
//! a view it showed a moment before. Now the document's display texture set remembers which
//! document it shows ([`DocTextures::shows`]). On a change of layer visibility alone, the set
//! is kept aside (a GPU copy) before the change is drawn into it, and when the document later
//! looks exactly like a kept set again, that set takes its place in the same frame: no
//! composite, no upload. A named view can also be drawn into a kept set ahead of time, a slice
//! per frame ([`GpuCanvas::prewarm_view_step`]), so even its first switch takes one frame.
//!
//! Kept sets count against [`super::canvas_budget`] (and `editor.setMemoryBudget` through
//! `doc_switch`): one is only made when it fits beside the documents' own sets, they are the first
//! to go when a document needs room, at most [`MAX_VIEW_SETS`] stay per document, and any other
//! edit of the document drops them. On a 2 GB phone the budget (64 MB) holds a page's own set at
//! most, so nothing is kept there.

use std::sync::Arc;

use photocraft_doc::{Document, Layer};

use super::{CanvasLookup, DocTextures, GpuCanvas, Resources, canvas_texture_bytes, now_ms, texel_bytes, texels, texture_buffer, wgpu};

/// The most kept sets per document (a page with three or four languages).
pub const MAX_VIEW_SETS: usize = 4;

/// A display texture set kept aside, with the document it shows.
pub(super) struct ViewSet {
    pub(super) key: u64,
    pub(super) set: DocTextures,
    /// When it was last kept or shown ([`now_ms`]): the least recent goes first.
    used: f64,
}

/// What [`GpuCanvas::switch_view`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewSwitch {
    /// A kept set shows the document as it is now: it is the document's set (nothing to draw).
    Restored,
    /// No kept set matched; the set as it was is kept for later (the change still has to be drawn).
    Kept,
    /// Neither (not a change of visibility alone, or no room in the budget).
    None,
}

/// Whether `a` and `b` differ in the visibility of their layers and in nothing else that draws
/// (the last applied layer comp aside). False for documents with the same visibility: those are
/// not a view switch.
pub fn visibility_only(a: &Document, b: &Document) -> bool {
    fn same_shape(a: &[Layer], b: &[Layer], differ: &mut bool, depth: usize) -> bool {
        if a.len() != b.len() || depth > 64 {
            return false;
        }
        a.iter().zip(b).all(|(x, y)| {
            *differ |= x.visible != y.visible;
            x.id == y.id && same_shape(x.children().unwrap_or(&[]), y.children().unwrap_or(&[]), differ, depth + 1)
        })
    }
    let mut differ = false;
    if !same_shape(&a.layers, &b.layers, &mut differ, 0) || !differ {
        return false;
    }
    same_but_visibility(a, b)
}

/// `a` with `b`'s layer visibility equals `b` in everything that draws. Shared pixel tiles
/// compare by pointer, so this costs a walk over the layers, not over their pixels.
pub fn same_but_visibility(a: &Document, b: &Document) -> bool {
    same_drawing(a, b, true)
}

/// `a` and `b` draw the same pixels (see [`same_drawing`]).
pub fn same_pixels(a: &Document, b: &Document) -> bool {
    same_drawing(a, b, false)
}

/// `a` and `b` draw the same pixels: they are equal once what never reaches the canvas is set
/// aside (the selection, guides, paths, layer comps, notes, metadata, the document's and layers'
/// names, lock flags and labels), and with `visibility`, the layers' visibility too. A click that
/// changes the selection or a host that redefines its views used to count as an edit, which
/// dropped every kept set (Capikoma request 18).
fn same_drawing(a: &Document, b: &Document, visibility: bool) -> bool {
    fn copy(to: &mut [Layer], from: &[Layer], visibility: bool, depth: usize) {
        if depth > 64 {
            return;
        }
        for (t, f) in to.iter_mut().zip(from) {
            if visibility {
                t.visible = f.visible;
            }
            t.name.clone_from(&f.name);
            t.locks = f.locks.clone();
            t.label = f.label.clone();
            t.xmp.clone_from(&f.xmp);
            if let (Some(to), Some(from)) = (t.children_mut(), f.children()) {
                copy(to, from, visibility, depth + 1);
            }
        }
    }
    if a.layers.len() != b.layers.len() {
        return false;
    }
    let mut a = a.clone();
    copy(&mut a.layers, &b.layers, visibility, 0);
    a.name.clone_from(&b.name);
    a.guides = b.guides.clone();
    a.ruler_origin = b.ruler_origin;
    a.selection.clone_from(&b.selection);
    a.metadata = b.metadata.clone();
    a.paths.clone_from(&b.paths);
    a.work_path.clone_from(&b.work_path);
    a.clipping_path.clone_from(&b.clipping_path);
    a.layer_comps.clone_from(&b.layer_comps);
    a.last_applied_comp = b.last_applied_comp;
    a.last_document_state.clone_from(&b.last_document_state);
    a.measurement = b.measurement.clone();
    a.notes.clone_from(&b.notes);
    a.slices = b.slices.clone();
    a.variables = b.variables.clone();
    a.file_encoding = b.file_encoding.clone();
    a == *b
}

/// Whether kept document `kept` shows exactly what `doc` does.
fn same_view(kept: &Arc<Document>, doc: &Arc<Document>) -> bool {
    Arc::ptr_eq(kept, doc) || same_drawing(kept, doc, false)
}

impl Resources {
    /// GPU bytes of the kept sets.
    pub(super) fn view_set_bytes(&self) -> u64 {
        self.view_sets.iter().map(|v| v.set.bytes()).fold(0, u64::saturating_add)
    }

    /// Drop the least recently used kept set. Returns its bytes (0: none left).
    pub(super) fn drop_oldest_view_set(&mut self) -> u64 {
        let Some(i) = self.view_sets.iter().enumerate().min_by(|a, b| a.1.used.total_cmp(&b.1.used)).map(|(i, _)| i) else { return 0 };
        self.view_sets.swap_remove(i).set.bytes()
    }

    /// Drop kept sets, least recently used first, until everything fits the canvas budget.
    pub(super) fn trim_view_sets(&mut self) {
        while !self.view_sets.is_empty() && self.canvas_bytes() > self.canvas_budget {
            self.drop_oldest_view_set();
        }
    }
}

impl DocTextures {
    /// A copy of this set (every tile and mip level, and what is out of date in it), or `None`
    /// when the copy can't be made.
    fn duplicate(&self, device: &wgpu::Device, queue: &wgpu::Queue, res: &Resources, tile: u32) -> Option<Self> {
        if self.tiles.iter().any(|t| t.page.is_some()) {
            return None;
        }
        let mut copy = Self::new(device, res, self.size, tile, self.format)?;
        if copy.tiles.len() != self.tiles.len() {
            return None;
        }
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("pc_view_set_copy") });
        for (from, to) in self.tiles.iter().zip(&copy.tiles) {
            if from.rect != to.rect || from.levels.len() != to.levels.len() {
                return None;
            }
            for level in 0..from.levels.len() as u32 {
                let size = from.texture.size().mip_level_size(level, wgpu::TextureDimension::D2);
                let at = |texture| wgpu::TexelCopyTextureInfo { texture, mip_level: level, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All };
                enc.copy_texture_to_texture(at(&from.texture), at(&to.texture), size);
            }
        }
        queue.submit(Some(enc.finish()));
        *copy.stale() = self.stale().clone();
        copy.shows = self.shows.clone();
        Some(copy)
    }
}

impl GpuCanvas {
    /// Note what document `key`'s display set now shows: `doc` as the canvas display `display`
    /// stores it, or `None` (a preview, or anything else that isn't the document as it is).
    pub fn set_shows(&self, key: u64, shows: Option<(Arc<Document>, u64)>) {
        let mut r = self.rs.renderer.write();
        if let Some(d) = r.callback_resources.canvas_mut(self.id).and_then(|res| res.docs.get_mut(&key)) {
            d.shows = shows;
        }
    }

    /// What document `key`'s display set shows ([`GpuCanvas::set_shows`]).
    pub fn shows(&self, key: u64) -> Option<(Arc<Document>, u64)> {
        let r = self.rs.renderer.read();
        r.callback_resources.canvas(self.id)?.docs.get(&key)?.shows.clone()
    }

    /// GPU bytes of document `key`'s kept sets.
    pub fn view_set_bytes(&self, key: u64) -> u64 {
        let r = self.rs.renderer.read();
        r.callback_resources.canvas(self.id).map_or(0, |res| res.view_sets.iter().filter(|v| v.key == key).map(|v| v.set.bytes()).fold(0, u64::saturating_add))
    }

    /// How many sets are kept for document `key` (tests and diagnostics).
    pub fn view_sets(&self, key: u64) -> usize {
        let r = self.rs.renderer.read();
        r.callback_resources.canvas(self.id).map_or(0, |res| res.view_sets.iter().filter(|v| v.key == key).count())
    }

    /// Drop document `key`'s kept sets (it was edited: they no longer show it in any view).
    pub fn drop_view_sets(&self, key: u64) {
        let mut r = self.rs.renderer.write();
        if let Some(res) = r.callback_resources.canvas_mut(self.id) {
            res.view_sets.retain(|v| v.key != key);
        }
    }

    /// Free kept sets of any document, least recently used first, until `want` bytes are free.
    /// Returns the bytes freed (memory pressure, `editor.setMemoryBudget`).
    pub fn free_view_sets(&self, want: u64) -> u64 {
        let mut r = self.rs.renderer.write();
        let Some(res) = r.callback_resources.canvas_mut(self.id) else { return 0 };
        let mut freed = 0u64;
        while freed < want {
            let b = res.drop_oldest_view_set();
            if b == 0 {
                break;
            }
            freed = freed.saturating_add(b);
        }
        freed
    }

    /// Document `key` changed from what its display set shows to `to` by layer visibility alone
    /// (the caller checked it with [`visibility_only`]). A kept set that shows `to` (as `display`
    /// stores it) takes the set's place, and the set is kept in its stead; otherwise the set is
    /// copied and kept, when the copy fits the budget, and the caller draws the change as usual.
    pub fn switch_view(&self, key: u64, to: &Arc<Document>, display: u64) -> ViewSwitch {
        let _s = crate::perf_span::span("canvas.view_switch");
        let (device, queue) = (&self.rs.device, &self.rs.queue);
        let mut r = self.rs.renderer.write();
        let Some(res) = r.callback_resources.canvas_mut(self.id) else { return ViewSwitch::None };
        let Some(current) = res.docs.get(&key) else { return ViewSwitch::None };
        let (size, format) = (current.size, current.format);
        if current.shows.is_none() {
            return ViewSwitch::None;
        }
        // Kept sets of an earlier state of the document (or of another size or format) show
        // nothing it can look like again.
        res.view_sets.retain(|v| v.key != key || (v.set.size == size && v.set.format == format && v.set.shows.as_ref().is_some_and(|(d, _)| Arc::ptr_eq(d, to) || same_but_visibility(d, to))));
        let now = now_ms();
        if let Some(i) = res.view_sets.iter().position(|v| v.key == key && v.set.shows.as_ref().is_some_and(|(d, k)| *k == display && same_view(d, to))) {
            let mut kept = res.view_sets.swap_remove(i);
            if let Some(mut old) = res.docs.insert(key, kept.set) {
                // (A page before a size change is never a view to come back to.)
                old.before = None;
                kept.set = old;
                kept.used = now;
                res.view_sets.push(kept);
            }
            if let Some(d) = res.docs.get_mut(&key) {
                // The same document, maybe another `Arc` of it.
                d.shows = Some((to.clone(), display));
                // Nothing of a document on screen waits (`Stale::blank`): the parts a prewarm
                // left undrawn go in the frame that shows it.
                d.stale().blank.clear();
            }
            return ViewSwitch::Restored;
        }
        // Keep a copy: room for it beside the documents' own sets (older kept sets give way).
        let bytes = canvas_texture_bytes(size, self.tile, texel_bytes(format));
        while res.view_sets.iter().filter(|v| v.key == key).count() >= MAX_VIEW_SETS || res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
            if res.drop_oldest_view_set() == 0 {
                break;
            }
        }
        if res.canvas_bytes().saturating_add(bytes) > res.canvas_budget || res.view_sets.iter().filter(|v| v.key == key).count() >= MAX_VIEW_SETS {
            return ViewSwitch::None;
        }
        let Some(current) = res.docs.get(&key) else { return ViewSwitch::None };
        let Some(copy) = current.duplicate(device, queue, res, self.tile) else { return ViewSwitch::None };
        res.view_sets.push(ViewSet { key, set: copy, used: now });
        ViewSwitch::Kept
    }

    /// Draw `view` (document `doc` as a named view shows it, `view.id == doc.id`) into a kept set
    /// ahead of time, a slice of about [`super::FRAME_WORK_MS`] per call, the part near `visible`
    /// first: when the document is switched to that view, [`GpuCanvas::switch_view`] shows it in
    /// one frame. `Some(true)` once it is all drawn (or already kept), `Some(false)` while more is
    /// left, `None` when it can't be kept (no display set for the document yet, no room in the
    /// budget, or the GPU is gone).
    pub fn prewarm_view_step(
        &self,
        view: &Arc<Document>,
        display: Option<&photocraft_engine::display_color::CanvasDisplay>,
        display_key: u64,
        visible: photocraft_geom::Rect,
    ) -> Option<bool> {
        let _s = crate::perf_span::span("canvas.prewarm_view");
        let key = view.id.0;
        let size = [view.size.width, view.size.height];
        let format = self.format_for(view.depth, size);
        if self.fault().is_some() || !self.has(key, size) || self.format_of(key) != Some(format) {
            return None;
        }
        let bounds = view.bounds();
        // Take the view's kept set out (made when missing), as the document's set for the
        // composites below, the document's own set aside meanwhile.
        let own = {
            let device = &self.rs.device;
            let mut r = self.rs.renderer.write();
            let res = r.callback_resources.canvas_mut(self.id)?;
            if res.docs.get(&key).and_then(|d| d.shows.as_ref()).is_some_and(|(d, k)| *k == display_key && same_view(d, view)) {
                return Some(true);
            }
            res.view_sets.retain(|v| v.key != key || v.set.size == size && v.set.format == format && v.set.shows.as_ref().is_some_and(|(d, _)| same_but_visibility(d, view)));
            let found = res.view_sets.iter().position(|v| v.key == key && v.set.shows.as_ref().is_some_and(|(d, k)| *k == display_key && same_view(d, view)));
            let set = match found {
                Some(i) => {
                    let v = res.view_sets.swap_remove(i);
                    if v.set.stale().is_empty() {
                        res.view_sets.push(v);
                        return Some(true);
                    }
                    v.set
                }
                None => {
                    let bytes = canvas_texture_bytes(size, self.tile, texel_bytes(format));
                    while res.view_sets.iter().filter(|v| v.key == key).count() >= MAX_VIEW_SETS || res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
                        if res.drop_oldest_view_set() == 0 {
                            break;
                        }
                    }
                    if res.canvas_bytes().saturating_add(bytes) > res.canvas_budget {
                        return None;
                    }
                    let mut set = DocTextures::new(device, res, size, self.tile, format)?;
                    set.shows = Some((view.clone(), display_key));
                    set
                }
            };
            res.docs.insert(key, set)?
        };
        // Pieces of what is still to draw, the view's part first, until the slice is used.
        let t0 = now_ms();
        let encode_srgb = display.is_some_and(|d| d.encode_srgb);
        let mut ok = true;
        loop {
            let piece = self.with_doc(key, |_, d| d.stale().next_piece(visible.intersect(&bounds), super::REFINE_WORK)).flatten();
            let Some(piece) = piece else { break };
            if self.composite_lod_with(view, piece, encode_srgb, 0, true, false).is_err() {
                // Off the GPU compositor (WebGL warming up, unsupported features): the CPU one.
                let buf = photocraft_compose::render(view, piece);
                let Some(fmt) = self.format_of(key) else {
                    ok = false;
                    break;
                };
                let buf = texture_buffer(display, &buf);
                let r = buf.rect;
                if r.x0 < 0 || r.y0 < 0 || !self.upload_rect(key, [r.x0 as u32, r.y0 as u32], [r.width(), r.height()], &texels(fmt, &buf.px)) {
                    ok = false;
                    break;
                }
            }
            if now_ms() - t0 >= super::frame_work_ms() {
                break;
            }
        }
        // Put the document's own set back and keep the view's.
        let mut r = self.rs.renderer.write();
        let res = r.callback_resources.canvas_mut(self.id)?;
        let set = res.docs.insert(key, own)?;
        if !ok || self.fault().is_some() {
            return None;
        }
        let done = set.stale().is_empty();
        res.view_sets.push(ViewSet { key, set, used: now_ms() });
        res.trim_view_sets();
        Some(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Document {
        let mut d = Document::new("t", photocraft_geom::Size::new(8, 8), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8);
        let mut a = Layer::raster("a", d.pixel_format());
        let mut b = Layer::raster("b", d.pixel_format());
        (a.id, b.id) = (photocraft_doc::LayerId(1), photocraft_doc::LayerId(2));
        a.visible = true;
        d.layers = vec![a, b];
        d
    }

    #[test]
    fn a_visibility_change_alone_is_told_apart_from_an_edit() {
        let a = doc();
        let mut b = a.clone();
        assert!(!visibility_only(&a, &b), "nothing changed");
        b.layers[0].visible = false;
        b.last_applied_comp = Some(3);
        assert!(visibility_only(&a, &b));
        assert!(same_but_visibility(&a, &b));
        b.layers[1].name = "renamed".into();
        b.layer_comps.clear();
        assert!(visibility_only(&a, &b), "a rename and the comps draw nothing");
        b.layers[1].opacity = 0.25;
        assert!(!visibility_only(&a, &b), "an opacity draws");
        let mut c = a.clone();
        c.layers[1].opacity = 0.5;
        assert!(!same_but_visibility(&a, &c));
    }
}
