//! Synthetic pages: a bubble drawn into the page's pixels, real type merged into them, and the
//! same page without the type as the reference the result is measured against.

use photocraft_doc::LayerId;
use serde_json::json;

use super::*;

const W: u32 = 1000;
const H: u32 = 1500;

/// A white page whose pixels are `f(x, y)` (straight RGB).
fn page(f: &dyn Fn(i32, i32) -> [f32; 3]) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": W, "height": H, "background": "white"})).unwrap();
    let id = s.active().unwrap().doc.layers[0].id;
    let fmt = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().format();
    let n = fmt.channels();
    let mut px = vec![0f32; (W * H) as usize * n];
    let mut enc = [0f32; 8];
    for y in 0..H as i32 {
        for x in 0..W as i32 {
            let c = f(x, y);
            from_rgba_into(&fmt, [c[0], c[1], c[2], 1.0], &mut enc);
            let k = (y as usize * W as usize + x as usize) * n;
            px[k..k + n].copy_from_slice(&enc[..n]);
        }
    }
    s.edit("Page", |doc, _| {
        doc.layer_mut(id).unwrap().surface_mut().unwrap().write_region(Rect::new(0, 0, W as i32, H as i32), &px);
        Ok(())
    })
    .unwrap();
    s
}

/// Type merged into the page (pixels, as in a scan).
fn letter(s: &mut Session, text: &str, x: f64, y: f64, size: f64, color: &str) {
    let bg = s.active().unwrap().doc.layers[0].id;
    s.execute("type.create", json!({"text": text, "x": x, "y": y, "size": size, "color": color})).unwrap();
    s.execute("layer.mergeDown", json!({})).unwrap();
    let d = s.active().unwrap();
    assert_eq!(d.doc.layers.len(), 1, "merged into the page");
    assert_eq!(d.active_layer, Some(bg));
}

fn pixels(s: &Session) -> Vec<[f32; 4]> {
    let d = s.active().unwrap();
    let surf = d.doc.layers[0].surface().unwrap();
    let fmt = surf.format();
    surf.read_region(Rect::new(0, 0, W as i32, H as i32)).chunks_exact(fmt.channels()).map(|c| to_rgba(&fmt, c)).collect()
}

fn diff(a: &[f32; 4], b: &[f32; 4]) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)
}

/// The result against the clean reference inside `bx` ([x0, y0, x1, y1]): the mean and the share
/// of pixels off by more than 8/255.
fn residue(got: &[[f32; 4]], clean: &[[f32; 4]], bx: [i32; 4], inside: &dyn Fn(i32, i32) -> bool) -> (f32, f32) {
    let (mut sum, mut off, mut n) = (0f32, 0usize, 0usize);
    for y in bx[1]..bx[3] {
        for x in bx[0]..bx[2] {
            if !inside(x, y) {
                continue;
            }
            let k = y as usize * W as usize + x as usize;
            let d = diff(&got[k], &clean[k]);
            sum += d;
            off += usize::from(d > 8.0 / 255.0);
            n += 1;
        }
    }
    (sum / n.max(1) as f32, off as f32 / n.max(1) as f32)
}

/// An ellipse: centre, radii; `e(x, y)` < 1 inside.
fn ell(cx: f32, cy: f32, rx: f32, ry: f32) -> impl Fn(i32, i32) -> f32 {
    move |x, y| ((x as f32 + 0.5 - cx) / rx).powi(2) + ((y as f32 + 0.5 - cy) / ry).powi(2)
}

/// A bubble with a black outline about 4 px wide, its inside `fill`, on white.
fn oval_page(fill: impl Fn(i32, i32) -> [f32; 3] + 'static) -> Session {
    let e = ell(500.0, 600.0, 260.0, 170.0);
    page(&move |x, y| {
        let v = e(x, y);
        if v < 0.97 {
            fill(x, y)
        } else if v < 1.03 {
            [0.0; 3]
        } else {
            [1.0; 3]
        }
    })
}

fn inside_oval(x: i32, y: i32) -> bool {
    ell(500.0, 600.0, 260.0, 170.0)(x, y) < 0.97
}

