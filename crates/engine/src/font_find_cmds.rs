//! `font.find`: which fonts draw the lettering in a rectangle, and with what look. By shapes alone
//! (`photocraft_fontmatch`): no model and no OCR, and the same zone gives the same list. Every
//! face's alphabet is drawn on first use, as a job, and kept until the font list changes.

use std::sync::Arc;

use photocraft_fontmatch::{Candidate, FaceSource, Library, Scope, Zone, find_in};
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
    let gradient = c.look.gradient.map_or(Value::Null, |(top, bottom)| json!({"top": hex(top), "bottom": hex(bottom)}));
    let mut params = json!({"font": c.family, "fontStyle": c.style, "size": pt, "color": hex(c.look.fill), "stroke": stroke});
    let mut effects = Vec::new();
    if let Some((top, bottom)) = c.look.gradient {
        // Photoshop's angle 90 runs up the layer: its first colour is at the bottom.
        effects.push(json!(["gradientOverlay", {"from": hex(bottom), "to": hex(top), "angle": 90}]));
    }
    if let Some((color, [dx, dy])) = c.look.shadow {
        // The angle is where the light comes from: the shadow falls the other way.
        let angle = f64::from(dy).atan2(f64::from(-dx)).to_degrees().round();
        effects.push(json!(["dropShadow", {"color": hex(color), "distance": f64::from(dx.hypot(dy)).round(), "angle": angle, "size": 0, "spread": 0, "opacity": 100, "blend": "Normal", "useGlobalLight": false}]));
    }
    if !effects.is_empty() {
        params[crate::type_bubble_cmds::ROLE_EFFECTS] = json!(effects);
    }
    let shadow = c.look.shadow.map_or(Value::Null, |(color, at)| json!({"color": hex(color), "offset": at}));
    json!({
        "family": c.family,
        "style": c.style,
        "distance": c.distance,
        "rotation": c.look.tilt,
        "ground": hex(c.look.ground),
        "gradient": gradient,
        "shadow": shadow,
        "mixed": c.look.mixed,
        // What the type commands take.
        "params": params,
    })
}

