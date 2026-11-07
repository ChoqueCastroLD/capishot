//! Finding text with the mask model: `select.detectText` runs it over the page and keeps the
//! pieces it found; `select.textPieces` makes a normal selection of the ones a person chose. The
//! model also marks art, so detecting selects nothing.

use std::sync::Arc;

use photocraft_ai_textmask::{Detection, Hooks, TextMasker, default_grow, selection_mask};
use photocraft_algo::selection as sel;
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const DETECT: &str = "select.detectText";
pub const PIECES: &str = "select.textPieces";
pub const STROKE: &str = "select.brushStroke";
pub const ZONE: &str = "select.textZone";
const LABEL: &str = "Detect Text";
const UNAVAILABLE: &str = "text detection isn't available in this build";

impl Session {
    /// Injects (or removes) the service that runs the mask model.
    pub fn set_text_masker(&mut self, masker: Option<Arc<dyn TextMasker>>) {
        self.text_masker = masker;
    }

    pub fn text_masker(&self) -> Option<&Arc<dyn TextMasker>> {
        self.text_masker.as_ref()
    }

    /// Replaces what the active document's detection found (what's left of it after a clean).
    pub fn set_text_detection(&mut self, detection: Arc<Detection>) {
        if let Some(id) = self.active().map(|st| st.doc.id) {
            self.text_detections.retain(|(d, _)| *d != id);
            self.text_detections.push((id, detection));
        }
    }

    /// Drops what the last detection on the active document found.
    pub fn forget_text_detection(&mut self) {
        if let Some(id) = self.active().map(|st| st.doc.id) {
            self.text_detections.retain(|(d, _)| *d != id);
        }
    }

    /// What the last detection on the active document found.
    pub fn text_detection(&self) -> Option<Arc<Detection>> {
        let id = self.active()?.doc.id;
        self.text_detections.iter().find(|(d, _)| *d == id).map(|(_, t)| t.clone())
    }
}

fn pieces_json(d: &Detection) -> Vec<Value> {
    d.regions.iter().map(|r| json!({"id": r.id, "group": r.group, "box": r.bbox, "area": r.area, "score": r.score, "suggested": r.suggested()})).collect()
}

