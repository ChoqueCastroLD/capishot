//! Upscale benchmark on synthetic pages (no files from the repository, nothing personal).
//!
//!   bench_upscale <model id> [--tile N|auto] [--runs N] [--device gpu|cpu] [--page gray|color|<png>]
//!                 [--out out.png] [--ref ref.png]
//!
//! Loads the model from the native models folder (`%LOCALAPPDATA%\Capishot\models` on Windows,
//! or `PHOTOCRAFT_AI_MODELS`), runs [`photocraft_ai::upscale::upscale`] `--runs` times on the page
//! (a synthetic manga page of 1000×1500 for grayscale models, a colour one of 760×1200 for colour
//! models) and prints one line per run: seconds, peak VRAM (from `nvidia-smi`, when present,
//! minus what was in use before the model loaded), the device. `--ref` prints the PSNR of the
//! result against an earlier output (8-bit), `--out` saves it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::BufRead;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use photocraft_ai::catalog::{Arch, Content};
use photocraft_ai::runtime::{Runtime, block_on};
use photocraft_ai::tile::TileOptions;
use photocraft_ai::upscale::{Loaded, default_tiles, upscale};
use photocraft_ai::{Planar, Progress};

struct Print(Instant);
impl Progress for Print {
    fn report(&self, done: u64, total: u64, _: &str) {
        eprint!("\r  {done}/{total} tiles {:.1}s   ", self.0.elapsed().as_secs_f64());
    }
}

/// Peak `memory.used` (MiB) reported by `nvidia-smi` every 50 ms while alive.
struct VramWatch {
    peak: Arc<AtomicU64>,
    child: Option<std::process::Child>,
}

impl VramWatch {
    fn start() -> Self {
        let peak = Arc::new(AtomicU64::new(0));
        let child = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits", "-lms", "50"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok();
        let mut child = child;
        if let Some(out) = child.as_mut().and_then(|c| c.stdout.take()) {
            let p = peak.clone();
            std::thread::spawn(move || {
                for line in std::io::BufReader::new(out).lines().map_while(Result::ok) {
                    if let Ok(v) = line.trim().parse::<u64>() {
                        p.fetch_max(v, Ordering::Relaxed);
                    }
                }
            });
        }
        Self { peak, child }
    }
    fn reset(&self) {
        self.peak.store(0, Ordering::Relaxed);
    }
    fn peak(&self) -> Option<u64> {
        std::thread::sleep(std::time::Duration::from_millis(150));
        self.child.as_ref().map(|_| self.peak.load(Ordering::Relaxed))
    }
}

impl Drop for VramWatch {
    fn drop(&mut self) {
        if let Some(c) = self.child.as_mut() {
            let _ = c.kill();
        }
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A deterministic manga-like test page: panel borders, line art, screentones of several
/// densities, a gradient, hatching and blocks of "text".
pub fn synthetic_gray(w: usize, h: usize) -> Planar {
    let mut d = vec![1.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32, y as f32);
            let mut v: f32 = 1.0;
            // Screentone (45°, period 7 px), density growing to the right, in the top band.
            if y > 40 && y < 460 && x > 40 && x < w - 40 {
                let (u, s) = ((fx + fy) / 7.0, (fx - fy) / 7.0);
                let (du, ds) = (u - u.round(), s - s.round());
                let r = (du * du + ds * ds).sqrt();
                let radius = 0.08 + 0.42 * (fx - 40.0) / (w as f32 - 80.0);
                v = smoothstep(radius - 0.06, radius + 0.06, r);
            }
            // A smooth vertical gradient band.
            if (500..760).contains(&y) && x > 40 && x < w - 40 {
                v = 0.15 + 0.8 * (fy - 500.0) / 260.0;
            }
            // Hatching with fine lines.
            if (800..1100).contains(&y) && x > 40 && x < w / 2 {
                let t = ((fx * 0.6 + fy) / 5.0).fract();
                v = smoothstep(0.0, 0.18, t.min(1.0 - t) * 2.0).max(0.12);
            }
            // Concentric circles (line art) on the right.
            if (800..1100).contains(&y) && x >= w / 2 && x < w - 40 {
                let (cx, cy) = (w as f32 * 0.75, 950.0);
                let r = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
                let ring = (r / 18.0).fract();
                v = smoothstep(0.03, 0.11, ring.min(1.0 - ring));
            }
            // "Text": small dark blocks in a speech area.
            if (1150..1440).contains(&y) && x > 80 && x < w - 80 {
                let (cx, cy) = ((x - 80) % 22, (y - 1150) % 30);
                if cx < 16 && cy < 20 && ((x / 22 + y / 30) % 7 != 0) {
                    let stroke = (cx % 5 == 0) || (cy % 6 == 0);
                    v = if stroke { 0.05 } else { v };
                }
            }
            // Panel borders.
            let border = |a: usize, b: usize| a.abs_diff(b) < 3;
            if border(y, 30) || border(y, 470) || border(y, 780) || border(y, 1120) || border(y, h - 30) || border(x, 30) || border(x, w - 30) {
                v = 0.0;
            }
            d[y * w + x] = v.clamp(0.0, 1.0);
        }
    }
    Planar::new(w, h, 1, d).unwrap()
}

