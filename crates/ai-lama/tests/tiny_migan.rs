//! A tiny MI-GAN (resolution 32, 4–8 channels) with deterministic pseudo-random weights, checked
//! against an independent eager PyTorch implementation (`tiny_migan_reference.py`), plus padding
//! and graceful-failure checks. The released weights are verified against the official PyTorch
//! generator outside the test suite (README.md › Verification).
#![cfg(feature = "flex")]
#![allow(clippy::excessive_precision)]

use photocraft_ai_lama::burn::tensor::Device;
use photocraft_ai_lama::{Generator, Migan};

#[cfg(feature = "wgpu")]
mod gpu;

const RES: usize = 32;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / 16777216.0) * 2.0 - 1.0
    }
}

/// Channels at a resolution (`min(128 / res, 8)`, as upstream's `min(ch_base // res, ch_max)`).
fn ch(res: usize) -> usize {
    (128 / res).min(8)
}

/// (name, shape) in file order.
fn tensors() -> Vec<(String, Vec<usize>)> {
    let mut v: Vec<(String, Vec<usize>)> = Vec::new();
    let mut sep = |p: &str, ci: usize, co: usize, noise: Option<usize>| {
        v.push((format!("{p}.conv1.weight"), vec![ci, 1, 3, 3]));
        v.push((format!("{p}.conv1.bias"), vec![ci]));
        v.push((format!("{p}.conv2.weight"), vec![co, ci, 1, 1]));
        if let Some(r) = noise {
            v.push((format!("{p}.noise_const"), vec![r, r]));
            v.push((format!("{p}.noise_strength"), vec![]));
        }
    };
    let mut res = RES;
    while res >= 8 {
        let (i, o) = (ch(res), ch(res / 2));
        sep(&format!("encoder.b{res}.conv1"), i, i, None);
        sep(&format!("encoder.b{res}.conv2"), i, o, None);
        res /= 2;
    }
    sep("encoder.b4.conv1", ch(4), ch(4), None);
    sep("encoder.b4.conv2", ch(4), ch(4), None);
    sep("synthesis.b4.conv1", ch(4), ch(4), None);
    sep("synthesis.b4.conv2", ch(4), ch(4), None);
    let mut res = 8;
    while res <= RES {
        sep(&format!("synthesis.b{res}.conv1"), ch(res / 2), ch(res), Some(res));
        sep(&format!("synthesis.b{res}.conv2"), ch(res), ch(res), Some(res));
        res *= 2;
    }
    v.push((format!("encoder.b{RES}.fromrgb.weight"), vec![ch(RES), 4, 1, 1]));
    v.push((format!("encoder.b{RES}.fromrgb.bias"), vec![ch(RES)]));
    let mut res = 4;
    while res <= RES {
        v.push((format!("synthesis.b{res}.torgb.weight"), vec![3, ch(res), 1, 1]));
        v.push((format!("synthesis.b{res}.torgb.bias"), vec![3]));
        res *= 2;
    }
    v
}

/// A safetensors file (F32): weights at He scale √(2/fan_in), biases ±0.1, noise N-ish ±1 with
/// strengths ±0.1.
fn tiny_safetensors() -> Vec<u8> {
    let mut rng = Lcg(11);
    let mut header = String::from("{\"__metadata__\":{\"arch\":\"migan\",\"resolution\":\"32\"}");
    let mut data: Vec<u8> = Vec::new();
    for (name, shape) in tensors() {
        let n: usize = shape.iter().product();
        let scale = match shape.len() {
            4 => (2.0 / (shape[1] * shape[2] * shape[3]) as f32).sqrt(),
            2 => 1.0,
            _ => 0.1,
        };
        let start = data.len();
        for _ in 0..n {
            data.extend_from_slice(&(rng.next() * scale).to_le_bytes());
        }
        header.push_str(&format!(",\"{name}\":{{\"dtype\":\"F32\",\"shape\":{shape:?},\"data_offsets\":[{start},{}]}}", data.len()));
    }
    header.push('}');
    while !header.len().is_multiple_of(8) {
        header.push(' ');
    }
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(&data);
    out
}

fn tiny_input(w: usize, h: usize) -> (Vec<f32>, Vec<f32>) {
    let mut rng = Lcg(5);
    let rgb: Vec<f32> = (0..w * h * 3).map(|_| rng.next() * 0.5 + 0.5).collect();
    let mask = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            if (8..20).contains(&x) && (10..22).contains(&y) { 1.0 } else { 0.0 }
        })
        .collect();
    (rgb, mask)
}

