//! Placement on synthetic manga-like pages: white ellipse bubbles with a black outline (some with
//! a tail) on a screentone background.

use photocraft_doc::text::TextShape;
use photocraft_doc::{Color, ColorMode, Document, LayerContent, LayerId, SampleType, Size};
use serde_json::{Value, json};

use super::*;

/// A bubble: centre, radii, and whether it has a tail hanging down-right.
#[derive(Clone, Copy)]
struct B {
    cx: f64,
    cy: f64,
    rx: f64,
    ry: f64,
    tail: bool,
}

fn inside(b: &B, x: f64, y: f64, grow: f64) -> bool {
    let (dx, dy) = ((x - b.cx) / (b.rx + grow), (y - b.cy) / (b.ry + grow));
    if dx * dx + dy * dy <= 1.0 {
        return true;
    }
    if b.tail {
        // From inside the ellipse's bottom edge, 0.6 ry long, leaning right.
        let len = 0.6 * b.ry;
        let t = (y - (b.cy + b.ry - 8.0)) / len;
        if (0.0..=1.0).contains(&t) {
            let half = 0.08 * b.rx * (1.0 - t) + grow;
            let tx = b.cx + 0.35 * b.rx + 0.3 * b.rx * t;
            return (x - tx).abs() <= half;
        }
    }
    false
}

/// RGB page `w × h` with a screentone background and the bubbles, as the bottom layer.
fn page(w: u32, h: u32, bubbles: &[B]) -> Document {
    let mut doc = Document::with_background("page", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let mut bytes = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
            let (mx, my) = (f64::from(x % 6) - 2.5, f64::from(y % 6) - 2.5);
            let mut v = if mx * mx + my * my < 4.0 { 70u8 } else { 225u8 };
            for b in bubbles {
                if inside(b, fx, fy, 0.0) {
                    v = 255;
                } else if inside(b, fx, fy, 4.0) {
                    v = 0;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            bytes[i..i + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let fmt = doc.pixel_format();
    let surf = photocraft_raster::Surface::from_interleaved(fmt, photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &bytes);
    doc.layers[0].content = LayerContent::Raster(surf);
    doc
}

const B1: B = B { cx: 200.0, cy: 160.0, rx: 140.0, ry: 90.0, tail: true };
const B2: B = B { cx: 560.0, cy: 200.0, rx: 110.0, ry: 120.0, tail: false };
const B3: B = B { cx: 330.0, cy: 470.0, rx: 160.0, ry: 70.0, tail: true };

const SCRIPT: &str = "Page 1\nWhere are we going?\nSFX: BOOM\nThis is a much longer line of dialogue that has to wrap over several lines.\nLast one.";

fn library() -> Value {
    json!({
        "styles": [
            {"id": "reg", "name": "Regular", "prefixes": [],
             "textProps": {"typeUnit": "pixelsUnit", "layerText": {"antiAlias": "antiAliasSmooth",
                "textStyleRange": [{"textStyle": {"fontName": "Inter", "fontStyleName": "Regular", "size": 20, "autoLeading": true}}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "center"}}]}}},
            {"id": "sfx", "name": "SFX", "prefixes": ["SFX:"],
             "textProps": {"typeUnit": "pixelsUnit", "layerText": {"textStyleRange": [{"textStyle": {"fontName": "Inter", "fontStyleName": "Bold", "size": 30, "autoLeading": false, "leading": 34}}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "center"}}]}},
             "stroke": {"enabled": true, "size": 3, "opacity": 100, "position": "outer", "color": {"r": 255, "g": 255, "b": 255}}}
        ],
        "includesSettings": true,
        "defaultStyleId": "reg"
    })
}

fn session(bubbles: &[B]) -> Session {
    let mut s = Session::new();
    s.add_document(page(760, 600, bubbles), None);
    s.execute("typer.library.import", json!({"json": library(), "now": 1})).unwrap();
    s.execute("typer.script.set", json!({"text": SCRIPT})).unwrap();
    s
}

fn steps(s: &Session) -> usize {
    s.active().unwrap().history.past_len()
}

fn text_layer(s: &Session, id: u64) -> photocraft_doc::TextLayer {
    match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
        LayerContent::Text(t) => t.clone(),
        _ => panic!("layer {id} is not a type layer"),
    }
}

fn ink_center(s: &Session, id: u64) -> (f64, f64) {
    support::ink(&text_layer(s, id)).unwrap().center()
}

fn near(a: (f64, f64), b: (f64, f64), tol: f64) -> bool {
    (a.0 - b.0).abs() <= tol && (a.1 - b.1).abs() <= tol
}

fn wand(s: &mut Session, x: f64, y: f64) {
    s.execute("select.magicWand", json!({"x": x, "y": y, "tolerance": 20, "sampleAllLayers": true})).unwrap();
}

