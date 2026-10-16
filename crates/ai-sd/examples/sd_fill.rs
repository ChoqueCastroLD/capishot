//! Generative Fill on an image file, natively (the end-to-end check and the quality runs).
//!
//! `sd_fill <weights dir> <image> <x,y,w,h> <prompt> <out prefix> [key=value…]`
//! keys: `variants` (2), `seed` (1), `steps` (4), `sampler` (tcd|lcm, tcd), `lora` (tcd|lcm|none,
//! = sampler), `device` (cpu|gpu), `f16` (0|1), `tiny` (0|1: TAESD), `side` (512), `gamma` (0.3),
//! `guidance` (1), `unet` (ds8|sdxs: the Ultra prototype, LCM sampling, TAESD), `prefill` (0|1: pull–push fill of the hole first), `strength` (1).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, deprecated, clippy::chunks_exact_to_as_chunks)]

use std::time::Instant;

use photocraft_ai_sd::burn::tensor::{DType, Device};
use photocraft_ai_sd::crop;
use photocraft_ai_sd::prompt;
use photocraft_ai_sd::runner::{Loader, Params, generate};
use photocraft_ai_sd::scheduler::Sampler;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = std::path::PathBuf::from(&a[1]);
    let img = image::open(&a[2]).unwrap().to_rgb8();
    let r: Vec<usize> = a[3].split(',').map(|v| v.parse().unwrap()).collect();
    let text = &a[4];
    let out = &a[5];
    let kv: std::collections::HashMap<String, String> = a[6..].iter().filter_map(|s| s.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let get = |k: &str, d: &str| kv.get(k).cloned().unwrap_or_else(|| d.to_string());
    let sampler_name = get("sampler", "tcd");
    let gamma: f64 = get("gamma", "0.3").parse().unwrap();
    let sampler = if sampler_name == "lcm" { Sampler::Lcm } else { Sampler::Tcd { gamma } };
    let lora = get("lora", &sampler_name);
    let (w, h) = (img.width() as usize, img.height() as usize);
    let rgb: Vec<f32> = img.as_raw().iter().map(|&v| f32::from(v) / 255.0).collect();
    let mut cover = vec![0f32; w * h];
    for y in r[1]..(r[1] + r[3]).min(h) {
        for x in r[0]..(r[0] + r[2]).min(w) {
            cover[y * w + x] = 1.0;
        }
    }
    if let Err(e) = prompt::check(text) {
        println!("refused: {e}");
        return;
    }
    let en = prompt::translate(text);
    println!("prompt: {text:?} -> {en:?}");
    let side: usize = get("side", "512").parse().unwrap();
    let c = crop::plan(&cover, w, h, side).unwrap();
    let mut crgb = c.extract(&rgb, w, 3);
    let cmask = c.extract(&cover, w, 1);
    if get("prefill", "0") == "1" {
        crgb = crop::prefill(&crgb, &cmask, c.work_w, c.work_h);
    }
    let device = if get("device", "cpu") == "gpu" { Device::wgpu_options().init().unwrap() } else { Device::flex() };
    let dtype = if get("f16", "0") == "1" { DType::F16 } else { DType::F32 };
    let t0 = Instant::now();
    let mut loader = Loader::new(&device, dtype).unwrap();
    let sdxs = get("unet", "ds8") == "sdxs";
    let mut files = vec!["sd15-text-ds8-f16.safetensors".to_string(), "taesd-f16.safetensors".into()];
    if sdxs {
        files.push("sdxs-ds-unet-f16.safetensors".into());
    } else {
        files.extend(["sd15-vae-enc-ds8-f16.safetensors".to_string(), "sd15-vae-dec-ds8-f16.safetensors".into()]);
        files.extend((0..4).map(|i| format!("ds8-inpaint-unet-f16.part{i}of4.safetensors")));
    }
    if lora != "none" && !sdxs {
        files.push(format!("{lora}-sd15-lora-f16.safetensors"));
    }
    for f in &files {
        let bytes = std::fs::read(dir.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        loader.add(&bytes).unwrap();
    }
    let models = loader.finish().unwrap();
    let load_s = t0.elapsed().as_secs_f64();
    let p = Params {
        prompt: en,
        negative: String::new(),
        rgb: crgb,
        mask: cmask,
        width: c.work_w,
        height: c.work_h,
        variants: get("variants", "2").parse().unwrap(),
        seed: get("seed", "1").parse().unwrap(),
        steps: get("steps", "4").parse().unwrap(),
        strength: get("strength", "1").parse().unwrap(),
        guidance: get("guidance", "1").parse().unwrap(),
        sampler,
        tiny_vae: get("tiny", "0") == "1",
    };
    let t1 = Instant::now();
    let mut last = Instant::now();
    let outs = pollster::block_on(generate(&models, &p, &mut |d, t| {
        println!("  step {d}/{t} {:.2}s", last.elapsed().as_secs_f64());
        last = Instant::now();
        true
    }))
    .unwrap();
    let gen_s = t1.elapsed().as_secs_f64();
    for (i, o) in outs.iter().enumerate() {
        let back = c.restore(o);
        let mut im = img.clone();
        for y in 0..c.h {
            for x in 0..c.w {
                let (ix, iy) = (c.x + x, c.y + y);
                let cv = cover[iy * w + ix];
                if cv > 0.0 {
                    let px = im.get_pixel_mut(ix as u32, iy as u32);
                    for k in 0..3 {
                        let g = back[(y * c.w + x) * 3 + k];
                        px.0[k] = ((f32::from(px.0[k]) / 255.0 * (1.0 - cv) + g * cv) * 255.0).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
        im.save(format!("{out}-{i}.png")).unwrap();
    }
    println!("{} variants {}x{} {} steps: load {load_s:.1}s, generate {gen_s:.1}s", outs.len(), c.work_w, c.work_h, p.steps);
}
