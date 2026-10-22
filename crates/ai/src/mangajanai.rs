//! MangaJaNai's model choice and preprocessing, as MangaJaNaiConverterGui's default "Upscale
//! Manga" workflow does it (backend `run_upscale.py`, `default_cli_configuration.json`):
//!
//! * Colour or grayscale is detected per page ([`is_grayscale`], threshold 12).
//! * Grayscale pages use the MangaJaNai model trained for the nearest page height:
//!   ≤1250 → 1200p, ≤1350 → 1300p, ≤1450 → 1400p, ≤1550 → 1500p, ≤1760 → 1600p, ≤1984 → 1920p,
//!   else 2048p; the 2x model for scales up to 2, the 4x one above. Levels are auto-adjusted
//!   first ([`auto_levels`]).
//! * Colour pages use IllustrationJaNai. (The GUI's default chain always runs the 4x model; for
//!   2x we run the 2x IllustrationJaNai release, which is four times cheaper.)
//!
//! One deliberate difference: the GUI's grayscale test adds per-pixel channel differences in
//! 8-bit arithmetic, which wraps for strongly coloured pixels; we add them without wrapping.

use crate::catalog::{Content, MODELS, ModelInfo};
use crate::image::to_u8;
use crate::{AiError, Planar, Result};

/// MangaJaNaiConverterGui's default grayscale detection threshold.
pub const GRAYSCALE_THRESHOLD: u8 = 12;

/// Is this RGB(A) image grayscale for MangaJaNai's purposes? One-channel images always are.
/// Pure black and pure white pixels are ignored; an image of only those is not grayscale.
pub fn is_grayscale(img: &Planar, threshold: u8) -> bool {
    if img.channels < 3 {
        return true;
    }
    let (r, g, b) = (img.plane(0), img.plane(1), img.plane(2));
    let t = i32::from(threshold);
    let mut sum: u64 = 0;
    let mut count: u64 = 0;
    for ((&r, &g), &b) in r.iter().zip(g).zip(b) {
        let (r, g, b) = (i32::from(to_u8(r)), i32::from(to_u8(g)), i32::from(to_u8(b)));
        if (r == 0 && g == 0 && b == 0) || (r == 255 && g == 255 && b == 255) {
            continue;
        }
        let d = |a: i32, b: i32| ((a - b).abs() - t).max(0) as u64;
        sum += d(r, g) + d(r, b) + d(g, b);
        count += 3;
    }
    if count == 0 {
        return false;
    }
    sum as f64 / count as f64 <= f64::from(threshold) / 12.0
}

/// Luma (BT.601, as OpenCV's and PIL's RGB→L) as one channel; one-channel images are copied.
pub fn to_gray(img: &Planar) -> Planar {
    if img.channels < 3 {
        return img.with_channels(1);
    }
    let (r, g, b) = (img.plane(0), img.plane(1), img.plane(2));
    let data = r.iter().zip(g).zip(b).map(|((&r, &g), &b)| 0.299 * r + 0.587 * g + 0.114 * b).collect();
    Planar { width: img.width, height: img.height, channels: 1, data }
}

/// MangaJaNai's "auto adjust levels" for grayscale pages (`enhance_contrast`): finds the black
/// level (the histogram peak in 0..=30, then following the climb above it) and the white level
/// (the peak in 225..=255, then down), and stretches that range to `0..=1`.
pub fn auto_levels(gray: &Planar) -> Planar {
    let mut hist = [0u64; 256];
    for &v in gray.plane(0) {
        hist[usize::from(to_u8(v))] += 1;
    }
    let (black, white) = levels(&hist);
    let lo = f32::from(black) / 255.0;
    let range = if white > black { f32::from(white - black) / 255.0 } else { 1.0 };
    let lo = if white > black { lo } else { 0.0 };
    let data = gray.plane(0).iter().map(|&v| ((f32::from(to_u8(v)) / 255.0 - lo).max(0.0) / range).clamp(0.0, 1.0)).collect();
    Planar { width: gray.width, height: gray.height, channels: 1, data }
}

