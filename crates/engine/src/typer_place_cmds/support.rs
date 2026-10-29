//! Shared machinery of the TypeR placement commands and queries: the cached page image bubbles
//! are found on, reading the selection as a mask, TypeR styles as Capishot type, and measuring
//! and moving text by its ink.

use std::collections::HashMap;
use std::sync::Arc;

use photocraft_doc::text::{AntiAlias, CharStyle, ParagraphRun, ParagraphStyle, TextDirection, TextRun, TextShape};
use photocraft_doc::{DocId, Document, Effect, FxCommon, FxPaint, Layer, LayerContent, LayerId, PixelFormat, StrokeFx, StrokePosition, TextLayer};
use photocraft_typer::bubble::{self, Mask, Rect};
use photocraft_typer::psjson::{self, MapOptions};
use photocraft_typer::{Direction, Style, markdown, place};
use serde_json::Value;

use crate::{EngineError, Result, Session};

/// Placement state kept with the TypeR session: the page image cache and the cleaning layers
/// hidden per document. Not persisted, not compared.
#[derive(Clone, Default)]
pub struct PlaceState {
    page: Option<PageImage>,
    /// `typer.toggleCleaningLayers`: the layers hidden in each document, restored on the next
    /// toggle.
    pub hidden_cleaning: HashMap<DocId, Vec<LayerId>>,
}

impl PlaceState {
    /// Bytes the cached page holds (Bubble Type and TypeR's bubble scans).
    pub fn page_bytes(&self) -> usize {
        self.page.as_ref().map_or(0, |p| p.img.len() * 4)
    }

    /// Drop the cached page (memory pressure; the next scan composites it again).
    pub fn forget_page(&mut self) {
        self.page = None;
    }
}

impl PartialEq for PlaceState {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl std::fmt::Debug for PlaceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaceState").field("cached_page", &self.page.is_some()).field("hidden_cleaning", &self.hidden_cleaning).finish()
    }
}

/// The page as the wand sees it: every visible layer except type layers, composited once and
/// reused while those layers' pixels are unchanged (placing text doesn't invalidate it).
#[derive(Clone)]
pub struct PageImage {
    doc_id: DocId,
    stripped: Arc<Document>,
    pub area: photocraft_geom::Rect,
    pub img: Arc<Vec<[u8; 4]>>,
}

impl PageImage {
    /// The page box.
    pub fn rect(&self) -> Rect {
        Rect::new(f64::from(self.area.x0), f64::from(self.area.y0), f64::from(self.area.width()), f64::from(self.area.height()))
    }
}

const MAX_DEPTH: usize = 512;

/// `doc` without its type layers.
fn without_text(doc: &Document) -> Document {
    fn strip(layers: &mut Vec<Layer>, depth: usize) {
        layers.retain(|l| !matches!(l.content, LayerContent::Text(_)));
        if depth >= MAX_DEPTH {
            return;
        }
        for l in layers.iter_mut() {
            if let Some(ch) = l.children_mut() {
                strip(ch, depth + 1);
            }
        }
    }
    let mut d = doc.clone();
    d.selection = None;
    strip(&mut d.layers, 0);
    d
}

/// The cached page image of the active document (recomposited only when its non-type pixels
/// changed).
/// Rows of the page composited at a time for [`page_image`].
const PAGE_BAND: i32 = 256;

pub fn page_image(s: &mut Session) -> Result<PageImage> {
    let d = s.active().ok_or(EngineError::NoDocument)?;
    let stripped = without_text(&d.doc);
    if let Some(p) = &s.typer.place.page
        && p.doc_id == d.doc.id
        && crate::layer_multi_cmds::same_pixels(&p.stripped, &stripped)
    {
        return Ok(p.clone());
    }
    let area = stripped.bounds();
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    // In bands: a whole page composited at once is four floats a pixel (a 2000 x 3000 page,
    // 96 MB) for the 8-bit copy kept here, and on the web that peak stays in the page's memory.
    let mut img: Vec<[u8; 4]> = Vec::with_capacity(area.width() as usize * area.height() as usize);
    let mut y = area.y0;
    while y < area.y1 {
        let y1 = (y + PAGE_BAND).min(area.y1);
        img.extend(photocraft_compose::render(&stripped, photocraft_geom::Rect::new(area.x0, y, area.x1, y1)).px.iter().map(|p| p.map(q)));
        y = y1;
    }
    let page = PageImage { doc_id: d.doc.id, stripped: Arc::new(stripped), area, img: Arc::new(img) };
    s.typer.place.page = Some(page.clone());
    Ok(page)
}

