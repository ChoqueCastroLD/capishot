//! `type.createInBubble`: type in the speech bubble under a point (the Bubble Type tool). The
//! bubble is found on the page without its type layers (`photocraft_typer::bubble_find`), its
//! body (the interior without the tail) traced and fitted with Béziers, and a type-in-path layer
//! made in it: centred both ways, with the margin (`inset`) given or automatic.

use photocraft_doc::text::{InPathAlign, TextShape};
use photocraft_doc::vector::Subpath;
use photocraft_doc::Affine;
use photocraft_typer::bubble_find::{self, Miss};
use serde_json::{Value, json};

use crate::commands::CommandSpec;
use crate::{EngineError, Result, Session};

const CMD: &str = "type.createInBubble";

/// How far (px) the fitted outline may stray from the traced body.
const FIT_TOLERANCE: f64 = 2.0;
/// Points on each side averaged to smooth the traced outline's pixel steps.
const SMOOTH: usize = 3;

fn bad(msg: impl Into<String>) -> EngineError {
    EngineError::BadParams { cmd: CMD.into(), msg: msg.into() }
}

/// The message for a point with no bubble (the UI shows "No bubble found here").
pub fn no_bubble(miss: Miss) -> EngineError {
    let why = match miss {
        Miss::Dark => "the point isn't on a light area",
        Miss::Edge => "the light area reaches the page edge",
        Miss::TooBig => "the light area is too large to be a bubble",
        Miss::TooSmall => "the area is too small for a line of text",
        Miss::NotBubble => "the area has no bubble outline",
    };
    EngineError::Other(format!("noBubble ({}): no bubble found here: {why}", miss.code()))
}

/// The body of the bubble at `(x, y)` as a closed path, document pixels.
pub fn bubble_path(s: &mut Session, x: i32, y: i32) -> Result<Subpath> {
    let page = crate::typer_place_cmds::page_image(s)?;
    let found = bubble_find::find(&page.img, page.area, (x, y)).map_err(no_bubble)?;
    let m = &found.body;
    let values: Vec<f32> = m.px.iter().map(|p| if *p { 1.0 } else { 0.0 }).collect();
    let rect = photocraft_geom::Rect::new(m.x0, m.y0, m.x0 + m.width as i32, m.y0 + m.height as i32);
    // The outer loop (the body has no holes; a stray piece would be smaller).
    let loops = photocraft_vector::trace::contours(&values, rect, 0.0);
    let area = |lp: &Vec<(f64, f64)>| (0..lp.len()).map(|i| lp[i].0 * lp[(i + 1) % lp.len()].1 - lp[(i + 1) % lp.len()].0 * lp[i].1).sum::<f64>().abs();
    let lp = loops.into_iter().max_by(|a, b| area(a).total_cmp(&area(b))).ok_or_else(|| no_bubble(Miss::TooSmall))?;
    // The pixel staircase smoothed (a circular moving average), then fitted with Béziers.
    let n = lp.len();
    let k = SMOOTH.min(n / 4);
    let smooth: Vec<[f64; 2]> = (0..n)
        .map(|i| {
            let (mut x, mut y) = (0.0, 0.0);
            for d in 0..=2 * k {
                let q = lp[(i + n + d - k) % n];
                (x, y) = (x + q.0, y + q.1);
            }
            let c = (2 * k + 1) as f64;
            [x / c, y / c]
        })
        .collect();
    let sub = photocraft_vector::trace::fit_trace(&smooth, FIT_TOLERANCE, true).ok_or_else(|| no_bubble(Miss::TooSmall))?;
    Ok(Subpath { op: Default::default(), ..sub })
}

fn create_in_bubble(s: &mut Session, p: &Value) -> Result<Value> {
    let coord = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() < 1e9).map(|v| v.floor() as i32).ok_or_else(|| bad("\"x\" and \"y\" must be document pixels"));
    let (x, y) = (coord("x")?, coord("y")?);
    let (inset, valign) = crate::type_cmds::in_path_props(p).map_err(bad)?;
    if p.get("inPath").is_some_and(|v| !v.is_null()) || p.get("box").is_some_and(|v| !v.is_null()) {
        return Err(bad("the bubble is the path: no \"inPath\" or \"box\""));
    }
    let path = bubble_path(s, x, y)?;
    // The layer's style from the usual keys (point text at the click), then the bubble's shape.
    let mut q = p.clone();
    if let Value::Object(m) = &mut q {
        m.remove("inset");
        m.remove("vertical");
        m.entry("align").or_insert(json!("center"));
    }
    let mut t = crate::type_cmds::new_text_layer(s, &q, CMD)?;
    t.shape = TextShape::InPath { path: path.clone(), inset: inset.flatten(), valign: valign.unwrap_or(InPathAlign::Center) };
    t.transform = Affine::IDENTITY;
    t.sync_summary();
    let mut out = crate::type_cmds::add_type_layer(s, &q, t)?;
    out["path"] = crate::vector_cmds::path_json(&photocraft_doc::vector::Path::new(vec![path]));
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "New Type in Bubble",
        menu: &[],
        shortcut: None,
        params: r##"{"x":px,"y":px (a point inside a speech bubble),"inset":px|"auto"="auto" (the margin: glyphs stay this far inside the bubble; auto: 5% of its smaller side),"vertical":"center|top"="center","text":str?,"align":"center"…, …character keys} → {"layer","bounds","path" (the bubble's body, document px)}. The bubble is the light area around the point, found on the page without its type layers: its outline may have small gaps, its tail is left out. No bubble (an area reaching the page edge, too large or small, art without an outline) is an error starting "noBubble" and creates nothing"##,
        enabled: |s| s.active().map(|_| ()).ok_or_else(|| "no document".into()),
        journal: true,
        run: create_in_bubble,
    }]
}

