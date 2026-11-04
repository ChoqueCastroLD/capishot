//! Rendering a document (or a part of it) to RGBA for thumbnails, compare views and previews:
//! the in-process `doc.render`. [`crate::Host::render`] renders now; [`crate::Host::snapshot`]
//! takes a cheap [`Snapshot`] (`Send`) to render on a worker thread while the editor keeps going.

use std::collections::HashSet;
use std::sync::Arc;

use photocraft_doc::{Document, Layer};
use photocraft_geom::Rect;

use crate::{DocumentId, Error, LayerId, LayerUid, Result};

/// Output pixels at most (width × height) of one render: 64 MP, about 256 MB of RGBA.
pub const MAX_OUTPUT_PIXELS: u64 = 64 << 20;
/// Canvas coordinates past this (either way) are refused as nonsense.
const MAX_COORD: i64 = 1 << 20;

/// What to render. The default is the whole canvas at full size, every layer as it is shown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RenderOptions {
    /// `[x, y, width, height]` in document pixels (clipped to the canvas); `None`: the canvas.
    pub rect: Option<[i32; 4]>,
    /// Output scale in (0, 1]; `None`: 1, or what `max_side` asks for.
    pub scale: Option<f64>,
    /// Fit the output's longer side into this many pixels (never enlarges). With `scale` too,
    /// the smaller of the two wins.
    pub max_side: Option<u32>,
    /// Draw only these layers (by session id), with the groups around them; `None`: all.
    pub layers: Option<Vec<LayerId>>,
    /// Draw only these layers (by stable id), added to `layers`.
    pub uids: Option<Vec<LayerUid>>,
}

/// Rendered pixels: straight (not premultiplied) 8-bit RGBA, row-major, `width * height * 4`
/// bytes, in the document's colour values (an RGB document's own RGB). `rect` is the canvas
/// area they show, `[x, y, width, height]` in document pixels.
#[derive(Clone, PartialEq, Eq)]
pub struct Rgba {
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// The canvas area rendered (after clipping), document pixels.
    pub rect: [i32; 4],
    /// `width * height` RGBA pixels.
    pub pixels: Vec<u8>,
}

impl std::fmt::Debug for Rgba {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rgba").field("width", &self.width).field("height", &self.height).field("rect", &self.rect).finish_non_exhaustive()
    }
}

impl Rgba {
    /// The pixels as a PNG file.
    pub fn to_png(&self) -> Result<Vec<u8>> {
        let image = photocraft_codecs::Image::from_u8(self.width, self.height, photocraft_codecs::ChannelLayout::Rgba, self.pixels.clone())
            .map_err(|e| Error::File(e.to_string()))?;
        photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).map_err(|e| Error::File(e.to_string()))
    }
}

/// An open document as it was when taken ([`crate::Host::snapshot`]): later edits don't change
/// it. Cheap (shared), `Send`: render it on any thread.
#[derive(Clone)]
pub struct Snapshot(Arc<Document>);

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot").field("name", &self.0.name).finish_non_exhaustive()
    }
}

impl Snapshot {
    /// Renders it as `opts` asks (see [`RenderOptions`]). Errors (never panics) on a rect off the
    /// canvas, a bad scale, an unknown layer or an output over [`MAX_OUTPUT_PIXELS`].
    pub fn render(&self, opts: &RenderOptions) -> Result<Rgba> {
        render(&self.0, opts)
    }

    /// The whole canvas fitted into `max_side` pixels (a thumbnail; never enlarged).
    pub fn thumbnail(&self, max_side: u32) -> Result<Rgba> {
        if max_side == 0 {
            return Err(bad("`max_side` must be at least 1"));
        }
        render(&self.0, &RenderOptions { max_side: Some(max_side), ..RenderOptions::default() })
    }
}

impl crate::Host {
    /// Open document `document` as it is now, to render elsewhere ([`Snapshot::render`]).
    pub fn snapshot(&self, document: DocumentId) -> Result<Snapshot> {
        let id: photocraft_doc::DocId = document.into();
        self.session().documents().iter().find(|st| st.doc.id == id).map(|st| Snapshot(st.doc.clone())).ok_or(Error::NoDocument(document))
    }

