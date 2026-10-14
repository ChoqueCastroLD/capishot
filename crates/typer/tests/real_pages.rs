//! Bubble detection, centring and TextShapeR on whole pages, with a metrics table.
//!
//! - `synthetic_pages` (always runs): generated manga-like pages (outlined bubbles, tails, joined
//!   double bubbles, narration boxes, SFX over screentone art). Floors are asserted.
//! - `real_pages` (`#[ignore]`, opt-in): local pages under `CAPISHOT_TYPER_PAGES` (never committed;
//!   see ATTRIBUTION.md › Test data). For every `*.psd` listed in its `bounds.json`
//!   (`{"<dir>/<file>.psd": {"t": [[index, "text with \r breaks", [x0, y0, x1, y1]], …]}}`) the page
//!   is composited without its type layers, then (a) every text layer is checked the way Align
//!   finds its bubble (seed left of the text, wand, judge), (b) the page-wide detector runs, and
//!   (d) TextShapeR shapes each multi-line text into its box: is the human breaking in the top 3?
//!   An optional `labels.json` (`{"<file>": {"<index>": "bubble" | "sfx" | "narration"}}`) turns
//!   (a) into a per-label check: bubbles must be found and centred, the rest refused. Without
//!   labels every layer is expected to be refused (the sample page 15 has no speech bubbles).
//!   Run: `CAPISHOT_TYPER_PAGES=/root/psd-work cargo test -p photocraft-typer --test real_pages -- --ignored --nocapture`.

use photocraft_typer::bubble::{self, Judge, Rect};
use photocraft_typer::pagedetect::{self, DetectOptions, Page};
use photocraft_typer::shaper::{self, Calibration, CharClass, Hyphenation, Options};
use photocraft_typer::synth::{Ellipse, Item, SynthPage};

fn area(w: usize, h: usize) -> photocraft_geom::Rect {
    photocraft_geom::Rect::new(0, 0, w as i32, h as i32)
}

/// What Align does for text whose ink box is `ink`: the wand left of it, opened, judged.
fn align_target(px: &[[u8; 4]], w: usize, h: usize, ink: &Rect, size_px: f64) -> Result<bubble::Opened, &'static str> {
    let seed = bubble::seed_left_of(ink);
    let (mask, outline) = bubble::wand_bubble(px, area(w, h), seed, bubble::WAND_TOLERANCE, Some(ink.center())).ok_or("outside")?;
    let o = bubble::opened_bounds(&mask).ok_or("empty")?;
    let judge = Judge {
        text: Some((ink, size_px)),
        seed: bubble::pixel(px, area(w, h), seed),
        page: Some(Rect::new(0.0, 0.0, w as f64, h as f64)),
        outline: Some(outline),
    };
    match judge.verdict(&o) {
        None => Ok(o),
        Some(why) => Err(why),
    }
}

#[derive(Default)]
struct Metrics {
    bubbles: usize,
    found: usize,
    detections: usize,
    true_detections: usize,
    centre_err: Vec<f64>,
    align_ok: usize,
    sfx: usize,
    sfx_refused: usize,
    shaped: usize,
    shaped_fit: usize,
}

impl Metrics {
    fn row(&self, name: &str) {
        let mean = if self.centre_err.is_empty() { 0.0 } else { self.centre_err.iter().sum::<f64>() / self.centre_err.len() as f64 };
        let max = self.centre_err.iter().copied().fold(0.0, f64::max);
        println!(
            "| {name} | {}/{} | {}/{} | {:.1} / {:.1} | {}/{} | {}/{} | {}/{} |",
            self.found,
            self.bubbles,
            self.true_detections,
            self.detections,
            mean,
            max,
            self.align_ok,
            self.bubbles,
            self.sfx_refused,
            self.sfx,
            self.shaped_fit,
            self.shaped
        );
    }
}

const LINES: [&str; 4] = [
    "¿De verdad crees que podemos escapar de aquí antes de que amanezca?",
    "Nunca pensé que llegaríamos tan lejos, pero aquí estamos.",
    "I never thought we would make it this far, but here we are.",
    "Você não sabe o que está fazendo, mas eu confio em você.",
];