/// Forget the cached page of a closed document.
pub fn forget_doc(s: &mut Session, id: DocId) {
    if s.typer.place.page.as_ref().is_some_and(|p| p.doc_id == id) {
        s.typer.place.page = None;
    }
    s.typer.place.hidden_cleaning.remove(&id);
}

/// The document selection as a mask (`None` without a selection).
pub fn selection_mask(doc: &Document) -> Option<Mask> {
    let sel = doc.selection.as_ref()?;
    let b = sel.content_bounds().intersect(&doc.bounds());
    if b.is_empty() {
        return None;
    }
    let cov = photocraft_algo::selection::mask_from_surface(Some(sel), b);
    Mask::from_coverage_f32(b.x0, b.y0, b.width() as usize, b.height() as usize, &cov)
}

/// TypeR's error for a missing or tiny target, as a `BadParams` naming the code.
pub fn typer_error(cmd: &str, code: &str) -> EngineError {
    let msg = match code {
        "noSelection" => "noSelection: select a bubble first (Magic Wand, Lasso, Marquee…), or pass \"bounds\"",
        "smallSelection" => "smallSelection: the selected area is too small (under 200 px after removing its tail)",
        "noTextLayer" => "noTextLayer: select a type layer",
        "noText" => "noText: there is no current line; load a script or pass \"text\"",
        "sizeSource" => "sizeSource: keep size needs an active type layer to take the size from",
        "noBubble" => "noBubble: no bubble around the text (the region found is far larger than the text)",
        other => other,
    };
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

pub fn bad(cmd: &str, msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: cmd.into(), msg: msg.into() }
}

/// `[x, y, w, h]` param.
pub fn rect_param(p: &Value, key: &str, cmd: &str) -> Result<Option<Rect>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) => {
            let v: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
            if v.len() != a.len() {
                return Err(bad(cmd, format!("\"{key}\" must be [x, y, width, height] in pixels")));
            }
            Rect::from_xywh(&v).map(Some).ok_or_else(|| bad(cmd, format!("\"{key}\" must be [x, y, width, height] with a positive size")))
        }
        Some(_) => Err(bad(cmd, format!("\"{key}\" must be [x, y, width, height] in pixels"))),
    }
}

pub fn bool_param(p: &Value, key: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(cmd, format!("\"{key}\" must be true or false"))),
    }
}

pub fn num_param(p: &Value, key: &str, cmd: &str, lo: f64, hi: f64) -> Result<Option<f64>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_f64()
            .filter(|n| n.is_finite() && (lo..=hi).contains(n))
            .map(Some)
            .ok_or_else(|| bad(cmd, format!("\"{key}\" must be a number in {lo}..={hi}"))),
    }
}

pub fn text_param(p: &Value, key: &str, cmd: &str) -> Result<Option<String>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(t)) if t.len() <= crate::typer_place_cmds::MAX_TEXT_BYTES => Ok(Some(t.clone())),
        Some(Value::String(_)) => Err(bad(cmd, format!("\"{key}\" is longer than {} bytes", crate::typer_place_cmds::MAX_TEXT_BYTES))),
        Some(_) => Err(bad(cmd, format!("\"{key}\" must be a string"))),
    }
}

