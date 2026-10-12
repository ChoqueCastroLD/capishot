//! The exact output bits of the box-pass blurs (Gaussian above radius 4, Box Blur above 4) on
//! multi-tile surfaces: every depth, alpha and none, inside and beyond the layer extent, with a
//! soft selection; pinned so that work on strips, bands and passes can't change them.
use super::*;
use photocraft_color::{ColorMode, PixelFormat, SampleType};

fn surface(fmt: PixelFormat, w: i32, h: i32, seed: u32) -> Surface {
    let n = fmt.channels();
    let mut s = Surface::new(fmt);
    let mut k = seed | 1;
    let mut px = vec![0.0f32; n];
    for y in 0..h {
        for x in 0..w {
            for (c, v) in px.iter_mut().enumerate() {
                k ^= k << 13;
                k ^= k >> 17;
                k ^= k << 5;
                let stripe = if (x / 9 + y / 13 + c as i32) % 2 == 0 { 0.2 } else { 0.75 };
                *v = (stripe + (k % 97) as f32 / 400.0).min(1.0);
            }
            if fmt.alpha {
                px[n - 1] = if (x - w / 2).pow(2) + (y - h / 2).pow(2) < (w / 3).pow(2) { 1.0 } else { 0.35 + (x % 7) as f32 / 20.0 };
            }
            s.write_pixel(x, y, &px);
        }
    }
    s
}

fn fnv(bits: impl Iterator<Item = u32>) -> u64 {
    bits.flat_map(u32::to_le_bytes).fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3))
}

#[test]
fn box_blur_output_bits_are_pinned() {
    let lab = PixelFormat::new(ColorMode::Lab, SampleType::F32, false);
    let gray16 = PixelFormat::new(ColorMode::Grayscale, SampleType::U16, true);
    let (w, h) = (700, 530);
    let bounds = Rect::new(0, 0, w, h);
    let mut sel = Surface::new(PixelFormat::new(ColorMode::Grayscale, SampleType::U8, false));
    for y in 0..h {
        for x in 0..w {
            sel.write_pixel(x, y, &[((x + y) as f32 / 600.0).clamp(0.0, 1.0)]);
        }
    }
    let mut got = Vec::new();
    for (i, fmt) in [PixelFormat::RGBA8, PixelFormat::RGBA16, PixelFormat::RGBA32F, lab, gray16].into_iter().enumerate() {
        let s = surface(fmt, w, h, 5 + i as u32);
        for params in [
            FilterParams::GaussianBlur { radius: 6.0 },
            FilterParams::GaussianBlur { radius: 40.0 },
            FilterParams::GaussianBlur { radius: 250.0 },
            FilterParams::BoxBlur { radius: 9.0 },
        ] {
            for (selection, extent) in [(None, bounds), (Some(&sel), bounds), (None, Rect::new(-50, -20, w + 80, h + 10))] {
                let area = output_area(&params, s.content_bounds(), bounds, None);
                let out = apply_in(&s, &params, area, bounds, selection, extent);
                got.push(fnv(out.read_region(extent.union(&bounds)).into_iter().map(f32::to_bits)));
            }
        }
    }
    assert_eq!(got.len(), PINNED.len());
    assert_eq!(got, PINNED);
}

const PINNED: [u64; 60] = [
    0xb76b_5a6c_0ca3_166b,
    0xe685_55e5_cf4a_cfd9,
    0xa70c_f123_7700_855f,
    0x2e17_bd56_0591_6cc8,
    0xe63d_cfa4_7eab_ec14,
    0x2ab8_a2a0_7dc7_6838,
    0x2a64_06d5_f075_2f4e,
    0x8a9a_b5f6_1127_2fe1,
    0x6f81_379e_a20a_81c5,
    0x04dc_058d_eda2_12f9,
    0x31ec_d6e8_0462_3ab0,
    0x1ff9_db71_156e_aebf,
    0x7b9e_189e_fd8b_1647,
    0x3795_b4ca_16de_eb1e,
    0xddad_c51c_1de6_b39b,
    0xcca8_4449_bfd3_402c,
    0x7733_ad94_f511_8999,
    0xb0f3_d649_c683_a400,
    0x8e95_e730_c934_ece7,
    0x04ce_002c_8942_bc74,
    0x8e2c_f577_ed34_4821,
    0x288a_e621_7126_a0b1,
    0xca46_9fee_23dd_e0a4,
    0xec2d_bc2b_db63_fb6e,
    0x5b15_cb1d_0255_8c02,
    0xd805_cbb3_d595_b2f9,
    0x8f3e_e25e_7254_be7b,
    0x839d_9ec1_d7d4_317c,
    0x7050_8553_fe6f_a484,
    0xfaad_059b_fb1a_7e56,
    0xc2b4_4ced_db5f_50e9,
    0x6a83_83d5_1687_846e,
    0x1be6_6ee5_e7a2_110d,
    0x9020_f0c7_44ca_0e77,
    0xce10_7721_25d9_2395,
    0x3d6e_35e7_1303_5437,
    0x6e29_a4ee_d5c6_b587,
    0x0a9d_68a3_45b2_daa1,
    0x889e_35c5_c76c_e617,
    0x7c4b_a00e_b761_b2ae,
    0xd5d0_9cb1_70da_5761,
    0x2ed9_a91f_a15e_95e4,
    0x9c2a_f56f_8566_bdec,
    0x2010_5738_97ef_4f15,
    0xb536_4698_22e7_b751,
    0x2389_589d_1c17_8247,
    0x39cd_0c8e_00e9_a64f,
    0x15fd_eab7_f069_501d,
    0xa7dd_8f1d_2619_b073,
    0x6e2c_c9b9_1523_a878,
    0xea7d_fb5d_9d3f_e95c,
    0xfacf_c2fb_e49d_951a,
    0xec46_56a5_8df5_c00c,
    0x34e9_9db2_4e2b_d714,
    0x21db_12ca_c231_ca1b,
    0xada3_6f56_beee_3756,
    0xd956_e11b_dffc_da6c,
    0x9e2d_c427_dd5c_0be3,
    0x0010_e11e_ba2f_f1dc,
    0x8168_7005_855a_bb04,
];