#[test]
fn paste_centres_on_the_bubble_body_not_its_tail() {
    let mut s = session(&[B1, B2]);
    wand(&mut s, B1.cx, B1.cy);
    let before = steps(&s);
    let r = s.execute("typer.paste", json!({})).unwrap();
    assert_eq!(steps(&s), before + 1, "one history step");
    assert_eq!(s.active().unwrap().history.undo_label(), Some("TypeR Paste"));
    let id = r["layer"].as_u64().unwrap();
    let c = ink_center(&s, id);
    assert!(near(c, (B1.cx, B1.cy), 1.0), "ink centre {c:?} vs bubble {:?}", (B1.cx, B1.cy));
    // The raw selection box (with the tail) has its centre well below the body's.
    let bbox = s.execute("typer.selectionBounds", json!({})).unwrap();
    let raw_cy = bbox["bbox"][1].as_f64().unwrap() + bbox["bbox"][3].as_f64().unwrap() / 2.0;
    assert!(raw_cy - B1.cy > 15.0, "the tail moves the raw centre: {raw_cy}");
    // Paste leaves the selection; the cursor advanced and recorded the style.
    assert!(s.active().unwrap().doc.selection.is_some());
    assert_eq!(r["state"]["text"], "BOOM");
    assert_eq!(r["advanced"], true);
    assert!(s.typer.state.used_line_styles.contains_key(&1));
    // Box: 0.9 × opened width − 2 × padding wide, refit to extent + size + 2 tall.
    let t = text_layer(&s, id);
    let opened_w = bbox["opened"][2].as_f64().unwrap();
    let TextShape::Box { width, height, .. } = t.shape else { panic!("box text expected") };
    assert!((f64::from(width) - (opened_w * 0.9 - 20.0)).abs() < 0.01, "{width} vs {opened_w}");
    let ink = support::ink(&t).unwrap();
    assert!((f64::from(height) - (ink.height + 20.0 + 2.0)).abs() <= 1.0, "{height} vs ink {}", ink.height);
    assert_eq!(t.runs[0].style.size_pt, 20.0, "20 px at 72 dpi");
    // Undo removes the layer in one step.
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.active().unwrap().doc.layer(LayerId(id)).is_none());
}

#[test]
fn paste_styles_point_text_and_strokes() {
    let mut s = session(&[B1, B2]);
    s.execute("typer.line.next", json!({})).unwrap();
    wand(&mut s, B2.cx, B2.cy);
    let r = s.execute("typer.paste", json!({"point": true})).unwrap();
    let id = r["layer"].as_u64().unwrap();
    let t = text_layer(&s, id);
    assert!(matches!(t.shape, TextShape::Point), "point text keeps its kind");
    assert_eq!(t.text, "BOOM", "the tag is not pasted");
    assert_eq!(t.runs[0].style.size_pt, 30.0);
    assert_eq!(t.runs[0].style.leading_pt, Some(34.0));
    assert!(near(ink_center(&s, id), (B2.cx, B2.cy), 1.0));
    let l = s.active().unwrap().doc.layer(LayerId(id)).unwrap();
    assert!(matches!(l.effects.items.first(), Some(Effect::Stroke(st)) if st.size == 3.0), "the style's stroke");
}

#[test]
fn rectangle_selections_centre_exactly_and_tiny_ones_are_refused() {
    let mut s = session(&[B1]);
    s.execute("select.rect", json!({"x": 400, "y": 300, "width": 200, "height": 120})).unwrap();
    let r = s.execute("typer.paste", json!({"text": "Hello there"})).unwrap();
    assert!(near(ink_center(&s, r["layer"].as_u64().unwrap()), (500.0, 360.0), 0.5 + 1e-9));
    assert_eq!(r["advanced"], false, "explicit text doesn't move the cursor");
    s.execute("select.rect", json!({"x": 10, "y": 10, "width": 12, "height": 12})).unwrap();
    let before = steps(&s);
    let e = s.execute("typer.paste", json!({})).unwrap_err().to_string();
    assert!(e.contains("smallSelection"), "{e}");
    assert_eq!(steps(&s), before);
    s.execute("select.deselect", json!({})).unwrap();
    let e = s.execute("typer.paste", json!({})).unwrap_err().to_string();
    assert!(e.contains("noSelection"), "{e}");
    // `bounds` stands in for the selection.
    let r = s.execute("typer.paste", json!({"text": "Hi", "bounds": [100, 100, 80, 60]})).unwrap();
    assert!(near(ink_center(&s, r["layer"].as_u64().unwrap()), (140.0, 130.0), 1.0));
}

