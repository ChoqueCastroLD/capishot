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
    let mut s = oval_page(grain);
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

/// A bubble cut by the image's left edge: still cleaned (its edge is its wall there).
#[test]
fn a_bubble_cut_by_the_image_edge_is_cleaned() {
    let e = ell(120.0, 600.0, 260.0, 170.0);
    let s0 = page(&|x, y| {
        let v = e(x, y);
        if v < 0.97 {
            [1.0; 3]
        } else if v < 1.03 {
            [0.0; 3]
        } else {
            [0.9; 3]
        }
    });
    let mut s = s0;
    let clean = pixels(&s);
    letter(&mut s, "HELLO", 60.0, 620.0, 56.0, "#000000");
    let r = s.execute(ID, json!({"x": 150, "y": 520})).unwrap();
    assert_ne!(r["region"], "bubble", "{r}");
    let got = pixels(&s);
    let inside = |x: i32, y: i32| e(x, y) < 0.9;
    let (mean, off) = residue(&got, &clean, [0, 430, 390, 770], &inside);
    assert!(mean < 0.002 && off < 0.002, "mean {mean} off {off}");
    assert_eq!(outline_changed(&got, &clean, &|x, y| (0.97..1.03).contains(&e(x, y))), 0);
}

/// A page of two panels and a caption that breaks out of the upper one into the gutter (no
/// outline on that side): the lettering's block is found and cleaned; the panel lines stay.
fn caption_page() -> (Session, Box<dyn Fn(i32, i32) -> bool>) {
    // Panel frames: 4 px black, the inside a light grey art fill, the gutter white.
    let frame = |x: i32, y: i32| {
        let r1 = (40..960).contains(&x) && (40..700).contains(&y);
        let r1i = (44..956).contains(&x) && (44..696).contains(&y);
        let r2 = (40..960).contains(&x) && (740..1460).contains(&y);
        let r2i = (44..956).contains(&x) && (744..1456).contains(&y);
        (r1 && !r1i) || (r2 && !r2i)
    };
    // The caption: a white quadrilateral over the upper panel's right edge, its outline only on
    // the panel's side (left and top), open to the gutter on the right.
    let caption = |x: i32, y: i32| (700..1000).contains(&x) && (300..560).contains(&y) && (x - 700) * 2 >= (300 - y).max(0);
    let caption_edge = |x: i32, y: i32| ((697..700).contains(&x) && (300..560).contains(&y)) || ((700..958).contains(&x) && (297..300).contains(&y));
    let s = page(&move |x, y| {
        if caption_edge(x, y) {
            [0.0; 3]
        } else if caption(x, y) {
            [1.0; 3]
        } else if frame(x, y) {
            [0.0; 3]
        } else if (44..956).contains(&x) && ((44..696).contains(&y) || (744..1456).contains(&y)) {
            [0.75; 3]
        } else {
            [1.0; 3]
        }
    });
    (s, Box::new(frame))
}

#[test]
fn a_caption_open_to_the_gutter_is_cleaned_and_the_panel_lines_stay() {
    let (mut s, frame) = caption_page();
    let clean = pixels(&s);
    letter(&mut s, "WHO", 760.0, 400.0, 44.0, "#000000");
    letter(&mut s, "DID IT", 740.0, 460.0, 44.0, "#000000");
    let r = s.execute(ID, json!({"x": 800, "y": 420})).unwrap();
    assert_eq!(r["region"], "textCluster", "{r}");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [720, 330, 990, 540], &|_, _| true);
    assert!(mean < 0.003 && off < 0.003, "mean {mean} off {off}");
    let lines = (0..H as i32).flat_map(|y| (0..W as i32).map(move |x| (x, y))).filter(|&(x, y)| frame(x, y)).filter(|&(x, y)| {
        let k = y as usize * W as usize + x as usize;
        diff(&got[k], &clean[k]) > 1.0 / 255.0
    });
    assert_eq!(lines.count(), 0, "the panel lines untouched");
}

#[test]
fn vertical_text_with_furigana_is_one_block() {
    let (mut s, _) = caption_page();
    let clean = pixels(&s);
    // Two columns of letters, each with small "furigana" beside it.
    for (col, x) in [(0, 900.0), (1, 858.0)] {
        for (row, ch) in ["A", "B", "C", "D"].iter().enumerate() {
            let y = 360.0 + row as f64 * 44.0;
            letter(&mut s, ch, x, y, 38.0, "#000000");
            if col == 0 {
                letter(&mut s, "k", x + 30.0, y - 14.0, 12.0, "#000000");
            }
        }
    }
    let r = s.execute(ID, json!({"x": 905, "y": 400})).unwrap();
    assert_ne!(r["region"], "bubble", "{r}");
    let got = pixels(&s);
    let (mean, off) = residue(&got, &clean, [830, 310, 958, 545], &|_, _| true);
    assert!(mean < 0.003 && off < 0.004, "every column and its furigana gone: mean {mean} off {off}");
}