pub fn direction_param(p: &Value, cmd: &str, fallback: Direction) -> Result<Direction> {
    match p.get("direction") {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::String(d)) if d == "ltr" => Ok(Direction::Ltr),
        Some(Value::String(d)) if d == "rtl" => Ok(Direction::Rtl),
        Some(_) => Err(bad(cmd, "\"direction\" must be \"ltr\" or \"rtl\"")),
    }
}

/// The `style` param: `{"id": …}` from the library, or an inline TypeR style object.
pub fn style_param(s: &Session, p: &Value, cmd: &str) -> Result<Option<Style>> {
    let Some(v) = p.get("style").filter(|v| !v.is_null()) else { return Ok(None) };
    let o = v.as_object().ok_or_else(|| bad(cmd, "\"style\" must be {\"id\": …} or a TypeR style object"))?;
    let only_id = o.len() == 1 && o.contains_key("id");
    if only_id {
        let id = photocraft_typer::Id::from_value(&o["id"]).ok_or_else(|| bad(cmd, "\"style.id\" must be a style id"))?;
        return s.typer.state.library.style(&id).cloned().map(Some).ok_or_else(|| bad(cmd, format!("no style {id}")));
    }
    photocraft_typer::exchange::validate(&serde_json::json!({ "styles": [v] })).map_err(|e| bad(cmd, e))?;
    let mut m = o.clone();
    m.entry("id").or_insert_with(|| Value::String("inline".into()));
    m.entry("name").or_insert_with(|| Value::String("inline".into()));
    Style::from_json(&Value::Object(m)).map(Some).ok_or_else(|| bad(cmd, "invalid style"))
}

/// A TypeR style resolved for one document: what goes on the layer.
#[derive(Clone, Debug)]
pub struct Styled {
    pub char_style: CharStyle,
    pub paragraph: ParagraphStyle,
    pub antialias: Option<AntiAlias>,
    pub stroke: Option<psjson::Stroke>,
}

/// Resolve `style` at the document's resolution (text scale applied by the caller's options).
/// `size_pt` overrides the style size (keep size); a style without a size takes `fallback_pt`.
pub fn styled(style: &Style, opts: &MapOptions, size_pt: Option<f32>, fallback_pt: f32) -> Styled {
    let m = psjson::map_style(style, opts);
    let mut c = m.char_style;
    c.size_pt = size_pt.or(m.size_pt).unwrap_or(fallback_pt).clamp(crate::type_cmds::TYPE_SIZE_MIN_PT as f32, crate::type_cmds::TYPE_SIZE_MAX_PT as f32);
    if let Some(l) = c.leading_pt {
        c.leading_pt = Some(l.clamp(0.01, 5000.0));
    }
    Styled { char_style: c, paragraph: m.paragraph, antialias: m.antialias, stroke: m.stroke }
}

/// The text and its character runs: Markdown bold/italic becomes faux bold/italic runs (S9).
pub fn runs_for(text: &str, base: &CharStyle, interpret_markdown: bool) -> (String, Vec<TextRun>) {
    let text = crate::type_cmds::norm_text(text);
    if !interpret_markdown {
        return (text.clone(), vec![TextRun { len: text.len(), style: base.clone() }]);
    }
    let parsed = markdown::parse_runs(&text);
    let mut runs: Vec<TextRun> = Vec::new();
    for r in parsed.runs.iter().filter(|r| !r.text.is_empty()) {
        let mut st = base.clone();
        st.faux_bold |= r.bold;
        st.faux_italic |= r.italic;
        match runs.last_mut() {
            Some(last) if last.style == st => last.len += r.text.len(),
            _ => runs.push(TextRun { len: r.text.len(), style: st }),
        }
    }
    let total: usize = runs.iter().map(|r| r.len).sum();
    if total != parsed.text.len() || runs.is_empty() {
        return (parsed.text.clone(), vec![TextRun { len: parsed.text.len(), style: base.clone() }]);
    }
    (parsed.text, runs)
}