    /// Renders open document `document` as `opts` asks, now (the in-process `doc.render`).
    pub fn render(&self, document: DocumentId, opts: &RenderOptions) -> Result<Rgba> {
        self.snapshot(document)?.render(opts)
    }
}

fn render(doc: &Document, opts: &RenderOptions) -> Result<Rgba> {
    let rect = region(doc, opts.rect)?;
    let scale = output_scale(rect, opts.scale, opts.max_side)?;
    let (w, h) = output_size(rect, scale);
    if u64::from(w).saturating_mul(u64::from(h)) > MAX_OUTPUT_PIXELS {
        return Err(bad(format!("the output would be {w}×{h} pixels, over {MAX_OUTPUT_PIXELS}: pass a smaller rect, scale or max_side")));
    }
    let mut ids: Option<Vec<photocraft_doc::LayerId>> = opts.layers.as_ref().map(|v| v.iter().map(|&l| l.into()).collect());
    if let Some(uids) = &opts.uids {
        ids.get_or_insert_with(Vec::new).extend(layers_by_uid(doc, uids)?);
    }
    let filtered;
    let doc = match &ids {
        Some(ids) => {
            filtered = only_layers(doc, ids)?;
            &filtered
        }
        None => doc,
    };
    let img = photocraft_compose::render_reduced_rect(doc, rect, w, h).to_rgba8();
    Ok(Rgba { width: img.width, height: img.height, rect: rect_array(rect), pixels: img.pixels })
}

fn bad(message: impl Into<String>) -> Error {
    crate::error::bad("render", message)
}

fn rect_array(r: Rect) -> [i32; 4] {
    [r.x0, r.y0, i32::try_from(r.width()).unwrap_or(i32::MAX), i32::try_from(r.height()).unwrap_or(i32::MAX)]
}

/// `rect` (or the canvas) clipped to the canvas.
fn region(doc: &Document, rect: Option<[i32; 4]>) -> Result<Rect> {
    let canvas = doc.bounds();
    let Some([x, y, w, h]) = rect else {
        if canvas.is_empty() {
            return Err(bad("the document has no pixels"));
        }
        return Ok(canvas);
    };
    if [x, y, w, h].iter().any(|v| i64::from(*v).unsigned_abs() > MAX_COORD as u64) {
        return Err(bad("`rect` is out of range"));
    }
    if w <= 0 || h <= 0 {
        return Err(bad("`rect` needs a positive width and height"));
    }
    // In range above: the sums can't overflow.
    let r = Rect::new(x, y, x.saturating_add(w), y.saturating_add(h)).intersect(&canvas);
    if r.is_empty() {
        return Err(bad("`rect` is outside the canvas"));
    }
    Ok(r)
}

/// The scale `scale` and `max_side` ask for, in (0, 1].
fn output_scale(rect: Rect, scale: Option<f64>, max_side: Option<u32>) -> Result<f64> {
    let mut s = match scale {
        None => 1.0,
        Some(s) if s.is_finite() && s > 0.0 && s <= 1.0 => s,
        Some(s) => return Err(bad(format!("`scale` must be a number in (0, 1], got {s}"))),
    };
    if let Some(m) = max_side {
        if m == 0 {
            return Err(bad("`max_side` must be at least 1"));
        }
        let longest = rect.width().max(rect.height()).max(1);
        s = s.min(f64::from(m) / f64::from(longest));
    }
    Ok(s)
}

/// The output size of `rect` at `scale`, at least 1×1 and at most the rect's own size.
fn output_size(rect: Rect, scale: f64) -> (u32, u32) {
    // `scale` is in (0, 1]: the product fits u32.
    let side = |n: u32| ((f64::from(n) * scale).round() as u32).clamp(1, n.max(1));
    (side(rect.width()), side(rect.height()))
}

/// The session ids of the layers with stable ids `uids`, in that order.
fn layers_by_uid(doc: &Document, uids: &[LayerUid]) -> Result<Vec<photocraft_doc::LayerId>> {
    let walk = doc.walk();
    uids.iter()
        .map(|u| walk.iter().find(|(_, _, l)| l.psd_id == Some(u.0)).map(|(_, _, l)| l.id).ok_or_else(|| Error::NoLayer(format!("uid {}", u.0))))
        .collect()
}