fn synthetic(name: &str, page: &SynthPage, m: &mut Metrics) {
    let (w, h) = (page.width, page.height);
    let d = pagedetect::detect(&Page { px: &page.px, width: w, height: h }, (0.0, 0.0), &DetectOptions::default());
    let centres = page.bubble_centers();
    let cleaned = SynthPage::new(
        w,
        h,
        page.items
            .iter()
            .map(|it| match it {
                Item::Bubble { e, tail, .. } => Item::Bubble { e: *e, tail: *tail, text: false },
                Item::Double { a, b, .. } => Item::Double { a: *a, b: *b, text: false },
                other => other.clone(),
            })
            .collect(),
    );
    m.detections += d.len();
    m.true_detections += d.iter().filter(|b| centres.iter().any(|c| inside(&b.opened, *c))).count();
    let mut li = 0;
    for c in &centres {
        m.bubbles += 1;
        if let Some(b) = d.iter().find(|b| inside(&b.opened, *c)) {
            m.found += 1;
            m.centre_err.push(((b.center.0 - c.0).powi(2) + (b.center.1 - c.1).powi(2)).sqrt());
            // TextShapeR in the detected body, TypeR's character-class widths at a 9 px unit.
            let text = LINES[li % LINES.len()];
            li += 1;
            let opts = Options {
                hyphenation: Hyphenation::for_text(None, text),
                width: Some(b.opened.width),
                height: Some(b.opened.height),
                rows: b.rows.clone(),
                calibration: Some(Calibration { unit_px: Some(9.0), line_px: 22.0 }),
                ..Default::default()
            };
            let v = shaper::shape(text, &opts, &mut CharClass);
            m.shaped += 1;
            if v.first().is_some_and(|x| x.fits == Some(true)) {
                m.shaped_fit += 1;
            }
        }
        // Align on the cleaned page (the bubble's own text removed): text ink 60 × 24 px at the
        // centre.
        let ink = Rect::new(c.0 - 30.0, c.1 - 12.0, 60.0, 24.0);
        match align_target(&cleaned.px, w, h, &ink, 20.0) {
            Ok(o) => {
                let (ox, oy) = o.opened.center();
                if ((ox - c.0).powi(2) + (oy - c.1).powi(2)).sqrt() <= 0.08 * o.opened.width.min(o.opened.height).max(1.0) + 6.0 {
                    m.align_ok += 1;
                } else {
                    println!("  {name}: align at {c:?} centred on {:?}", (ox, oy));
                }
            }
            Err(why) => println!("  {name}: align at {c:?} refused: {why}"),
        }
    }
    for it in &page.items {
        if let Item::Sfx { x, y, size } = it {
            m.sfx += 1;
            let ink = Rect::new(*x - size * 0.4, *y - size * 0.5, size * 2.4, *size);
            match align_target(&page.px, w, h, &ink, *size) {
                Err(_) => m.sfx_refused += 1,
                Ok(o) => println!("  {name}: SFX at {:?} got {:?}", (x, y), o.bbox),
            }
        }
    }
}

fn inside(r: &Rect, c: (f64, f64)) -> bool {
    r.x <= c.0 && c.0 <= r.x + r.width && r.y <= c.1 && c.1 <= r.y + r.height
}