/// A new type layer (not rendered) with `text` in `st`, as point text at the origin or box text
/// `width` wide at the origin.
pub fn new_text(text: &str, st: &Styled, point: bool, width: f64, height: f64, interpret_markdown: bool) -> TextLayer {
    let (text, runs) = runs_for(text, &st.char_style, interpret_markdown);
    let shape = if point { TextShape::Point } else { TextShape::Box { x: 0.0, y: 0.0, width: width as f32, height: height as f32 } };
    let mut t = TextLayer {
        paragraphs: vec![ParagraphRun { len: text.len(), style: st.paragraph.clone() }],
        runs,
        text,
        shape,
        antialias: st.antialias.unwrap_or(AntiAlias::Smooth),
        ..Default::default()
    };
    t.sync_summary();
    t
}

/// Ink bounds of a rendered type layer, document pixels.
pub fn ink(t: &TextLayer) -> Option<Rect> {
    let b = t.cache.as_ref()?.content_bounds();
    (!b.is_empty()).then(|| Rect::new(f64::from(b.x0), f64::from(b.y0), f64::from(b.width()), f64::from(b.height())))
}

/// Render the layer's pixels (no PSD text data) and return its ink.
pub fn measure(t: &mut TextLayer, dpi: f32, fmt: PixelFormat) -> Option<Rect> {
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    let (_, r) = eng.render(t, dpi, fmt);
    t.cache = Some(r.surface);
    drop(eng);
    ink(t)
}

/// Render the layer's pixels and regenerate its PSD `TySh` data (what `type_cmds::refresh` does).
pub fn finish(t: &mut TextLayer, dpi: f32, fmt: PixelFormat) {
    let mut eng = photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner());
    let (layout, r) = eng.render(t, dpi, fmt);
    t.cache = Some(r.surface);
    t.psd_raw = Some(Arc::new(photocraft_text::psd::build_tysh(t, dpi, layout.bounds())));
}

/// A box tall (and, with `wide`, wide) enough that nothing is clipped while measuring.
fn measure_span(doc_w: f64, doc_h: f64) -> f32 {
    (2.0 * doc_w.max(doc_h)).clamp(1000.0, 30_000.0) as f32
}

/// Size of the first run in pixels.
pub fn size_px(t: &TextLayer, dpi: f32) -> f64 {
    let pt = t.runs.first().map_or(t.size_pt, |r| r.style.size_pt);
    place::pt_to_px(f64::from(pt), f64::from(dpi))
}

/// How a box is refit after the text changed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    /// Keep the shape (point text, or a plain re-centre).
    Keep,
    /// Keep the box width, refit its height to the text (`extent + size + 2`), measured in an
    /// oversized box so no line is clipped.
    Height,
    /// Shrink both to the text (`extent + max(2, 0.4 size)` × `extent + size + 2`): the lines
    /// are kept as given (TextShapeR apply).
    Both,
}

/// Document numbers placement needs: width, height, resolution, pixel format.
#[derive(Clone, Copy, Debug)]
pub struct DocInfo {
    pub width: f64,
    pub height: f64,
    pub dpi: f32,
    pub format: PixelFormat,
}

impl DocInfo {
    pub fn of(doc: &Document) -> DocInfo {
        DocInfo { width: f64::from(doc.size.width), height: f64::from(doc.size.height), dpi: doc.resolution_dpi, format: doc.pixel_format() }
    }
}