#[test]
fn text_scale_dpi_and_keep_size() {
    let mut s = session(&[B1, B2]);
    // 300 dpi page: 20 px = 4.8 pt.
    s.active_mut().unwrap().doc = std::sync::Arc::new({
        let mut d = (*s.active().unwrap().doc).clone();
        d.resolution_dpi = 300.0;
        d
    });
    s.execute("typer.settings.set", json!({"key": "textScale", "value": 150})).unwrap();
    wand(&mut s, B1.cx, B1.cy);
    let r = s.execute("typer.paste", json!({})).unwrap();
    let a = r["layer"].as_u64().unwrap();
    assert!((text_layer(&s, a).runs[0].style.size_pt - 20.0 * 1.5 * 72.0 / 300.0).abs() < 1e-3);
    s.execute("typer.settings.set", json!({"key": "textScale", "value": null})).unwrap();
    // Keep size: the active type layer's size wins over the style's.
    s.execute("type.edit", json!({"layer": a, "runs": [{"start": 0, "size": 9.5}]})).unwrap();
    wand(&mut s, B2.cx, B2.cy);
    let r = s.execute("typer.paste", json!({"keepSize": true})).unwrap();
    assert_eq!(text_layer(&s, r["layer"].as_u64().unwrap()).runs[0].style.size_pt, 9.5);
    // Without a type layer to take the size from: sizeSource.
    let bg = s.active().unwrap().doc.layers[0].id;
    s.select_layer(bg).unwrap();
    let e = s.execute("typer.paste", json!({"keepSize": true})).unwrap_err().to_string();
    assert!(e.contains("sizeSource"), "{e}");
}

fn new_type(s: &mut Session, text: &str, x: f64, y: f64, box_w: Option<f64>) -> u64 {
    let p = match box_w {
        Some(w) => json!({"text": text, "box": [x, y, w, 200], "size": 18, "align": "center"}),
        None => json!({"text": text, "x": x, "y": y, "size": 18}),
    };
    s.execute("type.create", p).unwrap()["layer"].as_u64().unwrap()
}

#[test]
fn align_finds_the_bubble_left_of_the_text() {
    let mut s = session(&[B1, B2, B3]);
    // Off-centre text inside each bubble: point, box and box.
    let a = new_type(&mut s, "Off centre", B1.cx - 70.0, B1.cy - 30.0, None);
    let b = new_type(&mut s, "Box text here", B2.cx - 60.0, B2.cy + 10.0, Some(120.0));
    let c = new_type(&mut s, "Wide", B3.cx - 20.0, B3.cy - 40.0, Some(90.0));
    let before = steps(&s);
    let ids = [a, b, c];
    let r = s.execute("typer.align", json!({"layers": ids})).unwrap();
    assert_eq!(steps(&s), before + 1, "one step for all layers");
    assert_eq!(s.active().unwrap().history.undo_label(), Some("TypeR Align"));
    assert_eq!(r["errors"], json!([]));
    for (id, bub) in ids.iter().zip([B1, B2, B3]) {
        let c = ink_center(&s, *id);
        assert!(near(c, (bub.cx, bub.cy), 1.0), "layer {id}: {c:?} vs {:?}", (bub.cx, bub.cy));
    }
    assert!(matches!(text_layer(&s, a).shape, TextShape::Point), "point text stays point");
    // With a selection, the first layer goes into it and the selection is dropped.
    s.execute("select.rect", json!({"x": 300, "y": 300, "width": 100, "height": 100})).unwrap();
    s.execute("typer.align", json!({"layers": [a]})).unwrap();
    assert!(near(ink_center(&s, a), (350.0, 350.0), 0.5 + 1e-9));
    assert!(s.active().unwrap().doc.selection.is_none(), "align deselects");
    // Text over the screentone: the wand finds the whole background, far bigger than the text.
    let d = new_type(&mut s, "SFX", 20.0, 590.0, None);
    let e = s.execute("typer.align", json!({"layers": [d]})).unwrap_err().to_string();
    assert!(e.contains("noBubble"), "{e}");
}

#[test]
fn align_can_resize_the_box_to_the_bubble() {
    let mut s = session(&[B2]);
    let b = new_type(&mut s, "Some words to fit", B2.cx - 50.0, B2.cy, Some(60.0));
    s.execute("typer.align", json!({"layers": [b], "resizeBox": true, "padding": 0})).unwrap();
    let t = text_layer(&s, b);
    let TextShape::Box { width, .. } = t.shape else { panic!() };
    let hit = s.execute("typer.detectBubble", json!({"layer": b})).unwrap();
    assert!((f64::from(width) - 0.9 * hit["opened"][2].as_f64().unwrap()).abs() < 0.01);
    assert!(near(ink_center(&s, b), (B2.cx, B2.cy), 1.0));
}

