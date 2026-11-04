use super::paths::{file_name, read_file, stem, template_name};
use super::{
    Affine, Arc, Document, EngineError, Layer, LayerContent, Resample, Result, Session, SmartObject, SmartSource, Value, f64_param, flattened_checked, json,
    resize_surface, str_param, translate_surface,
};

pub(crate) fn import(name: &str, bytes: &[u8]) -> Result<Document> {
    import_result(name, bytes).map(|r| r.document)
}

/// [`import`] for a file that becomes a document of its own (Revert, Load Files into Stack;
/// Batch's scratch sessions use [`import_at_raw_ppi`]): a developed raw gets Preferences › Raw
/// Defaults › Resolution, as it does through File › Open.
pub(crate) fn import_opened(s: &Session, name: &str, bytes: &[u8]) -> Result<Document> {
    import_at_raw_ppi(s.prefs().raw_defaults.resolution, name, bytes)
}

pub(super) fn import_at_raw_ppi(raw_ppi: f64, name: &str, bytes: &[u8]) -> Result<Document> {
    let r = import_result(name, bytes)?;
    let mut doc = r.document;
    raw_resolution(raw_ppi, &r.warnings, &mut doc);
    Ok(doc)
}

fn import_result(name: &str, bytes: &[u8]) -> Result<photocraft_io::ImportResult> {
    crate::allocation::checkpoint("importing document")?;
    let r = photocraft_io::import(name, bytes).map_err(|e| EngineError::Other(format!("{name}: {e}")))?;
    // Auxiliary imports return only a document and cannot surface the preview's fidelity warning.
    // Open has its own warning-preserving path; never silently place or process a thumbnail.
    if r.preview_only {
        return Err(EngineError::Other(format!(
            "{name}: only this Affinity file's embedded preview could be read; open it with File › Open to see the warning, or export PSD or PNG from Affinity before using it here"
        )));
    }
    Ok(r)
}

// ---------- close / revert / save a copy / open as ----------

/// Closes every shown document; a host's hidden helper documents stay open.
pub(super) fn close_all(s: &mut Session) -> Result<Value> {
    let n = s.visible_count();
    while let Some(last) = s.visible_count().checked_sub(1) {
        s.close(last);
    }
    Ok(json!({"closed": n}))
}

pub(super) fn close_others(s: &mut Session, p: &Value) -> Result<Value> {
    let keep = p.get("document").and_then(Value::as_u64).map(|v| v as usize).or(s.active_index()).ok_or(EngineError::NoDocument)?;
    if keep >= s.visible_count() {
        return Err(EngineError::BadParams { cmd: "file.closeOthers".into(), msg: format!("no document {keep}") });
    }
    let n = s.visible_count() - 1;
    for i in (0..s.visible_count()).rev() {
        if i != keep {
            s.close(i);
        }
    }
    s.set_active(0);
    Ok(json!({"closed": n}))
}

/// File › Revert: reload the saved file as one history step (undoable, like Photoshop's
/// "Revert" history state); the document is clean afterwards.
pub(super) fn revert(s: &mut Session) -> Result<Value> {
    let path = s.active().and_then(|d| d.path.clone()).ok_or(EngineError::Other("the document has never been saved".into()))?;
    let bytes = read_file(&path)?;
    let fresh = import_opened(s, &file_name(&path), &bytes)?;
    s.edit("Revert", |doc, active| {
        let (id, name) = (doc.id, doc.name.clone());
        *doc = fresh;
        // Keep the identity (colour state, views and caches are keyed by it) and the tab name.
        doc.id = id;
        doc.name = name;
        *active = doc.top_layer();
        Ok(())
    })?;
    let st = s.active_mut().ok_or(EngineError::NoDocument)?;
    st.saved_revision = st.revision;
    Ok(json!({"path": path, "layers": st.doc.layers.len()}))
}

/// Open a file's bytes, decoding them as the format `as_ext` (Open As) when given.
/// EXR/HDR rarely states its colour space, so io tags it with the linear-sRGB default.
/// With File Handling's ACEScg option on, such an EXR opens as ACEScg instead (AP1,
/// linear) — the working space ACES render pipelines write. Only the io default is
/// replaced; an embedded or EXIF-declared profile is kept untouched.
pub(crate) fn tag_exr_colorspace(s: &Session, name: &str, doc: &mut Document) {
    if !name.to_ascii_lowercase().ends_with(".exr") || !s.prefs.get().file_handling.exr_aces_cg {
        return;
    }
    let linear = photocraft_cms::Builtin::LinearSrgb.profile().to_bytes();
    if doc.icc_profile.as_ref().map(|v| v.as_slice()) == Some(linear.as_slice()) {
        doc.icc_profile = Some(photocraft_cms::Builtin::AcesCg.profile().to_bytes());
    }
}

