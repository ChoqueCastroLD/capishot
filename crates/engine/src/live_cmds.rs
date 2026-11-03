//! Commands for hosts that sync a document live between several people and agents (Visoria's
//! multiplayer editing over the embed API and `capishot-cli serve`): cheap change detection
//! (`layer.pixelRevision`), and pixels or a layer mask put into an existing layer in one step from
//! an image (`layer.putPixels`, `layer.addImage`, `layer.layerMask.set`).

use photocraft_doc::{LayerContent, LayerId, LayerMask};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

/// The largest image a call takes (base64 text, about 192 MiB of file).
const MAX_BASE64: usize = 256 << 20;

fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

fn has_doc(s: &Session) -> std::result::Result<(), String> {
    s.active().map(|_| ()).ok_or_else(|| "no document".into())
}

fn object<'a>(p: &'a Value, cmd: &str) -> Result<Option<&'a serde_json::Map<String, Value>>> {
    match p {
        Value::Object(o) => Ok(Some(o)),
        Value::Null => Ok(None),
        _ => Err(bad(cmd, "params must be an object")),
    }
}

/// `layer.pixelRevision {layer?}` → `{layer, pixelRev}`; without `layer`, every layer's.
fn pixel_revision(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.pixelRevision";
    let o = object(p, ID)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    match o.and_then(|o| o.get("layer")).filter(|v| !v.is_null()) {
        Some(v) => {
            let id = photocraft_doc::LayerId(v.as_u64().ok_or_else(|| bad(ID, "`layer` must be a layer id"))?);
            let rev = st.pixel_revs.revision(&st.doc, id).ok_or(EngineError::NoLayer(id))?;
            Ok(json!({"layer": id.0, "pixelRev": rev}))
        }
        None => {
            let revs = st.pixel_revs.revisions(&st.doc);
            let layers: Vec<Value> =
                st.doc.walk().into_iter().map(|(_, _, l)| json!({"layer": l.id.0, "pixelRev": revs.get(&l.id).copied().unwrap_or(0)})).collect();
            Ok(json!({"layers": layers}))
        }
    }
}

/// The image of a `png` (or `data`) param: base64, with or without a `data:…;base64,` prefix.
fn image_bytes(cmd: &str, p: &Value) -> Result<Vec<u8>> {
    let text = p.get("png").or_else(|| p.get("data")).and_then(Value::as_str).ok_or_else(|| bad(cmd, "needs `png`: the image as base64 (PNG, JPEG, WebP…)"))?;
    let text = match text.split_once(";base64,") {
        Some((head, rest)) if head.starts_with("data:") => rest,
        _ => text,
    };
    if text.len() > MAX_BASE64 {
        return Err(bad(cmd, "the image is too large (at most 256 MiB of base64)"));
    }
    let bytes = photocraft_paint::tile::b64_decode(text.trim()).ok_or_else(|| bad(cmd, "`png` is not base64"))?;
    if bytes.is_empty() {
        return Err(bad(cmd, "`png` is empty"));
    }
    Ok(bytes)
}

/// `x`, `y`: whole document pixels (default 0), within ±10⁷.
fn origin(cmd: &str, p: &Value) -> Result<(i32, i32)> {
    let one = |k: &str| -> Result<i32> {
        match p.get(k) {
            None | Some(Value::Null) => Ok(0),
            Some(v) => v
                .as_f64()
                .filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() <= 1e7)
                .map(|f| f as i32)
                .ok_or_else(|| bad(cmd, format!("`{k}` must be a whole number of pixels"))),
        }
    };
    Ok((one("x")?, one("y")?))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PutMode {
    Replace,
    Over,
}

fn put_mode(cmd: &str, p: &Value) -> Result<PutMode> {
    match p.get("mode") {
        None | Some(Value::Null) => Ok(PutMode::Replace),
        Some(Value::String(m)) if m == "replace" => Ok(PutMode::Replace),
        Some(Value::String(m)) if m == "over" => Ok(PutMode::Over),
        Some(v) => Err(bad(cmd, format!("`mode` must be \"replace\" or \"over\", got {v}"))),
    }
}

