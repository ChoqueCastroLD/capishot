//! Edit Contents of type (Type › Perspective): longer text grows the contents' canvas instead of
//! being clipped, and the placement takes the shift, so the glyphs that were there stay put.

use super::*;

fn text_layer(s: &Session) -> LayerId {
    s.active().unwrap().doc.walk().into_iter().find(|(_, _, l)| matches!(l.content, LayerContent::Text(_))).map(|(_, _, l)| l.id).unwrap()
}

/// Type → smart object → a perspective: `spread` pulls the bottom corners out (only enlarging),
/// else the top corners come in (converging). The frame it started from.
fn perspective_type(s: &mut Session, text: &str, spread: bool) -> photocraft_geom::Rect {
    s.execute("file.new", json!({"width": 900, "height": 400, "background": "white"})).unwrap();
    s.execute("type.create", json!({"text": text, "size": 60, "x": 100, "y": 200})).unwrap();
    let id = LayerId(s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap());
    let r = active_smart(s).cache.unwrap().content_bounds();
    let (x0, y0, x1, y1) = (f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1));
    let w = x1 - x0;
    let quad = if spread {
        json!([[x0, y0], [x1, y0], [x1 + 0.2 * w, y1 + 10.0], [x0 - 0.2 * w, y1 + 10.0]])
    } else {
        json!([[x0 + 0.15 * w, y0], [x1 - 0.15 * w, y0], [x1, y1], [x0, y1]])
    };
    s.execute("edit.transform", json!({"layer": id, "rect": [x0, y0, x1, y1], "quad": quad})).unwrap();
    assert!(active_smart(s).perspective.is_some());
    r
}

fn edit_text(s: &mut Session, text: &str) -> usize {
    let child = s.execute("layer.smartObjects.editContents", json!({})).unwrap()["document"].as_u64().unwrap() as usize;
    let t = text_layer(s);
    s.execute("type.edit", json!({"layer": t.0, "text": text})).unwrap();
    s.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    s.set_active(0);
    child
}

/// Over the left 40 % of `frame` inset by 3 px (the letters that were there; at the old edge the
/// contents were cut off, so those pixels are better now, not the same): the largest channel
/// difference, and the mean alpha difference with `b` moved by each of 0 and ±1 px.
fn compare(a: &Surface, b: &Surface, frame: Rect) -> (f32, [f64; 5]) {
    let (xs, ys) = (frame.x0 + 3..frame.x0 + frame.width() as i32 * 2 / 5, frame.y0 + 3..frame.y1 - 3);
    let mut worst = 0.0f32;
    let mut inked = 0;
    for y in ys.clone() {
        for x in xs.clone() {
            let (p, q) = (a.rgba(x, y), b.rgba(x, y));
            inked += usize::from(p[3] > 0.5);
            worst = worst.max((0..4).map(|k| (p[k] - q[k]).abs()).fold(0.0, f32::max));
        }
    }
    assert!(inked > 50, "the compared part has ink");
    let means = [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)].map(|(dx, dy)| {
        let (mut sum, mut n) = (0.0f64, 0u32);
        for y in ys.clone() {
            for x in xs.clone() {
                sum += f64::from((a.rgba(x, y)[3] - b.rgba(x + dx, y + dy)[3]).abs());
                n += 1;
            }
        }
        sum / f64::from(n)
    });
    (worst, means)
}

#[test]
fn longer_type_grows_the_contents_and_keeps_its_glyphs_in_place() {
    for spread in [true, false] {
        let mut s = Session::new();
        let frame = perspective_type(&mut s, "ABC", spread);
        // The baseline is rendered from the contents too (a save with the text unchanged), as the
        // longer text's is: the first render after the transform resamples the old pixels instead.
        edit_text(&mut s, "ABC");
        let before = active_smart(&s).cache.unwrap();
        let b0 = before.content_bounds();
        let steps = s.active().unwrap().history.past_len();
        let child = edit_text(&mut s, "ABCDEFGHIJKL");
        // Nothing clipped: the contents' canvas holds all the text, with room around it.
        let cdoc = s.documents()[child].doc.clone();
        let ink = cdoc.walk().iter().filter_map(|(_, _, l)| crate::layer_multi_cmds::layer_bounds(l)).fold(Rect::EMPTY, |a, b| a.union(&b));
        assert!(cdoc.bounds().contains_rect(&ink) && cdoc.bounds() != ink, "canvas {:?} ink {:?}", cdoc.bounds(), ink);
        let after = active_smart(&s).cache.unwrap();
        let b1 = after.content_bounds();
        assert!(b1.x1 > b0.x1 + (frame.width() as i32), "the longer text reaches further: {b0:?} → {b1:?}");
        let (worst, means) = compare(&before, &after, b0);
        // The old letters are where they were: no 1 px move matches better than none.
        assert!(means[1..].iter().all(|m| *m > means[0] * 2.0), "spread={spread}: means by shift {means:?}");
        if spread {
            // Only enlarging, the warp samples the contents directly: the same pixels.
            assert!(worst < 0.02, "the old glyphs changed: max difference {worst}");
        } else {
            // Converging, the warp first shrinks the contents by the scale at their centre, which
            // moves as they grow: the same places, resampled a little differently.
            assert!(means[0] < 0.02, "mean difference {}", means[0]);
        }
        // One undo step in the parent: Edit Contents.
        let st = s.active().unwrap();
        assert_eq!(st.history.past_len(), steps + 1);
        assert_eq!(st.history.entries().last().map(ToString::to_string).as_deref(), Some("Edit Contents"));
        s.undo();
        assert_eq!(active_smart(&s).cache.unwrap().content_bounds(), b0, "undo: the short text as it was");
    }
}

