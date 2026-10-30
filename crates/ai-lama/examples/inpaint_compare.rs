//! Compares an inpainting generator with a PyTorch reference (native; weights and references
//! stay outside the repository; see README.md › Verification).
//!
//! ```sh
//! cargo run --release -p photocraft-ai-lama --features flex,rayon --example inpaint_compare -- \
//!     WEIGHTS.safetensors REF_PREFIX SIZE [flex|wgpu] [runs]
//! ```
//!
//! `REF_PREFIX.rgb.f32` (H×W×3, 0..1), `.mask.f32` (H×W, 1 = fill) and `.out.f32` (H×W×3, the
//! raw generator output in 0..1) come from `ref_migan.py`.

use std::time::Instant;

use photocraft_ai_lama::Generator;
use photocraft_ai_lama::burn::tensor::Device;

fn read(path: &str) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    Ok(std::fs::read(path)?.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 4 {
        return Err("usage: inpaint_compare WEIGHTS REF_PREFIX SIZE [flex|wgpu] [runs]".into());
    }
    let s: usize = a[3].parse()?;
    let device = match a.get(4).map(String::as_str).unwrap_or("flex") {
        #[cfg(feature = "flex")]
        "flex" => Device::flex(),
        #[cfg(feature = "wgpu")]
        "wgpu" => Device::wgpu_options().init()?,
        other => return Err(format!("device {other} not compiled in").into()),
    };
    let runs: usize = a.get(5).map(|r| r.parse()).transpose()?.unwrap_or(2);
    let t0 = Instant::now();
    let g = Generator::from_safetensors(&std::fs::read(&a[1])?, &device)?;
    eprintln!("loaded in {:.2?}", t0.elapsed());
    let (rgb, mask, reference) = (read(&format!("{}.rgb.f32", a[2]))?, read(&format!("{}.mask.f32", a[2]))?, read(&format!("{}.out.f32", a[2]))?);
    for run in 0..runs {
        let t = Instant::now();
        let out = g.generate(&rgb, &mask, s, s)?;
        let dt = t.elapsed();
        let (mut max, mut max8, mut n8, mut se) = (0f32, 0u8, 0usize, 0f64);
        for (x, r) in out.iter().zip(&reference) {
            max = max.max((x - r).abs());
            let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
            let d8 = q(*x).abs_diff(q(*r));
            max8 = max8.max(d8);
            n8 += usize::from(d8 > 0);
            se += f64::from(x - r).powi(2);
        }
        let psnr = 10.0 * (1.0 / (se / out.len() as f64)).log10();
        println!(
            "run {run}: {s}x{s} in {:.3}s; max|diff| {max:.2e}, 8-bit max {max8}/255 ({n8} of {} values differ), PSNR {psnr:.1} dB",
            dt.as_secs_f64(),
            out.len()
        );
    }
    Ok(())
}
