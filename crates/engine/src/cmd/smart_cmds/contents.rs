use super::filters::{smart, smart_mut};
use super::placing::detach_psd;
use super::rendering::{refresh, refresh_or_fail, unapplied_filters};
use super::source::{authorized_source_bytes, source_bytes};
use super::{
    Arc, DocId, Document, EngineError, LayerContent, LayerId, Metadata, Result, Session, SmartContentsId, SmartLink, SmartSource, Value, bad, base_name,
    decode_source, encode_source, json, layer_param, other,
};

pub(super) fn path_param<'a>(cmd: &str, p: &'a Value) -> Result<&'a str> {
    p.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| bad(cmd, "pass `path`"))
}

/// Instances sharing contents in this document, including those inside groups.
fn instances(doc: &Document, contents_id: SmartContentsId) -> Vec<LayerId> {
    doc.walk().into_iter().filter_map(|(_, _, l)| matches!(&l.content, LayerContent::Smart(sm) if sm.contents_id == contents_id).then_some(l.id)).collect()
}

/// Swaps shared contents, keeping each instance's transform and filters, and re-renders.
pub(super) fn set_source(
    s: &mut Session,
    p: &Value,
    label: &str,
    keep_psd: bool,
    make: impl FnOnce(&Metadata, &SmartSource) -> Result<SmartSource>,
) -> Result<Value> {
    let id = layer_param(s, p)?;
    let doc = &s.active().ok_or(EngineError::NoDocument)?.doc;
    let new = make(&doc.metadata, &smart(doc, id)?.source)?;
    set_shared_source(s, id, label, keep_psd, new)
}

// Keep the shared-instance transaction out of the per-command source-builder instantiations.
fn set_shared_source(s: &mut Session, id: LayerId, label: &str, keep_psd: bool, new: SmartSource) -> Result<Value> {
    let (parent, contents_id, changed, warnings) = s.edit(label, |doc, _| {
        let contents_id = smart(doc, id)?.contents_id;
        let changed = instances(doc, contents_id);
        let mut warnings = std::collections::BTreeSet::new();
        for &instance in &changed {
            smart_mut(doc, instance)?.source = new.clone();
            if !keep_psd {
                detach_psd(doc.layer_mut(instance).ok_or(EngineError::NoLayer(instance))?);
            }
            warnings
                .extend(unapplied_filters(smart(doc, instance)?).into_iter().map(|n| format!("the {n} smart filter is not supported, so it was not applied")));
            refresh_or_fail(doc, instance)?;
        }
        Ok((doc.id, contents_id, changed, warnings))
    })?;
    // An old editor must not overwrite newly replaced/relinked contents on close. Keep its
    // document open, but the next Edit Contents opens the replacement source.
    s.smart_links.retain(|l| l.parent != parent || l.contents_id != contents_id);
    // Every instance changed, not only `layer`: list them, so a caller doesn't repeat the
    // command once per copy (#4075).
    Ok(json!({"layer": id.0, "instances": changed.iter().map(|l| l.0).collect::<Vec<_>>(), "warnings": warnings}))
}

pub(super) fn replace_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_param("layer.smartObjects.replaceContents", p)?.to_string();
    let bytes = photocraft_format::read_file(std::path::Path::new(&path)).map_err(|e| other(format!("can't read {path}: {e}")))?;
    let id = layer_param(s, p)?;
    replace_contents_bytes(s, id, &base_name(&path), bytes)
}

/// Replace Contents with a file the caller already read, for the smart object `p` names (`layer`,
/// else the active layer): the body of the command minus its ambient path read (#8061).
pub fn replace_contents_from_bytes(s: &mut Session, p: &Value, file_name: &str, bytes: Vec<u8>) -> Result<Value> {
    let id = layer_param(s, p)?;
    replace_contents_bytes(s, id, file_name, bytes)
}

/// Replace Contents from a file's name and bytes: what the desktop and web file pickers hand over.
pub fn replace_contents_bytes(s: &mut Session, id: LayerId, file_name: &str, bytes: Vec<u8>) -> Result<Value> {
    decode_source(file_name, &bytes)?; // fail before touching the document
    set_shared_source(s, id, "Replace Contents", false, SmartSource::Embedded { file_name: file_name.to_string(), bytes: Arc::new(bytes) })
}