#[test]
fn synthetic_pages() {
    let mut total = Metrics::default();
    println!("| page | bubbles found | detections true | centre err mean/max px | align centred | SFX refused | TextShapeR top fits |");
    println!("|---|---|---|---|---|---|---|");
    let pages = [
        ("busy 1000×1500", SynthPage::busy(1000, 1500)),
        ("busy 1400×2000", SynthPage::busy(1400, 2000)),
        (
            "doubles 1200×900",
            SynthPage::new(
                1200,
                900,
                vec![
                    // Joined at a neck, diagonally and side by side.
                    Item::Double {
                        a: Ellipse { cx: 280.0, cy: 200.0, rx: 130.0, ry: 85.0 },
                        b: Ellipse { cx: 480.0, cy: 330.0, rx: 120.0, ry: 80.0 },
                        text: true,
                    },
                    Item::Double {
                        a: Ellipse { cx: 780.0, cy: 650.0, rx: 120.0, ry: 80.0 },
                        b: Ellipse { cx: 1000.0, cy: 650.0, rx: 110.0, ry: 75.0 },
                        text: false,
                    },
                    Item::Bubble { e: Ellipse { cx: 850.0, cy: 230.0, rx: 160.0, ry: 110.0 }, tail: true, text: true },
                    Item::Sfx { x: 250.0, y: 650.0, size: 80.0 },
                ],
            ),
        ),
    ];
    for (name, page) in &pages {
        let mut m = Metrics::default();
        synthetic(name, page, &mut m);
        m.row(name);
        total.bubbles += m.bubbles;
        total.found += m.found;
        total.detections += m.detections;
        total.true_detections += m.true_detections;
        total.centre_err.extend(m.centre_err);
        total.align_ok += m.align_ok;
        total.sfx += m.sfx;
        total.sfx_refused += m.sfx_refused;
        total.shaped += m.shaped;
        total.shaped_fit += m.shaped_fit;
    }
    total.row("all");
    // Floors (raise them as the algorithms improve).
    assert_eq!(total.found, total.bubbles, "every synthetic bubble is detected");
    assert_eq!(total.true_detections, total.detections, "no false detections");
    assert!(total.centre_err.iter().all(|e| *e <= 12.0), "{:?}", total.centre_err);
    assert_eq!(total.align_ok, total.bubbles, "Align centres every bubble (each lobe of a double)");
    // SFX over a light flat panel: the wand fills the panel, which its black frame seals and the
    // 4 em minimum of the 60× rule (WP1) lets through for large type. Floor at today's value.
    assert!(total.sfx_refused >= 2, "SFX refused {}/{}", total.sfx_refused, total.sfx);
    assert!(total.shaped_fit * 10 >= total.shaped * 9, "TextShapeR fits {}/{}", total.shaped_fit, total.shaped);
}

// ---------------------------------------------------------------------------------------------
// Local pages

/// The page composited from its visible pixel layers (normal blending over white), type layers
/// left out, as the engine's page image.
fn composite(file: &photocraft_psd::PsdFile) -> (Vec<[u8; 4]>, usize, usize) {
    let (w, h) = (file.header.width as usize, file.header.height as usize);
    let mut px = vec![[255.0f32; 3]; w * h];
    for layer in file.iter_layers() {
        let r = layer.record;
        if r.section_type() != photocraft_psd::tagged::SectionType::Other || !r.is_visible() || r.block(b"TySh").is_some() {
            continue;
        }
        let Ok(img) = layer.rgba8() else { continue };
        let op = f32::from(r.opacity) / 255.0 * f32::from(r.fill_opacity()) / 255.0;
        for y in 0..img.height as i64 {
            for x in 0..img.width as i64 {
                let (dx, dy) = (img.left as i64 + x, img.top as i64 + y);
                if dx < 0 || dy < 0 || dx >= w as i64 || dy >= h as i64 {
                    continue;
                }
                let i = ((y * img.width as i64 + x) * 4) as usize;
                let a = f32::from(img.data[i + 3]) / 255.0 * op;
                let p = &mut px[dy as usize * w + dx as usize];
                for (k, c) in p.iter_mut().enumerate() {
                    *c = *c * (1.0 - a) + f32::from(img.data[i + k]) * a;
                }
            }
        }
    }
    (px.into_iter().map(|p| [p[0] as u8, p[1] as u8, p[2] as u8, 255]).collect(), w, h)
}

