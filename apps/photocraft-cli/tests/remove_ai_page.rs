//! Remove with AI on real pages: after a rough stroke, nothing of what it touched is left.
//!
//! Opt-in, on local pages that are never committed: `CAPISHOT_REMOVE_AI_CASES` names a JSON file
//! with a list of cases, and Manga LaMa's weights must be where the CLI finds them
//! (`CAPISHOT_LAMA_WEIGHTS`). Without the variable the test passes without doing anything. Each
//! case:
//!
//! ```json
//! {"name": "bubble", "page": "page.jpg", "size": 42, "strokes": [[[515, 1060], [640, 1060]]],
//!  "white": {"seed": [500, 1200], "margin": 4}, "blur": 0, "max": 0.06}
//! {"name": "sfx on tone", "page": "sfx.png", "size": 30, "strokes": [...],
//!  "reference": "tone.png", "blur": 3, "max": 0.1}
//! ```
//!
//! The residue is the largest deviation, over the stroke's footprint, of the result from what
//! should be there: the bubble's white (`white`: measured inside the bubble around `seed`, `margin`
//! pixels in from its outline) or a clean `reference` page, both box-blurred by `blur` pixels (so a
//! screentone compares by its tone, not dot by dot). It must be at most `max` (0..=1). The same
//! stroke through the Remove tool's AI mode (the hole grown by 2 pixels, a hard edge) is reported
//! next to it. With `CAPISHOT_REMOVE_AI_OUT` set, both results are written there as PNGs.
#![cfg(feature = "ai")]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

struct Gray {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

fn load(path: &Path) -> Gray {
    let img = photocraft_codecs::decode(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let v = img.to_rgba8().as_chunks::<4>().0.iter().map(|c| (0.299 * f32::from(c[0]) + 0.587 * f32::from(c[1]) + 0.114 * f32::from(c[2])) / 255.0).collect();
    Gray { w, h, v }
}

/// Box blur of radius `r` (summed-area table).
fn blur(g: &Gray, r: usize) -> Vec<f32> {
    if r == 0 {
        return g.v.clone();
    }
    let (w, h) = (g.w, g.h);
    let mut sat = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        for x in 0..w {
            sat[(y + 1) * (w + 1) + x + 1] = f64::from(g.v[y * w + x]) + sat[y * (w + 1) + x + 1] + sat[(y + 1) * (w + 1) + x] - sat[y * (w + 1) + x];
        }
    }
    let mut out = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (x0, y0, x1, y1) = (x.saturating_sub(r), y.saturating_sub(r), (x + r + 1).min(w), (y + r + 1).min(h));
            let s = sat[y1 * (w + 1) + x1] - sat[y0 * (w + 1) + x1] - sat[y1 * (w + 1) + x0] + sat[y0 * (w + 1) + x0];
            out[y * w + x] = (s / ((x1 - x0) * (y1 - y0)) as f64) as f32;
        }
    }
    out
}