#[cfg(test)]
mod tests {
    use photocraft_doc::{LayerContent, LayerId};
    use serde_json::json;

    use super::*;

    fn page() -> Session {
        let mut s = Session::new();
        s.execute("file.new", json!({"width": 600, "height": 400, "background": "white"})).unwrap();
        s
    }

    fn outlined(s: &mut Session, kind: &str, rect: [i32; 4], extra: Value) {
        let mut p = json!({"kind": kind, "rect": rect, "fill": "#ffffff", "stroke": {"width": 3, "color": "#000000"}});
        if let (Value::Object(m), Value::Object(e)) = (&mut p, extra) {
            m.extend(e);
        }
        s.execute("shape.create", p).unwrap();
    }

    fn shape_of(s: &Session, id: u64) -> TextShape {
        match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
            LayerContent::Text(t) => t.shape.clone(),
            _ => panic!("not a type layer"),
        }
    }

    /// The path's extent, sampled along its curves.
    fn knots_box(path: &Value) -> [f64; 4] {
        let ks = path["subpaths"][0]["knots"].as_array().unwrap();
        let pt = |v: &Value| (v[0].as_f64().unwrap(), v[1].as_f64().unwrap());
        let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
        for i in 0..ks.len() {
            let (a, c) = (&ks[i], &ks[(i + 1) % ks.len()]);
            let (p0, p1, p2, p3) = (pt(&a["anchor"]), pt(&a["out"]), pt(&c["in"]), pt(&c["anchor"]));
            for s in 0..=16 {
                let t = f64::from(s) / 16.0;
                let u = 1.0 - t;
                let f = |a: f64, b: f64, c: f64, d: f64| u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d;
                let (x, y) = (f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1));
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
        b
    }

    #[test]
    fn a_click_in_an_oval_makes_centred_type_in_its_shape() {
        let mut s = page();
        outlined(&mut s, "ellipse", [100, 80, 400, 240], json!({}));
        let steps = s.active().unwrap().history.past_len();
        let r = s.execute(CMD, json!({"x": 300, "y": 200, "text": "Hello there, this is a speech bubble.", "size": 18})).unwrap();
        assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "one undo step");
        let id = r["layer"].as_u64().unwrap();
        let TextShape::InPath { path, inset, valign } = shape_of(&s, id) else { panic!("type in a path") };
        assert_eq!((inset, valign), (None, InPathAlign::Center), "auto margin, centred");
        assert!(path.closed && path.knots.len() >= 4 && path.knots.len() <= 40, "a smooth path: {} knots", path.knots.len());
        let b = knots_box(&r["path"]);
        assert!((b[0] - 103.0).abs() < 4.0 && (b[2] - 497.0).abs() < 4.0 && (b[1] - 83.0).abs() < 4.0 && (b[3] - 317.0).abs() < 4.0, "{b:?}");
        let info = s.execute("type.info", json!({"layer": id})).unwrap();
        assert_eq!(info["paragraphs"][0]["style"]["align"], "Center");
        let ink = info["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        let (cx, cy) = ((ink[0] + ink[2]) / 2.0, (ink[1] + ink[3]) / 2.0);
        assert!((cx - 300.0).abs() < 25.0 && (cy - 200.0).abs() < 25.0, "centred: {ink:?}");
        // The margin re-wraps it.
        let lines = |s: &mut Session| s.execute("type.info", json!({"layer": id})).unwrap()["lines"].as_array().unwrap().len();
        let before = lines(&mut s);
        s.execute("type.edit", json!({"layer": id, "inset": 60})).unwrap();
        assert!(lines(&mut s) > before, "a wider margin, more lines");
        // Undo twice: no layer.
        s.execute("edit.undo", json!({})).unwrap();
        s.execute("edit.undo", json!({})).unwrap();
        assert!(s.active().unwrap().doc.layer(LayerId(id)).is_none());
    }

    #[test]
    fn a_tailed_bubble_keeps_the_text_out_of_the_tail() {
        let mut s = page();
        let body = json!({"subpaths": [{"closed": true, "knots": [
            {"anchor": [300, 60], "in": [200, 60], "out": [400, 60], "smooth": true},
            {"anchor": [460, 160], "in": [460, 100], "out": [460, 220], "smooth": true},
            {"anchor": [360, 258], "in": [420, 252], "out": [360, 258]},
            [380, 360],
            {"anchor": [300, 260], "in": [300, 260], "out": [200, 260]},
            {"anchor": [140, 160], "in": [140, 220], "out": [140, 100], "smooth": true}]}]});
        s.execute("shape.create", json!({"kind": "path", "path": body, "fill": "#ffffff", "stroke": {"width": 3, "color": "#000000"}})).unwrap();
        let r = s.execute(CMD, json!({"x": 300, "y": 160, "text": "Tail", "inset": 8})).unwrap();
        let b = knots_box(&r["path"]);
        assert!(b[3] < 300.0, "the tail (down to y 360) is left out: {b:?}");
        assert!(b[0] < 150.0 && b[2] > 450.0, "{b:?}");
        let TextShape::InPath { inset, .. } = shape_of(&s, r["layer"].as_u64().unwrap()) else { panic!() };
        assert_eq!(inset, Some(8.0));
    }

    #[test]
    fn a_gap_in_the_outline_and_a_spiky_bubble() {
        let mut s = page();
        outlined(&mut s, "ellipse", [40, 80, 240, 200], json!({}));
        // A 3 px gap in the oval's left side.
        s.execute("shape.create", json!({"kind": "rect", "rect": [36, 178, 10, 3], "fill": "#ffffff"})).unwrap();
        assert!(s.execute(CMD, json!({"x": 160, "y": 180, "text": "Gap"})).is_ok(), "still a bubble");
        outlined(&mut s, "star", [320, 60, 260, 280], json!({"sides": 14, "starRatio": 0.75}));
        let r = s.execute(CMD, json!({"x": 450, "y": 200, "text": "BOOM!"})).unwrap();
        let b = knots_box(&r["path"]);
        assert!(b[2] - b[0] > 120.0 && b[3] - b[1] > 120.0, "{b:?}");
    }

    #[test]
    fn art_and_the_page_are_no_bubble() {
        let mut s = page();
        outlined(&mut s, "ellipse", [100, 80, 400, 240], json!({}));
        let layers = |s: &Session| s.active().unwrap().doc.walk().len();
        let (n, steps) = (layers(&s), s.active().unwrap().history.past_len());
        let e = s.execute(CMD, json!({"x": 20, "y": 20})).unwrap_err().to_string();
        assert!(e.contains("noBubble") && e.contains("no bubble found here"), "{e}");
        // Mid-grey art with a light patch and no outline (text over art).
        let mut art = Session::new();
        art.execute("file.new", json!({"width": 400, "height": 300, "background": "#b4b4b4"})).unwrap();
        art.execute("shape.create", json!({"kind": "ellipse", "rect": [120, 90, 160, 120], "fill": "#fafafa"})).unwrap();
        assert!(art.execute(CMD, json!({"x": 200, "y": 150})).unwrap_err().to_string().contains("noBubble"));
        assert_eq!((layers(&s), s.active().unwrap().history.past_len()), (n, steps), "nothing created");
        assert!(s.execute(CMD, json!({"x": "a", "y": 2})).is_err());
    }

    /// Real pages (never committed): `CAPISHOT_BUBBLE_PAGES=<folder>` holding `<name>.png` (or any
    /// image Capishot opens) with `<name>.txt`, one `x y` click inside a bubble per line.
    #[test]
    fn real_pages_from_the_environment() {
        let Ok(dir) = std::env::var("CAPISHOT_BUBBLE_PAGES") else { return };
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("the pages folder") {
            let path = entry.expect("an entry").path();
            let Ok(clicks) = std::fs::read_to_string(path.with_extension("txt")) else { continue };
            if path.extension().is_some_and(|e| e == "txt") {
                continue;
            }
            let bytes = std::fs::read(&path).expect("the page");
            let mut s = Session::new();
            s.add_document(photocraft_io::import(&path.to_string_lossy(), &bytes).expect("an image").document, None);
            for line in clicks.lines().filter(|l| !l.trim().is_empty()) {
                let v: Vec<f64> = line.split_whitespace().filter_map(|n| n.parse().ok()).collect();
                let r = s.execute(CMD, json!({"x": v[0], "y": v[1], "text": "Test"}));
                assert!(r.is_ok(), "{}: {line}: {:?}", path.display(), r.err());
                checked += 1;
            }
        }
        eprintln!("{checked} bubbles on real pages");
    }
}