/// Pixels of the outline (and just around it) that changed.
fn outline_changed(got: &[[f32; 4]], clean: &[[f32; 4]], band: &dyn Fn(i32, i32) -> bool) -> usize {
    (0..H as i32)
        .flat_map(|y| (0..W as i32).map(move |x| (x, y)))
        .filter(|&(x, y)| band(x, y))
        .filter(|&(x, y)| {
            let k = y as usize * W as usize + x as usize;
            diff(&got[k], &clean[k]) > 1.0 / 255.0
        })
        .count()
}

fn white(_: i32, _: i32) -> [f32; 3] {
    [1.0; 3]
}

#[test]
fn black_text_in_a_white_bubble_goes_and_the_outline_stays() {
    let mut s = oval_page(white);
    let clean = pixels(&s);
    letter(&mut s, "HELLO THERE!", 330.0, 590.0, 48.0, "#000000");
    letter(&mut s, "HOW ARE YOU?", 340.0, 660.0, 48.0, "#000000");
    let dirty = pixels(&s);
    let (before, _) = residue(&dirty, &clean, [240, 430, 760, 770], &inside_oval);
    assert!(before > 0.02, "there is text: {before}");
    let steps = s.active().unwrap().history.past_len();
    let r = s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    assert_eq!(r["background"], "solid", "{r}");
    assert!(r["pixels"].as_u64().unwrap() > 500, "{r}");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.001 && off < 0.0005, "residue: mean {mean}, off {off}");
    let e = ell(500.0, 600.0, 260.0, 170.0);
    assert_eq!(outline_changed(&got, &clean, &|x, y| (0.9..1.1).contains(&e(x, y)) && !(0.9..0.97).contains(&e(x, y))), 0, "the outline untouched");
    assert_eq!(s.active().unwrap().history.past_len(), steps + 1, "one step");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(pixels(&s), dirty, "undone");
}

#[test]
fn a_click_on_a_letter_works() {
    let mut s = oval_page(white);
    let clean = pixels(&s);
    letter(&mut s, "IIIII", 420.0, 600.0, 90.0, "#000000");
    // A dark pixel of a letter.
    let dirty = pixels(&s);
    let (x, y) =
        (380..620).flat_map(|x| (520..680).map(move |y| (x, y))).find(|&(x, y)| dirty[y as usize * W as usize + x as usize][0] < 0.1).expect("a letter");
    s.execute(ID, json!({"x": x, "y": y})).unwrap();
    let (mean, _) = residue(&pixels(&s), &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.001, "{mean}");
}

#[test]
fn a_narration_box_is_cleaned() {
    let inside = |x: i32, y: i32| (203..697).contains(&x) && (303..497).contains(&y);
    let mut s = page(&move |x, y| {
        if inside(x, y) {
            [1.0, 0.98, 0.9]
        } else if (200..700).contains(&x) && (300..500).contains(&y) {
            [0.0; 3]
        } else {
            [0.55; 3]
        }
    });
    let clean = pixels(&s);
    letter(&mut s, "Meanwhile, far away...", 240.0, 410.0, 40.0, "#1a1a1a");
    let r = s.execute(ID, json!({"x": 450, "y": 330})).unwrap();
    assert_eq!(r["background"], "solid");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [200, 300, 700, 500], &inside);
    assert!(mean < 0.001 && off < 0.0005, "mean {mean} off {off}");
    let border = |x: i32, y: i32| (195..705).contains(&x) && (295..505).contains(&y) && !inside(x, y);
    assert_eq!(outline_changed(&got, &clean, &border), 0);
}

#[test]
fn sfx_inside_a_bubble_goes() {
    let mut s = oval_page(white);
    let clean = pixels(&s);
    letter(&mut s, "BAM!!", 330.0, 650.0, 140.0, "#c01010");
    let r = s.execute(ID, json!({"x": 320, "y": 600})).unwrap();
    assert!(r["pixels"].as_u64().unwrap() > 3000);
    let (mean, off) = residue(&pixels(&s), &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.001 && off < 0.001, "mean {mean} off {off}");
}