/// Lay the text out, refit its box per `fit`, then move it by whole pixels so its ink centre
/// lands on `target`. Renders the final pixels and PSD data. Returns the final ink.
pub fn fit_and_center(t: &mut TextLayer, fit: Fit, target: (f64, f64), d: DocInfo) -> Option<Rect> {
    let size = size_px(t, d.dpi);
    let mut measured = None;
    if fit != Fit::Keep
        && let TextShape::Box { x, y, width, .. } = t.shape
    {
        let span = measure_span(d.width, d.height);
        let w = if fit == Fit::Both { span } else { width };
        t.shape = TextShape::Box { x, y, width: w, height: span };
        let i = measure(t, d.dpi, d.format);
        let new_w = match (fit, i) {
            (Fit::Both, Some(i)) => place::fit_width(i.width, size) as f32,
            _ => width,
        };
        t.shape = TextShape::Box { x, y, width: new_w, height: place::fit_height(i.map_or(0.0, |i| i.height), size) as f32 };
        // Box text is top-aligned: a taller box leaves the ink where it was. A narrower one moves
        // centred lines, so measure again.
        measured = if fit == Fit::Height { i } else { None };
    }
    let i = match measured {
        Some(i) => Some(i),
        None => measure(t, d.dpi, d.format),
    };
    if let Some(i) = i {
        let (dx, dy) = place::center_offset(target, &i);
        t.transform.m[4] += dx;
        t.transform.m[5] += dy;
    }
    finish(t, d.dpi, d.format);
    // Anything that moved the layout after measuring gets one correction.
    if let Some(after) = ink(t) {
        let (dx, dy) = place::center_offset(target, &after);
        if dx.abs() >= 1.0 || dy.abs() >= 1.0 {
            t.transform.m[4] += dx;
            t.transform.m[5] += dy;
            finish(t, d.dpi, d.format);
        }
    }
    ink(t)
}

/// The TypeR stroke as a layer-style Stroke (outside), replacing the layer's stroke if it has
/// one; a style without a stroke leaves the layer's effects alone.
pub fn set_stroke(l: &mut Layer, stroke: Option<&psjson::Stroke>, mode: photocraft_color::ColorMode) {
    let Some(st) = stroke else { return };
    let color = photocraft_color::Color::rgb(f32::from(st.color[0]) / 255.0, f32::from(st.color[1]) / 255.0, f32::from(st.color[2]) / 255.0);
    let fx = Effect::Stroke(StrokeFx {
        common: FxCommon::new(photocraft_color::BlendMode::Normal, (st.opacity / 100.0).clamp(0.0, 1.0) as f32),
        size: (st.size as f32).clamp(0.0, 250.0),
        position: StrokePosition::Outside,
        paint: FxPaint::Color(color),
        overprint: false,
    })
    .in_mode(mode);
    l.effects.enabled = true;
    match l.effects.items.iter_mut().find(|e| matches!(e, Effect::Stroke(_))) {
        Some(slot) => *slot = fx,
        None => l.effects.items.push(fx),
    }
}

/// The paragraph style a text-only apply keeps (TypeR's "safe" paragraph properties): the first
/// paragraph's, without its direction.
pub fn safe_paragraph(t: &TextLayer) -> ParagraphStyle {
    let mut p = t.paragraph_runs().first().map(|r| r.style.clone()).unwrap_or_default();
    p.direction = TextDirection::Auto;
    p
}

/// Pixels a rendered layer (plus its stroke) covers, for the canvas damage.
pub fn damage_of(l: &Layer) -> Option<photocraft_geom::Rect> {
    let LayerContent::Text(t) = &l.content else { return None };
    let b = t.cache.as_ref()?.tile_bounds();
    let fx = l.effects.items.iter().filter_map(|e| if let Effect::Stroke(s) = e { Some(s.size) } else { None }).fold(0.0f32, f32::max);
    Some(b.inflate(fx.ceil() as i32 + 2))
}

