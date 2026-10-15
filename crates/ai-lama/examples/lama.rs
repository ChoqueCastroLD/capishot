//! Runs Manga LaMa on a PNG + mask PNG (native; for verification and timing).
//!
//! ```sh
//! cargo run --release -p photocraft-ai-lama --features flex,rayon --example lama -- \
//!     WEIGHTS.safetensors IMAGE.png MASK.png OUT_PREFIX [flex|wgpu] [raw|tiles] [max_side]
//! ```
//!
//! `raw` runs the whole image once (mirror-padded to a multiple of 8, like the PyTorch
//! reference) and also writes the generator output as planar `f32` (`OUT_PREFIX.raw.f32`);
//! `tiles` runs the crop-around-mask pipeline.

use std::time::Instant;

use photocraft_ai_lama::Lama;
use photocraft_ai_lama::burn::tensor::Device;
use photocraft_ai_lama::tiles::{self, Fit, Rect, Tile, TileOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        return Err("usage: lama WEIGHTS IMAGE MASK OUT_PREFIX [flex|wgpu] [raw|tiles] [max_side]".into());
    }
    let dev_name = a.get(5).map(String::as_str).unwrap_or("flex");
    let mode = a.get(6).map(String::as_str).unwrap_or("raw");
    let max_side: usize = a.get(7).map(|s| s.parse()).transpose()?.unwrap_or(512);
    let device = match dev_name {
        #[cfg(feature = "flex")]
        "flex" => Device::flex(),
        #[cfg(feature = "wgpu")]
        "wgpu" => Device::wgpu_options().init()?,
        other => return Err(format!("device {other} not compiled in").into()),
    };
    let t0 = Instant::now();
    let bytes = std::fs::read(&a[1])?;
    let model = Lama::from_safetensors(&bytes, &device)?;
    eprintln!("loaded {:?} in {:.2?}", model.arch(), t0.elapsed());
    let img = image::open(&a[2])?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut rgb: Vec<f32> = img.as_raw().iter().map(|v| *v as f32 / 255.0).collect();
    let mask: Vec<f32> = image::open(&a[3])?.to_luma8().as_raw().iter().map(|v| *v as f32 / 255.0).collect();
    let out_prefix = &a[4];
    let tile_list = if mode == "raw" {
        let full = Rect { x: 0, y: 0, w, h };
        vec![Tile { rect: full, core: full, work_w: w.div_ceil(8) * 8, work_h: h.div_ceil(8) * 8, fit: Fit::Pad }]
    } else {
        tiles::plan(&mask, w, h, &TileOptions { max_side, ..TileOptions::default() })?
    };
    let src = rgb.clone();
    for (i, t) in tile_list.iter().enumerate() {
        let inp = tiles::extract(&src, &mask, w, h, t)?;
        // Warm-up run on the first tile so the timing excludes kernel compilation/autotune.
        if i == 0 && std::env::var_os("LAMA_WARMUP").is_some() {
            model.generate(&inp.rgb, &inp.mask, inp.w, inp.h)?;
        }
        let t1 = Instant::now();
        let out = model.generate(&inp.rgb, &inp.mask, inp.w, inp.h)?;
        eprintln!("tile {i} {:?} work {}x{} ({:?}): {:.2?}", t.rect, inp.w, inp.h, t.fit, t1.elapsed());
        if mode == "raw" {
            let n = inp.w * inp.h;
            let mut planar = vec![0f32; n * 3];
            for p in 0..n {
                for c in 0..3 {
                    planar[c * n + p] = out[p * 3 + c];
                }
            }
            let bytes: Vec<u8> = planar.iter().flat_map(|v| v.to_le_bytes()).collect();
            std::fs::write(format!("{out_prefix}.raw.f32"), bytes)?;
        }
        tiles::paste(&mut rgb, &mask, w, h, t, &out)?;
    }
    // 8-bit output as the reference does: truncate, not round.
    let px: Vec<u8> = rgb.iter().map(|v| (v * 255.0).clamp(0.0, 255.0) as u8).collect();
    image::RgbImage::from_raw(w as u32, h as u32, px).ok_or("buffer size")?.save(format!("{out_prefix}.png"))?;
    eprintln!("total {:.2?}", t0.elapsed());
    Ok(())
}