/// A background job (the page's main thread stays free on phones): the same result as inline,
/// and a refusal is the job's error with the document unchanged.
#[test]
fn runs_as_a_background_job_like_inline() {
    let make = || {
        let mut s = oval_page(white);
        letter(&mut s, "HELLO THERE!", 330.0, 590.0, 48.0, "#000000");
        s
    };
    let wait = |s: &mut Session, started: crate::jobs::Started| match started {
        crate::jobs::Started::Job(id) => {
            let t = std::time::Instant::now();
            loop {
                if let Some(e) = s.poll_jobs().into_iter().find(|e| e.id == id) {
                    break e.outcome;
                }
                assert!(t.elapsed().as_secs() < 60, "job did not finish");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        crate::jobs::Started::Done(v) => panic!("ran inline: {v}"),
    };
    let mut inline = make();
    inline.execute(ID, json!({"x": 500, "y": 520})).unwrap();
    let mut bg = make();
    let started = bg.start(ID, json!({"x": 500, "y": 520})).unwrap();
    let out = wait(&mut bg, started);
    assert!(matches!(out, crate::jobs::JobOutcome::Done(_)), "{out:?}");
    assert!(pixels(&inline) == pixels(&bg), "same pixels as inline");
    // A second click: the page cached by the job is patched where the first click painted.
    let past = bg.active().unwrap().history.past_len();
    let started = bg.start(ID, json!({"x": 500, "y": 520})).unwrap();
    let out = wait(&mut bg, started);
    let crate::jobs::JobOutcome::Failed(err) = out else { panic!("{out:?}") };
    assert!(err.contains("noText"), "{err}");
    assert_eq!(bg.active().unwrap().history.past_len(), past, "a refusal changes nothing");
    assert!(crate::typer_place_cmds::page_ready(&bg), "the job left the page cached");
}

/// A `w` x `h` white page with an oval bubble (outline about 4 px) centred at `(cx, cy)`, radii
/// `rx`, `ry`, and two lines of black type merged into it.
fn big_page(w: u32, h: u32, (cx, cy, rx, ry): (f32, f32, f32, f32)) -> Session {
    let e = ell(cx, cy, rx, ry);
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
    let id = s.active().unwrap().doc.layers[0].id;
    let fmt = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().format();
    let n = fmt.channels();
    let (mut px, mut enc) = (vec![0f32; (w * h) as usize * n], [0f32; 8]);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let v = e(x, y);
            let c = if (0.97..1.03).contains(&v) { 0.0 } else { 1.0 };
            from_rgba_into(&fmt, [c, c, c, 1.0], &mut enc);
            let k = (y as usize * w as usize + x as usize) * n;
            px[k..k + n].copy_from_slice(&enc[..n]);
        }
    }
    s.edit("Page", |doc, _| {
        doc.layer_mut(id).unwrap().surface_mut().unwrap().write_region(Rect::new(0, 0, w as i32, h as i32), &px);
        Ok(())
    })
    .unwrap();
    letter(&mut s, "HELLO THERE!", f64::from(cx) - 170.0, f64::from(cy) - 10.0, 48.0, "#000000");
    letter(&mut s, "HOW ARE YOU?", f64::from(cx) - 160.0, f64::from(cy) + 60.0, 48.0, "#000000");
    s
}

fn layer_pixels(s: &Session) -> Vec<f32> {
    let d = s.active().unwrap();
    let b = d.doc.bounds();
    d.doc.layers[0].surface().unwrap().read_region(b)
}