/// Selected layers in click order (`DocState::selected_layers` keeps the order layers were
/// selected in); the active layer alone when the set is stale.
pub fn click_order(st: &crate::DocState) -> Vec<LayerId> {
    let Some(active) = st.active_layer else { return Vec::new() };
    let raw: Vec<LayerId> = if st.selected_layers.contains(&active) { st.selected_layers.clone() } else { vec![active] };
    let mut out = Vec::new();
    for id in raw {
        if st.doc.layer(id).is_some() && !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// `layers` param (ids), else the selection in click order.
pub fn target_layers(s: &Session, p: &Value, cmd: &str) -> Result<Vec<LayerId>> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    match p.get("layers") {
        None | Some(Value::Null) => Ok(click_order(st)),
        Some(Value::Array(a)) => {
            let mut out = Vec::new();
            for v in a {
                let id = v.as_u64().map(LayerId).ok_or_else(|| bad(cmd, "\"layers\" must be a list of layer ids"))?;
                if st.doc.layer(id).is_none() {
                    return Err(EngineError::NoLayer(id));
                }
                if !out.contains(&id) {
                    out.push(id);
                }
            }
            if out.len() > crate::typer_place_cmds::MAX_LAYERS {
                return Err(bad(cmd, format!("at most {} layers at once", crate::typer_place_cmds::MAX_LAYERS)));
            }
            Ok(out)
        }
        Some(_) => Err(bad(cmd, "\"layers\" must be a list of layer ids")),
    }
}

/// The type layers among `ids`, refusing locked ones; `strict` refuses non-type layers too.
pub fn text_targets(s: &Session, ids: &[LayerId], cmd: &str, strict: bool) -> Result<Vec<LayerId>> {
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let mut out = Vec::new();
    for id in ids {
        let l = st.doc.layer(*id).ok_or(EngineError::NoLayer(*id))?;
        if !matches!(l.content, LayerContent::Text(_)) {
            if strict {
                return Err(bad(cmd, format!("noTextLayer: layer {} (\"{}\") is not a type layer", id.0, l.name)));
            }
            continue;
        }
        if st.doc.effective_locks(*id).all {
            return Err(bad(cmd, format!("the layer \"{}\" is locked", l.name)));
        }
        out.push(*id);
    }
    if out.is_empty() {
        return Err(typer_error(cmd, "noTextLayer"));
    }
    Ok(out)
}

/// The size (px) of a type layer's first run.
pub fn layer_size_px(doc: &Document, id: LayerId) -> f64 {
    match doc.layer(id).map(|l| &l.content) {
        Some(LayerContent::Text(t)) => size_px(t, doc.resolution_dpi),
        _ => 0.0,
    }
}

/// The ink box of a type layer (rendered if it has no pixels yet).
pub fn layer_ink(doc: &Document, id: LayerId) -> Option<Rect> {
    let LayerContent::Text(t) = &doc.layer(id)?.content else { return None };
    match ink(t) {
        Some(i) => Some(i),
        None => {
            let mut c = t.clone();
            measure(&mut c, doc.resolution_dpi, doc.pixel_format())
        }
    }
}

/// The bubble around a type layer: the wand just left of its ink on the page image (D1), opened.
/// `Err(code)` with TypeR's error code when there is none.
pub fn bubble_for_ink(page: &PageImage, ink: &Rect, size_px: f64, tolerance: f32) -> std::result::Result<bubble::Opened, &'static str> {
    let seed = bubble::seed_left_of(ink);
    // The lobe of a double bubble that holds the text, judged with its outline (WP4).
    let (mask, outline) = bubble::wand_bubble(&page.img, page.area, seed, tolerance, Some(ink.center())).ok_or("noSelection")?;
    let o = bubble::opened_bounds(&mask).ok_or("noSelection")?;
    let judge = bubble::Judge { text: Some((ink, size_px)), seed: bubble::pixel(&page.img, page.area, seed), page: Some(page.rect()), outline: Some(outline) };
    match judge.verdict(&o) {
        None => Ok(o),
        Some(why) => Err(why),
    }
}

/// The opened bounds of the document selection: `Ok(None)` without one, `Err` when too small.
pub fn selection_target(doc: &Document, cmd: &str) -> Result<Option<bubble::Opened>> {
    let Some(mask) = selection_mask(doc) else { return Ok(None) };
    let o = bubble::opened_bounds(&mask).ok_or_else(|| typer_error(cmd, "noSelection"))?;
    if o.opened.area() < bubble::MIN_AREA {
        return Err(typer_error(cmd, "smallSelection"));
    }
    Ok(Some(o))
}

/// A `Rect` as `[x, y, w, h]` JSON.
pub fn rect_json(r: &Rect) -> Value {
    serde_json::json!([r.x, r.y, r.width, r.height])
}