/// Edit › Preferences › Raw Defaults › Resolution: the ppi a raw developed on open gets, as
/// Camera Raw's workflow options set it (the raw itself states none). `warnings` are the
/// import's notes, which say whether the raw was developed or only its preview opened.
pub fn raw_defaults(s: &Session, warnings: &[String], doc: &mut Document) {
    raw_resolution(s.prefs().raw_defaults.resolution, warnings, doc);
}

fn raw_resolution(ppi: f64, warnings: &[String], doc: &mut Document) {
    if ppi.is_finite() && warnings.iter().any(|w| w.contains(photocraft_io::raw::DEVELOPED_NOTE)) {
        doc.resolution_dpi = ppi.clamp(1.0, 10_000.0) as f32;
    }
}

pub fn open_bytes_as(s: &mut Session, name: &str, bytes: &[u8], as_ext: Option<&str>, path: Option<String>) -> Result<Value> {
    crate::allocation::checkpoint("importing document")?;
    let decode_name = match as_ext {
        Some(ext) => format!("{}.{}", stem(name), ext.trim_start_matches('.')),
        None => name.to_string(),
    };
    let r = photocraft_io::import(&decode_name, bytes).map_err(|e| EngineError::Other(format!("{decode_name}: {e}")))?;
    let mut doc = r.document;
    tag_exr_colorspace(s, &decode_name, &mut doc);
    raw_defaults(s, &r.warnings, &mut doc);
    let path = match template_name(s, name) {
        Some(untitled) => {
            doc.name = untitled;
            None
        }
        None => {
            doc.name = file_name(name);
            path
        }
    };
    // Color Settings policies (preserve / convert / discard the embedded profile).
    let (i, color) = s.open_document(doc, path.filter(|_| !r.source_read_only));
    if let Some(st) = s.active_mut() {
        st.source_read_only = r.source_read_only;
    }
    // Import notes (e.g. how a camera raw was developed, or that only its preview opened).
    Ok(json!({"document": i, "color": color, "warnings": r.warnings}))
}

pub(super) fn open_as(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_param(p, "path", "file.openAs")?.to_string();
    let bytes = read_file(&path)?;
    let as_ext = p.get("as").or_else(|| p.get("format")).and_then(Value::as_str);
    open_bytes_as(s, &path, &bytes, as_ext, Some(path.clone()))
}

// ---------- place ----------

/// History label of an embedded place.
pub const PLACE_EMBEDDED: &str = "Place Embedded";

/// Place a file's bytes as a smart object layer, centred, and (with Preferences ▸ General ▸
/// Resize Image During Place, on by default) scaled down to fit when larger than the canvas.
/// `linked` makes it a linked smart object that refers to that path instead of embedding the bytes.
/// With Preferences ▸ General ▸ Always Create Smart Objects When Placing off, an embedded place
/// lands as a plain pixel layer instead (an explicit `"smartObject": false` asks for one).
pub fn place_bytes(s: &mut Session, name: &str, bytes: Vec<u8>, linked: Option<String>, p: &Value) -> Result<Value> {
    let plan = PlacePlan::new(s, name, linked, p)?;
    let placed = plan.decode(bytes)?;
    placed.apply(s)
}

/// Place Embedded/Linked as a background job: the file is decoded and scaled on a worker (the
/// threads build's pool in the browser), then placed as one history step on this thread. The
/// result is [`place_bytes`]'s.
pub fn start_place_bytes(s: &mut Session, name: &str, bytes: Vec<u8>, linked: Option<String>, p: &Value) -> Result<crate::jobs::Started> {
    let plan = PlacePlan::new(s, name, linked, p)?;
    let label = format!("Placing {}", file_name(name));
    s.start_job_in_worker(
        "file.placeEmbedded",
        json!({"name": file_name(name)}),
        &label,
        true,
        move |ctx| {
            ctx.progress(0.0, "Decoding");
            let placed = plan.decode(bytes)?;
            ctx.check()?;
            Ok(placed)
        },
        |s, placed: Placed| placed.apply(s),
    )
}

/// What a place needs from the session, taken up front so the decoding can run anywhere.
struct PlacePlan {
    name: String,
    linked: Option<String>,
    canvas: (f64, f64),
    fmt: photocraft_color::PixelFormat,
    opts: photocraft_io::ImportOptions,
    fit: bool,
    scale: Option<f64>,
    center: Option<(f64, f64)>,
    /// `x`, `y`, `width`, `height`: the top-left corner and the size (either or both; a missing
    /// one keeps the aspect, neither keeps the size) the placed image takes, in document pixels.
    rect: Option<PlaceRect>,
    smart_object: bool,
}

