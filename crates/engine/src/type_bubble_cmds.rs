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
    traced(&found.body)
}

/// A mask's outline, smoothed and fitted with Béziers into a closed path.
fn traced(m: &photocraft_typer::bubble::Mask) -> Result<Subpath> {
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

/// What a click makes: type in a bubble, in a narration box, or floating point text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Bubble,
    Narration,
    Floating,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Bubble => "bubble",
            Kind::Narration => "narration",
            Kind::Floating => "floating",
        }
    }

    fn parse(v: Option<&Value>) -> Result<Option<Kind>> {
        match v.and_then(Value::as_str) {
            None if v.is_none_or(Value::is_null) => Ok(None),
            Some("auto") => Ok(None),
            Some("bubble") => Ok(Some(Kind::Bubble)),
            Some("narration") => Ok(Some(Kind::Narration)),
            Some("floating") => Ok(Some(Kind::Floating)),
            _ => Err(bad("\"style\" must be \"auto\", \"bubble\", \"narration\" or \"floating\"")),
        }
    }
}

/// The keys of one style (`styles.<kind>`) over the command's own: character keys, "align",
/// "inset", "vertical" and "stroke".
fn style_params(p: &Value, kind: Kind) -> Result<Value> {
    let mut q = p.clone();
    if let Value::Object(m) = &mut q {
        for k in ["x", "y", "style", "styles"] {
            m.remove(k);
        }
        match p.get("styles").and_then(|st| st.get(kind.name())) {
            None | Some(Value::Null) => {}
            Some(Value::Object(o)) => m.extend(o.clone()),
            Some(_) => return Err(bad(format!("\"styles.{}\" must be an object", kind.name()))),
        }
        m.entry("align").or_insert(json!(if kind == Kind::Narration { "left" } else { "center" }));
    }
    Ok(q)
}

/// A style's `"stroke"`: `{"width": px, "color": "#rrggbb"}` (an outside layer-style stroke), or
/// `null`/`false`/absent for none.
fn stroke_param(q: &Value) -> Result<Option<photocraft_typer::psjson::Stroke>> {
    let v = match q.get("stroke") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => return Ok(None),
        Some(v) => v,
    };
    let width = v.get("width").and_then(Value::as_f64).filter(|w| w.is_finite() && (0.0..=250.0).contains(w)).ok_or_else(|| bad("\"stroke.width\" must be 0..=250 px"))?;
    let hex = v.get("color").and_then(Value::as_str).unwrap_or("#ffffff");
    let c = hex.strip_prefix('#').filter(|h| h.len() == 6).and_then(|h| u32::from_str_radix(h, 16).ok()).ok_or_else(|| bad("\"stroke.color\" must be \"#rrggbb\""))?;
    if width <= 0.0 {
        return Ok(None);
    }
    Ok(Some(photocraft_typer::psjson::Stroke { size: width, opacity: 100.0, color: [(c >> 16) as u8, (c >> 8) as u8, c as u8] }))
}

/// The narration box's path: the minimum-area rectangle around the box's inside.
fn box_path(mask: &photocraft_typer::bubble::Mask) -> Result<Subpath> {
    let (_, corners) = bubble_find::rectangularity(mask).ok_or_else(|| no_bubble(Miss::TooSmall))?;
    let knots = corners.iter().map(|&(x, y)| photocraft_doc::vector::Knot::corner(x, y)).collect();
    Ok(Subpath { closed: true, knots, op: Default::default() })
}

/// The bubble's path and kind at `(x, y)`: a box (rectangularity above
/// [`bubble_find::BOX_RECTANGULARITY`]) is narration, anything else a bubble. `want` forces one.
fn region_at(s: &mut Session, x: i32, y: i32, want: Option<Kind>) -> Result<(Kind, Subpath, f64)> {
    let page = crate::typer_place_cmds::page_image(s)?;
    let found = bubble_find::find(&page.img, page.area, (x, y)).map_err(no_bubble)?;
    let rect = bubble_find::rectangularity(&found.interior).map_or(0.0, |(r, _)| r);
    let kind = want.unwrap_or(if rect > bubble_find::BOX_RECTANGULARITY { Kind::Narration } else { Kind::Bubble });
    let path = if kind == Kind::Narration { box_path(&found.interior)? } else { traced(&found.body)? };
    Ok((kind, path, rect))
}