#[test]
fn apply_keeps_the_centre_and_refits_the_box() {
    let mut s = session(&[B1]);
    let a = new_type(&mut s, "x", 120.0, 100.0, Some(160.0));
    let c0 = ink_center(&s, a);
    s.execute("typer.line.set", json!({"index": 3})).unwrap();
    let before = steps(&s);
    let r = s.execute("typer.apply", json!({})).unwrap();
    assert_eq!(steps(&s), before + 1);
    assert_eq!(s.active().unwrap().history.undo_label(), Some("TypeR Change"));
    let t = text_layer(&s, a);
    assert!(t.text.starts_with("This is a much longer"));
    let TextShape::Box { width, height, .. } = t.shape else { panic!() };
    assert_eq!(width, 160.0, "box width kept");
    let ink = support::ink(&t).unwrap();
    assert!(ink.height > 40.0, "wrapped over several lines");
    assert!((f64::from(height) - (ink.height + 22.0)).abs() <= 1.0, "nothing clipped: {height} vs {}", ink.height);
    assert!(near(ink.center(), c0, 1.0), "{:?} vs {c0:?}", ink.center());
    assert_eq!(t.runs[0].style.size_pt, 20.0, "the line's style");
    assert_eq!(r["state"]["text"], "Last one.");
    // Text only keeps the layer's style; style only keeps the text.
    s.execute("typer.apply", json!({"mode": "textOnly", "text": "Short"})).unwrap();
    let t = text_layer(&s, a);
    assert_eq!((t.text.as_str(), t.runs[0].style.size_pt), ("Short", 20.0));
    s.execute("typer.applyStyle", json!({"style": {"id": "sfx"}})).unwrap();
    let t = text_layer(&s, a);
    assert_eq!((t.text.as_str(), t.runs[0].style.size_pt), ("Short", 30.0));
    assert!(near(ink_center(&s, a), c0, 1.0));
    // Keep size.
    s.execute("typer.apply", json!({"text": "Kept", "style": {"id": "reg"}, "keepSize": true})).unwrap();
    assert_eq!(text_layer(&s, a).runs[0].style.size_pt, 30.0);
    // Markdown becomes faux bold/italic runs.
    s.execute("typer.apply", json!({"text": "a **bold** word"})).unwrap();
    let t = text_layer(&s, a);
    assert_eq!(t.text, "a bold word");
    assert!(t.runs.iter().any(|r| r.style.faux_bold));
}

#[test]
fn apply_lines_follows_click_order_and_rolls_back() {
    let mut s = session(&[B1, B2, B3]);
    let a = new_type(&mut s, "one", 100.0, 100.0, Some(150.0));
    let b = new_type(&mut s, "two", 500.0, 200.0, Some(150.0));
    let c = new_type(&mut s, "three", 300.0, 450.0, Some(150.0));
    let bg = s.active().unwrap().doc.layers[0].id;
    let select = |s: &mut Session, ids: &[u64]| {
        let st = s.active_mut().unwrap();
        st.selected_layers = ids.iter().map(|i| LayerId(*i)).collect();
        st.active_layer = ids.last().map(|i| LayerId(*i));
    };
    // A non-type layer in the batch: nothing changes.
    select(&mut s, &[c, bg.0, a]);
    let (rev, before) = (s.active().unwrap().revision, steps(&s));
    let e = s.execute("typer.applyLines", json!({})).unwrap_err().to_string();
    assert!(e.contains("noTextLayer"), "{e}");
    assert_eq!((s.active().unwrap().revision, steps(&s)), (rev, before));
    assert_eq!(text_layer(&s, c).text, "three");
    // Click order c, a, b gets lines 1, 2, 3 (the SFX tag style for line 2).
    select(&mut s, &[c, a, b]);
    let r = s.execute("typer.applyLines", json!({})).unwrap();
    assert_eq!(steps(&s), before + 1);
    assert_eq!(text_layer(&s, c).text, "Where are we going?");
    assert_eq!(text_layer(&s, a).text, "BOOM");
    assert_eq!(text_layer(&s, a).runs[0].style.size_pt, 30.0);
    assert!(text_layer(&s, b).text.starts_with("This is"));
    assert_eq!(r["lines"], json!([1, 2, 3]));
    assert_eq!(r["state"]["text"], "Last one.", "the cursor sits after the last line used");
    assert_eq!(s.typer.state.used_line_styles.len(), 3);
    // The Paste key with 2+ type layers and no selection does the same.
    s.execute("typer.line.set", json!({"index": 1})).unwrap();
    select(&mut s, &[b, c]);
    let r = s.execute("typer.paste", json!({})).unwrap();
    assert_eq!(r["mode"], "applyLines");
    assert_eq!(text_layer(&s, b).text, "Where are we going?");
}