/// The black and white levels of a 256-bin histogram, as `enhance_contrast` finds them.
pub fn levels(hist: &[u64; 256]) -> (u8, u8) {
    let mut black = 0u8;
    let mut max_black = hist[0];
    for (i, &h) in hist.iter().enumerate().take(31).skip(1) {
        if h > max_black {
            max_black = h;
            black = i as u8;
        }
    }
    let mut run = 0;
    for (i, &h) in hist.iter().enumerate().skip(31) {
        if h > max_black {
            run = 0;
            max_black = h;
            black = i as u8;
        } else if h < max_black {
            run += 1;
            if run > 1 {
                break;
            }
        }
    }
    let mut white = 255u8;
    let mut max_white = hist[255];
    for i in (225..=254).rev() {
        if hist[i] > max_white {
            max_white = hist[i];
            white = i as u8;
        }
    }
    let mut run = 0;
    for i in (0..=223).rev() {
        if hist[i] > max_white {
            run = 0;
            max_white = hist[i];
            white = i as u8;
        } else if hist[i] < max_white {
            run += 1;
            if run > 1 {
                break;
            }
        }
    }
    (black, white)
}

/// The MangaJaNai height bin (trained page height) for an input page height.
pub fn height_bin(height: usize) -> u32 {
    match height {
        0..=1250 => 1200,
        1251..=1350 => 1300,
        1351..=1450 => 1400,
        1451..=1550 => 1500,
        1551..=1760 => 1600,
        1761..=1984 => 1920,
        _ => 2048,
    }
}

/// The model MangaJaNai would use for a page `height` pixels tall at `scale` (2 or 4).
pub fn select(height: usize, scale: u32, content: Content) -> Result<&'static ModelInfo> {
    let model_scale = if scale <= 2 { 2 } else { 4 };
    let want = match content {
        Content::Grayscale => Some(height_bin(height)),
        Content::Color => None,
        Content::Inpaint | Content::Any | Content::Generative => return Err(AiError::UnknownModel(format!("{content:?} isn't a MangaJaNai content"))),
    };
    MODELS
        .iter()
        .find(|m| m.content == content && m.scale == model_scale && m.height == want)
        .ok_or_else(|| AiError::UnknownModel(format!("{content:?} {model_scale}x for {height}px")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(px: &[(u8, u8, u8)]) -> Planar {
        let flat: Vec<u8> = px.iter().flat_map(|&(r, g, b)| [r, g, b]).collect();
        Planar::from_interleaved_u8(px.len(), 1, 3, &flat).unwrap()
    }

    #[test]
    fn grayscale_detection() {
        assert!(is_grayscale(&rgb(&[(10, 10, 10), (200, 200, 200), (0, 0, 0)]), 12));
        assert!(is_grayscale(&rgb(&[(100, 110, 105)]), 12));
        assert!(!is_grayscale(&rgb(&[(200, 30, 30), (20, 200, 30)]), 12));
        assert!(!is_grayscale(&rgb(&[(0, 0, 0), (255, 255, 255)]), 12));
        assert!(is_grayscale(&Planar::zeros(2, 2, 1).unwrap(), 12));
    }

    #[test]
    fn selection_follows_the_gui_chains() {
        let id = |h, s, c| select(h, s, c).unwrap().id;
        assert_eq!(id(1138, 4, Content::Grayscale), "mangajanai-v1-4x-1200p");
        assert_eq!(id(1250, 2, Content::Grayscale), "mangajanai-v1-2x-1200p");
        assert_eq!(id(1251, 4, Content::Grayscale), "mangajanai-v1-4x-1300p");
        assert_eq!(id(1760, 3, Content::Grayscale), "mangajanai-v1-4x-1600p");
        assert_eq!(id(1984, 4, Content::Grayscale), "mangajanai-v1-4x-1920p");
        assert_eq!(id(5000, 2, Content::Grayscale), "mangajanai-v1-2x-2048p");
        assert_eq!(id(800, 4, Content::Color), "illustrationjanai-v1-4x");
        assert_eq!(id(800, 2, Content::Color), "illustrationjanai-v1-2x");
    }

    #[test]
    fn levels_stretch() {
        let mut h = [0u64; 256];
        h[12] = 50;
        h[240] = 80;
        h[128] = 10;
        assert_eq!(levels(&h), (12, 240));
        let g = Planar::from_interleaved_u8(3, 1, 1, &[12, 240, 126]).unwrap();
        let out = auto_levels(&g);
        assert!((out.data[0]).abs() < 1e-6);
        assert!((out.data[1] - 1.0).abs() < 1e-6);
        // Flat images don't divide by zero.
        let flat = auto_levels(&Planar::from_interleaved_u8(2, 1, 1, &[7, 7]).unwrap());
        assert!(flat.data.iter().all(|v| v.is_finite()));
    }
}