/// The open layer `id` of the active document, checked for `cmd`.
fn target_layer(s: &Session, p: &Value) -> Result<LayerId> {
    let id = crate::commands::layer_param(s, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    Ok(id)
}

/// Source-over of straight-alpha pixels `top` onto `under` (both `n` channels, alpha last).
fn over(top: &[f32], under: &mut [f32], n: usize) {
    if n == 0 {
        return;
    }
    for (t, u) in top.chunks_exact(n).zip(under.chunks_exact_mut(n)) {
        let (ta, ua) = (t[n - 1].clamp(0.0, 1.0), u[n - 1].clamp(0.0, 1.0));
        let oa = ta + ua * (1.0 - ta);
        if oa <= 0.0 {
            u.iter_mut().for_each(|c| *c = 0.0);
            continue;
        }
        for c in 0..n - 1 {
            u[c] = (t[c] * ta + u[c] * ua * (1.0 - ta)) / oa;
        }
        u[n - 1] = oa;
    }
}

/// `layer.putPixels {layer, png, x, y, mode}`: an image into an existing pixel layer, its
/// top-left at `(x, y)`, converted to the document's colour profile and depth as a copied layer
/// is. `replace` sets the rect to the image (its transparency too), `over` composites it on top.
fn put_pixels(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.putPixels";
    object(p, ID)?.ok_or_else(|| bad(ID, "needs {layer, png, x, y}"))?;
    let id = target_layer(s, p)?;
    let mode = put_mode(ID, p)?;
    let (x, y) = origin(ID, p)?;
    let bytes = image_bytes(ID, p)?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let layer = st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
    let LayerContent::Raster(dst) = &layer.content else {
        return Err(EngineError::Other(format!("layer {} isn't a pixel layer (rasterize it first)", id.0)));
    };
    let locks = st.doc.effective_locks(id);
    if locks.all || locks.pixels {
        return Err(EngineError::Other(format!("layer {}'s pixels are locked", id.0)));
    }
    let dst_format = dst.format();
    let (w, h, mut data) = image_pixels(s, ID, &bytes, dst_format)?;
    let rect = Rect::from_xywh(x, y, w, h);
    s.edit("Put Pixels", |doc, _| {
        let surf = doc.layer_mut(id).and_then(|l| l.surface_mut()).ok_or(EngineError::NoLayer(id))?;
        if mode == PutMode::Over && dst_format.alpha {
            let mut under = surf.read_region(rect);
            over(&data, &mut under, dst_format.channels());
            data = under;
        }
        surf.write_region(rect, &data);
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "rect": [x, y, w, h]}))
}

/// The image `bytes` decoded, converted to the active document's profile and depth (as a copied
/// layer is) and to `format`: `(width, height, pixels)`.
fn image_pixels(s: &Session, cmd: &str, bytes: &[u8], format: photocraft_color::PixelFormat) -> Result<(u32, u32, Vec<f32>)> {
    let dest = s.active().ok_or(EngineError::NoDocument)?.doc.clone();
    let kind = photocraft_codecs::detect(bytes).ok_or_else(|| bad(cmd, "`png` is not an image this build reads"))?;
    let ext = kind.extensions().first().copied().unwrap_or("png");
    let mut src = photocraft_io::import(&format!("pixels.{ext}"), bytes).map_err(|e| EngineError::Other(format!("{cmd}: {e}")))?.document;
    if src.layers.len() != 1 || !matches!(src.layers.first().map(|l| &l.content), Some(LayerContent::Raster(_))) {
        return Err(bad(cmd, "`png` must be a flat image (PNG, JPEG, WebP…), not a layered file"));
    }
    crate::layer_copy_cmds::adapt_copies(s, &mut src, &dest)?;
    let (w, h) = (src.size.width, src.size.height);
    let surface = src.layers.first().and_then(|l| l.surface()).ok_or_else(|| bad(cmd, "the image has no pixels"))?.convert(format);
    Ok((w, h, surface.read_region(Rect::from_xywh(0, 0, w, h))))
}

