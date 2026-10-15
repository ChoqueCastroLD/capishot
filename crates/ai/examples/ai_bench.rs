//! Developer bench for photocraft-ai (weights and pages stay outside the repository).
//!
//!   ai_bench compare <model.safetensors> <ref-prefix> <cpu|gpu> [gray]
//!       runs one tile against a PyTorch reference (`<prefix>.in.bin`, `.out.bin`, `.json`)
//!   ai_bench page <model.safetensors> <in.png|jpg> <out.png> <cpu|gpu> [gray] [tile]
//!       upscales a page with tiling and reports the time

use std::time::Instant;

use photocraft_ai::compact::Compact;
use photocraft_ai::esrgan::Esrgan;
use photocraft_ai::runtime::{Network, Runtime, block_on, run_tiled};
use photocraft_ai::tile::TileOptions;
use photocraft_ai::weights::Weights;
use photocraft_ai::{Planar, Progress};

struct Print;
impl Progress for Print {
    fn report(&self, done: u64, total: u64, _: &str) {
        eprint!("\r{done}/{total} tiles");
    }
}

fn runtime(kind: &str) -> Runtime {
    match kind {
        "gpu" => Runtime::gpu_blocking().expect("gpu"),
        _ => Runtime::cpu(),
    }
}

/// ESRGAN or Compact, by the file's `arch` metadata.
fn load(rt: &Runtime, w: &Weights, gray: bool) -> Box<dyn Network> {
    if w.metadata.get("arch").map(String::as_str) == Some("compact") {
        let scale = std::env::var("SCALE").ok().and_then(|s| s.parse().ok()).unwrap_or(Compact::config(w).expect("config").upscale);
        Box::new(Compact::load(rt, w, scale, gray).expect("load"))
    } else {
        Box::new(Esrgan::load(rt, w, gray).expect("load"))
    }
}

fn read_f32(path: &str) -> Vec<f32> {
    let b = std::fs::read(path).expect(path);
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let gray = a.iter().any(|x| x == "gray");
    match a.get(1).map(String::as_str) {
        Some("compare") => {
            let w = Weights::from_safetensors(&std::fs::read(&a[2]).expect("model")).expect("weights");
            let rt = runtime(&a[4]);
            let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(format!("{}.json", a[3])).expect("json")).expect("json");
            let s = meta["size"].as_u64().expect("size") as usize;
            let inp = Planar::new(s, s, 3, read_f32(&format!("{}.in.bin", a[3]))).expect("input");
            let reference = read_f32(&format!("{}.out.bin", a[3]));
            let t0 = Instant::now();
            let net = load(&rt, &w, gray);
            let load = t0.elapsed();
            let input = if gray { inp.crop(0, 0, s, s).with_channels(1) } else { inp };
            let input = photocraft_ai::runtime::reflect_pad(
                &input,
                s.div_ceil(net.multiple_of()) * net.multiple_of(),
                s.div_ceil(net.multiple_of()) * net.multiple_of(),
            );
            for run in 0..2 {
                let t1 = Instant::now();
                let out = block_on(rt.read(net.forward(rt.image(&input).expect("tensor")))).expect("run");
                let dt = t1.elapsed();
                let os = s * net.scale();
                let out = out.crop(0, 0, os, os);
                let plane = os * os;
                let mut max = 0f32;
                let mut max8 = 0u8;
                let mut se = 0f64;
                let channels = out.channels;
                for c in 0..channels {
                    for i in 0..plane {
                        let (x, r) = (out.data[c * plane + i], reference[c * plane + i]);
                        max = max.max((x - r).abs());
                        let (x8, r8) = (photocraft_ai::image::to_u8(x), photocraft_ai::image::to_u8(r));
                        max8 = max8.max(x8.abs_diff(r8));
                        let d = (x.clamp(0.0, 1.0) - r.clamp(0.0, 1.0)) as f64;
                        se += d * d;
                    }
                }
                let mse = se / (channels * plane) as f64;
                let psnr = if mse == 0.0 { f64::INFINITY } else { 10.0 * (1.0 / mse).log10() };
                println!(
                    "run {run}: {}x{} -> {os}x{os} ch {channels}: max|diff| {max:.2e} (8-bit {max8}/255), PSNR {psnr:.1} dB, load {:.2}s, forward {:.2}s, ref torch {:.2}s",
                    s,
                    s,
                    load.as_secs_f64(),
                    dt.as_secs_f64(),
                    meta["seconds"].as_f64().unwrap_or(0.0)
                );
            }
        }
        Some("page") => {
            let w = Weights::from_safetensors(&std::fs::read(&a[2]).expect("model")).expect("weights");
            let rt = runtime(&a[5]);
            let img = image::open(&a[3]).expect("image");
            let (iw, ih) = (img.width() as usize, img.height() as usize);
            let input = if gray {
                let g = img.to_luma8();
                Planar::from_interleaved_u8(iw, ih, 1, g.as_raw()).expect("planar")
            } else {
                let c = img.to_rgb8();
                Planar::from_interleaved_u8(iw, ih, 3, c.as_raw()).expect("planar")
            };
            let input = if gray && a.iter().any(|x| x == "levels") { photocraft_ai::mangajanai::auto_levels(&input) } else { input };
            let tile = a.get(7).and_then(|t| t.parse().ok()).unwrap_or(128);
            let net = load(&rt, &w, gray);
            let t0 = Instant::now();
            let out = block_on(run_tiled(&rt, net.as_ref(), &input, TileOptions { tile, overlap: 16 }, &Print)).expect("run");
            let dt = t0.elapsed();
            eprintln!();
            println!("{iw}x{ih} -> {}x{} in {:.1}s ({} tile {tile})", out.width, out.height, dt.as_secs_f64(), a[5]);
            let px = out.to_interleaved_u8();
            let buf = if out.channels == 1 {
                image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(out.width as u32, out.height as u32, px).expect("buf"))
            } else {
                image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(out.width as u32, out.height as u32, px).expect("buf"))
            };
            buf.save(&a[4]).expect("save");
        }
        _ => eprintln!("usage: see the file header"),
    }
}