/// The families a list parameter names.
fn names(p: &Value, key: &str) -> std::collections::HashSet<String> {
    p.get(key).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

/// The families a document's type uses.
fn used_families(list: &[photocraft_doc::Layer], out: &mut std::collections::HashSet<String>) {
    for l in list {
        if let photocraft_doc::LayerContent::Text(t) = &l.content {
            out.extend(t.runs.iter().map(|r| r.style.font_family.clone()));
        }
        if let Some(children) = l.children() {
            used_families(children, out);
        }
    }
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
    let mut scope = Scope { only: names(p, "families"), prefer: names(p, "prefer") };
    match p.get("scope").and_then(Value::as_str) {
        None | Some("all") => {}
        Some("document") => used_families(&doc.layers, &mut scope.only),
        Some(_) => return Err(bad("\"scope\" must be \"all\" or \"document\"")),
    }
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
            let found: Vec<Value> = find_in(&library, &zone, k, &scope).iter().map(|c| option(c, dpi)).collect();
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
        params: r#"{"rect": [x, y, width, height], "k"?: 1..20 = 5, "scope"?: "all"|"document" (the fonts the document's type uses), "families"?: [family, …] (only these), "prefer"?: [family, …] (these count as a little closer)}: the fonts closest to the lettering in the rectangle, by shapes alone (no model, no OCR; deterministic), as a job; answers {options: [{family, style, distance, rotation, ground, gradient: null|{top, bottom}, shadow: null|{color, offset}, mixed: bool (other lettering in the zone: this is the largest), params: {font, fontStyle, size, color, stroke}}], rect, faces}; five that differ, not weights or copies of one design. Empty options: nothing there reads as letters"#,
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
    fn a_gradient_in_the_lettering_comes_back_on_the_right_way_up() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 600, "height": 200, "background": "white"})).unwrap();
        let r = s.execute("type.create", json!({"x": 40, "y": 140, "text": "HIT", "font": "Inter", "size": 120, "color": "#000000"})).unwrap();
        let layer = r["layer"].clone();
        let top = |s: &Session, y: i32| photocraft_compose::render(&s.active().unwrap().doc, Rect::from_xywh(0, y, 600, 1)).px.iter().filter(|p| p[3] > 0.9 && p[1] < 0.2).map(|p| [p[0], p[2]]).next();
        // What the applied option draws: find a bar of the first letter's top and bottom.
        let o = option(
            &Candidate {
                family: "Inter".into(),
                style: "Regular".into(),
                distance: 0.0,
                look: photocraft_fontmatch::Look { fill: [0, 0, 0], ground: [255, 255, 255], outline: None, gradient: Some(([255, 0, 0], [0, 0, 255])), shadow: None, mixed: false, tilt: 0.0, cap: 80.0, size: 110.0 },
            },
            72.0,
        );
        let styles = json!({"floating": o["params"], "bubble": o["params"], "narration": o["params"]});
        s.execute("type.setBubbleStyle", json!({"layer": layer, "styles": styles})).unwrap();
        let info = s.execute("type.info", json!({"layer": layer})).unwrap();
        let b: Vec<f64> = info["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
        let first = top(&s, b[1] as i32 + 12).expect("ink near the top");
        let last = top(&s, b[3] as i32 - 12).expect("ink near the bottom");
        assert!(first[0] > first[1], "red on top: {first:?}");
        assert!(last[1] > last[0], "blue below: {last:?}");
    }

    #[test]
    fn a_measured_shadow_falls_the_same_way_when_applied() {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 600, "height": 240, "background": "#000000"})).unwrap();
        let r = s.execute("type.create", json!({"x": 60, "y": 170, "text": "HIT", "font": "Inter", "size": 120, "color": "#ffffff"})).unwrap();
        let layer = r["layer"].clone();
        let o = option(
            &Candidate {
                family: "Inter".into(),
                style: "Regular".into(),
                distance: 0.0,
                look: photocraft_fontmatch::Look { fill: [255, 255, 255], ground: [0, 0, 0], outline: None, gradient: None, shadow: Some(([120, 120, 120], [6.0, 6.0])), mixed: false, tilt: 0.0, cap: 90.0, size: 120.0 },
            },
            72.0,
        );
        let styles = json!({"floating": o["params"], "bubble": o["params"], "narration": o["params"]});
        s.execute("type.setBubbleStyle", json!({"layer": layer, "styles": styles})).unwrap();
        let doc = &s.active().unwrap().doc;
        let info_bounds = doc.bounds();
        let b = photocraft_compose::render(doc, info_bounds);
        let (w, px) = (b.rect.width() as usize, &b.px);
        // Grey pixels: their middle lies down and right of the white ones'.
        let mid = |pick: &dyn Fn(&[f32; 4]) -> bool| {
            let (mut sx, mut sy, mut n) = (0f64, 0f64, 0f64);
            for (i, p) in px.iter().enumerate() {
                if pick(p) {
                    sx += (i % w) as f64;
                    sy += (i / w) as f64;
                    n += 1.0;
                }
            }
            (sx / n.max(1.0), sy / n.max(1.0), n)
        };
        let white = mid(&|p| p[0] > 0.9);
        let grey = mid(&|p| (0.35..0.6).contains(&p[0]));
        assert!(grey.2 > 50.0, "a shadow shows: {}", grey.2);
        assert!(grey.0 > white.0 && grey.1 > white.1, "down and to the right: {grey:?} {white:?}");
    }

    #[test]
    fn a_scope_limits_the_list_and_a_preference_moves_it() {
        let mut s = page();
        let rect = json!([10, 40, 580, 120]);
        let doc = s.execute(ID, json!({"rect": rect, "scope": "document"})).unwrap();
        let names = |r: &Value| -> Vec<String> { r["options"].as_array().unwrap().iter().map(|o| o["family"].as_str().unwrap().to_string()).collect() };
        assert_eq!(names(&doc), vec!["Inter".to_string()], "the document only uses Inter");
        let only = s.execute(ID, json!({"rect": rect, "families": ["JetBrains Mono"]})).unwrap();
        assert_eq!(names(&only), vec!["JetBrains Mono".to_string()]);
        assert!(s.execute(ID, json!({"rect": rect, "scope": "elsewhere"})).is_err());
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