/// `layer.addImage {png, x, y, name?, above?}`: a new pixel layer holding the image at `(x, y)`,
/// above layer `above` (default: the active layer), in one step. The active layer stays active.
fn add_image(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.addImage";
    let o = object(p, ID)?.ok_or_else(|| bad(ID, "needs {png, x, y}"))?;
    let (x, y) = origin(ID, p)?;
    let name = match o.get("name") {
        None | Some(Value::Null) => None,
        Some(Value::String(n)) if n.chars().count() <= 255 => Some(n.clone()),
        Some(_) => return Err(bad(ID, "`name` must be a string of at most 255 characters")),
    };
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let above = match o.get("above") {
        None | Some(Value::Null) => st.active_layer,
        Some(v) => {
            let id = LayerId(v.as_u64().ok_or_else(|| bad(ID, "`above` must be a layer id"))?);
            st.doc.layer(id).ok_or(EngineError::NoLayer(id))?;
            Some(id)
        }
    };
    let format = photocraft_color::PixelFormat { alpha: true, ..st.doc.pixel_format() };
    let bytes = image_bytes(ID, p)?;
    let (w, h, data) = image_pixels(s, ID, &bytes, format)?;
    let mut surface = photocraft_raster::Surface::new(format);
    surface.write_region(Rect::from_xywh(x, y, w, h), &data);
    let id = s.edit("Add Image Layer", |doc, active| {
        let name = name.unwrap_or_else(|| doc.next_layer_name("Layer"));
        let layer = photocraft_doc::Layer::new(name, LayerContent::Raster(surface));
        let keep = *active;
        let id = doc.insert_above(above, layer);
        *active = keep;
        Ok(id)
    })?;
    Ok(json!({"layer": id.0, "rect": [x, y, w, h]}))
}

