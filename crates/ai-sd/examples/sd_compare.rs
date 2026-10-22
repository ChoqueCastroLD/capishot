//! Compares the port against diffusers reference tensors (`scripts/ref_sd.py`).
//!
//! `cargo run --release -p photocraft-ai-sd --features flex,wgpu --example sd_compare -- \
//!     <ref.safetensors> <weights dir> [cpu|gpu] [f32|f16] [clip,unet,vae,taesd,lcm,tcd]`
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, deprecated, clippy::chunks_exact_to_as_chunks)]

use std::time::Instant;

use photocraft_ai_sd::burn::tensor::{DType, Device, Tensor};
use photocraft_ai_sd::clip::TextEncoder;
use photocraft_ai_sd::nn::Store;
use photocraft_ai_sd::pipeline::{Decoder, Encoder, Generation, Request, decode};
use photocraft_ai_sd::scheduler::Sampler;
use photocraft_ai_sd::taesd::{TinyDecoder, TinyEncoder};
use photocraft_ai_sd::unet::Unet;
use photocraft_ai_sd::vae::{VaeDecoder, VaeEncoder};
use safetensors::SafeTensors;

fn floats(st: &SafeTensors, name: &str) -> (Vec<f32>, Vec<usize>) {
    let t = st.tensor(name).unwrap_or_else(|_| panic!("ref has no `{name}`"));
    let v = t.data().chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
    (v, t.shape().to_vec())
}

fn read(t: Tensor<4>) -> Vec<f32> {
    t.cast(DType::F32).into_data().convert::<f32>().to_vec::<f32>().unwrap()
}

fn report(name: &str, got: &[f32], want: &[f32], secs: f64) {
    assert_eq!(got.len(), want.len(), "{name}: length");
    let (mut max, mut se, mut peak) = (0f64, 0f64, 0f64);
    for (g, w) in got.iter().zip(want) {
        let d = f64::from(g - w).abs();
        max = max.max(d);
        se += d * d;
        peak = peak.max(f64::from(w.abs()));
    }
    let mse = se / got.len() as f64;
    let psnr = if mse > 0.0 { 10.0 * (peak * peak / mse).log10() } else { f64::INFINITY };
    println!("{name:12} max|d| {max:.3e}  rmse {:.3e}  PSNR(peak {peak:.2}) {psnr:.1} dB  {secs:.2}s", mse.sqrt());
}

