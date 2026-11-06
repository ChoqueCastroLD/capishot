//! Type in perspective in PSD files. Photoshop has no perspective on live type, so a type layer
//! with a [`TextPerspective`] is saved as what Photoshop can show and edit: a smart object whose
//! contents are the flat type layer, placed through the perspective (Photoshop draws it right and
//! re-renders it after an edit inside; psd-compiler renders the placement too). Its pixels are our
//! sharp rendering, and a block of our own on the layer ([`BLOCK`]) marks it, so Capishot opens it
//! back as the type layer it was, in perspective and editable in place.
//!
//! The other choice, a type layer keeping its text with our block and the baked pixels, would
//! show right in Photoshop only until it re-renders the text (any edit, a font update), which
//! flattens it silently, and psd-compiler, which renders type from its text, would show it flat.

use std::sync::Arc;

use photocraft_doc::text::TextPerspective;
use photocraft_doc::{Document, Layer, LayerContent, SmartObject, SmartSource, TextLayer};
use photocraft_geom::{Affine, Size};

/// The layer block marking a smart object that is type in perspective (Capishot's own key).
pub const BLOCK: [u8; 4] = *b"CpPT";

/// What [`BLOCK`] holds: a version, so later formats can be told apart.
const MARK: &[u8] = br#"{"capishotPerspectiveType":1}"#;

/// `doc` with each type layer in perspective turned into the smart object it is saved as, or
/// `None` when it has none (nothing to change).
pub(crate) fn wrap_for_psd(doc: &Document) -> Option<Document> {
    if !doc.walk().iter().any(|(_, _, l)| is_perspective_type(l)) {
        return None;
    }
    let mut out = doc.clone();
    wrap_layers(&mut out.layers, doc);
    Some(out)
}

fn is_perspective_type(l: &Layer) -> bool {
    matches!(&l.content, LayerContent::Text(t) if t.perspective.is_some_and(|p| p.is_valid()))
}

fn wrap_layers(layers: &mut [Layer], doc: &Document) {
    for l in layers {
        if let LayerContent::Group(g) = &mut l.content {
            wrap_layers(&mut g.children, doc);
            continue;
        }
        let LayerContent::Text(t) = &l.content else { continue };
        let Some(p) = t.perspective.filter(|p| p.is_valid()) else { continue };
        if let Some(sm) = smart_of(doc, t, &p) {
            l.content = LayerContent::Smart(sm);
            // The file's `TySh`, if it came from one, is the type as it was then: on opening it
            // would win over the contents.
            l.psd_blocks.retain(|(k, _)| *k != BLOCK && k != b"TySh");
            l.psd_blocks.push((BLOCK, Arc::new(MARK.to_vec())));
        }
    }
}

/// The smart object type `t` in perspective `p` is saved as: its contents a document the size of
/// the perspective's frame holding the flat type, placed through the perspective.
fn smart_of(doc: &Document, t: &TextLayer, p: &TextPerspective) -> Option<SmartObject> {
    let [x0, y0, x1, y1] = p.frame;
    let (w, h) = ((x1 - x0).ceil().max(1.0), (y1 - y0).ceil().max(1.0));
    if !(w.is_finite() && h.is_finite()) || w > 30_000.0 || h > 30_000.0 {
        return None;
    }
    let mut contents = Document::new("Type", Size::new(w as u32, h as u32), doc.mode, doc.pixel_format().sample);
    contents.resolution_dpi = doc.resolution_dpi;
    let mut flat = t.clone();
    flat.perspective = None;
    flat.transform = Affine::translate(-x0, -y0).mul(&t.transform);
    flat.cache = None;
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    let (layout, r) = eng.render(&flat, contents.resolution_dpi, contents.pixel_format());
    drop(eng);
    flat.cache = Some(r.surface);
    flat.psd_raw = Some(Arc::new(photocraft_text::psd::build_tysh(&flat, contents.resolution_dpi, layout.bounds())));
    contents.layers.push(Layer::new("Type", LayerContent::Text(flat)));
    let bytes = photocraft_format::save_to_bytes(&contents, &Default::default()).ok()?;
    // Contents pixels → document pixels: the frame's origin, then the perspective.
    let m = p.h;
    let placement = [m[0], m[1], m[0] * x0 + m[1] * y0 + m[2], m[3], m[4], m[3] * x0 + m[4] * y0 + m[5], m[6], m[7], m[6] * x0 + m[7] * y0 + m[8]];
    let n = placement.map(|v| v / placement[8]);
    let (o, ux, uy) = (apply(&n, 0.0, 0.0), apply(&n, 1.0, 0.0), apply(&n, 0.0, 1.0));
    let affine = Affine { m: [ux.0 - o.0, ux.1 - o.1, uy.0 - o.0, uy.1 - o.1, o.0, o.1] };
    let mut sm = SmartObject::new(SmartSource::Embedded { file_name: "Type.pcraft".into(), bytes: Arc::new(bytes) }, affine, t.cache.clone());
    sm.perspective = Some(n);
    Some(sm)
}

