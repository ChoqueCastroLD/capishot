//! Reading a layer's pixels without touching the document (live sync, `docs/control-protocol.md`
//! › Live sync): `layer.getPixels` reads a rect of a layer (or its mask) straight from its tiles,
//! of any open document, without activating it and without a history step; `layer.dirtySince`
//! says where a layer changed after a `pixelRev` the host read earlier, so it reads only that.

use photocraft_color::{ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Layer, LayerContent, LayerId};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::doc_state::DocState;
use crate::pixel_revs::Dirty;
use crate::{EngineError, Result, Session};

/// The largest area one call reads (pixels): 2.5 × a 36 MP page.
const MAX_AREA: u64 = 100_000_000;

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

/// The document `document` names (any open one, active or not), else the active one.
fn target_doc<'a>(s: &'a Session, cmd: &str, p: &Value) -> Result<&'a DocState> {
    match p.get("document").filter(|v| !v.is_null()) {
        None => s.active().ok_or(EngineError::NoDocument),
        Some(v) => {
            let id = v.as_u64().ok_or_else(|| bad(cmd, "`document` must be a document id"))?;
            let st = s.documents().iter().find(|d| d.doc.id.0 == id).ok_or_else(|| bad(cmd, format!("no open document {id}")))?;
            // A sleeping page's stub has no pixels to read.
            if st.sleeping.is_some() {
                return Err(EngineError::Other(format!("document {id} is asleep: activate it first")));
            }
            Ok(st)
        }
    }
}

/// The layer `layer` names in `st`: an id, or a name (the topmost layer with it).
fn target_layer<'a>(st: &'a DocState, cmd: &str, p: &Value) -> Result<&'a Layer> {
    let found = match p.get("layer") {
        Some(Value::Number(n)) => n.as_u64().and_then(|id| st.doc.layer(LayerId(id))),
        Some(Value::String(name)) => st.doc.walk().into_iter().rev().map(|(_, _, l)| l).find(|l| l.name == *name),
        _ => return Err(bad(cmd, "needs `layer`: a layer id (or name)")),
    };
    found.ok_or_else(|| bad(cmd, format!("no layer {} in document {}", p.get("layer").unwrap_or(&Value::Null), st.doc.id.0)))
}

