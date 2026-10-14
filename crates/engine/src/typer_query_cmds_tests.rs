//! `typer.shapeText` and `typer.detectBubbles` on synthetic manga pages.

use photocraft_doc::{Color, ColorMode, Document, LayerContent, LayerId, SampleType, Size};
use photocraft_typer::synth::{Ellipse, Item, SynthPage};
use serde_json::{Value, json};

use crate::{EngineError, Session};

fn doc_of(page: &SynthPage) -> Document {
    let (w, h) = (page.width as u32, page.height as u32);
    let mut doc = Document::with_background("page", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let bytes: Vec<u8> = page.px.iter().flatten().copied().collect();
    let fmt = doc.pixel_format();
    let surf = photocraft_raster::Surface::from_interleaved(fmt, photocraft_geom::Rect::new(0, 0, w as i32, h as i32), &bytes);
    doc.layers[0].content = LayerContent::Raster(surf);
    doc
}

fn library() -> Value {
    json!({
        "styles": [
            {"id": "reg", "name": "Regular", "prefixes": [],
             "textProps": {"typeUnit": "pixelsUnit", "layerText": {"antiAlias": "antiAliasSmooth",
                "textStyleRange": [{"textStyle": {"fontName": "Inter", "fontStyleName": "Regular", "size": 20, "autoLeading": true}}],
                "paragraphStyleRange": [{"paragraphStyle": {"alignment": "center", "hyphenate": true}}]}}}
        ],
        "defaultStyleId": "reg"
    })
}

const SCRIPT: &str = "Page 1\nNunca pensé que llegaríamos tan lejos, pero aquí estamos todavía juntos.\n## nota\nSegunda línea.\nTercera.\nCuarta.\nQuinta.\nSexta.\nSéptima.\nOctava.\nNovena.\nDécima.\nOnce.";

fn session(page: &SynthPage) -> Session {
    let mut s = Session::new();
    s.add_document(doc_of(page), None);
    s.execute("typer.library.import", json!({"json": library(), "now": 1})).unwrap();
    s.execute("typer.script.set", json!({"text": SCRIPT})).unwrap();
    s
}

fn one_bubble() -> SynthPage {
    SynthPage::new(900, 700, vec![Item::Bubble { e: Ellipse { cx: 450.0, cy: 330.0, rx: 220.0, ry: 170.0 }, tail: true, text: false }])
}

fn steps(s: &Session) -> usize {
    s.active().unwrap().history.past_len()
}

#[test]
fn shapes_the_active_layer_in_its_bubble_and_applies_a_variant() {
    let mut s = session(&one_bubble());
    let r = s.execute("typer.paste", json!({"bounds": [300, 220, 300, 220]})).unwrap();
    let id = r["layer"].as_u64().unwrap();
    let before = steps(&s);
    let v = s.execute("typer.shapeText", json!({})).unwrap();
    assert_eq!(steps(&s), before, "a query adds no history");
    assert_eq!(v["layer"], id);
    assert_eq!(v["bubble"]["found"], true, "{v}");
    assert_eq!(v["bubble"]["source"], "wand");
    assert_eq!(v["language"], "es");
    let variants = v["variants"].as_array().unwrap();
    assert!(!variants.is_empty() && variants.len() <= 12);
    let best = &variants[0];
    assert_eq!(best["fits"], true, "{best}");
    let lines: Vec<String> = best["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
    assert!(lines.len() >= 3, "{lines:?}");
    // The lines re-join to the text (no hyphen in a fitting top variant here).
    assert_eq!(lines.join(" ").replace("- ", ""), v["text"].as_str().unwrap());
    s.execute("typer.applyShape", json!({"layer": id, "lines": lines})).unwrap();
    assert_eq!(steps(&s), before + 1, "applying is one step");
    match &s.active().unwrap().doc.layer(LayerId(id)).unwrap().content {
        LayerContent::Text(t) => assert_eq!(t.text, lines.join("\n")),
        _ => panic!("not text"),
    }
    // Profiles and limits.
    let tall = s.execute("typer.shapeText", json!({"layer": id, "profile": "tall", "limit": 3, "hyphenate": false})).unwrap();
    assert!(tall["variants"].as_array().unwrap().len() <= 3);
    assert!(tall["variants"].as_array().unwrap().iter().all(|v| v["hyphens"] == 0));
}

#[test]
fn a_selection_wins_and_text_over_art_has_no_bubble() {
    let mut s = session(&one_bubble());
    let id = s.execute("typer.paste", json!({"bounds": [300, 220, 300, 220]})).unwrap()["layer"].as_u64().unwrap();
    s.execute("select.rect", json!({"x": 330, "y": 250, "width": 240, "height": 160})).unwrap();
    let v = s.execute("typer.shapeText", json!({"layer": id})).unwrap();
    assert_eq!(v["bubble"]["source"], "selection", "{v}");
    s.execute("select.deselect", json!({})).unwrap();
    // Text over dark art: no bubble, generic shapes.
    let mut dark = one_bubble();
    dark.px.iter_mut().for_each(|p| *p = [90, 90, 90, 255]);
    let mut s = session(&dark);
    let art = s.execute("typer.paste", json!({"bounds": [40, 40, 300, 200]})).unwrap()["layer"].as_u64().unwrap();
    let v = s.execute("typer.shapeText", json!({"layer": art})).unwrap();
    assert_eq!(v["bubble"]["found"], false, "{v}");
    assert!(!v["variants"].as_array().unwrap().is_empty());
}

#[test]
fn plain_text_needs_no_document() {
    let mut s = Session::new();
    let rows: Vec<Value> = (0..=12)
        .map(|i| {
            let y = f64::from(i) / 12.0;
            let w = (1.0 - (2.0 * y - 1.0).powi(2)).max(0.0).sqrt();
            json!({"y": y, "left": 0.5 - w / 2.0, "right": 0.5 + w / 2.0})
        })
        .collect();
    let v = s
        .execute(
            "typer.shapeText",
            json!({"text": "Je crois que nous devrions vraiment partir avant que la nuit tombe sur la ville.", "language": "fr", "width": 300, "height": 300, "rows": rows, "calibration": {"unitPx": 10, "linePx": 40}}),
        )
        .unwrap();
    assert_eq!(v["language"], "fr");
    assert_eq!(v["variants"][0]["fits"], true, "{v}");
    let plain = s.execute("typer.shapeText", json!({"text": "Bonjour"})).unwrap();
    assert_eq!(plain["variants"][0]["lines"], json!(["Bonjour"]));
}

#[test]
fn shape_text_refuses_bad_params() {
    let mut s = session(&one_bubble());
    for p in [
        json!({"text": "hi", "profile": "square"}),
        json!({"text": "hi", "limit": 0}),
        json!({"text": "hi", "limit": 65}),
        json!({"text": "hi", "maxLines": 9}),
        json!({"text": "hi", "language": "ja"}),
        json!({"text": "hi", "language": 3}),
        json!({"text": 3}),
        json!({"text": "x".repeat(5000)}),
        json!({"text": "hi", "width": -1, "height": 5}),
        json!({"text": "hi", "width": 100, "height": 80, "rows": [{"y": "a"}]}),
        json!({"text": "hi", "width": 100, "height": 80, "rows": "rows"}),
        json!({"text": "hi", "calibration": {"unitPx": 0, "linePx": 3}}),
        json!({"text": "hi", "hyphenate": "yes"}),
        json!({"layer": "x"}),
        json!({"layer": 99999}),
        json!({"layer": 1}),
    ] {
        assert!(s.execute("typer.shapeText", p.clone()).is_err(), "{p}");
    }
    // No active type layer and no text.
    assert!(matches!(s.execute("typer.shapeText", json!({})), Err(EngineError::BadParams { .. })));
}

#[test]
fn detects_every_bubble_and_fills_them_in_reading_order() {
    let page = SynthPage::busy(1000, 1500);
    let want = page.bubble_centers().len();
    let mut s = session(&page);
    let r = s.execute("typer.detectBubbles", json!({})).unwrap();
    assert_eq!(r["count"].as_u64(), Some(want as u64), "{r}");
    assert_eq!(r["order"], "rtl");
    let ltr = s.execute("typer.detectBubbles", json!({"rtl": false})).unwrap();
    let x = |v: &Value, i: usize| v["bubbles"][i]["center"][0].as_f64().unwrap();
    assert!(x(&r, 0) > x(&r, 1) && x(&ltr, 0) < x(&ltr, 1), "row order flips");
    // Store them with consecutive lines, then fill them in one step.
    let before = steps(&s);
    let r = s.execute("typer.detectBubbles", json!({"store": true})).unwrap();
    assert_eq!(r["stored"].as_u64(), Some(want as u64));
    let lines: Vec<Option<usize>> = s.typer.stored_selections.iter().map(|x| x.line_index).collect();
    assert_eq!(lines[..3], [Some(1), Some(3), Some(4)], "the note line is skipped");
    assert!(s.typer.state.settings.multi_bubble_mode);
    // Storing again adds nothing.
    let again = s.execute("typer.detectBubbles", json!({"store": true})).unwrap();
    assert_eq!(again["stored"].as_u64(), Some(want as u64));
    let pasted = s.execute("typer.pasteMany", json!({})).unwrap();
    assert_eq!(steps(&s), before + 1, "{pasted}");
    assert!(s.typer.stored_selections.is_empty());
}

#[test]
fn detect_bubbles_refuses_bad_params() {
    let mut s = session(&one_bubble());
    for p in [json!({"sensitivity": 0}), json!({"sensitivity": 10}), json!({"rtl": 1}), json!({"store": "y"}), json!({"rows": 40})] {
        assert!(s.execute("typer.detectBubbles", p.clone()).is_err(), "{p}");
    }
    let mut empty = Session::new();
    assert!(empty.execute("typer.detectBubbles", json!({})).is_err());
}

#[test]
#[ignore = "timing; run in release"]
fn detect_bubbles_perf_2000x3000() {
    let page = SynthPage::busy(2000, 3000);
    let mut s = session(&page);
    let t = std::time::Instant::now();
    s.execute("typer.detectBubbles", json!({"rows": 5})).unwrap();
    let first = t.elapsed();
    let t = std::time::Instant::now();
    let r = s.execute("typer.detectBubbles", json!({})).unwrap();
    println!("typer.detectBubbles 2000x3000: first (with composite) {first:?}, cached {:?}, {} bubbles", t.elapsed(), r["count"]);
}
