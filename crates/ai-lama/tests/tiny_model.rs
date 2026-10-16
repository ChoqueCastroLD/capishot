//! A tiny LaMa (ngf 8, 2 blocks) with deterministic pseudo-random weights, checked against values
//! from an independent PyTorch implementation (eager convs + `torch.fft.rfftn/irfftn`, see
//! `tiny_reference.py`), plus graceful-failure checks. The real weights are verified against the
//! PyTorch checkpoint outside the test suite (README.md › Verification).
#![cfg(feature = "flex")]
#![allow(clippy::excessive_precision)]

use photocraft_ai_lama::Lama;
use photocraft_ai_lama::burn::tensor::Device;

#[cfg(feature = "wgpu")]
mod gpu;

const NGF: usize = 8;
const BLOCKS: usize = 2;
const W: usize = 72;
const H: usize = 64;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / 16777216.0) * 2.0 - 1.0
    }
}

/// (name, shape) in file order.
fn tensors() -> Vec<(String, Vec<usize>)> {
    let (c, cl) = (NGF * 8, NGF * 2);
    let cg = c - cl;
    let ch = cg / 2;
    let mut v: Vec<(String, Vec<usize>)> = Vec::new();
    let mut conv = |n: &str, s: [usize; 4], bias: Option<usize>| {
        v.push((format!("{n}.w"), s.to_vec()));
        if let Some(b) = bias {
            v.push((format!("{n}.b"), vec![b]));
        }
    };
    conv("in", [NGF, 4, 7, 7], Some(NGF));
    conv("down1", [NGF * 2, NGF, 3, 3], Some(NGF * 2));
    conv("down2", [NGF * 4, NGF * 2, 3, 3], Some(NGF * 4));
    conv("down3.l", [cl, NGF * 4, 3, 3], Some(cl));
    conv("down3.g", [cg, NGF * 4, 3, 3], Some(cg));
    for b in 0..BLOCKS {
        for k in ["conv1", "conv2"] {
            let p = format!("blocks.{b}.{k}");
            conv(&format!("{p}.l2l"), [cl, cl, 3, 3], Some(cl));
            conv(&format!("{p}.g2l"), [cl, cg, 3, 3], None);
            conv(&format!("{p}.l2g"), [cg, cl, 3, 3], Some(cg));
            conv(&format!("{p}.st1"), [ch, cg, 1, 1], Some(ch));
            conv(&format!("{p}.fu"), [2 * ch, 2 * ch, 1, 1], Some(2 * ch));
            conv(&format!("{p}.st2"), [cg, ch, 1, 1], None);
        }
    }
    conv("up1", [c, c / 2, 3, 3], Some(c / 2));
    conv("up2", [c / 2, c / 4, 3, 3], Some(c / 4));
    conv("up3", [c / 4, c / 8, 3, 3], Some(c / 8));
    conv("out", [3, NGF, 7, 7], Some(3));
    v
}