#[test]
fn multi_bubble_capture_and_paste_in_one_step() {
    let mut s = session(&[B1, B2, B3]);
    assert_eq!(s.execute("typer.multiBubble.toggle", json!({})).unwrap()["on"], true);
    for b in [B1, B2, B3] {
        wand(&mut s, b.cx, b.cy);
        let r = s.execute("typer.multiBubble.capture", json!({})).unwrap();
        assert_eq!(r["added"], true);
    }
    // The same bubble twice is stored once.
    assert_eq!(s.execute("typer.multiBubble.capture", json!({})).unwrap()["added"], false);
    assert_eq!(s.typer.stored_selections.len(), 3);
    assert_eq!(s.typer.state.current_line().unwrap().text, "Last one.", "capture advanced three lines");
    // Remove the last: the cursor goes back to its line.
    let r = s.execute("typer.multiBubble.removeLast", json!({})).unwrap();
    assert_eq!((r["stored"].as_u64(), r["state"]["number"].as_u64()), (Some(2), Some(3)));
    s.execute("typer.multiBubble.capture", json!({})).unwrap();
    let before = steps(&s);
    // The Paste key in multi-bubble mode fills every stored bubble.
    let r = s.execute("typer.paste", json!({})).unwrap();
    assert_eq!(r["mode"], "pasteMany");
    assert_eq!(steps(&s), before + 1, "one step for all bubbles");
    assert_eq!(s.active().unwrap().history.undo_label(), Some("TypeR Multiple Paste"));
    let layers: Vec<u64> = r["layers"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert_eq!(layers.len(), 3);
    assert!(s.typer.stored_selections.is_empty());
    assert_eq!(text_layer(&s, layers[0]).text, "Where are we going?");
    assert_eq!(text_layer(&s, layers[1]).text, "BOOM");
    assert_eq!(text_layer(&s, layers[1]).runs[0].style.size_pt, 30.0, "line tag style");
    // TypeR centres multi paste on the raw box: the tail bubble's text sits lower.
    let raw = ink_center(&s, layers[0]);
    assert!(raw.1 > B1.cy + 10.0 && (raw.0 - B1.cx).abs() < 30.0, "{raw:?}");
    assert!(near(ink_center(&s, layers[1]), (B2.cx, B2.cy), 1.0));
    // With the Capishot setting, stored bubbles are opened like Paste.
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("typer.settings.set", json!({"key": "openStoredSelections", "value": true})).unwrap();
    s.execute("typer.line.set", json!({"index": 1})).unwrap();
    for b in [B1, B3] {
        wand(&mut s, b.cx, b.cy);
        s.execute("typer.multiBubble.capture", json!({})).unwrap();
    }
    let r = s.execute("typer.pasteMany", json!({})).unwrap();
    let ids: Vec<u64> = r["layers"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap()).collect();
    assert!(near(ink_center(&s, ids[0]), (B1.cx, B1.cy), 1.0));
    assert!(near(ink_center(&s, ids[1]), (B3.cx, B3.cy), 1.0));
    // Explicit entries; empty stored list errors.
    let r = s
        .execute(
            "typer.pasteMany",
            json!({"entries": [{"text": "A", "bounds": [10, 10, 60, 40]}, {"text": "B", "style": {"id": "sfx"}, "bounds": [100, 10, 60, 40]}]}),
        )
        .unwrap();
    assert_eq!(r["layers"].as_array().unwrap().len(), 2);
    assert!(s.execute("typer.pasteMany", json!({})).unwrap_err().to_string().contains("noSelection"));
    assert_eq!(s.execute("typer.multiBubble.toggle", json!({})).unwrap()["on"], false);
    assert_eq!(s.execute("typer.multiBubble.clear", json!({})).unwrap()["stored"], 0);
}

#[test]
fn resize_follows_size_leading_and_keeps_the_centre() {
    let mut s = session(&[B1]);
    let a = new_type(&mut s, "Grow me please", 100.0, 100.0, Some(150.0));
    s.execute("type.edit", json!({"layer": a, "runs": [{"start": 0, "leading": 22}]})).unwrap();
    let c0 = ink_center(&s, a);
    let before = steps(&s);
    let r = s.execute("typer.resize", json!({"delta": 2, "layers": [a]})).unwrap();
    assert_eq!(steps(&s), before + 1);
    assert_eq!(s.active().unwrap().history.undo_label(), Some("TypeR Resize"));
    let t = text_layer(&s, a);
    assert_eq!((t.runs[0].style.size_pt, t.runs[0].style.leading_pt), (20.0, Some(24.0)), "fixed leading adds the delta");
    let TextShape::Box { width, .. } = t.shape else { panic!() };
    assert!((width - 150.0 * 20.0 / 18.0).abs() < 0.01, "the box scales with the size");
    assert!(near(ink_center(&s, a), c0, 1.0));
    assert_eq!(r["sizes"], json!([20.0]));
    assert!(s.execute("typer.resize", json!({"delta": 0})).is_err());
}

#[test]
fn apply_shape_sets_lines_and_shrinks_the_box() {
    let mut s = session(&[B1]);
    let a = new_type(&mut s, "one two three four", 100.0, 100.0, Some(300.0));
    let c0 = ink_center(&s, a);
    s.execute("typer.applyShape", json!({"layer": a, "lines": ["one two", "three four"]})).unwrap();
    let t = text_layer(&s, a);
    assert_eq!(t.text, "one two\nthree four");
    let TextShape::Box { width, .. } = t.shape else { panic!() };
    let ink = support::ink(&t).unwrap();
    assert!(f64::from(width) < 300.0 && f64::from(width) >= ink.width, "{width} vs {}", ink.width);
    assert!(near(ink.center(), c0, 1.0));
}

#[test]
fn cleaning_layers_hide_and_restore() {
    let mut s = session(&[B1]);
    let cleaning = s.execute("layer.new.layer", json!({})).unwrap();
    let id = cleaning["layer"].as_u64().map(LayerId).unwrap_or_else(|| s.active().unwrap().active_layer.unwrap());
    let t = new_type(&mut s, "text", 10.0, 30.0, None);
    let before = steps(&s);
    let r = s.execute("typer.toggleCleaningLayers", json!({})).unwrap();
    assert_eq!(r["hidden"], true);
    assert_eq!(r["layers"], json!([id.0]), "not the page, not type");
    let doc = &s.active().unwrap().doc;
    assert!(!doc.layer(id).unwrap().visible && doc.layers[0].visible && doc.layer(LayerId(t)).unwrap().visible);
    assert_eq!(steps(&s), before + 1);
    let r = s.execute("typer.toggleCleaningLayers", json!({})).unwrap();
    assert_eq!((r["hidden"].as_bool(), r["count"].as_u64()), (Some(false), Some(1)));
    assert!(s.active().unwrap().doc.layer(id).unwrap().visible);
}

#[test]
fn style_from_layer_round_trips() {
    let mut s = session(&[B1]);
    wand(&mut s, B1.cx, B1.cy);
    s.execute("typer.line.set", json!({"index": 2})).unwrap();
    let r = s.execute("typer.paste", json!({})).unwrap();
    let a = r["layer"].as_u64().unwrap();
    let st = s.execute("typer.style.fromLayer", json!({"layer": a, "name": "Copied", "now": 5})).unwrap();
    let id = st["id"].clone();
    let res = s.execute("typer.style.resolve", json!({"id": id})).unwrap();
    assert_eq!(res["sizePt"], 30.0);
    assert_eq!(res["stroke"]["size"], 3.0);
    assert_eq!(st["style"]["name"], "Copied");
    assert!(s.execute("typer.style.fromLayer", json!({"layer": s.active().unwrap().doc.layers[0].id.0})).is_err());
}

#[test]
fn detect_bubble_query_and_selection_bounds() {
    let mut s = session(&[B1, B2]);
    let before = (steps(&s), s.active().unwrap().revision);
    let r = s.execute("typer.detectBubble", json!({"x": B1.cx, "y": B1.cy})).unwrap();
    assert_eq!(r["found"], true);
    let c = (r["center"][0].as_f64().unwrap(), r["center"][1].as_f64().unwrap());
    assert!(near(c, (B1.cx, B1.cy), 1.0), "{c:?}");
    assert_eq!(r["rows"].as_array().unwrap().len(), 21);
    assert_eq!((steps(&s), s.active().unwrap().revision), before, "a query changes nothing");
    let r = s.execute("typer.detectBubble", json!({"x": 5, "y": 590, "rows": 5})).unwrap();
    assert_eq!(r["rows"].as_array().unwrap().len(), 5);
    assert!(s.execute("typer.selectionBounds", json!({})).is_err());
    wand(&mut s, B2.cx, B2.cy);
    let r = s.execute("typer.selectionBounds", json!({})).unwrap();
    assert!(r["found"].as_bool().unwrap());
}

#[test]
fn page_image_is_cached_across_placements() {
    let mut s = session(&[B1, B2]);
    let a = page_image(&mut s).unwrap();
    wand(&mut s, B1.cx, B1.cy);
    s.execute("typer.paste", json!({})).unwrap();
    let b = page_image(&mut s).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a.img, &b.img), "adding type keeps the cached page");
    // Painting the page (not type) recomposites.
    s.edit("Paint", |doc, _| {
        let surf = doc.layers[0].surface_mut().unwrap();
        surf.fill_rect(photocraft_geom::Rect::new(0, 0, 10, 10), &[0.0, 0.0, 0.0, 1.0]);
        Ok(())
    })
    .unwrap();
    let c = page_image(&mut s).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a.img, &c.img));
}