/// `rect` `[x, y, width, height]` (whole pixels), or `None` when not given.
fn rect_param(cmd: &str, p: &Value) -> Result<Option<Rect>> {
    let Some(v) = p.get("rect").filter(|v| !v.is_null()) else { return Ok(None) };
    let a = v.as_array().filter(|a| a.len() == 4).ok_or_else(|| bad(cmd, "`rect` must be [x, y, width, height]"))?;
    let mut n = [0i32; 4];
    for (o, v) in n.iter_mut().zip(a) {
        *o = v
            .as_f64()
            .filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= 1e7)
            .map(|f| f as i32)
            .ok_or_else(|| bad(cmd, "`rect` holds whole numbers of pixels"))?;
    }
    if n[2] < 0 || n[3] < 0 {
        return Err(bad(cmd, "`rect`'s width and height can't be negative"));
    }
    Ok(Some(Rect::from_xywh(n[0], n[1], n[2] as u32, n[3] as u32)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Rgba,
}

/// 8-bit pixels of `r` (`channels` per pixel: 4 straight RGBA, or 1 for a mask).
struct Read {
    channels: usize,
    bytes: Vec<u8>,
}

/// `r` of `surface` as 8-bit values: RGBA (straight alpha) for pixels, gray for a mask. RGB
/// surfaces are read tile by tile as they are stored (the document's colour space, as
/// `layer.putPixels` takes them); other colour modes go through the document's profile to sRGB
/// as File › Export does.
fn read(st: &DocState, cmd: &str, surface: &photocraft_raster::Surface, r: Rect, mask: bool) -> Result<Read> {
    let n = (r.width() as usize).saturating_mul(r.height() as usize);
    if mask {
        let ch = surface.format().channels().max(1);
        let bytes = surface.read_region(r).iter().step_by(ch).map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
        return Ok(Read { channels: 1, bytes });
    }
    let fmt = surface.format();
    if fmt.mode == ColorMode::Rgb && fmt.sample != SampleType::F32 {
        let mut px = vec![[0u8; 4]; n];
        surface.read_rgba8_into(r, &mut px);
        return Ok(Read { channels: 4, bytes: px.into_iter().flatten().collect() });
    }
    // Gray, CMYK, Lab, 32-bit linear: colour-managed through the exporter.
    let mut tmp = photocraft_doc::Document::new("pixels", photocraft_geom::Size::new(r.width(), r.height()), st.doc.mode, st.doc.depth);
    tmp.icc_profile = st.doc.icc_profile.clone();
    let format = PixelFormat { alpha: true, ..fmt };
    let mut layer = photocraft_raster::Surface::new(format);
    layer.write_region(Rect::from_xywh(0, 0, r.width(), r.height()), &surface.convert(format).read_region(r));
    tmp.layers = vec![Layer::new("pixels", LayerContent::Raster(layer))];
    let png = photocraft_io::export(&tmp, "pixels.png", &Default::default()).map_err(|e| EngineError::Other(format!("{cmd}: {e}")))?.bytes;
    let img = photocraft_codecs::decode(&png).map_err(|e| EngineError::Other(format!("{cmd}: {e}")))?;
    let img = img.convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
    let bytes = img.as_u8().ok_or_else(|| EngineError::Other(format!("{cmd}: no pixels")))?.to_vec();
    Ok(Read { channels: 4, bytes })
}

/// `layer.getPixels {layer, rect?, format?, mask?, document?}`.
fn get_pixels(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.getPixels";
    if !p.is_object() {
        return Err(bad(ID, "needs {layer}"));
    }
    let format = match p.get("format") {
        None | Some(Value::Null) => Format::Png,
        Some(Value::String(f)) if f == "png" => Format::Png,
        Some(Value::String(f)) if f == "rgba" => Format::Rgba,
        Some(v) => return Err(bad(ID, format!("`format` must be \"png\" or \"rgba\", got {v}"))),
    };
    let mask = match p.get("mask") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(bad(ID, "`mask` must be true or false")),
    };
    let rect = rect_param(ID, p)?;
    let st = target_doc(s, ID, p)?;
    let layer = target_layer(st, ID, p)?;
    // Type waiting for its font (and any layer not drawn yet) reads as transparent.
    let undrawn = photocraft_raster::Surface::new(PixelFormat { alpha: true, ..st.doc.pixel_format() });
    let surface = if mask {
        &layer.mask.as_ref().ok_or_else(|| EngineError::Other(format!("layer {} has no layer mask", layer.id.0)))?.surface
    } else {
        match (layer.surface(), &layer.content) {
            (Some(s), _) => s,
            (None, LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_)) => &undrawn,
            (None, _) => return Err(EngineError::Other(format!("layer {} has no pixels of its own (a group or an adjustment)", layer.id.0))),
        }
    };
    // Default: the pixels' bounds (a mask's: the document).
    let r = match rect {
        Some(r) => r,
        None if mask => st.doc.bounds(),
        None => surface.content_bounds(),
    };
    let area = u64::from(r.width()) * u64::from(r.height());
    if area > MAX_AREA {
        return Err(bad(ID, format!("{}×{} is too large to read at once (at most {MAX_AREA} pixels): read it in parts", r.width(), r.height())));
    }
    let rev = st.pixel_revs.revision(&st.doc, layer.id).unwrap_or(0);
    let mut out = json!({
        "document": st.doc.id.0,
        "layer": layer.id.0,
        "rect": [r.x0, r.y0, r.width(), r.height()],
        "width": r.width(),
        "height": r.height(),
        "format": if format == Format::Png { "png" } else { "rgba" },
        "pixelRev": rev,
    });
    if r.is_empty() {
        out["channels"] = json!(if mask { 1 } else { 4 });
        out["data"] = json!("");
        return Ok(out);
    }
    let px = read(st, ID, surface, r, mask)?;
    out["channels"] = json!(px.channels);
    let bytes = match format {
        Format::Rgba => px.bytes,
        Format::Png => {
            let layout = if px.channels == 1 { photocraft_codecs::ChannelLayout::Gray } else { photocraft_codecs::ChannelLayout::Rgba };
            let img = photocraft_codecs::Image::from_raw(r.width(), r.height(), layout, photocraft_codecs::SampleType::U8, px.bytes)
                .map_err(|e| EngineError::Other(format!("{ID}: {e}")))?;
            let opts = photocraft_codecs::EncodeOptions { png_compression: photocraft_codecs::PngCompression::Fast, ..Default::default() };
            photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &opts).map_err(|e| EngineError::Other(format!("{ID}: {e}")))?
        }
    };
    out["data"] = json!(photocraft_paint::tile::b64_encode(&bytes));
    Ok(out)
}