/// The file name and bytes of smart object `id`'s contents.
pub fn contents_of(doc: &Document, id: LayerId) -> Result<(String, Arc<Vec<u8>>)> {
    source_bytes(&doc.metadata, &smart(doc, id)?.source).ok_or_else(|| other("the smart object's contents are unavailable"))
}

/// Writes the smart object's file as it is. To a path with another extension (a `.png` for
/// `.psb` or `.pcraft` contents) the contents are opened and exported in that format instead.
pub(super) fn export_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let path = path_param("layer.smartObjects.exportContents", p)?;
    let id = layer_param(s, p)?;
    let (name, bytes) = contents_of(&s.active().ok_or(EngineError::NoDocument)?.doc, id)?;
    let ext = |f: &str| {
        let e = std::path::Path::new(f).extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
        e.map(|e| match e.as_str() {
            "jpeg" => "jpg".to_string(),
            "tiff" => "tif".to_string(),
            _ => e,
        })
    };
    if ext(path).is_none() || ext(path) == ext(&name) {
        crate::file_cmds::write_file(path, &bytes)?;
        return Ok(json!({"path": path, "fileName": name, "bytes": bytes.len(), "converted": false}));
    }
    let contents = decode_source(&name, &bytes)?;
    let r = photocraft_io::export(&contents, path, &Default::default()).map_err(|e| other(format!("can't export the contents as {}: {e}", base_name(path))))?;
    crate::file_cmds::write_file(path, &r.bytes)?;
    Ok(json!({"path": path, "fileName": name, "bytes": r.bytes.len(), "converted": true, "warnings": r.warnings}))
}

