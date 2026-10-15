//! A tiny Real-ESRGAN Compact (2 middle convs, 8 filters, ×4) with deterministic weights, checked
//! against an independent eager PyTorch implementation (`tiny_compact_reference.py`) at ×4 and
//! ×2 (box-downsampled). The released weights are verified against the original release with
//! spandrel outside the test suite (crates/ai-lama/README.md › Verification, lite models).
#![allow(clippy::excessive_precision)]

use photocraft_ai::Planar;
use photocraft_ai::compact::Compact;
use photocraft_ai::runtime::{Network, Runtime, block_on};
use photocraft_ai::weights::{Weights, to_safetensors};

const NF: usize = 8;
const W: usize = 7;
const H: usize = 5;

fn lcg(seed: &mut u64) -> f32 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    ((*seed >> 40) as f32 / 16777216.0) * 2.0 - 1.0
}

fn tiny_bytes() -> Vec<u8> {
    let mut seed = 3u64;
    let mut t: Vec<(String, Vec<usize>, Vec<f32>)> = Vec::new();
    let shapes = [(NF, 3), (NF, NF), (NF, NF), (48, NF)];
    for (i, (o, inp)) in shapes.iter().enumerate() {
        let he = (2.0 / (inp * 9) as f32).sqrt();
        t.push((format!("body.{}.weight", 2 * i), vec![*o, *inp, 3, 3], (0..o * inp * 9).map(|_| lcg(&mut seed) * he).collect()));
        t.push((format!("body.{}.bias", 2 * i), vec![*o], (0..*o).map(|_| lcg(&mut seed) * 0.1).collect()));
        if i < 3 {
            t.push((format!("body.{}.weight", 2 * i + 1), vec![NF], (0..NF).map(|_| lcg(&mut seed) * 0.25 + 0.25).collect()));
        }
    }
    let refs: Vec<(&str, &[usize], &[f32])> = t.iter().map(|(n, s, d)| (n.as_str(), s.as_slice(), d.as_slice())).collect();
    to_safetensors(&refs, false, &[("arch", "compact")])
}

fn input() -> Planar {
    let mut seed = 9u64;
    Planar::new(W, H, 3, (0..W * H * 3).map(|_| lcg(&mut seed) * 0.5 + 0.5).collect()).unwrap()
}

/// Writes the fixture for `tiny_compact_reference.py`:
/// `cargo test -p photocraft-ai --test tiny_compact -- --ignored dump`
#[test]
#[ignore]
fn dump_tiny_fixture() {
    let dir = std::env::temp_dir().join("compact-tiny");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tiny.safetensors"), tiny_bytes()).unwrap();
    std::fs::write(dir.join("in.f32"), input().data.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
}

/// (index into the planar ×4 output, expected), (index into the ×2 output, expected).
const EXPECTED_X4: &[(usize, f32)] = &include!("tiny_compact_x4.in");
const EXPECTED_X2: &[(usize, f32)] = &include!("tiny_compact_x2.in");

#[test]
fn tiny_compact_matches_pytorch() {
    let rt = Runtime::cpu();
    let w = Weights::from_safetensors(&tiny_bytes()).unwrap();
    for (scale, expected) in [(4, EXPECTED_X4), (2, EXPECTED_X2)] {
        let net = Compact::load(&rt, &w, scale, false).unwrap();
        let out = block_on(rt.read(net.forward(rt.image(&input()).unwrap()))).unwrap();
        assert_eq!((out.width, out.height, out.channels), (W * scale, H * scale, 3));
        for &(i, want) in expected {
            let got = out.data[i];
            assert!((got - want).abs() < 2e-5, "×{scale} value {i}: {got} vs {want}");
        }
    }
}