#[test]
fn shorter_type_shrinks_the_contents_and_same_length_keeps_the_canvas() {
    let mut s = Session::new();
    perspective_type(&mut s, "ABCDEFGHIJKL", false);
    // The same text again: the canvas (the text's bounds) is left as it was.
    let child = edit_text(&mut s, "ABCDEFGHIJKL");
    let size = s.documents()[child].doc.size;
    let b0 = active_smart(&s).cache.unwrap().content_bounds();
    // Much shorter: the canvas shrinks to the text (plus padding).
    s.set_active(child);
    let t = text_layer(&s);
    s.execute("type.edit", json!({"layer": t.0, "text": "AB"})).unwrap();
    s.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    assert!(s.documents()[child].doc.size.width < size.width / 2, "{:?} → {:?}", size, s.documents()[child].doc.size);
    s.set_active(0);
    let b1 = active_smart(&s).cache.unwrap().content_bounds();
    assert!(b1.width() < b0.width() / 2 && b1.x0 >= b0.x0 - 2, "{b0:?} → {b1:?}");
}

#[test]
fn other_contents_keep_their_canvas() {
    // A pixel layer's contents: the canvas is the user's, never refitted.
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 200, "height": 100, "background": "white"})).unwrap();
    s.execute("layer.new.layer", json!({})).unwrap();
    s.execute("select.rect", json!({"x": 20, "y": 20, "width": 50, "height": 30})).unwrap();
    s.execute("edit.fill", json!({"color": "#ff0000"})).unwrap();
    s.execute("select.deselect", json!({})).unwrap();
    s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap();
    let child = s.execute("layer.smartObjects.editContents", json!({})).unwrap()["document"].as_u64().unwrap() as usize;
    let size = s.documents()[child].doc.size;
    s.execute("select.rect", json!({"x": 0, "y": 0, "width": 4, "height": 4})).unwrap();
    s.execute("edit.fill", json!({"color": "#0000ff"})).unwrap();
    s.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    assert_eq!(s.documents()[child].doc.size, size);
}

#[test]
fn text_that_would_pass_the_horizon_is_fitted_into_the_quad() {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 900, "height": 600, "background": "white"})).unwrap();
    s.execute("type.create", json!({"text": "ABC", "size": 60, "x": 100, "y": 300})).unwrap();
    let id = LayerId(s.execute("layer.smartObjects.convertToSmartObject", json!({})).unwrap()["layer"].as_u64().unwrap());
    let r = active_smart(&s).cache.unwrap().content_bounds();
    let (x0, y0, x1, y1) = (f64::from(r.x0), f64::from(r.y0), f64::from(r.x1), f64::from(r.y1));
    // The right edge five times the left's: the horizon is just past the text's right end.
    let h = y1 - y0;
    let quad = json!([[x0, y0], [x1, y0 - 2.0 * h], [x1, y1 + 2.0 * h], [x0, y1]]);
    s.execute("edit.transform", json!({"layer": id, "rect": [x0, y0, x1, y1], "quad": quad})).unwrap();
    let b0 = active_smart(&s).cache.unwrap().content_bounds();
    let steps = s.active().unwrap().history.past_len();
    let child = s.execute("layer.smartObjects.editContents", json!({})).unwrap()["document"].as_u64().unwrap() as usize;
    let size = s.active().unwrap().doc.size;
    let t = text_layer(&s);
    s.execute("type.edit", json!({"layer": t.0, "text": "ABCDEFGHIJKL"})).unwrap();
    let out = s.execute("layer.smartObjects.saveContents", json!({})).unwrap();
    assert_eq!(out["fittedIn"], json!(true), "{out}");
    // Nothing cut: the contents hold all the text.
    let cdoc = s.documents()[child].doc.clone();
    let ink = cdoc.walk().iter().filter_map(|(_, _, l)| crate::layer_multi_cmds::layer_bounds(l)).fold(Rect::EMPTY, |a, b| a.union(&b));
    assert!(cdoc.size.width > size.width * 2 && cdoc.bounds().contains_rect(&ink), "{size:?} → {:?}", cdoc.size);
    // And it all shows inside the quad the short text had (give or take a pixel), across it.
    s.set_active(0);
    let b1 = active_smart(&s).cache.unwrap().content_bounds();
    let quad_box = Rect::new(x0 as i32, (y0 - 2.0 * h) as i32, x1.ceil() as i32, (y1 + 2.0 * h).ceil() as i32).inflate(2);
    assert!(!b1.is_empty() && quad_box.contains_rect(&b1) && b1.width() + 10 >= b0.width(), "{b0:?} → {b1:?} in {quad_box:?}");
    assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "one Edit Contents step");
}