/// `file.placeEmbedded`'s `x`/`y`/`width`/`height`.
#[derive(Clone, Copy, Debug)]
struct PlaceRect {
    x: f64,
    y: f64,
    width: Option<f64>,
    height: Option<f64>,
}

impl PlaceRect {
    /// The rect of `p`, `None` without any of its keys; out-of-range values are errors.
    fn parse(p: &Value) -> Result<Option<Self>> {
        const KEYS: [&str; 4] = ["x", "y", "width", "height"];
        if KEYS.iter().all(|k| p.get(*k).is_none_or(Value::is_null)) {
            return Ok(None);
        }
        let num = |k: &str, min: f64| -> Result<Option<f64>> {
            match p.get(k) {
                None | Some(Value::Null) => Ok(None),
                Some(v) => v.as_f64().filter(|v| v.is_finite() && *v >= min && v.abs() <= 1e7).map(Some).ok_or_else(|| EngineError::BadParams {
                    cmd: "file.placeEmbedded".into(),
                    msg: format!("`{k}` must be a number of pixels{}", if min > 0.0 { " above 0" } else { "" }),
                }),
            }
        };
        let (width, height) = (num("width", f64::MIN_POSITIVE)?, num("height", f64::MIN_POSITIVE)?);
        Ok(Some(Self { x: num("x", -1e7)?.unwrap_or(0.0), y: num("y", -1e7)?.unwrap_or(0.0), width, height }))
    }

    /// The horizontal and vertical scale of a `w`×`h` image placed in this rect.
    fn scales(&self, w: f64, h: f64) -> (f64, f64) {
        let (w, h) = (w.max(1.0), h.max(1.0));
        match (self.width, self.height) {
            (Some(rw), Some(rh)) => (rw / w, rh / h),
            (Some(rw), None) => (rw / w, rw / w),
            (None, Some(rh)) => (rh / h, rh / h),
            (None, None) => (1.0, 1.0),
        }
    }
}

/// A decoded place, ready to land as a layer ([`Placed::apply`]).
struct Placed {
    name: String,
    px: photocraft_raster::Surface,
    source: SmartSource,
    vector: bool,
    scale: f64,
    /// The vertical scale, when it differs from `scale` (a `width` and `height` of another aspect).
    scale_y: f64,
    offset: (f64, f64),
    size: (f64, f64),
    smart: bool,
}

impl PlacePlan {
    fn new(s: &Session, name: &str, linked: Option<String>, p: &Value) -> Result<Self> {
        let d = s.active().ok_or(EngineError::NoDocument)?;
        let canvas = (d.doc.size.width as f64, d.doc.size.height as f64);
        let fmt = d.doc.pixel_format();
        // The implicit `fit` is Preferences ▸ General ▸ Resize Image During Place (on by default).
        let fit = p.get("fit").and_then(Value::as_bool).unwrap_or(s.prefs().general.resize_image_during_place);
        let center = match p.get("center").and_then(Value::as_array) {
            Some(c) if c.len() == 2 => Some((c[0].as_f64().unwrap_or(canvas.0 / 2.0), c[1].as_f64().unwrap_or(canvas.1 / 2.0))),
            _ => None,
        };
        // Preferences ▸ General ▸ Always Create Smart Objects When Placing (on by default): off, an
        // embedded place lands as a plain pixel layer. A linked place is always a smart object.
        // `rasterize: true` is `smartObject: false` (the automation's name for it).
        let smart_object = linked.is_some()
            || p.get("smartObject")
                .and_then(Value::as_bool)
                .or_else(|| p.get("rasterize").and_then(Value::as_bool).map(|r| !r))
                .unwrap_or(s.prefs().general.always_create_smart_objects_when_placing);
        let rect = PlaceRect::parse(p)?;
        Ok(PlacePlan {
            name: name.to_string(),
            linked,
            canvas,
            fmt,
            opts: crate::jobs::import_options(s.prefs()),
            fit,
            scale: f64_param(p, "scale"),
            center,
            rect,
            smart_object,
        })
    }