fn gradient(_: i32, y: i32) -> [f32; 3] {
    let t = ((y - 430) as f32 / 340.0).clamp(0.0, 1.0);
    [0.70 + 0.27 * t, 0.82 + 0.15 * t, 1.0]
}

#[test]
fn a_linear_gradient_bubble_keeps_its_gradient() {
    let mut s = oval_page(gradient);
    let clean = pixels(&s);
    letter(&mut s, "So pretty...", 330.0, 620.0, 56.0, "#202040");
    let r = s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    assert_eq!(r["background"], "gradient", "{r}");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.002 && off < 0.002, "mean {mean} off {off}");
    // Continuous: no step between neighbouring rows along the bubble's middle.
    for y in 520..700 {
        let (a, b) = (got[y * W as usize + 500], got[(y + 1) * W as usize + 500]);
        assert!(diff(&a, &b) < 3.0 / 255.0, "a step at row {y}: {a:?} {b:?}");
    }
}

#[test]
fn a_textured_bubble_is_refused() {
    // Screentone dots all over the inside.
    let dots = |x: i32, y: i32| {
        let (u, v) = ((x.rem_euclid(6) as f32) - 2.5, (y.rem_euclid(6) as f32) - 2.5);
        if u * u + v * v < 2.6 { [0.80; 3] } else { [1.0; 3] }
    };
    let mut s = oval_page(dots);
    letter(&mut s, "HELLO", 400.0, 620.0, 60.0, "#000000");
    let before = pixels(&s);
    let e = s.execute(ID, json!({"x": 500, "y": 520})).unwrap_err().to_string();
    assert!(e.contains("notPlain") && e.contains(NOT_PLAIN), "{e}");
    assert_eq!(pixels(&s), before, "nothing changed");
    // Grain.
    let grain = |x: i32, y: i32| [0.86 + 0.12 * (noise(x, y, 0) / 3.0).clamp(-1.0, 1.0); 3];
    let mut s = oval_page(grain);
    letter(&mut s, "HELLO", 400.0, 620.0, 60.0, "#000000");
    let e = s.execute(ID, json!({"x": 500, "y": 520})).unwrap_err().to_string();
    assert!(e.contains("notPlain"), "{e}");
}

#[test]
fn no_bubble_and_no_text_are_errors() {
    let mut s = oval_page(white);
    let e = s.execute(ID, json!({"x": 500, "y": 600})).unwrap_err().to_string();
    assert!(e.contains("noText"), "{e}");
    let e = s.execute(ID, json!({"x": 30, "y": 30})).unwrap_err().to_string();
    assert!(e.contains("noBubble"), "{e}");
}

#[test]
fn on_a_new_layer_the_page_stays() {
    let mut s = oval_page(white);
    let clean = pixels(&s);
    letter(&mut s, "HELLO", 400.0, 620.0, 60.0, "#000000");
    let dirty = pixels(&s);
    let r = s.execute(ID, json!({"x": 500, "y": 520, "newLayer": true})).unwrap();
    assert_eq!(pixels(&s), dirty, "the page untouched");
    let d = s.active().unwrap();
    assert_eq!(d.doc.layers.len(), 2);
    assert_eq!(d.active_layer, Some(LayerId(r["layer"].as_u64().unwrap())));
    let seen = photocraft_compose::render(&d.doc, Rect::new(0, 0, W as i32, H as i32));
    let seen: Vec<[f32; 4]> = seen.px.to_vec();
    let (mean, _) = residue(&seen, &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.001, "{mean}");
}

#[test]
fn on_a_type_layer_it_works_from_what_is_seen_on_a_new_layer() {
    let mut s = oval_page(white);
    letter(&mut s, "HELLO", 400.0, 620.0, 60.0, "#000000");
    s.execute("type.create", json!({"text": "Note", "x": 50, "y": 1400, "size": 30})).unwrap();
    let r = s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    assert_eq!(r["newLayer"], true);
}