/// The direct-kernel blurs (Gaussian up to radius 4, and Unsharp Mask's blur) likewise.
#[test]
fn direct_kernel_output_bits_are_pinned() {
    let lab = PixelFormat::new(ColorMode::Lab, SampleType::F32, false);
    let (w, h) = (600, 300);
    let bounds = Rect::new(0, 0, w, h);
    let mut got = Vec::new();
    for (i, fmt) in [PixelFormat::RGBA8, PixelFormat::RGBA16, lab, PixelFormat::GRAYA8].into_iter().enumerate() {
        let s = surface(fmt, w, h, 31 + i as u32);
        for params in [
            FilterParams::GaussianBlur { radius: 0.6 },
            FilterParams::GaussianBlur { radius: 2.5 },
            FilterParams::GaussianBlur { radius: 4.0 },
            FilterParams::UnsharpMask { amount: 1.2, radius: 3.0, threshold: 2.0 },
        ] {
            for extent in [bounds, Rect::new(-30, -7, w + 20, h + 9)] {
                let area = output_area(&params, s.content_bounds(), bounds, None);
                let out = apply_in(&s, &params, area, bounds, None, extent);
                got.push(fnv(out.read_region(extent.union(&bounds)).into_iter().map(f32::to_bits)));
            }
        }
    }
    assert_eq!(got, SMALL);
}

const SMALL: [u64; 32] = [
    0x0ee3_7b6d_c710_aeb8,
    0x378e_9c70_bd5a_92dc,
    0x5888_a94e_926c_1e8c,
    0x63e5_1f6b_2e20_de32,
    0xae8e_c817_6d88_b5a5,
    0x5814_1352_ddc8_dd8e,
    0x59cb_8862_acba_18a6,
    0x1a1b_c608_6caf_cfa7,
    0xb92d_0a54_41de_ac4a,
    0xfe1f_69c6_c518_a526,
    0x34e3_4439_8746_421b,
    0x2a19_8df5_30d0_97a0,
    0x65ea_36d9_8c36_43bb,
    0xdc0b_e476_7ca5_6759,
    0x1d6b_1b54_707f_48a4,
    0xedb7_38d0_52c1_9dc2,
    0x9339_b18f_af10_465b,
    0xb067_607f_fa3d_f0d2,
    0x4902_232f_97fd_b786,
    0xdf5a_2e8e_db14_d260,
    0x240c_c3e2_07de_5516,
    0x9faf_ce71_6543_6ec7,
    0xced9_cbd8_5a49_1ca7,
    0x1d5c_2cf7_7919_704b,
    0x0f1c_d00f_8699_9780,
    0x66b2_c9c9_9c8a_4cfa,
    0xedf9_3c68_a410_b906,
    0x3185_7eaf_607e_0898,
    0xe789_8829_5aae_bbdc,
    0x1910_a5f7_9297_8e0d,
    0x4cd3_9783_e932_1686,
    0xa323_b6c9_682a_c125,
];

/// The box blur's column bands start where they always have (multiples of 64 px), whatever the
/// strip width: the horizontal running sums start at a band's edge, so moving the edges changes
/// the bits of every large blur. The formula is the one the bands were cut by before.
#[test]
fn band_edges_do_not_move() {
    for column_bytes in (1..20_000).map(|k| k * 977).chain([1, 63, 64, 1 << 20, 1 << 28, usize::MAX]) {
        let before = i32::try_from(RESULT_BUDGET / column_bytes.max(1)).unwrap_or(i32::MAX).max(64) / 64 * 64;
        assert_eq!(band_width(column_bytes), before, "{column_bytes}");
    }
}