/// `layer.dirtySince {layer, rev, document?}` → `{layer, pixelRev, rect | null, full}`.
fn dirty_since(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.dirtySince";
    if !p.is_object() {
        return Err(bad(ID, "needs {layer, rev}"));
    }
    let since = p.get("rev").and_then(Value::as_u64).ok_or_else(|| bad(ID, "`rev` must be a pixelRev (a whole number ≥ 0)"))?;
    let st = target_doc(s, ID, p)?;
    let layer = target_layer(st, ID, p)?.id;
    let (dirty, rev) = st.pixel_revs.dirty_since(&st.doc, layer, since).ok_or(EngineError::NoLayer(layer))?;
    let (rect, full) = match dirty {
        Dirty::Clean => (Value::Null, false),
        Dirty::Rect(r) => (json!([r.x0, r.y0, r.width(), r.height()]), false),
        Dirty::Full => (Value::Null, true),
    };
    Ok(json!({"document": st.doc.id.0, "layer": layer.0, "pixelRev": rev, "rect": rect, "full": full}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "layer.getPixels",
            label: "Get Layer Pixels",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id|name,"rect":[x,y,w,h]? (default the layer's pixel bounds; a mask's: the canvas),"format":"png|rgba"="png","mask":bool=false,"document":id? (any open document, not activated)} → {document, layer, rect, width, height, format, channels, data (base64: a PNG, or raw 8-bit rows), pixelRev}: reads a layer's pixels (straight RGBA, the document's RGB values) or its layer mask (gray, channels 1) from its tiles; no history step, nothing redrawn, at most 100 MP per call"##,
            enabled: has_doc,
            journal: false,
            run: get_pixels,
        },
        CommandSpec {
            id: "layer.dirtySince",
            label: "Layer Changes Since",
            menu: &[],
            shortcut: None,
            params: r##"{"layer":id|name,"rev":pixelRev read earlier,"document":id?} → {document, layer, pixelRev, rect:[x,y,w,h]|null, full}: where the layer's pixels or mask changed after `rev` (tile-aligned union; null: unchanged). full: true when `rev` is older than the last 2048 changes remembered (or not this document's): read the whole layer"##,
            enabled: has_doc,
            journal: false,
            run: dirty_since,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(v: &Value) -> Vec<u8> {
        photocraft_paint::tile::b64_decode(v["data"].as_str().unwrap()).unwrap()
    }

    fn page() -> (Session, u64) {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 48, "background": "transparent"})).unwrap();
        let layer = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        s.execute("select.all", json!({})).ok();
        s.execute("select.rect", json!({"x": 10, "y": 5, "width": 4, "height": 3})).unwrap();
        s.execute("tools.setColors", json!({"foreground": "#ff0000"})).unwrap();
        s.execute("edit.fill", json!({"contents": "foreground"})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        (s, layer)
    }

    #[test]
    fn reads_a_layer_as_rgba_and_png_without_a_history_step() {
        let (mut s, layer) = page();
        let (rev, steps) = (s.active().unwrap().revision, s.active().unwrap().history.past_len());
        let r = s.execute("layer.getPixels", json!({"layer": layer, "format": "rgba"})).unwrap();
        assert_eq!(r["rect"], json!([10, 5, 4, 3]), "default: the pixels' bounds");
        assert_eq!((r["channels"].as_u64(), b64(&r).len()), (Some(4), 4 * 4 * 3));
        assert_eq!(&b64(&r)[..4], &[255, 0, 0, 255]);
        let png = s.execute("layer.getPixels", json!({"layer": layer, "rect": [8, 5, 4, 1]})).unwrap();
        let img = photocraft_codecs::decode(&b64(&png)).unwrap().convert(photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8);
        assert_eq!(img.as_u8().unwrap(), &[0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 255, 255, 0, 0, 255]);
        let st = s.active().unwrap();
        assert_eq!((st.revision, st.history.past_len()), (rev, steps), "a read changes nothing");
    }

    #[test]
    fn reads_another_document_without_activating_it_and_a_mask() {
        let (mut s, layer) = page();
        let first = s.active().unwrap().doc.id.0;
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let active = s.active_index();
        let r = s.execute("layer.getPixels", json!({"layer": layer, "document": first, "format": "rgba", "rect": [10, 5, 1, 1]})).unwrap();
        assert_eq!(b64(&r), [255, 0, 0, 255]);
        assert_eq!(s.active_index(), active, "nothing activated");
        assert!(s.execute("layer.getPixels", json!({"layer": layer, "mask": true, "document": first})).is_err(), "no mask yet");
        s.execute_scoped("layer.layerMask.revealAll", json!({"layer": layer}), Some(photocraft_doc::DocId(first)), crate::HistoryMode::Step).unwrap();
        let m = s.execute("layer.getPixels", json!({"layer": layer, "mask": true, "document": first, "format": "rgba", "rect": [0, 0, 2, 1]})).unwrap();
        assert_eq!((m["channels"].as_u64(), b64(&m)), (Some(1), vec![255, 255]));
    }

    #[test]
    fn dirty_since_follows_strokes() {
        let (mut s, layer) = page();
        let rev = s.execute("layer.pixelRevision", json!({"layer": layer})).unwrap()["pixelRev"].as_u64().unwrap();
        let none = s.execute("layer.dirtySince", json!({"layer": layer, "rev": rev})).unwrap();
        assert_eq!((none["rect"].clone(), none["full"].clone()), (Value::Null, json!(false)));
        s.execute("select.rect", json!({"x": 200, "y": 0, "width": 2, "height": 2})).ok();
        s.execute("select.rect", json!({"x": 40, "y": 40, "width": 2, "height": 2})).unwrap();
        s.execute("edit.fill", json!({"contents": "foreground"})).unwrap();
        let d = s.execute("layer.dirtySince", json!({"layer": layer, "rev": rev})).unwrap();
        let r = d["rect"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>();
        assert!(r[0] <= 40 && r[1] <= 40 && r[0] + r[2] >= 42 && r[1] + r[3] >= 42, "{d}");
        assert!(d["pixelRev"].as_u64().unwrap() > rev);
        assert_eq!(s.execute("layer.dirtySince", json!({"layer": layer, "rev": 1_000_000})).unwrap()["full"], json!(true));
    }

    #[test]
    fn bad_params_are_errors() {
        let (mut s, layer) = page();
        for p in [
            json!(null),
            json!({}),
            json!({"layer": 999_999}),
            json!({"layer": layer, "format": "jpeg"}),
            json!({"layer": layer, "mask": 1}),
            json!({"layer": layer, "rect": [0, 0, -1, 4]}),
            json!({"layer": layer, "rect": [0, 0, 1.5, 4]}),
            json!({"layer": layer, "rect": [0, 0]}),
            json!({"layer": layer, "rect": [0, 0, 100_000, 100_000]}),
            json!({"layer": layer, "document": 4242}),
            json!({"layer": layer, "document": "x"}),
        ] {
            assert!(s.execute("layer.getPixels", p.clone()).is_err(), "{p}");
        }
        for p in [json!({"layer": layer}), json!({"layer": layer, "rev": -1}), json!({"rev": 0}), json!([1])] {
            assert!(s.execute("layer.dirtySince", p.clone()).is_err(), "{p}");
        }
        let empty = s.execute("layer.getPixels", json!({"layer": layer, "rect": [0, 0, 0, 0]})).unwrap();
        assert_eq!(empty["data"], json!(""));
    }
}