/// Writes the fixture for `tiny_migan_reference.py`:
/// `cargo test -p photocraft-ai-lama --features flex --test tiny_migan -- --ignored dump`
#[test]
#[ignore]
fn dump_tiny_fixture() {
    let dir = std::env::temp_dir().join("migan-tiny");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tiny.safetensors"), tiny_safetensors()).unwrap();
    let (rgb, mask) = tiny_input(RES, RES);
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    std::fs::write(dir.join("rgb.f32"), f(&rgb)).unwrap();
    std::fs::write(dir.join("mask.f32"), f(&mask)).unwrap();
}

/// (pixel index, channel, expected) from `tiny_migan_reference.py`, plus the mean of all outputs.
const EXPECTED: &[(usize, usize, f32)] = &include!("tiny_migan_expected.in");
const EXPECTED_MEAN: f32 = include!("tiny_migan_expected_mean.in");

#[test]
fn tiny_migan_matches_pytorch() {
    let device = Device::flex();
    let g = Generator::from_safetensors(&tiny_safetensors(), &device).unwrap();
    let Generator::Migan(m) = &g else { panic!("arch metadata picks MI-GAN") };
    assert_eq!((m.resolution(), m.multiple(), g.max_side()), (32, 8, 32));
    let (rgb, mask) = tiny_input(RES, RES);
    let out = g.generate(&rgb, &mask, RES, RES).unwrap();
    assert_eq!(out.len(), RES * RES * 3);
    assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    for &(i, c, want) in EXPECTED {
        let got = out[i * 3 + c];
        assert!((got - want).abs() < 2e-5, "pixel {i} channel {c}: {got} vs {want}");
    }
    let mean = out.iter().sum::<f32>() / out.len() as f32;
    assert!((mean - EXPECTED_MEAN).abs() < 1e-5, "mean {mean} vs {EXPECTED_MEAN}");
}

/// Within WebGPU's workgroup memory (see `tiny_model`): the same output as the CPU. Skipped
/// without a GPU adapter.
#[cfg(feature = "wgpu")]
#[test]
fn runs_on_a_gpu_with_little_workgroup_memory() {
    let (rgb, mask) = tiny_input(RES, RES);
    let want = Generator::from_safetensors(&tiny_safetensors(), &Device::flex()).unwrap().generate(&rgb, &mask, RES, RES).unwrap();
    for limit in gpu::LIMITS {
        let Some(device) = gpu::limited(limit) else { return eprintln!("no GPU adapter: skipped") };
        let got = Generator::from_safetensors(&tiny_safetensors(), &device).unwrap().generate(&rgb, &mask, RES, RES).unwrap();
        let d = gpu::max_diff(&want, &got);
        assert!(d < 1e-3, "{limit} B: off by {d}");
    }
}

#[test]
fn tiles_smaller_than_the_resolution_are_padded() {
    let m = Migan::from_safetensors(&tiny_safetensors(), &Device::flex()).unwrap();
    for (w, h) in [(24, 24), (13, 30), (1, 1), (32, 9)] {
        let (rgb, mask) = tiny_input(w, h);
        let out = m.generate(&rgb, &mask, w, h).unwrap();
        assert_eq!(out.len(), w * h * 3, "{w}x{h}");
        assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{w}x{h}");
    }
}

#[test]
fn bad_weights_and_sizes_are_errors() {
    let device = Device::flex();
    assert!(Migan::from_safetensors(b"", &device).is_err());
    let good = tiny_safetensors();
    assert!(Migan::from_safetensors(&good[..good.len() - 64], &device).is_err(), "truncated");
    let m = Migan::from_safetensors(&good, &device).unwrap();
    let (rgb, mask) = tiny_input(RES, RES);
    assert!(m.generate(&rgb, &mask, 33, 32).is_err(), "over the resolution");
    assert!(m.generate(&rgb, &mask, 0, 32).is_err());
    assert!(m.generate(&rgb[..30], &mask, RES, RES).is_err(), "buffer mismatch");
    let mut nan = rgb.clone();
    nan[7] = f32::NAN;
    assert!(m.generate(&nan, &mask, RES, RES).unwrap().iter().all(|v| v.is_finite()));
}