#[test]
#[ignore = "local pages: set CAPISHOT_TYPER_PAGES"]
fn real_pages() {
    let Some(dir) = std::env::var_os("CAPISHOT_TYPER_PAGES") else {
        println!("CAPISHOT_TYPER_PAGES is not set: skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let bounds: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("bounds.json")).expect("bounds.json")).expect("bounds.json is JSON");
    let labels: serde_json::Value = std::fs::read(dir.join("labels.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    println!(
        "| page | layers | refused / expected | bubbles found / labelled | page detections (inside a text box) | TextShapeR human break in top 3 | top-1 line count |"
    );
    println!("|---|---|---|---|---|---|---|");
    let mut checked = 0;
    let mut failures = Vec::new();
    for (name, entry) in bounds.as_object().expect("bounds.json maps files to layers") {
        let path = dir.join(name);
        let Ok(bytes) = std::fs::read(&path) else {
            println!("| {name} | missing | | | | | |");
            continue;
        };
        let file = photocraft_psd::PsdFile::from_bytes(&bytes).expect("a readable PSD");
        let (px, w, h) = composite(&file);
        let layers = entry["t"].as_array().cloned().unwrap_or_default();
        let (mut expected_refusals, mut refused, mut bubbles, mut found) = (0, 0, 0, 0);
        let (mut multi, mut top3, mut count_ok) = (0, 0, 0);
        for l in &layers {
            let idx = l[0].as_u64().unwrap_or(0);
            let text = l[1].as_str().unwrap_or("");
            let b = l[2].as_array().map(|v| v.iter().filter_map(serde_json::Value::as_f64).collect::<Vec<_>>()).unwrap_or_default();
            if b.len() != 4 {
                continue;
            }
            let ink = Rect::new(b[0].max(0.0), b[1].max(0.0), (b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0));
            let label = labels[name.as_str()][idx.to_string()].as_str().unwrap_or("none");
            let lines: Vec<&str> = text.split(['\r', '\n']).filter(|s| !s.trim().is_empty()).collect();
            let size = ink.height / lines.len().max(1) as f64 * 0.8;
            let r = align_target(&px, w, h, &ink, size);
            if label == "bubble" {
                bubbles += 1;
                if r.is_ok() {
                    found += 1;
                }
            } else {
                expected_refusals += 1;
                match r {
                    Err(_) => refused += 1,
                    Ok(o) => println!("  {name} #{idx} {text:?} got a bubble {:?}", o.opened),
                }
            }
            // (d) TextShapeR into the text's own box (no outline): human breaks in the top 3?
            if lines.len() >= 2 {
                multi += 1;
                let joined = lines.join(" ");
                let opts =
                    Options { hyphenation: Hyphenation::for_text(None, &joined), width: Some(ink.width), height: Some(ink.height), ..Default::default() };
                let v = shaper::shape(&joined, &opts, &mut CharClass);
                let human: Vec<String> = lines.iter().map(|s| s.trim().to_string()).collect();
                if v.iter().take(3).any(|x| x.lines == human) {
                    top3 += 1;
                }
                if v.first().is_some_and(|x| x.lines.len() == human.len()) {
                    count_ok += 1;
                }
            }
        }
        let det = pagedetect::detect(&Page { px: &px, width: w, height: h }, (0.0, 0.0), &DetectOptions::default());
        // CAPISHOT_TYPER_DUMP=<dir>: the composite (PPM) and the detections (JSON), to look at.
        if let Some(out) = std::env::var_os("CAPISHOT_TYPER_DUMP") {
            let stem = name.replace(['/', '.'], "_");
            let mut ppm = format!("P6 {w} {h} 255\n").into_bytes();
            ppm.extend(px.iter().flat_map(|p| [p[0], p[1], p[2]]));
            let out = std::path::PathBuf::from(out);
            std::fs::write(out.join(format!("{stem}.ppm")), ppm).expect("write the composite");
            let boxes: Vec<_> =
                det.iter().map(|d| serde_json::json!({"bbox": [d.bbox.x, d.bbox.y, d.bbox.width, d.bbox.height], "confidence": d.confidence})).collect();
            std::fs::write(out.join(format!("{stem}.json")), serde_json::to_vec(&boxes).unwrap_or_default()).expect("write the detections");
        }
        let on_text = det
            .iter()
            .filter(|d| {
                layers.iter().any(|l| {
                    let b: Vec<f64> = l[2].as_array().map(|v| v.iter().filter_map(serde_json::Value::as_f64).collect()).unwrap_or_default();
                    b.len() == 4 && inside(&d.opened, ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0))
                })
            })
            .count();
        println!(
            "| {name} | {} | {refused}/{expected_refusals} | {found}/{bubbles} | {} ({on_text}) | {top3}/{multi} | {count_ok}/{multi} |",
            layers.len(),
            det.len()
        );
        failures.push((name.clone(), refused, expected_refusals, found, bubbles));
        checked += 1;
    }
    assert!(checked > 0, "no page from bounds.json was found under {}", dir.display());
    // Floors: text not in a speech bubble is never centred into the art; labelled bubbles are found.
    for (name, refused, expected, found, bubbles) in failures {
        assert!(refused + 1 >= expected, "{name}: non-bubble texts given a bubble: {}", expected - refused);
        assert_eq!(found, bubbles, "{name}: a labelled bubble was missed");
    }
}