fn create_in_bubble(s: &mut Session, p: &Value) -> Result<Value> {
    let coord = |k: &str| p.get(k).and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() < 1e9).ok_or_else(|| bad("\"x\" and \"y\" must be document pixels"));
    let (fx, fy) = (coord("x")?, coord("y")?);
    let (x, y) = (fx.floor() as i32, fy.floor() as i32);
    let want = Kind::parse(p.get("style"))?;
    if p.get("inPath").is_some_and(|v| !v.is_null()) || p.get("box").is_some_and(|v| !v.is_null()) {
        return Err(bad("the bubble is the path: no \"inPath\" or \"box\""));
    }
    // Every style is checked first, so a bad one fails whatever the click hits.
    for k in [Kind::Bubble, Kind::Narration, Kind::Floating] {
        let q = style_params(p, k)?;
        crate::type_cmds::in_path_props(&q).map_err(bad)?;
        stroke_param(&q)?;
    }
    let region = match want {
        Some(Kind::Floating) => None,
        _ => match region_at(s, x, y, want) {
            Ok(r) => Some(r),
            // Auto: nothing to fill is floating text.
            Err(_) if want.is_none() => None,
            Err(e) => return Err(e),
        },
    };
    let kind = region.as_ref().map_or(Kind::Floating, |r| r.0);
    let q = style_params(p, kind)?;
    let stroke = stroke_param(&q)?;
    let (inset, valign) = crate::type_cmds::in_path_props(&q).map_err(bad)?;
    let mut base = q.clone();
    if let Value::Object(m) = &mut base {
        for k in ["inset", "vertical", "stroke"] {
            m.remove(k);
        }
        m.insert("x".into(), json!(fx));
        m.insert("y".into(), json!(fy));
    }
    let mut t = crate::type_cmds::new_text_layer(s, &base, CMD)?;
    match &region {
        Some((_, path, _)) => {
            t.shape = TextShape::InPath { path: path.clone(), inset: inset.flatten(), valign: valign.unwrap_or(InPathAlign::Center) };
            t.transform = Affine::IDENTITY;
        }
        None => {
            // Floating: point text whose line is centred on the click (the baseline a third of
            // the size below it).
            let dpi = s.active().map_or(72.0, |d| f64::from(d.doc.resolution_dpi));
            let px = f64::from(t.size_pt) * dpi / 72.0;
            t.transform = Affine::translate(fx, fy + px * 0.35);
        }
    }
    t.sync_summary();
    let mut out = crate::type_cmds::add_type_layer_with(s, &base, t, |layer, mode| crate::typer_place_cmds::set_stroke(layer, stroke.as_ref(), mode))?;
    out["style"] = json!(kind.name());
    if let Some((_, path, rect)) = region {
        out["path"] = crate::vector_cmds::path_json(&photocraft_doc::vector::Path::new(vec![path]));
        out["rectangularity"] = json!((rect * 1000.0).round() / 1000.0);
    }
    Ok(out)
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: "New Type in Bubble",
        menu: &[],
        shortcut: None,
        params: r##"{"x":px,"y":px (the click),"style":"auto|bubble|narration|floating"="auto","styles":{"bubble":{…},"narration":{…},"floating":{…}}? (each: character keys, "align", "inset":px|"auto", "vertical":"center|top", "stroke":{"width":px,"color":"#rrggbb"}|null; over the command's own keys),"text":str?, …character keys} → {"layer","bounds","style" (what was made),"path"? (the bubble's or box's path, document px),"rectangularity"?}. Auto: the light area around the point (found on the page without its type layers, outline gaps closed, a click on lettering or the outline still finds it) becomes type in a narration box when it is rectangular (area over its minimum rectangle above 0.92: a rectangle fit), else type in the bubble's traced body (the tail left out); with no bubble there, floating point text centred on the point. A forced "bubble" or "narration" with no bubble there is an error starting "noBubble" and creates nothing. One undo step"##,
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
        assert_eq!(r["style"], "bubble");
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
        // A forced bubble or box with none there: an error, nothing made.
        for style in ["bubble", "narration"] {
            let e = s.execute(CMD, json!({"x": 20, "y": 20, "style": style})).unwrap_err().to_string();
            assert!(e.contains("noBubble") && e.contains("no bubble found here"), "{e}");
        }
        assert_eq!((layers(&s), s.active().unwrap().history.past_len()), (n, steps), "nothing created");
        // Mid-grey art with a light patch and no outline (text over art).
        let mut art = Session::new();
        art.execute("file.new", json!({"width": 400, "height": 300, "background": "#b4b4b4"})).unwrap();
        art.execute("shape.create", json!({"kind": "ellipse", "rect": [120, 90, 160, 120], "fill": "#fafafa"})).unwrap();
        assert!(art.execute(CMD, json!({"x": 200, "y": 150, "style": "bubble"})).unwrap_err().to_string().contains("noBubble"));
        // Auto there: floating point text centred on the click, in the floating style.
        let r = art
            .execute(CMD, json!({"x": 200, "y": 150, "text": "Fwoosh", "styles": {"floating": {"size": 36, "stroke": {"width": 4, "color": "#ffffff"}}}}))
            .unwrap();
        assert_eq!(r["style"], "floating");
        assert!(r.get("path").is_none());
        let id = LayerId(r["layer"].as_u64().unwrap());
        let doc = &art.active().unwrap().doc;
        let l = doc.layer(id).unwrap();
        let LayerContent::Text(t) = &l.content else { panic!() };
        assert!(matches!(t.shape, TextShape::Point));
        assert!(l.effects.items.iter().any(|e| matches!(e, photocraft_doc::Effect::Stroke(_))), "the white stroke");
        let b = r["bounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!(((b[0] + b[2]) / 2.0 - 200.0).abs() < 12.0 && ((b[1] + b[3]) / 2.0 - 150.0).abs() < 14.0, "centred on the click: {b:?}");
        assert!(s.execute(CMD, json!({"x": "a", "y": 2})).is_err());
        assert!(s.execute(CMD, json!({"x": 1, "y": 2, "style": "huge"})).is_err());
        assert!(s.execute(CMD, json!({"x": 1, "y": 2, "styles": {"floating": {"stroke": {"width": -1}}}})).is_err());
    }

    /// The styles' line spacing applies: a set leading spaces the bubble's lines that far apart,
    /// Auto 120% of the size (across a paragraph break too).
    #[test]
    fn bubble_styles_line_spacing_applies() {
        let mut s = page();
        outlined(&mut s, "ellipse", [40, 40, 320, 240], json!({}));
        let gaps = |s: &Session, id: u64| {
            let st = s.active().unwrap();
            let LayerContent::Text(t) = &st.doc.layer(LayerId(id)).unwrap().content else { panic!("not a type layer") };
            let l = photocraft_text::shared().lock().unwrap().layout(t, st.doc.resolution_dpi);
            l.lines.windows(2).map(|w| w[1].baseline - w[0].baseline).collect::<Vec<_>>()
        };
        for (leading, want) in [(json!(40), 40.0f32), (json!("auto"), 24.0)] {
            let styles = json!({"bubble": {"size": 20, "leading": leading}});
            let r = s.execute(CMD, json!({"x": 200, "y": 160, "text": "One line\nAnother", "styles": styles})).unwrap();
            assert_eq!(r["style"], "bubble");
            let g = gaps(&s, r["layer"].as_u64().unwrap());
            assert!(!g.is_empty() && g.iter().all(|v| (v - want).abs() < 0.01), "{leading}: {g:?}");
            s.undo();
        }
    }

    /// A box is narration: type in its rectangle, in the narration style (left, from the top);
    /// a rounded box with small corners too; an oval and a spiky bubble are bubbles.
    #[test]
    fn boxes_are_narration_and_bubbles_are_bubbles() {
        let mut s = page();
        outlined(&mut s, "rect", [40, 40, 240, 120], json!({}));
        outlined(&mut s, "rect", [320, 40, 240, 120], json!({"radius": 10}));
        outlined(&mut s, "ellipse", [40, 200, 240, 160], json!({}));
        outlined(&mut s, "star", [330, 190, 230, 200], json!({"sides": 14, "starRatio": 0.75}));
        let styles = json!({"narration": {"align": "left", "vertical": "top", "caps": "all", "inset": 6}, "bubble": {"size": 14}});
        let make = |s: &mut Session, x: i32, y: i32| s.execute(CMD, json!({"x": x, "y": y, "text": "Some words here", "size": 12, "styles": styles.clone()})).unwrap();
        let boxed = make(&mut s, 160, 100);
        assert_eq!(boxed["style"], "narration", "{boxed}");
        let ks = boxed["path"]["subpaths"][0]["knots"].as_array().unwrap();
        assert_eq!(ks.len(), 4, "a rectangle fit");
        let b = knots_box(&boxed["path"]);
        assert!((b[0] - 43.0).abs() < 3.0 && (b[2] - 277.0).abs() < 3.0 && (b[1] - 43.0).abs() < 3.0 && (b[3] - 157.0).abs() < 3.0, "{b:?}");
        let TextShape::InPath { inset, valign, .. } = shape_of(&s, boxed["layer"].as_u64().unwrap()) else { panic!() };
        assert_eq!((inset, valign), (Some(6.0), InPathAlign::Top));
        let info = s.execute("type.info", json!({"layer": boxed["layer"]})).unwrap();
        assert_eq!(info["paragraphs"][0]["style"]["align"], "Left");
        assert_eq!(info["runs"][0]["style"]["caps"], "AllCaps", "{}", info["runs"][0]["style"]);
        assert_eq!(make(&mut s, 440, 100)["style"], "narration", "a rounded box");
        assert_eq!(make(&mut s, 160, 280)["style"], "bubble", "an oval");
        assert_eq!(make(&mut s, 445, 290)["style"], "bubble", "a spiky bubble");
        // Forced: the oval as narration gets its rectangle.
        let forced = s.execute(CMD, json!({"x": 160, "y": 280, "style": "narration", "text": "x"})).unwrap();
        assert_eq!((forced["style"].as_str(), forced["path"]["subpaths"][0]["knots"].as_array().map(Vec::len)), (Some("narration"), Some(4)));
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