/// The page as the model reads it: what shows, over white.
fn page_rgb(doc: &photocraft_doc::Document) -> (Vec<u8>, usize, usize) {
    let b = photocraft_compose::flatten(doc);
    let (w, h) = (b.rect.width() as usize, b.rect.height() as usize);
    let mut rgb = Vec::with_capacity(w * h * 3);
    for p in &b.px {
        for c in &p[..3] {
            // `flatten` gives colour over the alpha that shows.
            let v = c * p[3] + (1.0 - p[3]);
            rgb.push((v.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    (rgb, w, h)
}

fn detect(s: &mut Session, p: &Value) -> Result<Value> {
    let in_selection = p.get("inSelection").and_then(Value::as_bool).unwrap_or(false);
    let masker = s.text_masker.clone().ok_or_else(|| EngineError::Other(UNAVAILABLE.into()))?;
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let (doc, id) = (st.doc.clone(), st.doc.id);
    crate::jobs::run_async(
        s,
        LABEL,
        true,
        move |ctx, done| {
            ctx.progress(0.0, LABEL);
            let (rgb, width, height) = page_rgb(&doc);
            let started = web_time::Instant::now();
            let (cancel, step) = (ctx.clone(), ctx.clone());
            let hooks = Hooks { cancelled: Arc::new(move || cancel.cancelled()), progress: Arc::new(move |f| step.progress(f, LABEL)) };
            let service = masker.clone();
            // The model always reads the whole page: a crop would reach it larger than it learned.
            masker.detect_then(
                rgb,
                width,
                height,
                hooks,
                Box::new(move |r| {
                    let r = r.map_err(|e| EngineError::Other(e.to_string())).map(|d| match doc.selection.as_ref().filter(|_| in_selection) {
                        Some(zone) => d.clipped(|x, y| zone.sample_channel(x as i32, y as i32, 0) >= 0.5),
                        None => d,
                    });
                    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    done.finish(r.map(|d| (Arc::new(d), service.device(), ms)));
                }),
            );
        },
        move |s, (d, device, ms): (Arc<Detection>, String, u64)| {
            s.text_detections.retain(|(doc, _)| *doc != id);
            let out = json!({"pieces": pieces_json(&d), "device": device, "ms": ms});
            s.text_detections.push((id, d));
            Ok(out)
        },
    )
}

/// `select.textPieces`: a selection of the chosen pieces.
fn pieces(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: PIECES.into(), msg: m.into() };
    let d = s.text_detection().ok_or_else(|| bad("detect the text first (select.detectText)"))?;
    let ids: Vec<u32> = match p.get("pieces") {
        Some(Value::String(a)) if a == "all" => d.regions.iter().map(|r| r.id).collect(),
        Some(Value::String(a)) if a == "suggested" => d.regions.iter().filter(|r| r.suggested()).map(|r| r.id).collect(),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect(),
        _ => return Err(bad("\"pieces\" must be a list of ids, \"all\" or \"suggested\"")),
    };
    let px = |key: &str| p.get(key).and_then(Value::as_f64).map(|v| v.round().max(0.0) as usize);
    let grow = px("grow").unwrap_or_else(|| default_grow(d.width, d.height));
    let area = Rect::new(0, 0, i32::try_from(d.width).unwrap_or(i32::MAX), i32::try_from(d.height).unwrap_or(i32::MAX));
    let mut mode = sel::SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("replace"));
    let mut mask: Vec<f32> = selection_mask(&d, &ids, grow).into_iter().map(|v| if v { 1.0 } else { 0.0 }).collect();
    // Taking a piece out leaves its neighbours' reach whole.
    if let Some(keep) = p.get("keep").and_then(Value::as_array) {
        let keep: Vec<u32> = keep.iter().filter_map(Value::as_u64).filter_map(|v| u32::try_from(v).ok()).collect();
        for (m, k) in mask.iter_mut().zip(selection_mask(&d, &keep, grow)) {
            if k {
                *m = 0.0;
            }
        }
    }
    // A new contour for pieces that have `from`: only their old reach comes out, so what a brush
    // added elsewhere stays.
    if let Some(from) = px("from") {
        let st = s.active().ok_or(EngineError::NoDocument)?;
        let now = sel::mask_from_surface(st.doc.selection.as_ref(), area);
        let old = selection_mask(&d, &ids, from);
        for ((m, n), o) in mask.iter_mut().zip(now).zip(old) {
            *m = m.max(if o { 0.0 } else { n });
        }
        mode = sel::SelectionMode::Replace;
    }
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, mode)
}

/// `select.brushStroke`: a brush stroke adds to the selection or takes from it.
fn stroke(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: STROKE.into(), msg: m.into() };
    let points: Vec<[f64; 2]> = p.get("points").cloned().and_then(|v| serde_json::from_value(v).ok()).ok_or_else(|| bad("\"points\" must be a list of [x, y]"))?;
    let radius = (p.get("size").and_then(Value::as_f64).ok_or_else(|| bad("\"size\" is required"))? / 2.0).max(0.5);
    let area = s.active().ok_or(EngineError::NoDocument)?.doc.bounds();
    let mode = sel::SelectionMode::parse(p.get("mode").and_then(Value::as_str).unwrap_or("add"));
    let mask = stroke_mask(&points, radius, area);
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, mode)
}

/// The inside of a closed shape.
fn polygon_mask(points: &[[f64; 2]], area: Rect) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut mask = vec![0f32; w * h];
    let mut crossings = Vec::new();
    for y in 0..h {
        let line = y as f64 + 0.5 + f64::from(area.y0);
        crossings.clear();
        for (i, a) in points.iter().enumerate() {
            let b = points[(i + 1) % points.len()];
            if (a[1] <= line) != (b[1] <= line) {
                crossings.push(a[0] + (line - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) - f64::from(area.x0));
            }
        }
        crossings.sort_by(f64::total_cmp);
        for pair in crossings.chunks_exact(2) {
            let (x0, x1) = (pair[0].round().clamp(0.0, w as f64) as usize, pair[1].round().clamp(0.0, w as f64) as usize);
            mask[y * w + x0..y * w + x1].fill(1.0);
        }
    }
    mask
}

