//! PSD save profile: the stages of `export(doc, "x.psd")` on scanlation-like pages.
//! `cargo run --release -p photocraft-io --example bench_psd_save [-- --reps N]`
//! Pages: 2000×3000 with 10 and 30 layers, 4000×6000 with 10. Stage columns are measured on
//! their own (model, bytes, psd-compiler render), then the whole save.
use std::time::Instant;

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer};
use photocraft_geom::{Rect, Size};

/// A scan-like background (noise over a gradient: RLE-poor) and `layers - 1` partial layers:
/// flat cleaning patches and "text" stripes, each over a fifth of the page.
fn page(w: u32, h: u32, layers: usize) -> Document {
    let mut doc = Document::new("bench", Size::new(w, h), ColorMode::Rgb, SampleType::U8);
    let fmt = doc.pixel_format();
    let mut bg = Layer::raster("page", fmt);
    let s = bg.surface_mut().unwrap();
    let mut seed = 0x9e37_79b9u32;
    let band = 64;
    for y in (0..h as i32).step_by(band) {
        for x in (0..w as i32).step_by(band) {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let v = 0.75 + (seed % 1000) as f32 / 4000.0;
            s.fill_rect(Rect::new(x, y, x + band as i32, y + band as i32), &[v, v * 0.98, v * 0.95, 1.0]);
        }
    }
    // Fine grain so the background doesn't compress to nothing.
    for y in (0..h as i32).step_by(3) {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let v = (seed % 100) as f32 / 400.0;
        s.fill_rect(Rect::new(0, y, w as i32, y + 1), &[v, v, v, 1.0]);
    }
    doc.layers.push(bg);
    for i in 1..layers {
        let mut l = Layer::raster(format!("layer {i}"), fmt);
        let s = l.surface_mut().unwrap();
        let (lw, lh) = (w as i32 / 2, h as i32 * 2 / 5);
        let x0 = (i as i32 * 211) % (w as i32 - lw);
        let y0 = (i as i32 * 397) % (h as i32 - lh);
        s.fill_rect(Rect::new(x0, y0, x0 + lw, y0 + lh), &[1.0, 1.0, 1.0, 1.0]);
        for k in 0..(lh / 12) {
            let y = y0 + k * 12;
            let len = lw * (3 + (k * 7 + i as i32) % 5) / 8;
            s.fill_rect(Rect::new(x0 + 8, y, x0 + 8 + len, y + 6), &[0.05, 0.05, 0.05, 1.0]);
        }
        doc.layers.push(l);
    }
    doc
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let reps = args.iter().position(|a| a == "--reps").and_then(|i| args.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(3usize);
    println!("bands {}", photocraft_io::pixels::bands(1000).len());
    println!("{:<28} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}", "page", "model", "bytes", "compile", "save", "save p50", "MB");
    let real: Vec<(String, Document)> = std::env::args()
        .skip(1)
        .filter(|a| a.ends_with(".psd"))
        .map(|p| (p.clone(), photocraft_io::import(&p, &std::fs::read(&p).unwrap()).unwrap().document))
        .collect();
    let synthetic = [(2000, 3000, 10), (2000, 3000, 30), (4000, 6000, 10)].map(|(w, h, n)| (format!("{w}x{h}/{n}"), page(w, h, n)));
    let synthetic = if std::env::var_os("REAL_ONLY").is_some() { Vec::new() } else { synthetic.to_vec() };
    for (name, doc) in synthetic.into_iter().chain(real) {
        let (w, h) = (doc.size.width, doc.size.height);
        let o = photocraft_io::PsdExportOptions { merged_image: false, ..Default::default() };
        let t = Instant::now();
        let (file, _) = photocraft_io::document_to_psd_with(&doc, &o);
        let model = ms(t);
        let t = Instant::now();
        let bytes = file.to_bytes().unwrap();
        let ser = ms(t);
        let t = Instant::now();
        let _ = photocraft_io::psdc::compile(&bytes, &Default::default()).unwrap();
        let compile = ms(t);
        let t = Instant::now();
        let parsed = psd_compiler::Document::parse(&bytes).unwrap();
        let parse = ms(t);
        let t = Instant::now();
        let options = psd_compiler::RenderOptions { keep_text_raster: true, ..Default::default() };
        let _ = psd_compiler::render(&parsed, &psd_compiler::FontDb::new(), &options);
        eprintln!("  parse {parse:.0} render {:.0}", ms(t));
        let t = Instant::now();
        let off = photocraft_io::ExportOptions { psdc_composite: false, ..Default::default() };
        let _ = photocraft_io::export(&doc, "x.psd", &off).unwrap();
        eprintln!("  save with the compositor's merged image {:.0}", ms(t));
        let t = Instant::now();
        let _ = photocraft_compose::flatten(&doc);
        eprintln!("  compose flatten {:.0}", ms(t));
        let mut saves = Vec::new();
        let mut size = 0;
        for _ in 0..reps {
            let t = Instant::now();
            let out = photocraft_io::export(&doc, "x.psd", &Default::default()).unwrap();
            saves.push(ms(t));
            size = out.bytes.len();
        }
        let first = saves[0];
        saves.sort_by(f64::total_cmp);
        println!(
            "{:<28} {model:>8.0} {ser:>8.0} {compile:>8.0} {first:>8.0} {:>8.0} {:>8.1}",
            format!("{name} {w}x{h}").chars().rev().take(28).collect::<String>().chars().rev().collect::<String>(),
            saves[saves.len() / 2],
            size as f64 / 1e6
        );
    }
}