/// A click on a stale page composites a window around it, not the page; the result is the
/// page's: for a bubble inside a window, one too large for one, one cut by the page's edge (found
/// from the page), and a miss.
#[test]
fn a_stale_page_is_composited_around_the_click_only_with_the_same_result() {
    for (size, bubble, click, windowed) in [
        ((1800, 2400), (1200.0, 1500.0, 260.0, 170.0), (1200, 1420), true),
        ((1800, 2400), (900.0, 1200.0, 700.0, 500.0), (900, 1120), false),
        ((1800, 2400), (1200.0, 1500.0, 260.0, 170.0), (100, 100), false),
        ((1800, 2400), (300.0, 300.0, 400.0, 220.0), (250, 250), false),
        ((1800, 2400), (1450.0, 2100.0, 260.0, 170.0), (1450, 2020), true),
        ((1800, 2400), (900.0, 1200.0, 1150.0, 1400.0), (900, 1120), false),
        // Only the larger window holds it (a page large enough for that window to be worth it).
        ((3000, 3600), (1500.0, 1800.0, 560.0, 300.0), (1500, 1720), true),
    ] {
        for sample_all in [false, true] {
            let p = json!({"x": click.0, "y": click.1, "sampleAll": sample_all});
            let mut stale = big_page(size.0, size.1, bubble);
            let a = stale.execute(ID, p.clone());
            assert_eq!(!crate::typer_place_cmds::page_cached(&stale), windowed, "{bubble:?}: windowed {windowed}");
            let mut ready = big_page(size.0, size.1, bubble);
            crate::typer_place_cmds::page_image(&mut ready).unwrap();
            let b = ready.execute(ID, p);
            match (a, b) {
                // Layer ids differ between the sessions.
                (Ok(mut a), Ok(mut b)) => {
                    a["layer"] = json!(0);
                    b["layer"] = json!(0);
                    assert_eq!(a, b, "{bubble:?}");
                }
                (a, b) => assert_eq!(a.map_err(|e| e.to_string()), b.map_err(|e| e.to_string()), "{bubble:?}"),
            }
            assert!(layer_pixels(&stale) == layer_pixels(&ready), "{bubble:?} sampleAll {sample_all}: same pixels");
        }
    }
}

/// A `w` x `h` page whose grey level is `f(x, y)`.
fn grey_page(w: u32, h: u32, f: &dyn Fn(i32, i32) -> f32) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": w, "height": h, "background": "white"})).unwrap();
    let id = s.active().unwrap().doc.layers[0].id;
    let fmt = s.active().unwrap().doc.layer(id).unwrap().surface().unwrap().format();
    let n = fmt.channels();
    let (mut px, mut enc) = (vec![0f32; (w * h) as usize * n], [0f32; 8]);
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let c = f(x, y);
            from_rgba_into(&fmt, [c, c, c, 1.0], &mut enc);
            let k = (y as usize * w as usize + x as usize) * n;
            px[k..k + n].copy_from_slice(&enc[..n]);
        }
    }
    s.edit("Page", |doc, _| {
        doc.layer_mut(id).unwrap().surface_mut().unwrap().write_region(Rect::new(0, 0, w as i32, h as i32), &px);
        Ok(())
    })
    .unwrap();
    s
}

/// The oval page's bubble at `k` times its size (outline 3k px, a 4k px break at its right) with
/// two lines of type `k` times as large, on a `k` times larger page.
fn scaled_oval_page(k: f32) -> Session {
    let (cx, cy, rx, ry) = (500.0 * k, 600.0 * k, 260.0 * k, 170.0 * k);
    let e = ell(cx, cy, rx, ry);
    let mut s = grey_page((W as f32 * k) as u32, (H as f32 * k) as u32, &|x, y| {
        // Distance (about) from the outline's middle, in px.
        let d = (e(x, y).sqrt() - 1.0) * ry;
        let gap = x as f32 > cx && (y as f32 + 0.5 - cy).abs() < 2.0 * k;
        if d.abs() < 1.5 * k && !gap { 0.0 } else { 1.0 }
    });
    let kk = f64::from(k);
    letter(&mut s, "HELLO THERE!", 330.0 * kk, 590.0 * kk, 48.0 * kk, "#000000");
    letter(&mut s, "HOW ARE YOU?", 340.0 * kk, 660.0 * kk, 48.0 * kk, "#000000");
    s
}

