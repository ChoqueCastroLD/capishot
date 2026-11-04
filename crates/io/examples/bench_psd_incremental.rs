//! PSD save of a synthetic scanlation page, full vs incremental ([`photocraft_io::PsdExportCache`])
//! after a one-layer edit. The page: the raw scan (screentone, line art, noise), a full-canvas
//! Clean layer (white patches over the bubbles) and 16 small typeset layers.
//!
//! ```sh
//! cargo run --release -p photocraft-io --example bench_psd_incremental -- [--size 3000x4300] [--reps 5] [--no-previews]
//! ```
//!
//! Each row is the median (and min) of `--reps` runs; every incremental save is checked
//! byte-identical to the full one.
use std::time::Instant;

use photocraft_doc::{Document, Layer};
use photocraft_geom::Rect;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn scan(w: u32, h: u32) -> photocraft_codecs::Image {
    let mut px = vec![0u8; w as usize * h as usize * 3];
    for (y, row) in px.chunks_mut(w as usize * 3).enumerate() {
        for x in 0..w as usize {
            let n = ((x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 997) as f32 / 997.0 - 0.5;
            // Screentone dots in the lower half, panel borders and line art strokes.
            let tone = y > h as usize / 2 && ((x % 6) as i32 - 3).pow(2) + ((y % 6) as i32 - 3).pow(2) < 5;
            let border = x % 700 < 6 || y % 1000 < 6;
            let line = ((x as f32 * 0.013).sin() * 140.0 + (y as f32 * 0.007).cos() * 90.0) as i32 % 37 == 0;
            let v = if border || line { 0.08 } else if tone { 0.35 } else { 0.94 } + n * 0.05;
            let q = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            row[x * 3..x * 3 + 3].copy_from_slice(&[q, q, q]);
        }
    }
    photocraft_codecs::Image::from_raw(w, h, photocraft_codecs::ChannelLayout::Rgb, photocraft_codecs::SampleType::U8, px).expect("image")
}

/// Black glyph-like strokes in a `w×h` box at (x, y).
fn type_layer(name: &str, fmt: photocraft_color::PixelFormat, x: i32, y: i32, w: u32, h: u32, seed: u32) -> Layer {
    let mut l = Layer::raster(name, fmt);
    if let Some(s) = l.surface_mut() {
        let data: Vec<f32> = (0..w * h)
            .flat_map(|i| {
                let (px, py) = (i % w, i / w);
                let ink = (px / 3 + seed) % 7 < 2 && (py / 18) % 2 == 0 && py % 18 > 3;
                if ink { [0.0, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 0.0, 0.0] }
            })
            .collect();
        s.write_region(Rect::from_xywh(x, y, w, h), &data);
    }
    l
}

fn page(w: u32, h: u32) -> Document {
    let png = photocraft_codecs::encode(&scan(w, h), photocraft_codecs::Format::Png, &Default::default()).expect("png");
    let mut doc = photocraft_io::import("page.png", &png).expect("import").document;
    let fmt = doc.pixel_format();
    let mut clean = Layer::raster("Clean", fmt);
    if let Some(s) = clean.surface_mut() {
        for i in 0..16u32 {
            let (bx, by) = ((i % 4) * w / 4 + w / 32, (i / 4) * h / 4 + h / 32);
            s.fill_rect(Rect::from_xywh(bx as i32, by as i32, w / 6, h / 10), &[1.0, 1.0, 1.0, 1.0]);
        }
    }
    doc.layers.push(clean);
    for i in 0..16u32 {
        let (bx, by) = ((i % 4) * w / 4 + w / 24, (i / 4) * h / 4 + h / 24);
        doc.layers.push(type_layer(&format!("Text {i}"), fmt, bx as i32, by as i32, w / 8, h / 14, i));
    }
    doc
}

fn median(mut t: Vec<f64>) -> (f64, f64) {
    t.sort_by(f64::total_cmp);
    (t[t.len() / 2], t[0])
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (w, h) = arg(&args, "--size").and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((3000u32, 4300u32));
    let reps: usize = arg(&args, "--reps").and_then(|s| s.parse().ok()).unwrap_or(5);
    // `--no-previews`: without the thumbnail resource, which renders the whole page small.
    let previews = !args.iter().any(|a| a == "--no-previews");
    let base = page(w, h);
    println!("page {w}×{h}, {} layers, {reps} reps (median / min ms)", base.layers.len());
    for (what, target) in [("one text layer", 5usize), ("the Clean layer", 1usize)] {
        let mut full = Vec::new();
        let mut inc = Vec::new();
        let cache = photocraft_io::PsdExportCache::default();
        let plain = photocraft_io::ExportOptions { image_previews: previews, ..Default::default() };
        let cached = photocraft_io::ExportOptions { psd_cache: Some(cache.clone()), ..plain.clone() };
        let mut doc = base.clone();
        photocraft_io::export(&doc, "x.psd", &cached).expect("first save");
        for rep in 0..reps {
            // A brush dab on the layer: one tile-sized spot changes.
            let at = (rep as i32 * 37) % 200;
            if let Some(s) = doc.layers[target].surface_mut() {
                let r = s.content_bounds();
                s.fill_rect(Rect::from_xywh(r.x0 + 10 + at, r.y0 + 10, 24, 24), &[0.9, 0.1, 0.1, 1.0]);
            }
            let t = Instant::now();
            let a = photocraft_io::export(&doc, "x.psd", &plain).expect("full").bytes;
            full.push(t.elapsed().as_secs_f64() * 1000.0);
            let t = Instant::now();
            let b = photocraft_io::export(&doc, "x.psd", &cached).expect("incremental").bytes;
            inc.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(a == b, "the incremental save differs from the full one");
        }
        let (f, fm) = median(full);
        let (i, im) = median(inc);
        println!("edit {what:>16}: full {f:8.1} / {fm:8.1}   incremental {i:8.1} / {im:8.1}   ({:.1}x)", f / i);
    }
}