    /// Decodes `bytes` and scales the pixels into place.
    fn decode(self, bytes: Vec<u8>) -> Result<Placed> {
        let PlacePlan { name, linked, canvas: (cw, ch), fmt, opts, fit, scale, center, rect, smart_object } = self;
        let vector = photocraft_io::svg::is_svg(&bytes);
        // Before decoding anything: on the web a placed file that doesn't fit would trap (#10024).
        crate::memory_gate::ensure(&format!("place \u{201c}{}\u{201d}", file_name(&name)), crate::memory_gate::place_need(&bytes, fmt.bytes_per_pixel()))?;
        let imported = photocraft_io::import_with_options(&name, &bytes, &opts).map_err(|e| EngineError::Other(format!("{name}: {e}")))?;
        if imported.preview_only {
            return Err(EngineError::Other(format!(
                "{name}: only this Affinity file's embedded preview could be read; open it with File › Open to see the warning, or export PSD or PNG from Affinity before using it here"
            )));
        }
        let src = imported.document;
        let (w, h) = (src.size.width as f64, src.size.height as f64);
        let scale = match scale {
            Some(k) => (k / 100.0).max(1e-4),
            None if fit && (w > cw || h > ch) => (cw / w).min(ch / h),
            None => 1.0,
        };
        let center = center.unwrap_or((cw / 2.0, ch / 2.0));
        let (mut dx, mut dy) = ((center.0 - w * scale / 2.0).round(), (center.1 - h * scale / 2.0).round());
        let (mut scale, mut scale_y) = (scale, scale);
        if let Some(r) = rect {
            (scale, scale_y) = r.scales(w, h);
            (dx, dy) = (r.x.round(), r.y.round());
            // The scaled image is allocated: refuse one that can't be (as an over-large canvas).
            let px = (w * scale).ceil() * (h * scale_y).ceil();
            if px.is_nan() || px > MAX_PLACED_PIXELS {
                return Err(EngineError::BadParams { cmd: "file.placeEmbedded".into(), msg: format!("the placed image would be too large ({px} pixels)") });
            }
        }
        let mut px = flattened_checked(&src, fmt, "file.place")?;
        if (scale - 1.0).abs() > 1e-9 || (scale_y - 1.0).abs() > 1e-9 {
            px = resize_surface(&px, scale, scale_y, Resample::Bicubic);
        }
        let px = translate_surface(&px, dx as i32, dy as i32);
        let source = match linked {
            Some(path) => SmartSource::Linked { path },
            None => SmartSource::Embedded { file_name: file_name(&name), bytes: Arc::new(bytes) },
        };
        Ok(Placed { name, px, source, vector, scale, scale_y, offset: (dx, dy), size: (w, h), smart: smart_object })
    }
}

impl Placed {
    /// Lands the placed pixels as a layer above the active one (a smart object unless turned
    /// off), as one history step.
    fn apply(self, s: &mut Session) -> Result<Value> {
        let Placed { name, px, source, vector, scale, scale_y, offset: (dx, dy), size: (w, h), smart } = self;
        let layer_name = stem(&name);
        let label = if matches!(&source, SmartSource::Linked { .. }) { "Place Linked" } else { PLACE_EMBEDDED };
        let id = s.edit(label, |doc, active| {
            let layer = if smart {
                let mut so = SmartObject::new(source, Affine { m: [scale, 0.0, 0.0, scale_y, dx, dy] }, Some(px));
                // A vector source (SVG) renders at its placement scale instead of as a resampled raster.
                if vector && let Some(sharp) = crate::smart_cmds::render(doc, &so)? {
                    so.cache = Some(sharp);
                }
                Layer::new(layer_name, LayerContent::Smart(so))
            } else {
                Layer::new(layer_name, LayerContent::Raster(px))
            };
            let id = crate::layer_place::insert_new(doc, *active, layer, crate::layer_place::Place::AutoEditable);
            *active = Some(id);
            Ok(id)
        })?;
        let mut r = json!({"layer": id.0, "scale": scale * 100.0, "bounds": [dx, dy, dx + w * scale, dy + h * scale_y], "smartObject": smart});
        if (scale_y - scale).abs() > 1e-12 {
            r["scaleY"] = json!(scale_y * 100.0);
        }
        Ok(r)
    }
}

/// Most pixels a placed image may be scaled to (`x`/`y`/`width`/`height`): 2^30, as a canvas.
const MAX_PLACED_PIXELS: f64 = (1u64 << 30) as f64;

pub(super) fn place(s: &mut Session, p: &Value, linked: bool) -> Result<Value> {
    let cmd = if linked { "file.placeLinked" } else { "file.placeEmbedded" };
    // The image itself (`data`: base64 or a data: URL, `name` telling its format): nothing is
    // read from disk, so automation allows it.
    if !linked && p.get("path").is_none() && p.get("data").is_some() {
        let bytes = crate::live_cmds::image_bytes(cmd, p)?;
        let name = p.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).unwrap_or("Placed.png").to_string();
        return place_bytes(s, &name, bytes, None, p);
    }
    let path = str_param(p, "path", cmd)?.to_string();
    let bytes = read_file(&path)?;
    // The link is stored absolute: a relative one would later be taken from the document's
    // folder (`doc_path`), not the working directory the path was given in.
    let link = linked.then(|| {
        if std::path::Path::new(&path).is_relative() {
            std::path::absolute(&path).map_or_else(|_| path.clone(), |a| a.to_string_lossy().into_owned())
        } else {
            path.clone()
        }
    });
    place_bytes(s, &path, bytes, link, p)
}