#[test]
fn bad_params_fail_cleanly() {
    let mut s = session(&[B1]);
    wand(&mut s, B1.cx, B1.cy);
    for (cmd, p) in [
        ("typer.paste", json!({"text": 5})),
        ("typer.paste", json!({"text": ""})),
        ("typer.paste", json!({"bounds": [0, 0, -1, 5]})),
        ("typer.paste", json!({"bounds": "x"})),
        ("typer.paste", json!({"style": {"id": "ghost"}})),
        ("typer.paste", json!({"style": 3})),
        ("typer.paste", json!({"direction": "up"})),
        ("typer.paste", json!({"padding": -3})),
        ("typer.paste", json!({"point": "yes"})),
        ("typer.paste", json!({"text": "x".repeat(MAX_TEXT_BYTES + 1)})),
        ("typer.pasteMany", json!({"entries": []})),
        ("typer.pasteMany", json!({"entries": [{"text": "a"}]})),
        ("typer.pasteMany", json!({"entries": [5]})),
        ("typer.apply", json!({"mode": "sideways"})),
        ("typer.apply", json!({"layers": "all"})),
        ("typer.apply", json!({"layers": [999_999]})),
        ("typer.applyLines", json!({"layers": [1.5]})),
        ("typer.align", json!({"tolerance": 900})),
        ("typer.resize", json!({"delta": "big"})),
        ("typer.resize", json!({"delta": 1, "unit": "em"})),
        ("typer.applyShape", json!({"lines": "one"})),
        ("typer.applyShape", json!({"lines": []})),
        ("typer.applyShape", json!({"lines": [1, 2]})),
        ("typer.multiBubble.capture", json!({"bounds": [1, 2, 3]})),
        ("typer.style.fromLayer", json!({"layer": "x"})),
        ("typer.detectBubble", json!({})),
        ("typer.detectBubble", json!({"x": "a", "y": 2})),
        ("typer.detectBubble", json!({"x": 1, "y": 2, "rows": 99})),
    ] {
        assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
    }
    // Without a document every placement is disabled, and the session commands still answer.
    let mut empty = Session::new();
    for cmd in ["typer.paste", "typer.pasteMany", "typer.apply", "typer.align", "typer.resize", "typer.toggleCleaningLayers", "typer.detectBubble"] {
        assert!(!empty.is_enabled(cmd), "{cmd}");
    }
    assert!(empty.execute("typer.multiBubble.capture", json!({})).is_err());
    assert!(empty.execute("typer.multiBubble.removeLast", json!({})).is_ok());
}