/// `select.textZone`: what a person drew joins the pieces as a piece of its own, or leaves them.
fn zone(s: &mut Session, p: &Value) -> Result<Value> {
    let bad = |m: &str| EngineError::BadParams { cmd: ZONE.into(), msg: m.into() };
    let d = s.text_detection().ok_or_else(|| bad("detect the text first (select.detectText)"))?;
    let area = Rect::new(0, 0, i32::try_from(d.width).unwrap_or(i32::MAX), i32::try_from(d.height).unwrap_or(i32::MAX));
    let points: Option<Vec<[f64; 2]>> = p.get("points").cloned().and_then(|v| serde_json::from_value(v).ok());
    let mask = if let Some(r) = p.get("rect").cloned().and_then(|v| serde_json::from_value::<[f64; 4]>(v).ok()) {
        polygon_mask(&[[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]], area)
    } else if let Some(points) = points.filter(|v| v.len() >= 2) {
        match p.get("size").and_then(Value::as_f64) {
            Some(size) => stroke_mask(&points, (size / 2.0).max(0.5), area),
            None => polygon_mask(&points, area),
        }
    } else {
        return Err(bad("give \"rect\" or \"points\""));
    };
    let inside = |x: usize, y: usize| mask[y * d.width + x] > 0.0;
    let mode = p.get("mode").and_then(Value::as_str).unwrap_or("add");
    let subtract = mode == "subtract";
    let changed = if subtract { Some(d.clipped(|x, y| !inside(x, y))) } else { d.with_zone(inside) };
    match changed {
        Some(left) if left.regions.is_empty() => s.forget_text_detection(),
        Some(left) => s.set_text_detection(Arc::new(left)),
        None => {}
    }
    crate::selection_cmds::set_selection(s, "Text Selection", area, mask, sel::SelectionMode::parse(mode))
}

/// The discs of a stroke along `points`, close enough to read as a line.
fn stroke_mask(points: &[[f64; 2]], radius: f64, area: Rect) -> Vec<f32> {
    let (w, h) = (area.width() as usize, area.height() as usize);
    let mut mask = vec![0f32; w * h];
    let mut disc = |cx: f64, cy: f64| {
        let (cx, cy) = (cx - f64::from(area.x0), cy - f64::from(area.y0));
        let (y0, y1) = ((cy - radius).floor().max(0.0) as usize, ((cy + radius).ceil().max(0.0) as usize).min(h));
        for y in y0..y1 {
            let dy = y as f64 + 0.5 - cy;
            let dx = (radius * radius - dy * dy).max(0.0).sqrt();
            let (x0, x1) = ((cx - dx).round().max(0.0) as usize, ((cx + dx).round().max(0.0) as usize).min(w));
            if x0 < x1 {
                mask[y * w + x0..y * w + x1].fill(1.0);
            }
        }
    };
    let step = (radius / 2.0).max(0.5);
    for (i, b) in points.iter().enumerate() {
        let a = points[i.saturating_sub(1)];
        let n = ((b[0] - a[0]).hypot(b[1] - a[1]) / step).ceil().max(1.0);
        for k in 1..=n as usize {
            let t = k as f64 / n;
            disc(a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t);
        }
    }
    mask
}