/// The pixels within `size / 2` of the strokes' polylines.
fn footprint(w: usize, h: usize, strokes: &[Vec<[f64; 2]>], size: f64) -> Vec<bool> {
    let r = size / 2.0;
    let mut m = vec![false; w * h];
    for s in strokes {
        let segs: Vec<([f64; 2], [f64; 2])> = if s.len() == 1 { vec![(s[0], s[0])] } else { s.windows(2).map(|p| (p[0], p[1])).collect() };
        for (a, b) in segs {
            let (x0, x1) = ((a[0].min(b[0]) - r).floor().max(0.0) as usize, ((a[0].max(b[0]) + r).ceil() as usize).min(w));
            let (y0, y1) = ((a[1].min(b[1]) - r).floor().max(0.0) as usize, ((a[1].max(b[1]) + r).ceil() as usize).min(h));
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let len2 = dx * dx + dy * dy;
            for y in y0..y1 {
                for x in x0..x1 {
                    let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
                    let t = if len2 > 0.0 { (((px - a[0]) * dx + (py - a[1]) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                    let (qx, qy) = (a[0] + t * dx - px, a[1] + t * dy - py);
                    if qx * qx + qy * qy <= r * r {
                        m[y * w + x] = true;
                    }
                }
            }
        }
    }
    m
}

/// The inside of the bubble around `seed` on the original page, letters included, `margin`
/// pixels in from the outline; and its white (the median of its light pixels).
fn bubble(g: &Gray, seed: [usize; 2], margin: usize) -> (Vec<bool>, f32) {
    let (w, h) = (g.w, g.h);
    let light = |i: usize| g.v[i] > 0.5;
    let flood = |start: Vec<usize>, pass: &dyn Fn(usize) -> bool| {
        let mut seen = vec![false; w * h];
        let mut stack = start;
        for &i in &stack {
            seen[i] = true;
        }
        while let Some(i) = stack.pop() {
            let (x, y) = (i % w, i / w);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h && !seen[ny * w + nx] && pass(ny * w + nx) {
                    seen[ny * w + nx] = true;
                    stack.push(ny * w + nx);
                }
            }
        }
        seen
    };
    let white = flood(vec![seed[1] * w + seed[0]], &light);
    // Its letters: what the page's edge can't reach without crossing the white.
    let border: Vec<usize> = (0..w * h).filter(|&i| (i % w == 0 || i / w == 0 || i % w == w - 1 || i / w == h - 1) && !white[i]).collect();
    let outside = flood(border, &|i| !white[i]);
    let mut inside: Vec<bool> = outside.iter().map(|o| !o).collect();
    for _ in 0..margin {
        let prev = inside.clone();
        for i in 0..w * h {
            let (x, y) = (i % w, i / w);
            if prev[i] && (x == 0 || y == 0 || x + 1 == w || y + 1 == h || !prev[i - 1] || !prev[i + 1] || !prev[i - w] || !prev[i + w]) {
                inside[i] = false;
            }
        }
    }
    let mut lights: Vec<f32> = (0..w * h).filter(|&i| white[i]).map(|i| g.v[i]).collect();
    lights.sort_by(f32::total_cmp);
    (inside, lights[lights.len() / 2])
}

/// Runs `cmd` on `page` and returns the result.
fn run(page: &Path, cmd: &str, params: &Value, out: &Path) -> Gray {
    let o = Command::new(env!("CARGO_BIN_EXE_capishot-cli"))
        .arg("run")
        .arg(page)
        .args(["--cmd", cmd, "--params", &params.to_string(), "--out"])
        .arg(out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{cmd}: {}\n{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    load(out)
}

#[test]
fn a_rough_stroke_leaves_no_residue_on_real_pages() {
    let Some(cases) = std::env::var_os("CAPISHOT_REMOVE_AI_CASES") else { return };
    let cases = PathBuf::from(cases);
    let dir = cases.parent().unwrap_or(Path::new(".")).to_path_buf();
    let list: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&cases).unwrap()).unwrap();
    let out_dir = std::env::var_os("CAPISHOT_REMOVE_AI_OUT").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut failures = Vec::new();
    for case in &list {
        let name = case["name"].as_str().unwrap_or("case");
        let page = dir.join(case["page"].as_str().unwrap());
        let size = case["size"].as_f64().unwrap();
        let strokes: Vec<Vec<[f64; 2]>> = serde_json::from_value(case["strokes"].clone()).unwrap();
        let orig = load(&page);
        let r = case["blur"].as_u64().unwrap_or(0) as usize;
        let (area, target): (Vec<bool>, Vec<f32>) = if let Some(white) = case.get("white") {
            let seed: [usize; 2] = serde_json::from_value(white["seed"].clone()).unwrap();
            let (inside, level) = bubble(&orig, seed, white["margin"].as_u64().unwrap_or(4) as usize);
            (inside, vec![level; orig.w * orig.h])
        } else {
            let reference = load(&dir.join(case["reference"].as_str().unwrap()));
            (vec![true; orig.w * orig.h], blur(&reference, r))
        };
        let foot = footprint(orig.w, orig.h, &strokes, size);
        let measure = |g: &Gray| {
            let b = blur(g, r);
            (0..g.w * g.h).filter(|&i| foot[i] && area[i]).map(|i| (b[i] - target[i]).abs()).fold(0.0f32, f32::max)
        };
        let pts: Vec<Vec<[f64; 3]>> = strokes.iter().map(|s| s.iter().map(|p| [p[0], p[1], 1.0]).collect()).collect();
        let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        let after = run(&page, "edit.removeAi", &json!({"strokes": pts, "size": size}), &out_dir.join(format!("{slug}-after.png")));
        let before = run(
            &page,
            "paint.remove",
            &json!({"strokes": pts, "size": size, "ai": true, "model": "manga-lama", "quality": "best", "maxSide": 768, "margin": 128}),
            &out_dir.join(format!("{slug}-before.png")),
        );
        let (b, a) = (measure(&before), measure(&after));
        let max = case["max"].as_f64().unwrap_or(0.06) as f32;
        println!("{name}: residue {:.1}/255 before, {:.1}/255 after (at most {:.1})", b * 255.0, a * 255.0, max * 255.0);
        if a > max {
            failures.push(format!("{name}: {:.1}/255", a * 255.0));
        }
    }
    assert!(failures.is_empty(), "residue left: {failures:?}");
}