/// `cargo test -p photocraft-engine --release typer_place_perf -- --ignored --nocapture`
#[test]
#[ignore]
fn typer_place_perf() {
    // `CAPISHOT_TYPER_PERF_SCALE=2.5` times a 5000 × 7500 (37.5 MP) page.
    let k = std::env::var("CAPISHOT_TYPER_PERF_SCALE").ok().and_then(|v| v.parse::<f64>().ok()).filter(|k| (0.5..=4.0).contains(k)).unwrap_or(1.0);
    let bubbles: Vec<B> = (0..12)
        .map(|i| B { cx: k * (300.0 + f64::from(i % 3) * 700.0), cy: k * (300.0 + f64::from(i / 3) * 700.0), rx: k * 260.0, ry: k * 180.0, tail: i % 2 == 0 })
        .collect();
    let (w, h) = ((2000.0 * k) as u32, (3000.0 * k) as u32);
    let mut s = Session::new();
    s.add_document(page(w, h, &bubbles), None);
    s.execute("typer.library.import", json!({"json": library(), "now": 1})).unwrap();
    s.execute("typer.script.set", json!({"text": SCRIPT.repeat(5)})).unwrap();
    // Text scales with the page, as on a real high-resolution scan.
    s.execute("typer.settings.set", json!({"key": "textScale", "value": (100.0 * k).round()})).unwrap();
    let t = std::time::Instant::now();
    page_image(&mut s).unwrap();
    let composite = t.elapsed();
    let mut paste = Vec::new();
    let mut align = Vec::new();
    for b in &bubbles {
        wand(&mut s, b.cx, b.cy);
        let t = std::time::Instant::now();
        let r = s.execute("typer.paste", json!({"text": "Some dialogue for the bubble that wraps a little"})).unwrap();
        paste.push(t.elapsed());
        let id = r["layer"].as_u64().unwrap();
        s.execute("type.edit", json!({"layer": id, "move": [-15, 20]})).unwrap();
        s.execute("select.deselect", json!({})).unwrap();
        let t = std::time::Instant::now();
        s.execute("typer.align", json!({"layers": [id]})).unwrap();
        align.push(t.elapsed());
        assert!(near(ink_center(&s, id), (b.cx, b.cy), 1.0));
    }
    let ms = |v: &[std::time::Duration]| v.iter().map(|d| d.as_secs_f64() * 1000.0).sum::<f64>() / v.len() as f64;
    println!(
        "{w}x{h}: first composite {:.1} ms, paste {:.1} ms, align {:.1} ms (mean of {})",
        composite.as_secs_f64() * 1000.0,
        ms(&paste),
        ms(&align),
        paste.len()
    );
}