pub(super) fn edit_contents(s: &mut Session, p: &Value) -> Result<Value> {
    let id = layer_param(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let parent = st.doc.id;
    let contents_id = smart(&st.doc, id)?.contents_id;
    // Already open for editing: switch to that document rather than opening another copy.
    if let Some(index) =
        s.smart_links.iter().find(|l| l.parent == parent && l.contents_id == contents_id).and_then(|l| s.documents().iter().position(|d| d.doc.id == l.child))
    {
        s.set_active(index);
        return Ok(json!({"document": index, "parentLayer": id.0}));
    }
    let source = &smart(&st.doc, id)?.source;
    let (name, bytes) = authorized_source_bytes(s, "layer.smartObjects.editContents", &st.doc.metadata, source)?
        .ok_or_else(|| other("the smart object's contents are unavailable"))?;
    let mut child = decode_source(&name, &bytes)?;
    // Bundles keep their document id; each open copy needs its own.
    child.id = DocId::fresh();
    child.name = name;
    let index = s.add_document(child, None);
    // Admission may replace an ID already owned by another open document.
    let child_id = s.documents().get(index).ok_or(EngineError::NoDocument)?.doc.id;
    s.smart_links.retain(|l| l.child != child_id);
    s.smart_links.push(SmartLink { child: child_id, parent, layer: id, contents_id });
    Ok(json!({"document": index, "parentLayer": id.0}))
}

/// Writes an Edit Contents document back into all its parent instances (one undoable step in the
/// parent) and marks it saved. Returns false if `index` isn't an Edit Contents document.
pub fn commit_child(s: &mut Session, index: usize) -> Result<bool> {
    commit_child_fitting(s, index).map(|(updated, _)| updated)
}

/// [`commit_child`], also saying whether type contents had to stay cut off: their text left the
/// canvas, and growing it would have carried the text past an instance's perspective horizon.
pub fn commit_child_fitting(s: &mut Session, index: usize) -> Result<(bool, bool)> {
    let Some(st) = s.docs.get(index) else { return Ok((false, false)) };
    let child_id = st.doc.id;
    let Some(link) = s.smart_links.iter().find(|l| l.child == child_id).copied() else { return Ok((false, false)) };
    let parent = s.docs.iter().position(|d| d.doc.id == link.parent).ok_or_else(|| other("the smart object's document was closed"))?;
    // Type contents whose text outgrew its canvas (or shrank well inside it): the canvas follows
    // the text, and each instance's placement takes the shift, so nothing is clipped and what
    // was there stays where it was on the page.
    let (shift, cut) = match fit_type_canvas(s, index, parent, link.contents_id)? {
        Fit::Same => (None, false),
        Fit::Moved(x0, y0) => (Some((x0, y0)), false),
        Fit::Cut => (None, true),
    };
    let Some(st) = s.docs.get(index) else { return Ok((false, cut)) };
    let child = st.doc.clone();
    let bytes = Arc::new(encode_source(&child)?);
    let prev = s.active;
    s.active = Some(parent);
    s.note_active_change();
    let r = s.edit("Edit Contents", |doc, _| {
        let ids = instances(doc, link.contents_id);
        if ids.is_empty() {
            return Err(other("the smart object's instances were removed"));
        }
        for id in ids {
            let sm = smart_mut(doc, id)?;
            let name = match &sm.source {
                SmartSource::Embedded { file_name, .. } => file_name.as_str(),
                SmartSource::Linked { .. } => child.name.as_str(),
            };
            let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
            sm.source = SmartSource::Embedded { file_name: format!("{stem}.pcraft"), bytes: bytes.clone() };
            if let Some((x0, y0)) = shift {
                place_after_shift(sm, x0, y0);
            }
            detach_psd(doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?);
            refresh_or_fail(doc, id)?;
        }
        Ok(())
    });
    s.active = prev;
    s.note_active_change();
    r?;
    if let Some(st) = s.docs.get_mut(index) {
        st.saved_revision = st.revision;
    }
    Ok((true, cut))
}

/// Room left around the text when the canvas is fitted to it: a tenth of the text's height, at
/// least this many pixels.
const TYPE_PAD_MIN: i32 = 4;

/// What [`fit_type_canvas`] did to the contents' canvas.
enum Fit {
    /// Left as it was (not type contents, or the text fits it).
    Same,
    /// Fitted to the text: the new canvas's origin in the old one's pixels.
    Moved(i32, i32),
    /// Left as it was although the text leaves it: grown, it would reach an instance's horizon.
    Cut,
}

/// Edit Contents of type (every layer of the contents a type layer, as Type › Perspective makes
/// it): when the text's bounds leave the canvas, or the canvas reaches past the text by more than
/// the padding, the contents document's canvas becomes the text's bounds plus padding (a "Canvas
/// Size" step there), and [`place_after_shift`] keeps each instance's pixels in place. An instance
/// with a warp keeps its canvas (the warp is laid out over it), and so does a perspective whose
/// horizon the grown canvas would reach or come near ([`before_horizon`]).
fn fit_type_canvas(s: &mut Session, index: usize, parent: usize, contents_id: SmartContentsId) -> Result<Fit> {
    let (Some(st), Some(pst)) = (s.docs.get(index), s.docs.get(parent)) else { return Ok(Fit::Same) };
    let placed: Vec<&photocraft_doc::SmartObject> = instances(&pst.doc, contents_id)
        .into_iter()
        .filter_map(|id| match pst.doc.layer(id).map(|l| &l.content) {
            Some(LayerContent::Smart(sm)) => Some(sm),
            _ => None,
        })
        .collect();
    let layers = st.doc.walk();
    if placed.iter().any(|sm| sm.warp.is_some()) || layers.is_empty() || !layers.iter().all(|(_, _, l)| matches!(l.content, LayerContent::Text(_))) {
        return Ok(Fit::Same);
    }
    let ink = layers
        .iter()
        .filter(|(_, _, l)| l.visible)
        .filter_map(|(_, _, l)| crate::layer_multi_cmds::layer_bounds(l))
        .fold(photocraft_geom::Rect::EMPTY, |a, b| if a.is_empty() { b } else { a.union(&b) });
    if ink.is_empty() {
        return Ok(Fit::Same);
    }
    let pad = (ink.height() as i32 / 10).max(TYPE_PAD_MIN);
    let target = ink.inflate(pad);
    let canvas = st.doc.bounds();
    if canvas.contains_rect(&ink) && target.contains_rect(&canvas) {
        return Ok(Fit::Same);
    }
    if !placed.iter().all(|sm| sm.perspective.is_none_or(|h| before_horizon(&h, canvas, target))) {
        return Ok(if canvas.contains_rect(&ink) { Fit::Same } else { Fit::Cut });
    }
    let (x0, y0) = (target.x0, target.y0);
    let prev = s.active;
    s.active = Some(index);
    s.note_active_change();
    let r = s.edit("Canvas Size", |doc, _| {
        crate::image_cmds::translate_doc(doc, "layer.smartObjects.saveContents", -x0, -y0)?;
        doc.size = target.size();
        Ok(())
    });
    s.active = prev;
    s.note_active_change();
    r?;
    Ok(Fit::Moved(x0, y0))
}

/// The most a grown canvas may be magnified, at its far end, against its least magnified point:
/// beyond this the perspective's horizon is close (the text would grow without bound).
const MAX_PERSPECTIVE_SPREAD: f64 = 32.0;

/// Does `target` (in the old contents' pixels, plus the sampler's pixel) stay on the near side of
/// the projective map `h`'s horizon, as `canvas` does, and short of it by [`MAX_PERSPECTIVE_SPREAD`]?
fn before_horizon(h: &[f64; 9], canvas: photocraft_geom::Rect, target: photocraft_geom::Rect) -> bool {
    let w = |x: i32, y: i32| h[6] * f64::from(x) + h[7] * f64::from(y) + h[8];
    let corners = |r: photocraft_geom::Rect| [(r.x0 - 1, r.y0 - 1), (r.x1 + 1, r.y0 - 1), (r.x1 + 1, r.y1 + 1), (r.x0 - 1, r.y1 + 1)];
    let side = if w(canvas.x0, canvas.y0) < 0.0 { -1.0 } else { 1.0 };
    let ws: Vec<f64> = corners(target).iter().map(|&(x, y)| side * w(x, y)).collect();
    let (lo, hi) = ws.iter().fold((f64::MAX, 0.0f64), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    lo > 0.0 && lo.is_finite() && hi / lo <= MAX_PERSPECTIVE_SPREAD
}

/// The contents' canvas moved its origin to `(x0, y0)` of the old canvas: the placement maps each
/// old pixel where it went before (the same affine or projective map, just over more of it).
fn place_after_shift(sm: &mut photocraft_doc::SmartObject, x0: i32, y0: i32) {
    let (x, y) = (f64::from(x0), f64::from(y0));
    sm.transform = sm.transform.mul(&photocraft_geom::Affine::translate(x, y));
    if let Some(h) = sm.perspective.as_mut() {
        // H · T(x, y): the third column takes the shift.
        h[2] += h[0] * x + h[1] * y;
        h[5] += h[3] * x + h[4] * y;
        h[8] += h[6] * x + h[7] * y;
    }
}

/// Called before a document closes: an edited Edit Contents document updates its parent.
pub(crate) fn on_close(s: &mut Session, index: usize) {
    let Some(st) = s.docs.get(index) else { return };
    let id = st.doc.id;
    if st.is_dirty() && s.smart_links.iter().any(|l| l.child == id) {
        let _ = commit_child(s, index);
    }
    s.smart_links.retain(|l| l.child != id && l.parent != id);
    // Decoded sources are shared by content hash across documents, so the closing document's
    // can't be told apart: drop them all. Other documents decode again on their next re-render.
    if s.docs.get(index).is_some_and(|st| st.doc.walk().iter().any(|(_, _, l)| matches!(l.content, LayerContent::Smart(_)))) {
        super::purge_source_cache();
    }
}

pub(super) fn is_smart_child(s: &Session) -> std::result::Result<(), String> {
    let d = s.active().ok_or("no document open")?;
    if s.smart_links.iter().any(|l| l.child == d.doc.id) { Ok(()) } else { Err("this document isn't smart object contents".into()) }
}

pub(super) fn update_all(s: &mut Session) -> Result<Value> {
    let ids: Vec<LayerId> = s
        .active()
        .ok_or(EngineError::NoDocument)?
        .doc
        .walk()
        .into_iter()
        .filter(|(_, _, l)| matches!(&l.content, LayerContent::Smart(sm) if matches!(sm.source, SmartSource::Linked { .. })))
        .map(|(_, _, l)| l.id)
        .collect();
    let mut updated = Vec::new();
    s.edit("Update All Modified Content", |doc, _| {
        for id in &ids {
            if refresh(doc, *id)? {
                updated.push(id.0);
            }
        }
        Ok(())
    })?;
    Ok(json!({"updated": updated}))
}