/// At 1x, 2x and 3x the bubble is found on the first try (a closed bubble, not a fallback): the
/// search is sized for the ink; with 1x sizes forced on the upscaled page the break leaks.
#[test]
fn a_high_resolution_bubble_is_found_first_try() {
    for k in [1.0f32, 2.0, 3.0] {
        let mut s = scaled_oval_page(k);
        let at = |x: f32, y: f32| json!({"x": x * k, "y": y * k});
        let t = std::time::Instant::now();
        let r = s.execute(ID, at(500.0, 520.0)).unwrap();
        eprintln!("{k}x: {r} in {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(r["region"], "bubble", "{k}x: {r}");
        let b = r["bubbleBounds"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect::<Vec<_>>();
        assert!((b[2] - 514.0 * f64::from(k)).abs() < 12.0 * f64::from(k), "{k}x: the bubble's width {b:?}");
        let scale = r["search"]["scale"].as_f64().unwrap();
        if k == 1.0 {
            assert_eq!(r["search"], json!({"scale": 1.0, "gapRadius": 2, "reach": 24, "window": 1024}));
        } else {
            assert!(scale > 0.6 * f64::from(k) && r["search"]["gapRadius"].as_i64().unwrap() > 2, "{k}x: {r}");
            s.execute("edit.undo", json!({})).unwrap();
            let one_x = s.execute(ID, json!({"x": 500.0 * k, "y": 520.0 * k, "gapRadius": 2, "reach": 24}));
            assert!(one_x.as_ref().map_or(true, |r| r["region"] != "bubble"), "{k}x with 1x sizes: {one_x:?}");
        }
    }
}

#[test]
fn search_params_are_checked() {
    let mut s = oval_page(white);
    letter(&mut s, "HELLO", 400.0, 620.0, 60.0, "#000000");
    for bad in [json!({"gapRadius": 0}), json!({"gapRadius": 99}), json!({"reach": "far"}), json!({"window": 4}), json!({"window": "all"}), json!({"window": f64::MAX})] {
        let mut p = bad.clone();
        p["x"] = json!(500);
        p["y"] = json!(520);
        let e = s.execute(ID, p).unwrap_err().to_string();
        assert!(e.contains("must be"), "{bad}: {e}");
    }
    let r = s.execute(ID, json!({"x": 500, "y": 520, "gapRadius": 3, "reach": "auto", "window": "page"})).unwrap();
    // The 60 px lettering measures thicker than a 1x page's: the reach follows, the gap radius
    // asked for stays.
    let auto = bubble_find::Opts::scaled(r["search"]["scale"].as_f64().unwrap());
    assert_eq!((&r["search"]["gapRadius"], &r["search"]["reach"], &r["search"]["window"]), (&json!(3), &json!(auto.reach), &json!("page")), "{r}");
    s.execute("edit.undo", json!({})).unwrap();
    let r = s.execute(ID, json!({"x": 500, "y": 520, "window": 300})).unwrap();
    assert_eq!(r["search"]["window"], 300);
    // A window smaller than the bubble refuses, saying why.
    s.execute("edit.undo", json!({})).unwrap();
    let e = s.execute(ID, json!({"x": 500, "y": 520, "window": 100})).unwrap_err().to_string();
    assert!(e.contains("beyondWindow"), "{e}");
}

/// A 3000 x 4300 page (a 1500 px page at 2x): panels with thick borders and gutters, a dark blob
/// and a bubble. A click that finds no bubble searches around it only and answers fast (a
/// release build, the page cached), with the reason.
#[test]
fn a_miss_on_a_large_page_is_fast() {
    let (w, h) = (3000u32, 4300u32);
    let panels = [(80, 80, 1460, 2100), (1540, 80, 2920, 2100), (80, 2180, 2920, 4220)];
    let e = ell(2200.0, 1000.0, 300.0, 200.0);
    let mut s = grey_page(w, h, &|x, y| {
        let border = panels.iter().any(|&(x0, y0, x1, y1)| (x0..x1).contains(&x) && (y0..y1).contains(&y) && (x < x0 + 8 || x >= x1 - 8 || y < y0 + 8 || y >= y1 - 8));
        let blob = (600..1400).contains(&x) && (3000..3800).contains(&y);
        let v = e(x, y);
        if border || blob || (1.0..1.04).contains(&v) { 0.05 } else { 1.0 }
    });
    letter(&mut s, "HELLO THERE!", 2030.0, 990.0, 48.0, "#000000");
    crate::typer_place_cmds::page_image(&mut s).unwrap();
    let mut worst = 0f64;
    for (x, y) in [(40, 1000), (1500, 2140), (1000, 3400), (2000, 3000), (700, 1000), (2990, 4290)] {
        let t = std::time::Instant::now();
        let e = s.execute(ID, json!({"x": x, "y": y})).unwrap_err().to_string();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        eprintln!("miss at ({x}, {y}) in {ms:.1} ms: {e}");
        assert!(e.contains("noBubble ("), "{e}");
    }
    let r = s.execute(ID, json!({"x": 2200, "y": 980})).unwrap();
    assert_eq!(r["region"], "bubble", "{r}");
    if !cfg!(debug_assertions) {
        assert!(worst < 150.0, "{worst} ms");
    }
}