fn load(device: &Device, dt: DType, files: &[std::path::PathBuf]) -> Store {
    let mut s = Store::new(device, dt).unwrap();
    for f in files {
        let bytes = std::fs::read(f).unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        s.add(&bytes).unwrap();
    }
    s
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let ref_bytes = std::fs::read(&a[1]).unwrap();
    let r = SafeTensors::deserialize(&ref_bytes).unwrap();
    let dir = std::path::PathBuf::from(&a[2]);
    let device = match a.get(3).map(String::as_str) {
        Some("gpu") => Device::wgpu_options().init().unwrap(),
        _ => Device::flex(),
    };
    let dt = if a.get(4).map(String::as_str) == Some("f16") { DType::F16 } else { DType::F32 };
    let what = a.get(5).cloned().unwrap_or_else(|| "clip,unet,vae,taesd,lcm,tcd".into());
    let has = |w: &str| what.split(',').any(|x| x == w);
    let unet_files: Vec<_> = (0..4).map(|i| dir.join(format!("ds8-inpaint-unet-f16.part{i}of4.safetensors"))).collect();

    let text_store = load(&device, dt, &[dir.join("sd15-text-ds8-f16.safetensors")]);
    let text = TextEncoder::from_store(&text_store).unwrap();
    if has("clip") {
        let (ids, _) =
            (r.tensor("clip.ids").unwrap().data().chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u32).collect::<Vec<_>>(), ());
        let mut row = [0u32; 77];
        row.copy_from_slice(&ids);
        let t0 = Instant::now();
        let out = text.forward(&[row]).unwrap();
        let got = out.cast(DType::F32).into_data().convert::<f32>().to_vec::<f32>().unwrap();
        report("clip", &got, &floats(&r, "clip.out").0, t0.elapsed().as_secs_f64());
    }
    if has("unet") {
        let s = load(&device, dt, &unet_files);
        let unet = Unet::from_store(&s).unwrap();
        let (x, xs) = floats(&r, "unet.x");
        let (c, cs) = floats(&r, "unet.ctx");
        let x = s.tensor(x, [xs[0], xs[1], xs[2], xs[3]]).unwrap();
        let c = s.tensor(c, [cs[0], cs[1], cs[2]]).unwrap();
        let t0 = Instant::now();
        let got = read(unet.forward(x, 500.0, &c).unwrap());
        report("unet", &got, &floats(&r, "unet.out").0, t0.elapsed().as_secs_f64());
    }
    if has("vae") {
        let s = load(&device, dt, &[dir.join("sd15-vae-enc-ds8-f16.safetensors"), dir.join("sd15-vae-dec-ds8-f16.safetensors")]);
        let (enc, dec) = (VaeEncoder::from_store(&s).unwrap(), VaeDecoder::from_store(&s).unwrap());
        let (img, is) = floats(&r, "vae.img");
        let t0 = Instant::now();
        let got = read(enc.forward(s.tensor(img, [is[0], is[1], is[2], is[3]]).unwrap()).unwrap());
        report("vae.enc", &got, &floats(&r, "vae.lat").0, t0.elapsed().as_secs_f64());
        let (z, zs) = floats(&r, "vae.z");
        let t0 = Instant::now();
        let got = read(dec.forward(s.tensor(z, [zs[0], zs[1], zs[2], zs[3]]).unwrap()).unwrap());
        report("vae.dec", &got, &floats(&r, "vae.dec").0, t0.elapsed().as_secs_f64());
    }
    if has("taesd") {
        let s = load(&device, dt, &[dir.join("taesd-f16.safetensors")]);
        let (enc, dec) = (TinyEncoder::from_store(&s).unwrap(), TinyDecoder::from_store(&s).unwrap());
        let (img, is) = floats(&r, "vae.img");
        let img: Vec<f32> = img.iter().map(|v| (v + 1.0) / 2.0).collect();
        let t0 = Instant::now();
        let got = read(enc.forward(s.tensor(img, [is[0], is[1], is[2], is[3]]).unwrap()).unwrap());
        report("taesd.enc", &got, &floats(&r, "taesd.lat").0, t0.elapsed().as_secs_f64());
        let (z, zs) = floats(&r, "vae.z");
        let t0 = Instant::now();
        let got = read(dec.forward(s.tensor(z, [zs[0], zs[1], zs[2], zs[3]]).unwrap()).unwrap());
        report("taesd.dec", &got, &floats(&r, "taesd.dec").0, t0.elapsed().as_secs_f64());
    }
    for (tag, sampler) in [("lcm", Sampler::Lcm), ("tcd", Sampler::Tcd { gamma: 0.3 })] {
        if !has(tag) {
            continue;
        }
        let mut s = load(&device, dt, &unet_files);
        let lora = load(&device, dt, &[dir.join(format!("{tag}-sd15-lora-f16.safetensors"))]);
        let t0 = Instant::now();
        let n = photocraft_ai_sd::lora::fuse(&mut s, &lora, 1.0).unwrap();
        drop(lora);
        let unet = Unet::from_store(&s).unwrap();
        let vae = load(&device, dt, &[dir.join("sd15-vae-enc-ds8-f16.safetensors"), dir.join("sd15-vae-dec-ds8-f16.safetensors")]);
        let (enc, dec) = (VaeEncoder::from_store(&vae).unwrap(), VaeDecoder::from_store(&vae).unwrap());
        let fuse_s = t0.elapsed().as_secs_f64();
        let (rgb, rs) = floats(&r, "pipe.rgb");
        let (mask, _) = floats(&r, "pipe.mask");
        let ids: Vec<u32> = r.tensor("clip.ids").unwrap().data().chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as u32).collect();
        let mut prompt = [0u32; 77];
        prompt.copy_from_slice(&ids);
        let req =
            Request { prompt, negative: None, guidance: 1.0, rgb, mask, width: rs[1], height: rs[0], variants: 2, seed: 7, steps: 4, strength: 1.0, sampler };
        let t0 = Instant::now();
        let mut g = Generation::start(&s, &text, &unet, Encoder::Full(&enc), &req).unwrap();
        while !g.done() {
            g.step(&s, &unet).unwrap();
        }
        let got = read(decode(Decoder::Full(&dec), g.latents()).unwrap());
        report(&format!("pipe.{tag}"), &got, &floats(&r, &format!("pipe.{tag}.img")).0, t0.elapsed().as_secs_f64());
        println!("  ({n} LoRA layers fused in {fuse_s:.1}s incl. load)");
        if let Ok(out) = std::env::var("SD_COMPARE_PNG") {
            let [w, h] = [rs[1] as u32, rs[0] as u32];
            let plane = (w * h) as usize;
            let mut im = image::RgbImage::new(w * 2, h);
            for v in 0..2usize {
                for y in 0..h {
                    for x in 0..w {
                        let i = (y * w + x) as usize;
                        let px = |c: usize| (got[v * 3 * plane + c * plane + i] * 255.0).round().clamp(0.0, 255.0) as u8;
                        im.put_pixel(v as u32 * w + x, y, image::Rgb([px(0), px(1), px(2)]));
                    }
                }
            }
            im.save(format!("{out}-{tag}.png")).unwrap();
        }
    }
}