/// A deterministic colour illustration-like page: gradients, outlined discs, colour halftone,
/// hatching.
pub fn synthetic_color(w: usize, h: usize) -> Planar {
    let mut d = vec![0.0f32; w * h * 3];
    let plane = w * h;
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let mut rgb = [0.55 + 0.4 * fy, 0.7 - 0.3 * fx, 0.95 - 0.5 * fy];
            for (i, &(cx, cy, r, col)) in
                [(0.3, 0.25, 0.18, [0.9, 0.3, 0.25]), (0.7, 0.4, 0.15, [0.2, 0.7, 0.35]), (0.45, 0.7, 0.22, [0.95, 0.85, 0.3])].iter().enumerate()
            {
                let dist = ((fx - cx) * w as f32).hypot((fy - cy) * h as f32) / w as f32;
                if dist < r {
                    let shade = 0.75 + 0.25 * ((fx - cx + fy - cy) * -6.0).tanh();
                    rgb = [col[0] * shade, col[1] * shade, col[2] * shade];
                    if i == 2 {
                        // Colour halftone inside the last disc.
                        let (u, s) = ((x + y) as f32 / 6.0, (x as f32 - y as f32) / 6.0);
                        let rr = ((u - u.round()).powi(2) + (s - s.round()).powi(2)).sqrt();
                        if rr < 0.3 {
                            rgb = [0.85, 0.2, 0.55];
                        }
                    }
                }
                let edge = (dist - r).abs() * w as f32;
                if edge < 2.5 {
                    rgb = [0.05, 0.04, 0.06];
                }
            }
            if fy > 0.88 {
                let t = ((x as f32 * 0.7 + y as f32) / 6.0).fract();
                if t < 0.25 {
                    rgb = [0.1, 0.1, 0.2];
                }
            }
            for (c, v) in rgb.iter().enumerate() {
                d[c * plane + y * w + x] = v.clamp(0.0, 1.0);
            }
        }
    }
    Planar::new(w, h, 3, d).unwrap()
}

fn save(p: &Planar, path: &str) {
    let px = p.to_interleaved_u8();
    let (w, h) = (p.width as u32, p.height as u32);
    let img = if p.channels == 1 {
        image::DynamicImage::ImageLuma8(image::GrayImage::from_raw(w, h, px).unwrap())
    } else {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, px).unwrap())
    };
    img.save(path).unwrap();
}