/// `layer.layerMask.set {layer, png, x, y, mode, fill}`: a layer mask from an image (its
/// luminance, white reveals), created when the layer has none (`fill` the rest: `reveal`, the
/// default, or `hide`) or replacing part of the one it has.
fn set_mask(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "layer.layerMask.set";
    object(p, ID)?.ok_or_else(|| bad(ID, "needs {layer, png, x, y}"))?;
    let id = target_layer(s, p)?;
    let mode = put_mode(ID, p)?;
    let (x, y) = origin(ID, p)?;
    let hide = match p.get("fill").filter(|v| !v.is_null()) {
        None => false,
        Some(Value::String(f)) if f == "reveal" => false,
        Some(Value::String(f)) if f == "hide" => true,
        Some(v) => return Err(bad(ID, format!("`fill` must be \"reveal\" or \"hide\", got {v}"))),
    };
    let bytes = image_bytes(ID, p)?;
    {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        if st.doc.is_background(id) {
            return Err(EngineError::Other("the Background can't have a layer mask (make it a layer first)".into()));
        }
        if st.doc.effective_locks(id).all {
            return Err(EngineError::Other(format!("layer {} is locked", id.0)));
        }
    }
    let img = photocraft_codecs::decode(&bytes).map_err(|e| bad(ID, format!("`png`: {e}")))?;
    let img = img.convert(photocraft_codecs::ChannelLayout::GrayA, photocraft_codecs::SampleType::U8);
    let (w, h) = (img.width(), img.height());
    let px = img.as_u8().ok_or_else(|| bad(ID, "the image has no pixels"))?;
    // Gray × alpha: transparent pixels hide (`replace`) or keep the mask as it is (`over`).
    let values: Vec<(f32, f32)> = px.as_chunks::<2>().0.iter().map(|&[v, a]| (f32::from(v) / 255.0, f32::from(a) / 255.0)).collect();
    let rect = Rect::from_xywh(x, y, w, h);
    let created = s.active().and_then(|st| st.doc.layer(id)).is_some_and(|l| l.mask.is_none());
    s.edit("Set Layer Mask", |doc, _| {
        let l = doc.layer_mut(id).ok_or(EngineError::NoLayer(id))?;
        let mask = l.mask.get_or_insert_with(|| if hide { LayerMask::hide_all() } else { LayerMask::reveal_all() });
        let format = mask.surface.format();
        let n = format.channels().max(1);
        let mut data = mask.surface.read_region(rect);
        for (out, (v, a)) in data.chunks_exact_mut(n).zip(&values) {
            out[0] = match mode {
                PutMode::Replace => v * a,
                PutMode::Over => v * a + out[0] * (1.0 - a),
            };
            if format.alpha {
                out[n - 1] = 1.0;
            }
        }
        mask.surface.write_region(rect, &data);
        Ok(())
    })?;
    Ok(json!({"layer": id.0, "rect": [x, y, w, h], "created": created}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "layer.pixelRevision",
        label: "Layer Pixel Revision",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id?} → {layer, pixelRev} ({layers:[{layer, pixelRev}]} without `layer`): a counter that moves whenever the layer's pixels or layer mask change (strokes, fills, Remove, clears, undo, redo, jobs); also `pixelRev` on every layer of document.inspect"##,
        enabled: has_doc,
        journal: false,
        run: pixel_revision,
    },
    CommandSpec {
        id: "layer.putPixels",
        label: "Put Pixels",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id,"png":base64 image (PNG, JPEG, WebP…; a data: URL works),"x":px=0,"y":px=0,"mode":"replace|over"="replace"} → {layer, rect:[x,y,w,h]}: the image into an existing pixel layer, its top-left at (x, y), in one history step (layer id and name kept). replace: the rect becomes the image, transparency included; over: composited on top. Converted to the document's profile and depth"##,
        enabled: has_doc,
        journal: true,
        run: put_pixels,
    },
    CommandSpec {
        id: "layer.addImage",
        label: "Add Image Layer",
        menu: &[],
        shortcut: None,
        params: r##"{"png":base64 image (PNG, JPEG, WebP…; a data: URL works),"x":px=0,"y":px=0,"name":str?,"above":id? (default the active layer)} → {layer, rect:[x,y,w,h]}: a new pixel layer with the image at (x, y), converted to the document's profile and depth, in one history step; the active layer stays active"##,
        enabled: has_doc,
        journal: true,
        run: add_image,
    },
    CommandSpec {
        id: "layer.layerMask.set",
        label: "Set Layer Mask from Image",
        menu: &[],
        shortcut: None,
        params: r##"{"layer":id,"png":base64 image,"x":px=0,"y":px=0,"mode":"replace|over"="replace","fill":"reveal|hide"="reveal" (the rest of a new mask)} → {layer, rect, created}: the image's luminance (white reveals) times its alpha becomes the mask in that rect (over: transparent pixels keep the mask); creates the mask when the layer has none. One history step"##,
        enabled: has_doc,
        journal: true,
        run: set_mask,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_revision_moves_with_a_stroke_and_with_undo() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        let layer = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let rev = |s: &mut Session| s.execute("layer.pixelRevision", json!({"layer": layer})).unwrap()["pixelRev"].as_u64().unwrap();
        let r0 = rev(&mut s);
        s.execute("edit.fill", json!({"contents": "black"})).unwrap();
        let r1 = rev(&mut s);
        assert_ne!(r0, r1, "a fill moves it");
        let inspected = s.execute("document.inspect", json!({})).unwrap();
        assert_eq!(inspected["layers"][0]["pixelRev"], json!(r1), "inspect reports the same number");
        s.execute("layer.renameLayer", json!({"layer": layer, "name": "x"})).unwrap();
        assert_eq!(rev(&mut s), r1, "a rename doesn't");
        assert!(s.undo() && s.undo());
        assert_ne!(rev(&mut s), r1, "undoing the fill does");
        let all = s.execute("layer.pixelRevision", json!({})).unwrap();
        assert_eq!(all["layers"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn inspect_with_text_style_gives_runs_paragraphs_and_effect_settings() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100})).unwrap();
        let layer = s.execute("type.create", json!({"text": "HI", "x": 10, "y": 50, "size": 24})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.layerStyle.stroke", json!({"layer": layer, "size": 3, "color": "#ffffff", "add": true})).unwrap();
        let plain = s.execute("document.inspect", json!({})).unwrap();
        let node = plain["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(layer)).unwrap().clone();
        assert!(node["text"].get("style").is_none(), "opt-in only");
        assert!(node["effects"]["items"][0].get("fx").is_none());
        let full = s.execute("document.inspect", json!({"textStyle": true})).unwrap();
        let node = full["layers"].as_array().unwrap().iter().find(|l| l["id"] == json!(layer)).unwrap().clone();
        let style = &node["text"]["style"];
        assert_eq!(style["runs"][0]["start"], 0);
        assert_eq!(style["runs"][0]["end"], 2);
        assert!(style["runs"][0]["style"].is_object() && style["paragraphs"][0]["style"].is_object(), "{style}");
        let info = s.execute("type.info", json!({"layer": layer})).unwrap();
        assert_eq!(style["runs"], info["runs"], "the same runs as type.info");
        assert_eq!(node["effects"]["items"][0]["id"], "stroke");
        assert!(node["effects"]["items"][0]["fx"].is_object());
        assert!(s.execute("document.inspect", json!({"textStyle": 1})).is_err());
    }

    /// A `w`×`h` RGBA PNG of one colour, as base64.
    fn png(w: u32, h: u32, rgba: [u8; 4]) -> String {
        let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
        let img = photocraft_codecs::Image::from_raw(w, h, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, data).unwrap();
        let bytes = photocraft_codecs::encode(&img, photocraft_codecs::Format::Png, &Default::default()).unwrap();
        photocraft_paint::tile::b64_encode(&bytes)
    }

    fn pixel(s: &Session, layer: u64, x: i32, y: i32) -> Vec<f32> {
        let st = s.active().unwrap();
        st.doc.layer(LayerId(layer)).unwrap().surface().unwrap().read_region(Rect::from_xywh(x, y, 1, 1))
    }

    #[test]
    fn put_pixels_replaces_or_composites_in_one_step_keeping_the_layer() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32, "background": "white"})).unwrap();
        let layer = s.execute("layer.new.layer", json!({"name": "ink"})).unwrap()["layer"].as_u64().unwrap();
        let steps = s.active().unwrap().history.entries().len();
        let r = s.execute("layer.putPixels", json!({"layer": layer, "png": png(4, 4, [255, 0, 0, 255]), "x": 2, "y": 3})).unwrap();
        assert_eq!(r, json!({"layer": layer, "rect": [2, 3, 4, 4]}));
        assert_eq!(s.active().unwrap().history.entries().len(), steps + 1, "one step");
        let red = pixel(&s, layer, 2, 3);
        assert!(red[0] > 0.99 && red[1] < 0.01 && red[3] > 0.99, "{red:?}");
        assert_eq!(pixel(&s, layer, 1, 3)[3], 0.0, "outside the rect is untouched");
        // Half-transparent blue over the red: a mix; replace: exactly the half-transparent blue.
        let blue = png(4, 4, [0, 0, 255, 128]);
        s.execute("layer.putPixels", json!({"layer": layer, "png": blue, "x": 2, "y": 3, "mode": "over"})).unwrap();
        let mixed = pixel(&s, layer, 2, 3);
        assert!(mixed[0] > 0.3 && mixed[2] > 0.3 && mixed[3] > 0.99, "{mixed:?}");
        s.execute("layer.putPixels", json!({"layer": layer, "png": format!("data:image/png;base64,{blue}"), "x": 2, "y": 3})).unwrap();
        let replaced = pixel(&s, layer, 2, 3);
        assert!(replaced[0] < 0.01 && (replaced[3] - 0.5).abs() < 0.01, "{replaced:?}");
        let st = s.active().unwrap();
        assert_eq!(st.doc.layer(LayerId(layer)).unwrap().name, "ink");
        // Without a step: history "none" keeps it out of Undo.
        s.execute_scoped("layer.putPixels", json!({"layer": layer, "png": png(1, 1, [0, 255, 0, 255])}), None, crate::HistoryMode::None).unwrap();
        assert!(s.undo());
        assert!(pixel(&s, layer, 0, 0)[1] > 0.99, "the step-less pixels survive undo");
    }

    #[test]
    fn put_pixels_refuses_what_it_cant_do() {
        let mut s = Session::new();
        assert!(s.execute("layer.putPixels", json!({"png": png(1, 1, [0; 4])})).is_err(), "no document");
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        let layer = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let text = s.execute("type.create", json!({"text": "A", "x": 1, "y": 6, "size": 6})).unwrap()["layer"].as_u64().unwrap();
        for p in [
            json!({"layer": layer}),
            json!({"layer": layer, "png": "!!"}),
            json!({"layer": layer, "png": "aGVsbG8="}),
            json!({"layer": layer, "png": png(1, 1, [0; 4]), "mode": "under"}),
            json!({"layer": layer, "png": png(1, 1, [0; 4]), "x": 1.5}),
            json!({"layer": layer, "png": png(1, 1, [0; 4]), "x": 1e9}),
            json!({"layer": 999_999, "png": png(1, 1, [0; 4])}),
            json!({"layer": text, "png": png(1, 1, [0; 4])}),
            json!([1]),
        ] {
            assert!(s.execute("layer.putPixels", p.clone()).is_err(), "{p}");
        }
    }

    #[test]
    fn add_image_makes_a_layer_without_changing_the_active_one() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 16, "height": 16})).unwrap();
        let below = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let top = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let r = s.execute("layer.addImage", json!({"png": png(2, 2, [0, 255, 0, 255]), "x": 5, "y": 6, "name": "remote", "above": below})).unwrap();
        let id = r["layer"].as_u64().unwrap();
        let st = s.active().unwrap();
        assert_eq!(st.active_layer, Some(LayerId(top)));
        let order: Vec<u64> = st.doc.layers.iter().map(|l| l.id.0).collect();
        let at = |x: u64| order.iter().position(|o| *o == x).unwrap();
        assert_eq!(at(id), at(below) + 1, "right above `above`");
        assert_eq!(st.doc.layer(LayerId(id)).unwrap().name, "remote");
        assert!(pixel(&s, id, 5, 6)[1] > 0.99);
        assert_eq!(pixel(&s, id, 4, 6)[3], 0.0);
        assert!(s.execute("layer.addImage", json!({"png": png(1, 1, [0; 4]), "above": 999_999})).is_err());
        assert!(s.execute("layer.addImage", json!({})).is_err());
    }

    #[test]
    fn layer_mask_set_creates_or_replaces_part_of_a_mask() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 32, "height": 32})).unwrap();
        let layer = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        let mask_at = |s: &Session, x: i32, y: i32| {
            s.active().unwrap().doc.layer(LayerId(layer)).unwrap().mask.as_ref().unwrap().surface.read_region(Rect::from_xywh(x, y, 1, 1))[0]
        };
        let r = s.execute("layer.layerMask.set", json!({"layer": layer, "png": png(4, 4, [0, 0, 0, 255]), "x": 8, "y": 8})).unwrap();
        assert_eq!(r["created"], true);
        assert_eq!(mask_at(&s, 8, 8), 0.0, "black hides");
        assert_eq!(mask_at(&s, 0, 0), 1.0, "a new mask reveals the rest");
        // Replace part of it: white reveals again; transparent `over` keeps the mask.
        let r = s.execute("layer.layerMask.set", json!({"layer": layer, "png": png(2, 2, [255, 255, 255, 255]), "x": 8, "y": 8})).unwrap();
        assert_eq!(r["created"], false);
        assert_eq!(mask_at(&s, 8, 8), 1.0);
        assert_eq!(mask_at(&s, 10, 10), 0.0, "the rest of the old mask stays");
        s.execute("layer.layerMask.set", json!({"layer": layer, "png": png(4, 4, [255, 255, 255, 0]), "x": 8, "y": 8, "mode": "over"})).unwrap();
        assert_eq!(mask_at(&s, 10, 10), 0.0);
        let hidden = s.execute("layer.new.layer", json!({})).unwrap()["layer"].as_u64().unwrap();
        s.execute("layer.layerMask.set", json!({"layer": hidden, "png": png(1, 1, [255, 255, 255, 255]), "fill": "hide"})).unwrap();
        let m = s.active().unwrap().doc.layer(LayerId(hidden)).unwrap().mask.as_ref().unwrap().surface.read_region(Rect::from_xywh(0, 0, 2, 1));
        assert_eq!(m, [1.0, 0.0]);
        assert!(s.undo());
        assert!(s.active().unwrap().doc.layer(LayerId(hidden)).unwrap().mask.is_none(), "one step");
        for p in [json!({"layer": layer}), json!({"layer": layer, "png": png(1, 1, [0; 4]), "fill": "grey"}), json!({"layer": layer, "png": "zz"})] {
            assert!(s.execute("layer.layerMask.set", p.clone()).is_err(), "{p}");
        }
    }

    #[test]
    fn pixel_revision_refuses_bad_params() {
        let mut s = Session::new();
        assert!(s.execute("layer.pixelRevision", json!({})).is_err(), "no document");
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(s.execute("layer.pixelRevision", json!({"layer": "x"})).is_err());
        assert!(s.execute("layer.pixelRevision", json!({"layer": 999_999})).is_err());
        assert!(s.execute("layer.pixelRevision", json!([1])).is_err());
    }
}