/// A safetensors file (F32) with LCG weights at He scale √(2/fan_in) (fan_in = dim 1 · k · k, dim 0
/// for transposed convs; the output conv ×8 so the sigmoid sees a spread), biases ±0.1.
fn tiny_safetensors(meta_arch: &str) -> Vec<u8> {
    let mut rng = Lcg(42);
    let mut header = String::from("{");
    header.push_str(&format!("\"__metadata__\":{{\"arch\":\"{meta_arch}\",\"layout\":\"bn-folded-v1\"}}"));
    let mut data: Vec<u8> = Vec::new();
    for (name, shape) in tensors() {
        let n: usize = shape.iter().product();
        let scale = if shape.len() == 1 {
            0.1
        } else {
            let fan = if name.starts_with("up") { shape[0] } else { shape[1] } * shape[2] * shape[3];
            let he = (2.0 / fan as f32).sqrt();
            if name == "out.w" { 8.0 * he } else { he }
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

fn tiny_input() -> (Vec<f32>, Vec<f32>) {
    let mut rng = Lcg(7);
    let rgb: Vec<f32> = (0..W * H * 3).map(|_| rng.next() * 0.5 + 0.5).collect();
    let mask: Vec<f32> = (0..W * H)
        .map(|i| {
            let (x, y) = (i % W, i / W);
            if (20..44).contains(&x) && (16..40).contains(&y) { 1.0 } else { 0.0 }
        })
        .collect();
    (rgb, mask)
}

/// Writes the fixture for `tiny_reference.py`:
/// `cargo test -p photocraft-ai-lama --features flex --test tiny_model -- --ignored dump`
#[test]
#[ignore]
fn dump_tiny_fixture() {
    let dir = std::env::temp_dir().join("lama-tiny");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tiny.safetensors"), tiny_safetensors("lama-ffc")).unwrap();
    let (rgb, mask) = tiny_input();
    let f = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    std::fs::write(dir.join("rgb.f32"), f(&rgb)).unwrap();
    std::fs::write(dir.join("mask.f32"), f(&mask)).unwrap();
}

/// (pixel index, channel, expected) from `tiny_reference.py`, plus the mean of all outputs.
const EXPECTED: &[(usize, usize, f32)] = &include!("tiny_expected.in");
const EXPECTED_MEAN: f32 = include!("tiny_expected_mean.in");

#[test]
fn tiny_model_matches_pytorch() {
    let device = Device::flex();
    let model = Lama::from_safetensors(&tiny_safetensors("lama-ffc"), &device).unwrap();
    assert_eq!(model.arch().ngf, NGF);
    assert_eq!(model.arch().blocks, BLOCKS);
    let (rgb, mask) = tiny_input();
    let out = model.generate(&rgb, &mask, W, H).unwrap();
    assert_eq!(out.len(), W * H * 3);
    assert!(out.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
    for &(i, c, want) in EXPECTED {
        let got = out[i * 3 + c];
        assert!((got - want).abs() < 2e-5, "pixel {i} channel {c}: {got} vs {want}");
    }
    let mean = out.iter().sum::<f32>() / out.len() as f32;
    assert!((mean - EXPECTED_MEAN).abs() < 1e-5, "mean {mean} vs {EXPECTED_MEAN}");
}

/// The Fourier unit's transforms within WebGPU's workgroup memory: before they were 1×1
/// convolutions, burn's matmul asked for more than the device had and panicked. Skipped without
/// a GPU adapter (lavapipe: `VK_ICD_FILENAMES`).
#[cfg(feature = "wgpu")]
#[test]
fn runs_on_a_gpu_with_little_workgroup_memory() {
    let bytes = tiny_safetensors("lama-ffc");
    let (rgb, mask) = tiny_input();
    let want = Lama::from_safetensors(&bytes, &Device::flex()).unwrap().generate(&rgb, &mask, W, H).unwrap();
    for limit in gpu::LIMITS {
        let Some(device) = gpu::limited(limit) else { return eprintln!("no GPU adapter: skipped") };
        let got = Lama::from_safetensors(&bytes, &device).unwrap().generate(&rgb, &mask, W, H).unwrap();
        let d = gpu::max_diff(&want, &got);
        assert!(d < 1e-3, "{limit} B: off by {d}");
    }
}

#[test]
fn bad_weights_and_sizes_are_errors() {
    let device = Device::flex();
    assert!(Lama::from_safetensors(b"", &device).is_err());
    assert!(Lama::from_safetensors(&[0u8; 64], &device).is_err());
    assert!(Lama::from_safetensors(&tiny_safetensors("esrgan"), &device).is_err());
    let good = tiny_safetensors("lama-ffc");
    // Truncated data section.
    assert!(Lama::from_safetensors(&good[..good.len() - 100], &device).is_err());
    let model = Lama::from_safetensors(&good, &device).unwrap();
    let (rgb, mask) = tiny_input();
    assert!(model.generate(&rgb, &mask, W - 8, H).is_err(), "buffer/size mismatch");
    assert!(model.generate(&rgb[..30], &mask[..10], 5, 2).is_err(), "not a multiple of 8");
    assert!(model.generate(&[0.0; 56 * 56 * 3], &[0.0; 56 * 56], 56, 56).is_err(), "below 64");
    let big = 2056;
    assert!(Lama::check_size(big, 64).is_err());
    // NaN input is sanitized, not propagated.
    let mut nan = rgb.clone();
    nan[5] = f32::NAN;
    let out = model.generate(&nan, &mask, W, H).unwrap();
    assert!(out.iter().all(|v| v.is_finite()));
}

struct NoHooks;
impl photocraft_ai_lama::service::Hooks for NoHooks {
    fn progress(&self, _: f32, _: &str) {}
    fn cancelled(&self) -> bool {
        false
    }
}

fn run_native(i: &photocraft_ai_lama::native::NativeInpainter) -> photocraft_ai_lama::Result<Vec<f32>> {
    use photocraft_ai_lama::service::{Inpainter, Request};
    let (rgb, mask) = tiny_input();
    let tile = photocraft_ai_lama::tiles::TileInput { rgb, mask, w: W, h: H };
    let (tx, rx) = std::sync::mpsc::channel();
    i.run(Request::lama(tile, false), std::sync::Arc::new(NoHooks), Box::new(move |r| tx.send(r).unwrap()));
    rx.recv().unwrap()
}

#[test]
fn native_inpainter_reports_missing_weights_and_runs_a_file() {
    use photocraft_ai_lama::native::NativeInpainter;
    use photocraft_ai_lama::service::{Inpainter, ModelState};
    // Built with `flex` alone (as the test commands here do), the device is the CPU.
    let dir = std::env::temp_dir().join(format!("lama-native-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let missing = NativeInpainter::new(dir.join("nope.safetensors"));
    assert!(matches!(missing.model_state(), ModelState::Unavailable(_)));
    assert!(run_native(&missing).is_err());
    let garbage = dir.join("garbage.safetensors");
    std::fs::write(&garbage, b"not a model").unwrap();
    assert!(run_native(&NativeInpainter::new(garbage)).is_err());
    let good = dir.join("tiny.safetensors");
    std::fs::write(&good, tiny_safetensors("lama-ffc")).unwrap();
    let i = NativeInpainter::new(good);
    assert_eq!(i.model_state(), ModelState::Ready);
    let out = run_native(&i).unwrap();
    assert_eq!(out.len(), W * H * 3);
    assert!(!i.device().is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
