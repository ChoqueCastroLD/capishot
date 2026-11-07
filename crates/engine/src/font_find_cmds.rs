//! `font.find`: which fonts draw the lettering in a rectangle, and with what look. By shapes alone
//! (`photocraft_fontmatch`): no model and no OCR, and the same zone gives the same list. Every
//! face's alphabet is drawn on first use, as a job, and kept until the font list changes.

use std::sync::Arc;

use photocraft_fontmatch::{Candidate, FaceSource, Library, Zone, find};
use photocraft_geom::Rect;
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

pub const ID: &str = "font.find";
const LABEL: &str = "Find Font";
/// What a person gets to compare.
pub const OPTIONS: usize = 5;

fn bad(m: &str) -> EngineError {
    EngineError::BadParams { cmd: ID.into(), msg: m.into() }
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

impl Session {
    /// The last `font.find`'s answer.
    pub fn font_found(&self) -> Option<Arc<Value>> {
        self.font_found.clone()
    }
}

fn option(c: &Candidate, dpi: f64) -> Value {
    let pt = f64::from(c.look.size) * 72.0 / dpi;
    let stroke = c.look.outline.map_or(Value::Null, |(color, width)| json!({"color": hex(color), "width": width.round()}));
    json!({
        "family": c.family,
        "style": c.style,
        "distance": c.distance,
        "rotation": c.look.tilt,
        "ground": hex(c.look.ground),
        // What the type commands take.
        "params": {"font": c.family, "fontStyle": c.style, "size": pt, "color": hex(c.look.fill), "stroke": stroke},
    })
}

fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let side = |i: usize| p.get("rect").and_then(|r| r.get(i)).and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() < 1e7);
    let (Some(x), Some(y), Some(w), Some(h)) = (side(0), side(1), side(2), side(3)) else { return Err(bad("\"rect\" must be [x, y, width, height]")) };
    let k = p.get("k").and_then(Value::as_u64).map_or(OPTIONS, |v| (v as usize).clamp(1, 20));
    let st = s.active().ok_or(EngineError::NoDocument)?;
    let doc = st.doc.clone();
    let area = Rect::from_xywh(x.floor() as i32, y.floor() as i32, w.ceil() as u32, h.ceil() as u32).intersect(&doc.bounds());
    if area.is_empty() {
        return Err(bad("the zone is outside the page"));
    }
    let dpi = f64::from(doc.resolution_dpi).max(1.0);
    let generation = photocraft_text::fonts::generation();
    let cached = s.font_library.as_ref().filter(|(g, _)| *g == generation).map(|(_, l)| l.clone());
    let faces = if cached.is_some() { Vec::new() } else { photocraft_text::shared().lock().unwrap_or_else(|e| e.into_inner()).fonts.face_blobs() };
    crate::jobs::run_async(
        s,
        LABEL,
        false,
        move |ctx, done| {
            let library = cached.unwrap_or_else(|| {
                let sources: Vec<FaceSource<'_>> = faces.iter().map(|f| FaceSource { family: &f.family, style: &f.style, data: f.bytes(), index: f.index }).collect();
                let step = ctx.clone();
                Arc::new(Library::build(&sources, &mut |f| step.progress(f * 0.9, LABEL)))
            });
            ctx.progress(0.9, LABEL);
            let buffer = photocraft_compose::render(&doc, area);
            let mut rgb = Vec::with_capacity(buffer.px.len() * 3);
            for px in &buffer.px {
                for c in &px[..3] {
                    // What shows, over white.
                    rgb.push(((c * px[3] + 1.0 - px[3]).clamp(0.0, 1.0) * 255.0).round() as u8);
                }
            }
            let zone = Zone { rgb: &rgb, width: area.width() as usize, height: area.height() as usize };
            let found: Vec<Value> = find(&library, &zone, k).iter().map(|c| option(c, dpi)).collect();
            done.finish(Ok((library, generation, json!({"options": found, "rect": [area.x0, area.y0, area.width(), area.height()], "faces": 0}))));
        },
        move |s, (library, generation, mut out): (Arc<Library>, u64, Value)| {
            out["faces"] = json!(library.faces.len());
            s.font_library = Some((generation, library));
            s.font_found = Some(Arc::new(out.clone()));
            Ok(out)
        },
    )
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: LABEL,
        menu: &[],
        shortcut: None,
        params: r#"{"rect": [x, y, width, height], "k"?: 1..20 = 5}: the fonts closest to the lettering in the rectangle, by shapes alone (no model, no OCR; deterministic), as a job; answers {options: [{family, style, distance, rotation, ground, params: {font, fontStyle, size, color, stroke}}], rect, faces}. Empty options: nothing there reads as letters"#,
        enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".to_string()),
        journal: false,
        run,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Black lettering in a bundled font, on white.
    fn page() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 600, "height": 200, "background": "white"})).unwrap();
        s.execute("type.create", json!({"x": 40, "y": 110, "text": "NO PUEDE SER", "font": "Inter", "size": 60, "color": "#000000"})).unwrap();
        s
    }

    #[test]
    fn it_finds_the_font_and_says_the_same_twice() {
        let mut s = page();
        let a = s.execute(ID, json!({"rect": [10, 40, 580, 120]})).unwrap();
        let b = s.execute(ID, json!({"rect": [10, 40, 580, 120]})).unwrap();
        assert_eq!(a["options"], b["options"], "the same zone, the same answer");
        let names: Vec<&str> = a["options"].as_array().unwrap().iter().filter_map(|o| o["family"].as_str()).collect();
        assert!(names.contains(&"Inter"), "{names:?}");
        assert!(names.len() <= OPTIONS);
        assert_eq!(a["options"][0]["params"]["color"], "#000000");
        assert!(s.font_found().is_some());
    }

    #[test]
    fn nothing_to_read_is_no_options_and_a_bad_zone_is_an_error() {
        let mut s = page();
        let r = s.execute(ID, json!({"rect": [0, 170, 300, 30]})).unwrap();
        assert!(r["options"].as_array().unwrap().is_empty(), "{r}");
        assert!(s.execute(ID, json!({"rect": [5000, 5000, 10, 10]})).is_err());
        assert!(s.execute(ID, json!({})).is_err());
    }
}