/// A 1000 × 1500 page, as fast as a click should be (a release build; the page composite
/// included).
#[test]
fn fast_on_a_page() {
    let mut s = oval_page(white);
    letter(&mut s, "HELLO THERE!", 330.0, 590.0, 48.0, "#000000");
    letter(&mut s, "HOW ARE YOU?", 340.0, 660.0, 48.0, "#000000");
    let t = std::time::Instant::now();
    s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    // Again on a cached page (a second bubble's click before any change): the command alone.
    s.execute("edit.undo", json!({})).unwrap();
    crate::typer_place_cmds::page_image(&mut s).unwrap();
    let t = std::time::Instant::now();
    s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    let cached = t.elapsed().as_secs_f64() * 1000.0;
    eprintln!("edit.removeBubbleText on {W}x{H}: {ms:.1} ms (page cached: {cached:.1} ms)");
    if !cfg!(debug_assertions) {
        assert!(ms < 100.0, "{ms} ms");
    }
}

/// A scan's bubble: paper with a little noise (as JPEG and scanning leave it). The text goes and
/// the fill carries about the same noise.
#[test]
fn a_noisy_scan_bubble_is_cleaned_with_matching_noise() {
    let grain = |x: i32, y: i32| [0.97 + 0.012 * noise(x + 7, y + 3, 0); 3];
    let mut s = oval_page(move |x, y| grain(x, y));
    let clean = pixels(&s);
    letter(&mut s, "HELLO THERE!", 330.0, 590.0, 48.0, "#000000");
    letter(&mut s, "HOW ARE YOU?", 340.0, 660.0, 48.0, "#000000");
    let r = s.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    assert_eq!(r["background"], "solid", "{r}");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [240, 430, 760, 770], &inside_oval);
    assert!(mean < 0.02 && off < 0.05, "mean {mean} off {off}");
    // No dark remains of the letters.
    let darkest = (430..770)
        .flat_map(|y| (240..760).map(move |x| (x, y)))
        .filter(|&(x, y)| ell(500.0, 600.0, 260.0, 170.0)(x, y) < 0.9)
        .map(|(x, y)| got[y as usize * W as usize + x as usize][0])
        .fold(1.0, f32::min);
    assert!(darkest > 0.85, "a letter's remains: {darkest}");
    // The cleaned area isn't flat: it has the paper's grain.
    let spread: Vec<f32> = (575..605).flat_map(|y| (360..640).map(move |x| (x, y))).map(|(x, y)| got[y as usize * W as usize + x as usize][0]).collect();
    let m = spread.iter().sum::<f32>() / spread.len() as f32;
    let sd = (spread.iter().map(|v| (v - m).powi(2)).sum::<f32>() / spread.len() as f32).sqrt();
    assert!(sd > 0.002, "grain in the fill: {sd}");
}

/// A real page, only when asked: `CAPISHOT_BUBBLE_PAGE` (an image or a layered file, flattened),
/// `CAPISHOT_BUBBLE_POINTS`
/// ("x,y x,y …", the clicks) and `CAPISHOT_BUBBLE_OUT` (a PNG of the result) are set.
#[test]
fn a_real_page_when_given() {
    let (Ok(path), Ok(points)) = (std::env::var("CAPISHOT_BUBBLE_PAGE"), std::env::var("CAPISHOT_BUBBLE_POINTS")) else { return };
    let bytes = std::fs::read(&path).unwrap();
    let doc = photocraft_io::import(&path, &bytes).unwrap().document;
    let mut s = Session::new();
    s.add_document(doc, None);
    // One pixel layer, the lettering in it, as in a scan.
    s.execute("layer.flattenImage", json!({})).unwrap();
    for pt in points.split_whitespace() {
        let v: Vec<f64> = pt.split(',').map(|c| c.parse().unwrap()).collect();
        let t = std::time::Instant::now();
        let r = s.execute(ID, json!({"x": v[0], "y": v[1]}));
        eprintln!(
            "{pt}: {:.1} ms {}",
            t.elapsed().as_secs_f64() * 1000.0,
            match &r {
                Ok(v) => v.to_string(),
                Err(e) => e.to_string(),
            }
        );
    }
    if let Ok(out) = std::env::var("CAPISHOT_BUBBLE_OUT") {
        let d = s.active().unwrap();
        let png = photocraft_io::export(&d.doc, "out.png", &Default::default()).unwrap();
        std::fs::write(out, png.bytes).unwrap();
    }
}
