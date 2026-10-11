//! Timings for the exact-output speed-ups in `docs/exact-speedups-performance.md`: one timed run
//! per invocation, so before/after binaries can be run in alternating pairs, and the complete
//! output written as little-endian f32 sample bits for `cmp`.
//!
//! `cargo run --release -p photocraft-algo --example bench_exact_speedups -- <case> [arg] [out.bin]`
//!
//! Cases: `scale` (Content-Aware Scale of 6000×4000 RGBA to 90 % width), `blur <radius>`
//! (Gaussian Blur of a 7360×4912 RGBA8 layer), `median <radius>` (Median of 6000×4000 RGBA8),
//! `auto` (Content-Aware Fill's Auto sampling area of a 4382×4000 window), `fill` (Content-Aware
//! Fill of a 200 px hole in 3872×2592).
use photocraft_algo::content_aware::{FillOptions, auto_sampling, fill};
use photocraft_algo::{FilterParams, apply_in, output_area, seam};
use photocraft_color::PixelFormat;
use photocraft_geom::Rect;
use photocraft_raster::Surface;
use std::time::Instant;

/// A photo-like RGBA image: a lit disc on a backdrop with deterministic noise and soft alpha.
fn pixels(w: usize, h: usize) -> Vec<f32> {
    let mut px = vec![0.0f32; w * h * 4];
    let (cx, cy, r) = (w as f32 * 0.55, h as f32 * 0.5, w.min(h) as f32 / 3.0);
    for (i, p) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = (i % w, i / w);
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let n = ((x.wrapping_mul(73_856_093) ^ y.wrapping_mul(19_349_663)) % 997) as f32 / 997.0 - 0.5;
        let blob = (-(((fx - 0.4) * 3.0).powi(2) + ((fy - 0.55) * 4.0).powi(2))).exp();
        let inside = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() < r;
        let (r0, g0, b0) = if inside { (0.85, 0.3 + 0.2 * (fx * 40.0).sin(), 0.15) } else { (0.35, 0.45, 0.55) };
        let a = if x % 97 < 3 { 0.5 } else { 1.0 };
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() / 255.0;
        *p = [q(r0 + 0.1 * blob + n * 0.08), q(g0 + 0.1 * blob + n * 0.08), q(b0 + 0.2 * fy + n * 0.08), a];
    }
    px
}

fn surface(w: usize, h: usize) -> Surface {
    let mut s = Surface::new(PixelFormat::RGBA8);
    s.write_region(Rect::new(0, 0, w as i32, h as i32), &pixels(w, h));
    s
}

fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
    (0..w * h).map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) < r).collect()
}

fn filtered(w: usize, h: usize, p: FilterParams) -> (f64, Vec<f32>) {
    let s = surface(w, h);
    let r = Rect::new(0, 0, w as i32, h as i32);
    let area = output_area(&p, s.content_bounds(), r, None).intersect(&r);
    let t = Instant::now();
    let out = apply_in(&s, &p, area, r, None, r);
    let ms = t.elapsed().as_secs_f64() * 1e3;
    (ms, out.read_region(r))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let case = args.get(1).map_or("blur", String::as_str);
    let num = |i: usize, d: f32| args.get(i).and_then(|v| v.parse().ok()).unwrap_or(d);
    let (ms, out, path) = match case {
        "scale" => {
            let (w, h) = (6000, 4000);
            let img = pixels(w, h);
            let t = Instant::now();
            let out = seam::carve(w, h, 4, &img, None, w * 9 / 10, h);
            (t.elapsed().as_secs_f64() * 1e3, out, args.get(2))
        }
        "blur" => {
            let (ms, out) = filtered(7360, 4912, FilterParams::GaussianBlur { radius: num(2, 40.0) });
            (ms, out, args.get(3))
        }
        "median" => {
            let (ms, out) = filtered(6000, 4000, FilterParams::Median { radius: num(2, 1.0) });
            (ms, out, args.get(3))
        }
        "auto" => {
            let (w, h) = (4382, 4000);
            let (img, hole) = (pixels(w, h), disc(w, h, 2200.0, 2000.0, 560.0));
            let t = Instant::now();
            let area = auto_sampling(w, h, 4, &img, &hole);
            (t.elapsed().as_secs_f64() * 1e3, area.into_iter().map(f32::from).collect(), args.get(2))
        }
        "fill" => {
            let (w, h) = (3872, 2592);
            let (img, hole) = (pixels(w, h), disc(w, h, 1400.0, 900.0, 100.0));
            let source = vec![true; w * h];
            let t = Instant::now();
            let out = fill(w, h, 4, &img, &hole, &source, &FillOptions::default());
            (t.elapsed().as_secs_f64() * 1e3, out, args.get(2))
        }
        other => {
            eprintln!("unknown case `{other}`: scale | blur <r> | median <r> | auto | fill");
            std::process::exit(2);
        }
    };
    println!("{case} {}: {ms:.1} ms", args.get(2).filter(|a| a.parse::<f32>().is_ok()).map_or("", String::as_str));
    if let Some(p) = path {
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_bits().to_le_bytes()).collect();
        if let Err(e) = std::fs::write(p, bytes) {
            eprintln!("write {p}: {e}");
            std::process::exit(1);
        }
    }
}