fn psnr(a: &Planar, path: &str) -> f64 {
    let r = image::open(path).unwrap();
    let rb: Vec<u8> = if a.channels == 1 { r.to_luma8().into_raw() } else { r.to_rgb8().into_raw() };
    let ab = a.to_interleaved_u8();
    assert_eq!(ab.len(), rb.len(), "reference size differs");
    let se: f64 = ab.iter().zip(&rb).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum();
    let mse = se / ab.len() as f64;
    if mse == 0.0 { f64::INFINITY } else { 10.0 * (255.0f64 * 255.0 / mse).log10() }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let opt = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let id = a.get(1).expect("model id");
    let model = photocraft_ai::catalog::model(id).unwrap_or_else(|| panic!("unknown model {id}"));
    let runs: usize = opt("--runs").and_then(|s| s.parse().ok()).unwrap_or(2);
    let device = opt("--device").unwrap_or_else(|| "gpu".into());
    let dir = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Capishot").join("models")).unwrap_or_else(|| "models".into());
    let cache = photocraft_ai::cache::DiskCache::in_dir(&dir);
    let bytes = cache.load(model).unwrap_or_else(|| panic!("{} not in {}", model.file, dir.display()));
    let gray = model.content == Content::Grayscale && model.arch != Arch::Fdat;
    let mut page = match opt("--page").as_deref() {
        Some("gray") | None if gray => synthetic_gray(1000, 1500),
        Some("color") | None => synthetic_color(760, 1200),
        Some("gray") => synthetic_gray(1000, 1500).with_channels(3),
        Some(path) => {
            let img = image::open(path).unwrap();
            if gray {
                let g = img.to_luma8();
                Planar::from_interleaved_u8(g.width() as usize, g.height() as usize, 1, g.as_raw()).unwrap()
            } else {
                let c = img.to_rgb8();
                Planar::from_interleaved_u8(c.width() as usize, c.height() as usize, 3, c.as_raw()).unwrap()
            }
        }
    };
    // Sensitivity probe: the page rounded to f16 (what an f16 network sees of it).
    if opt("--perturb").as_deref() == Some("f16") {
        for v in &mut page.data {
            *v = half::f16::from_f32(*v).to_f32();
        }
    }
    if let Some(p) = opt("--save-page") {
        save(&page, &p);
    }
    let d = default_tiles(model);
    let tiles = match opt("--tile").as_deref() {
        None | Some("auto") => None,
        Some(t) => Some(TileOptions { tile: t.parse().expect("tile"), overlap: d.overlap }),
    };
    let watch = VramWatch::start();
    let base = watch.peak().unwrap_or(0);
    let t0 = Instant::now();
    let directml = device == "directml";
    let rt = match device.as_str() {
        "cpu" | "directml" => Runtime::cpu(),
        _ => Runtime::gpu_blocking().expect("gpu"),
    };
    let rt = match opt("--fp16").as_deref() {
        Some("on") => rt.with_half(true),
        Some("off") => rt.with_half(false),
        _ => rt,
    };
    let (adapter, api) = if directml {
        ("directml".to_string(), "dml".to_string())
    } else {
        rt.info().map_or_else(|| ("cpu".to_string(), "-".to_string()), |i| (i.name.clone(), i.api.clone()))
    };
    eprintln!("adapter {adapter} api {api} fp16 {}", rt.half());
    let init = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    let loaded = if directml { new_directml(model, &bytes, gray) } else { Loaded::new(&rt, model, &bytes, gray).expect("load") };
    let load = t1.elapsed().as_secs_f64();
    eprintln!(
        "{id} x{} {}x{} ch{} tile {} dev {device} init {init:.2}s load {load:.2}s",
        model.scale,
        page.width,
        page.height,
        page.channels,
        tiles.map_or_else(|| format!("auto({})", d.tile), |t| t.tile.to_string())
    );
    let mut out = None;
    for run in 1..=runs {
        watch.reset();
        let t = Instant::now();
        let r = block_on(upscale(&rt, &loaded, &page, tiles.unwrap_or(d), &Print(t))).expect("upscale");
        let s = t.elapsed().as_secs_f64();
        eprintln!();
        let vram = watch.peak().map(|p| p.saturating_sub(base));
        println!(
            "RESULT model={id} scale={} tile={} device={device} api={api} fp16={} run={run} seconds={s:.2} vram_peak_mib={} out={}x{}",
            model.scale,
            tiles.map_or_else(|| format!("auto({})", d.tile), |t| t.tile.to_string()),
            rt.half(),
            vram.map_or("n/a".into(), |v| v.to_string()),
            r.width,
            r.height
        );
        out = Some(r);
    }
    let out = out.expect("at least one run");
    if let Some(r) = opt("--ref") {
        println!("PSNR vs {r}: {:.2} dB", psnr(&out, &r));
    }
    if let Some(o) = opt("--out") {
        save(&out, &o);
    }
}

#[cfg(all(feature = "directml", windows))]
fn new_directml(model: &'static photocraft_ai::catalog::ModelInfo, bytes: &[u8], gray: bool) -> Loaded {
    photocraft_ai::directml::available().expect("directml");
    Loaded::new_directml(model, bytes, gray).expect("load")
}

#[cfg(not(all(feature = "directml", windows)))]
fn new_directml(_: &'static photocraft_ai::catalog::ModelInfo, _: &[u8], _: bool) -> Loaded {
    panic!("built without the directml feature")
}