fn has_masker(s: &Session) -> std::result::Result<(), String> {
    s.active().ok_or("no document")?;
    if s.text_masker.is_none() {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: DETECT,
            label: LABEL,
            menu: &[],
            shortcut: None,
            params: r#"{"inSelection"?: bool = false (keep only what is inside the selection, cut at its edge)}: finds the text and sound effects on the page with the mask model, as a job; answers {pieces: [{id, group, box, area, score, suggested}], device, ms}. Selects nothing: the model also marks art, so a person chooses"#,
            enabled: has_masker,
            journal: false,
            run: detect,
        },
        CommandSpec {
            id: PIECES,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"pieces": [id, …] | "all" | "suggested", "grow"?: px past the letters (by default in proportion to the page), "mode"?: "replace"|"add"|"subtract"|"intersect", "keep"?: [id, …] (pieces whose reach a subtraction leaves), "from"?: px (the contour these pieces have now: they change to "grow" and the rest of the selection stays)}: a normal selection of pieces from the last select.detectText"#,
            enabled: has_masker,
            journal: true,
            run: pieces,
        },
        CommandSpec {
            id: STROKE,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"points": [[x, y], …], "size": px, "mode"?: "add"|"subtract"}: a brush stroke adds to the selection or takes from it"#,
            enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".to_string()),
            journal: true,
            run: stroke,
        },
        CommandSpec {
            id: ZONE,
            label: "Text Selection",
            menu: &[],
            shortcut: None,
            params: r#"{"rect": [x0, y0, x1, y1] | "points": [[x, y], …] (a closed shape; with "size": a brush stroke of that width), "mode"?: "add"|"subtract"}: adds a zone to the pieces as a piece of its own and to the selection, or cuts it from both"#,
            enabled: has_masker,
            journal: true,
            run: zone,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_ai_textmask::Page;
    use photocraft_ai_textmask::regions::find_regions;

    /// Finds whatever is dark.
    struct Dark;
    impl TextMasker for Dark {
        fn model_state(&self) -> photocraft_ai_textmask::ModelState {
            photocraft_ai_textmask::ModelState::Ready
        }
        fn device(&self) -> String {
            "test".into()
        }
        fn detect(&self, page: Page<'_>, _: &dyn Fn() -> bool) -> std::result::Result<Detection, photocraft_ai_textmask::Error> {
            let prob: Vec<f32> = page.rgb.chunks_exact(3).map(|c| if c[0] < 128 { 0.995 } else { 0.0 }).collect();
            Ok(find_regions(&prob, page.width, page.height))
        }
    }

    fn page() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 300, "height": 200, "background": "white"})).unwrap();
        s.set_text_masker(Some(Arc::new(Dark)));
        for (x, y) in [(20, 20), (200, 120)] {
            s.execute("select.rect", json!({"x": x, "y": y, "width": 40, "height": 30})).unwrap();
            s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        s.execute("select.deselect", json!({})).unwrap();
        s
    }

    #[test]
    fn detecting_selects_nothing_and_a_person_picks_the_pieces() {
        let mut s = page();
        assert!(s.execute(PIECES, json!({"pieces": "all"})).is_err(), "nothing detected yet");
        let r = s.execute(DETECT, json!({})).unwrap();
        let pieces = r["pieces"].as_array().unwrap();
        assert_eq!(pieces.len(), 2);
        assert!(s.active().unwrap().doc.selection.is_none(), "detecting selects nothing");
        let top = pieces.iter().find(|p| p["box"][1] == 20).unwrap()["id"].clone();
        s.execute(PIECES, json!({"pieces": [top], "grow": 0})).unwrap();
        let sel = s.active().unwrap().doc.selection.clone().unwrap();
        assert_eq!(sel.content_bounds(), Rect::new(20, 20, 60, 50), "only that piece");
        s.execute(PIECES, json!({"pieces": "all", "grow": 3})).unwrap();
        let b = s.active().unwrap().doc.selection.clone().unwrap().content_bounds();
        assert_eq!(b, Rect::new(17, 17, 243, 153), "both, grown");
    }

    fn covered(s: &Session, x: i32, y: i32) -> bool {
        s.active().unwrap().doc.selection.as_ref().is_some_and(|m| m.sample_channel(x, y, 0) >= 0.5)
    }

    #[test]
    fn taking_a_piece_out_leaves_its_neighbours_reach() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 200, "height": 100, "background": "white"})).unwrap();
        s.set_text_masker(Some(Arc::new(Dark)));
        // Two pieces 10 px apart: grown by 10 they overlap.
        for x in [20, 70] {
            s.execute("select.rect", json!({"x": x, "y": 40, "width": 40, "height": 20})).unwrap();
            s.execute("edit.fill", json!({"color": "#000000"})).unwrap();
        }
        s.execute("select.deselect", json!({})).unwrap();
        let r = s.execute(DETECT, json!({})).unwrap();
        let id = |x: i64| r["pieces"].as_array().unwrap().iter().find(|p| p["box"][0] == x).unwrap()["id"].clone();
        s.execute(PIECES, json!({"pieces": "all", "grow": 10})).unwrap();
        s.execute(PIECES, json!({"pieces": [id(20)], "grow": 10, "mode": "subtract", "keep": [id(70)]})).unwrap();
        assert!(!covered(&s, 30, 50), "the piece went");
        assert!(covered(&s, 62, 50), "the neighbour's reach stayed, overlap and all");
    }

    #[test]
    fn a_new_contour_keeps_what_a_brush_added() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(PIECES, json!({"pieces": "all", "grow": 10})).unwrap();
        s.execute(STROKE, json!({"points": [[120, 20], [150, 20]], "size": 10})).unwrap();
        assert!(covered(&s, 135, 20), "the brush added");
        assert!(covered(&s, 14, 30));
        s.execute(PIECES, json!({"pieces": "all", "grow": 2.0, "from": 10.0})).unwrap();
        assert!(!covered(&s, 14, 30), "the contour came in");
        assert!(covered(&s, 19, 30), "to its new reach");
        assert!(covered(&s, 135, 20), "the brush's stayed");
        s.execute(STROKE, json!({"points": [[120, 20], [150, 20]], "size": 10, "mode": "subtract"})).unwrap();
        assert!(!covered(&s, 135, 20), "and comes out with the brush");
    }

    #[test]
    fn a_zone_joins_the_pieces_and_another_cuts_them() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(ZONE, json!({"rect": [100, 20, 150, 60]})).unwrap();
        let d = s.text_detection().unwrap();
        assert_eq!(d.regions.len(), 3, "a piece of its own");
        assert_eq!(d.regions[2].bbox, [100, 20, 150, 60]);
        assert!(covered(&s, 120, 40), "and selected");
        s.execute(ZONE, json!({"points": [[90, 10], [160, 10], [160, 30], [90, 30]], "mode": "subtract"})).unwrap();
        let d = s.text_detection().unwrap();
        assert_eq!(d.regions[2].bbox, [100, 30, 150, 60], "cut");
        assert!(!covered(&s, 120, 25) && covered(&s, 120, 40));
        s.execute(ZONE, json!({"points": [[0, 100], [90, 100]], "size": 20})).unwrap();
        assert!(covered(&s, 45, 100), "a stroke");
        assert!(!covered(&s, 45, 80));
        s.execute(ZONE, json!({"points": [[100, 150], [150, 150], [125, 190]]})).unwrap();
        assert!(covered(&s, 125, 160) && !covered(&s, 102, 185), "a lasso");
        assert!(s.execute(ZONE, json!({})).is_err());
    }

    #[test]
    fn suggested_pieces_are_a_list_of_their_own() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute(PIECES, json!({"pieces": "suggested", "grow": 0})).unwrap();
        assert!(covered(&s, 30, 30));
    }

    #[test]
    fn a_detection_can_stay_inside_the_selection() {
        let mut s = page();
        s.execute("select.rect", json!({"x": 0, "y": 0, "width": 100, "height": 100})).unwrap();
        let r = s.execute(DETECT, json!({"inSelection": true})).unwrap();
        assert_eq!(r["pieces"].as_array().unwrap().len(), 1, "only the piece in the zone");
        let r = s.execute(DETECT, json!({})).unwrap();
        assert_eq!(r["pieces"].as_array().unwrap().len(), 2, "by default the whole page");
    }

    #[test]
    fn a_detection_belongs_to_its_document() {
        let mut s = page();
        s.execute(DETECT, json!({})).unwrap();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap();
        assert!(s.text_detection().is_none());
        assert!(s.execute(PIECES, json!({"pieces": "all"})).is_err());
    }

    #[test]
    fn with_no_masker_the_commands_are_off() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 8, "height": 8})).unwrap();
        assert!(s.execute(DETECT, json!({})).is_err());
    }
}
