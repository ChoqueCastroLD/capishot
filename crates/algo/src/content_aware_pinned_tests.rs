//! The exact output bits of Content-Aware Fill and of its Auto sampling area over a spread of
//! images (1/3/4 channels, smooth and noisy, holes of several shapes, every adaptation, windows
//! big enough to be reduced), pinned so that speed and cancellation work can't change them.

use super::*;

fn noise(len: usize, levels: u32, seed: u32) -> Vec<f32> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s % levels) as f32 / (levels - 1) as f32
        })
        .collect()
}

/// A textured image: stripes, a gradient and quantised noise, `ch` channels.
fn image(w: usize, h: usize, ch: usize, seed: u32) -> Vec<f32> {
    let n = noise(w * h * ch, 17, seed);
    (0..w * h * ch)
        .map(|i| {
            let (p, c) = (i / ch, i % ch);
            let (x, y) = (p % w, p / w);
            let stripe = if (x + 2 * c) % 7 < 3 { 0.25 } else { 0.7 };
            (stripe + 0.2 * y as f32 / h as f32 + 0.1 * (n[i] - 0.5)).clamp(0.0, 1.0)
        })
        .collect()
}

fn disc(w: usize, h: usize, cx: f32, cy: f32, r: f32) -> Vec<bool> {
    (0..w * h).map(|i| ((i % w) as f32 - cx).hypot((i / w) as f32 - cy) < r).collect()
}

fn fnv(bits: impl Iterator<Item = u32>) -> u64 {
    bits.flat_map(u32::to_le_bytes).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

#[test]
fn fill_output_bits_are_pinned() {
    let cases: [(usize, usize, usize, &str, &str, bool, bool); 6] = [
        (48, 32, 1, "none", "none", false, false),
        (60, 44, 3, "default", "none", false, false),
        (52, 40, 4, "high", "low", false, true),
        (40, 36, 3, "veryHigh", "full", true, false),
        (64, 30, 3, "default", "medium", true, true),
        (33, 47, 1, "default", "high", false, false),
    ];
    let got: Vec<u64> = cases
        .iter()
        .enumerate()
        .map(|(i, &(w, h, ch, color, rot, scale, mirror))| {
            let img = image(w, h, ch, 3 + i as u32);
            let hole = disc(w, h, w as f32 * 0.55, h as f32 * 0.45, 5.5 + i as f32);
            let src: Vec<bool> = (0..w * h).map(|p| (p % w) % 11 != 0).collect();
            let opts = FillOptions { color_adaptation: color_level(color), rotations: rotation_level(rot), scale, mirror, seed: 11 + i as u64 };
            fnv(fill(w, h, ch, &img, &hole, &src, &opts).into_iter().map(f32::to_bits))
        })
        .collect();
    assert_eq!(got, FILL);
}

#[test]
fn auto_sampling_bits_are_pinned() {
    // Small windows (no reduction) and ones past AUTO_SIDE (reduced by k = 2 and 3).
    let cases: [(usize, usize, usize); 5] = [(64, 48, 3), (90, 70, 1), (700, 300, 3), (420, 1100, 4), (1000, 999, 3)];
    let got: Vec<u64> = cases
        .iter()
        .enumerate()
        .map(|(i, &(w, h, ch))| {
            let mut img = image(w, h, ch, 21 + i as u32);
            // A different-looking region the sampling area should leave out.
            for p in 0..w * h {
                if p % w < w / 4 {
                    for c in 0..ch {
                        img[p * ch + c] = 0.05 + 0.02 * c as f32;
                    }
                }
            }
            let hole = disc(w, h, w as f32 * 0.6, h as f32 * 0.5, (w.min(h) as f32) * 0.12);
            fnv(auto_sampling(w, h, ch, &img, &hole).into_iter().map(u32::from))
        })
        .collect();
    assert_eq!(got, AUTO);
}

#[test]
fn cancelled_auto_sampling_stops() {
    let (w, h) = (700, 300);
    let img = image(w, h, 3, 5);
    let hole = disc(w, h, 400.0, 150.0, 40.0);
    let yes = || true;
    assert!(auto_sampling_with(w, h, 3, &img, &hole, &photocraft_raster::Interrupt::cancel_only(&yes)).is_err());
    // A cancel that lands during the cut (after a few hundred checks) still stops it.
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let later = || calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed) > 300;
    assert!(auto_sampling_with(w, h, 3, &img, &hole, &photocraft_raster::Interrupt::cancel_only(&later)).is_err());
    let no = || false;
    assert_eq!(auto_sampling_with(w, h, 3, &img, &hole, &photocraft_raster::Interrupt::cancel_only(&no)).unwrap(), auto_sampling(w, h, 3, &img, &hole));
}

const FILL: [u64; 6] =
    [0x42b7_3fa7_1d75_b420, 0x2739_10dc_ac44_334c, 0xf394_0840_6b15_6fd9, 0xa3b4_1b63_3cb2_f316, 0xb8c7_e8af_7ce0_c04b, 0x8a0e_862f_de9f_8890];
const AUTO: [u64; 5] = [0x317f_65c4_b457_a3c5, 0x3ee4_d4ba_585f_dbb5, 0x80b2_b885_7d24_ecc5, 0xfca4_68c5_df11_a2e4, 0x91d8_f53f_0610_f325];