/// `doc` with only `ids` drawn: each listed layer is shown (with what a listed group holds, as
/// it is), and so is every group around one; every other layer is hidden. An id not in the
/// document is an error. (The same rule as the control protocol's `doc.render {layers}`.)
fn only_layers(doc: &Document, ids: &[photocraft_doc::LayerId]) -> Result<Document> {
    let mut keep: HashSet<photocraft_doc::LayerId> = HashSet::new();
    let mut inside: HashSet<photocraft_doc::LayerId> = HashSet::new();
    for id in ids {
        let path = doc.path_of(*id).ok_or_else(|| Error::NoLayer(id.0.to_string()))?;
        for n in 1..=path.len() {
            if let Some(l) = path.get(..n).and_then(|p| doc.layer_at(p)) {
                keep.insert(l.id);
            }
        }
        if let Some(l) = doc.layer(*id) {
            inside.extend(walk_ids(l));
        }
    }
    let mut out = doc.clone();
    let all: Vec<photocraft_doc::LayerId> = doc.walk().into_iter().map(|(_, _, l)| l.id).collect();
    for id in all {
        if let Some(l) = out.layer_mut(id) {
            if ids.contains(&id) {
                l.visible = true;
            } else if !keep.contains(&id) && !inside.contains(&id) {
                l.visible = false;
            }
        }
    }
    Ok(out)
}

/// The ids of the layers inside `l` (a group), at any depth (iterative: no recursion limit).
fn walk_ids(l: &Layer) -> Vec<photocraft_doc::LayerId> {
    let mut out = Vec::new();
    let mut stack: Vec<&Layer> = l.children().map(|c| c.iter().collect()).unwrap_or_default();
    while let Some(c) = stack.pop() {
        out.push(c.id);
        if let Some(ch) = c.children() {
            stack.extend(ch.iter());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(w: u32, h: u32) -> Document {
        Document::new("t", photocraft_doc::Size::new(w, h), photocraft_doc::ColorMode::Rgb, photocraft_doc::SampleType::U8)
    }

    #[test]
    fn whole_canvas_at_full_size_and_scaled() {
        let d = doc(40, 20);
        let full = render(&d, &RenderOptions::default()).unwrap();
        assert_eq!((full.width, full.height, full.rect), (40, 20, [0, 0, 40, 20]));
        assert_eq!(full.pixels.len(), 40 * 20 * 4);
        let half = render(&d, &RenderOptions { scale: Some(0.5), ..Default::default() }).unwrap();
        assert_eq!((half.width, half.height), (20, 10));
        let thumb = Snapshot(Arc::new(d.clone())).thumbnail(10).unwrap();
        assert_eq!((thumb.width, thumb.height), (10, 5));
        assert!(thumb.to_png().unwrap().starts_with(b"\x89PNG"));
    }

    #[test]
    fn regions_are_clipped_to_the_canvas() {
        let d = doc(40, 20);
        let r = render(&d, &RenderOptions { rect: Some([30, 10, 50, 50]), ..Default::default() }).unwrap();
        assert_eq!((r.width, r.height, r.rect), (10, 10, [30, 10, 10, 10]));
    }

    #[test]
    fn bad_requests_are_errors_not_panics() {
        let d = doc(40, 20);
        for o in [
            RenderOptions { rect: Some([100, 100, 5, 5]), ..Default::default() },
            RenderOptions { rect: Some([0, 0, 0, 5]), ..Default::default() },
            RenderOptions { rect: Some([i32::MAX, i32::MIN, i32::MAX, i32::MAX]), ..Default::default() },
            RenderOptions { scale: Some(0.0), ..Default::default() },
            RenderOptions { scale: Some(f64::NAN), ..Default::default() },
            RenderOptions { scale: Some(2.0), ..Default::default() },
            RenderOptions { max_side: Some(0), ..Default::default() },
            RenderOptions { layers: Some(vec![LayerId(999_999)]), ..Default::default() },
            RenderOptions { uids: Some(vec![LayerUid(999_999)]), ..Default::default() },
        ] {
            assert!(render(&d, &o).is_err(), "{o:?}");
        }
        assert!(Snapshot(Arc::new(d)).thumbnail(0).is_err());
    }
}
