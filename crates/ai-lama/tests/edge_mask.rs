//! Capikoma request 21: letters cut by the page edge, masked down to the last row, must not come
//! back after Manga LaMa. Their repro (a white 256x256 image, a black bar at x 100-140 reaching
//! the bottom, its mask at x 95-145, y 195 to the bottom) and the same at a size that needs
//! mirrored padding (301x203, padded to 304x208), through the tiles the editor runs. Needs the
//! weights (`CAPISHOT_LAMA_WEIGHTS`) and the `flex` feature:
//! `CAPISHOT_LAMA_WEIGHTS=… cargo test -p photocraft-ai-lama --features flex,rayon --test edge_mask`.
#![cfg(feature = "flex")]

use photocraft_ai_lama::Lama;
use photocraft_ai_lama::burn::tensor::Device;
use photocraft_ai_lama::tiles::{self, Fit, Rect, Tile, TileOptions};

/// The darkest value left inside the mask after the fill, over a white page with a black bar
/// under the mask that reaches the image's last row.
fn darkest_left(model: &Lama, (w, h): (usize, usize), bar: [usize; 2], mask_rect: [usize; 4], whole: bool) -> f32 {
    let mut rgb = vec![1.0f32; w * h * 3];
    let mut mask = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            if x >= bar[0] && x < bar[1] && y + 56 >= h {
                rgb[(y * w + x) * 3..][..3].copy_from_slice(&[0.0; 3]);
            }
            if x >= mask_rect[0] && x < mask_rect[2] && y >= mask_rect[1] && y < mask_rect[3] {
                mask[y * w + x] = 1.0;
            }
        }
    }
    let plan = if whole {
        let full = Rect { x: 0, y: 0, w, h };
        vec![Tile { rect: full, core: full, work_w: w.div_ceil(8) * 8, work_h: h.div_ceil(8) * 8, fit: Fit::Pad, split: false }]
    } else {
        tiles::plan(&mask, w, h, &TileOptions::default()).expect("plan")
    };
    let src = rgb.clone();
    for t in &plan {
        let inp = tiles::extract(&src, &mask, w, h, t).expect("extract");
        let out = model.generate(&inp.rgb, &inp.mask, inp.w, inp.h).expect("generate");
        tiles::paste(&mut rgb, &mask, w, h, t, &out).expect("paste");
    }
    let mut darkest = 1.0f32;
    for y in mask_rect[1]..mask_rect[3] {
        for x in mask_rect[0]..mask_rect[2] {
            let p = &rgb[(y * w + x) * 3..][..3];
            darkest = darkest.min((p[0] + p[1] + p[2]) / 3.0);
        }
    }
    darkest
}

#[test]
fn letters_masked_to_the_page_edge_stay_gone() {
    let Some(path) = std::env::var_os("CAPISHOT_LAMA_WEIGHTS") else {
        eprintln!("skipping: CAPISHOT_LAMA_WEIGHTS isn't set");
        return;
    };
    let bytes = std::fs::read(path).expect("weights");
    let model = Lama::from_safetensors(&bytes, &Device::flex()).expect("model");
    for (size, bar, mask, whole) in [
        ((256, 256), [100, 141], [95, 195, 146, 256], false),
        ((301, 203), [100, 141], [95, 145, 146, 203], false),
        ((301, 203), [100, 141], [95, 145, 146, 203], true),
    ] {
        let d = darkest_left(&model, size, bar, mask, whole);
        eprintln!("{size:?} whole {whole}: darkest left in the mask {:.0}/255", d * 255.0);
        assert!(d > 0.8, "{size:?}: a dark pixel ({:.0}/255) came back in the mask at the page edge", d * 255.0);
    }
}