fn apply(m: &[f64; 9], x: f64, y: f64) -> (f64, f64) {
    let w = m[6] * x + m[7] * y + m[8];
    ((m[0] * x + m[1] * y + m[2]) / w, (m[3] * x + m[4] * y + m[5]) / w)
}

/// After a PSD opened: each smart object Capishot saved for type in perspective ([`BLOCK`])
/// becomes that type layer again, keeping the pixels it was saved with.
pub(crate) fn unwrap_after_import(doc: &mut Document) {
    let meta = doc.metadata.clone();
    unwrap_layers(&mut doc.layers, &meta);
}

fn unwrap_layers(layers: &mut [Layer], meta: &photocraft_doc::Metadata) {
    for l in layers {
        if let LayerContent::Group(g) = &mut l.content {
            unwrap_layers(&mut g.children, meta);
            continue;
        }
        if !l.psd_blocks.iter().any(|(k, _)| *k == BLOCK) {
            continue;
        }
        let LayerContent::Smart(sm) = &l.content else { continue };
        if let Some(t) = type_of(sm, meta) {
            l.content = LayerContent::Text(t);
            l.psd_blocks.retain(|(k, _)| *k != BLOCK);
        }
    }
}

/// The type in perspective a marked smart object holds: its contents' type layer, with the
/// placement as its perspective framed on the contents' canvas.
fn type_of(sm: &SmartObject, meta: &photocraft_doc::Metadata) -> Option<TextLayer> {
    // The embedded file: in the layer, or (as a PSD stores it) in the document's linked files.
    let (file_name, bytes): (String, Arc<Vec<u8>>) = match &sm.source {
        SmartSource::Embedded { file_name, bytes } => (file_name.clone(), bytes.clone()),
        SmartSource::Linked { path } => {
            let f = crate::linked::find_linked_file(meta, path)?;
            (f.file_name, Arc::new(f.bytes))
        }
    };
    let contents = if crate::is_psd(&bytes) { crate::import(&file_name, &bytes).ok()?.document } else { photocraft_format::load_from_bytes(&bytes).ok()? };
    let mut t = contents.walk().into_iter().find_map(|(_, _, l)| match &l.content {
        LayerContent::Text(t) => Some(t.clone()),
        _ => None,
    })?;
    let a = sm.transform.m;
    let mut h = sm.perspective.unwrap_or([a[0], a[2], a[4], a[1], a[3], a[5], 0.0, 0.0, 1.0]);
    let frame = [0.0, 0.0, f64::from(contents.size.width), f64::from(contents.size.height)];
    let w = h[6] * frame[2] / 2.0 + h[7] * frame[3] / 2.0 + h[8];
    if !(w.is_finite() && w.abs() > 1e-12) {
        return None;
    }
    h = h.map(|v| v / w);
    let p = TextPerspective { h, frame };
    if !p.is_valid() {
        return None;
    }
    t.perspective = Some(p);
    t.cache = sm.cache.clone();
    Some(t)
}