/// Real pages, local only: `CAPISHOT_TYPER_PAGES=<folder with real pages> cargo test -p photocraft-engine
/// typer_real_pages -- --ignored --nocapture`. Reads `es/15.psd` and `bounds.json` there (never
/// committed). Detection: the bubble found at each text's centre; align: each dialogue layer
/// aligned with no selection, compared with where the typesetter put it.
#[test]
#[ignore]
fn typer_real_pages() {
    let Ok(dir) = std::env::var("CAPISHOT_TYPER_PAGES") else {
        eprintln!("CAPISHOT_TYPER_PAGES not set: skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let bounds: Value = std::fs::read_to_string(dir.join("bounds.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    for rel in ["es/15.psd", "en/15_EN_photopea.psd", "pt/15_PT_photopea.psd"] {
        let path = dir.join(rel);
        if !path.exists() {
            continue;
        }
        let mut s = Session::new();
        let bytes = std::fs::read(&path).unwrap();
        s.add_document(crate::file_cmds::import(rel, &bytes).unwrap(), None);
        let doc = s.active().unwrap().doc.clone();
        let texts: Vec<(LayerId, String, Rect)> = doc
            .walk()
            .into_iter()
            .filter_map(|(_, _, l)| match &l.content {
                LayerContent::Text(t) if l.visible => support::ink(t).map(|i| (l.id, t.text.clone(), i)),
                _ => None,
            })
            .collect();
        let gt: Vec<(String, Rect)> = bounds[rel]["t"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|e| {
                        let b: Vec<f64> = e[2].as_array()?.iter().filter_map(Value::as_f64).collect();
                        Some((e[1].as_str()?.to_string(), Rect::new(*b.first()?, *b.get(1)?, b.get(2)? - b.first()?, b.get(3)? - b.get(1)?)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let t = std::time::Instant::now();
        page_image(&mut s).unwrap();
        let comp_ms = t.elapsed().as_secs_f64() * 1000.0;
        let (mut found, mut no_bubble, mut total) = (0, 0, 0);
        let mut detect_ms = 0.0;
        for (text, r) in &gt {
            total += 1;
            let (cx, cy) = r.center();
            let t = std::time::Instant::now();
            let hit = s.execute("typer.detectBubble", json!({"x": cx, "y": cy})).unwrap();
            detect_ms += t.elapsed().as_secs_f64() * 1000.0;
            let area = hit["bbox"][2].as_f64().unwrap_or(0.0) * hit["bbox"][3].as_f64().unwrap_or(0.0);
            let bubble = hit["found"] == true && area <= bubble::MAX_BUBBLE_TO_TEXT * r.area().max(1.0);
            if bubble {
                found += 1;
            } else {
                no_bubble += 1;
            }
            eprintln!(
                "  {:>5} {:<28} {}",
                if bubble { "found" } else { "none" },
                text.replace('\r', " / ").chars().take(28).collect::<String>(),
                hit["opened"]
            );
        }
        // Align each layer alone, no selection; error = distance from where it was.
        let mut errs = Vec::new();
        let mut align_ms = 0.0;
        for (id, text, ink) in &texts {
            let t = std::time::Instant::now();
            let r = s.execute("typer.align", json!({"layers": [id.0]}));
            align_ms += t.elapsed().as_secs_f64() * 1000.0;
            if let Ok(r) = r {
                let c = ink_center(&s, id.0);
                let (ox, oy) = ink.center();
                let e = ((c.0 - ox).powi(2) + (c.1 - oy).powi(2)).sqrt();
                errs.push(e);
                eprintln!("  align {:<28} moved {e:.1} px", text.replace(['\r', '\n'], " / ").chars().take(28).collect::<String>());
                s.execute("edit.undo", json!({})).unwrap();
                let _ = r;
            } else {
                eprintln!("  align {:<28} no bubble", text.replace(['\r', '\n'], " / ").chars().take(28).collect::<String>());
            }
        }
        errs.sort_by(|a, b| a.total_cmp(b));
        let within = |px: f64| errs.iter().filter(|e| **e <= px).count();
        eprintln!(
            "{rel}: {}x{}, composite {comp_ms:.0} ms; detection {found}/{total} found ({no_bubble} none), {:.1} ms each; align {} of {} layers, ≤6 px {}, ≤12 px {}, median {:.1} px, {:.1} ms each",
            doc.size.width,
            doc.size.height,
            detect_ms / total.max(1) as f64,
            errs.len(),
            texts.len(),
            within(6.0),
            within(12.0),
            errs.get(errs.len() / 2).copied().unwrap_or(f64::NAN),
            align_ms / texts.len().max(1) as f64,
        );
    }
}
