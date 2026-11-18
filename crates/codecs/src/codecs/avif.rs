//! AVIF encode (feature `avif`) with ravif and rav1e, pure Rust (no nasm, no C): 8-bit 4:4:4
//! YCbCr, colour tagged as sRGB (BT.709 primaries, sRGB transfer), alpha as a second AV1 image
//! when the picture has any transparency. Decoding needs an AV1 decoder (dav1d is C), so AVIF
//! stays write-only (see `format::ASYMMETRIC_EXCEPTIONS`).
//!
//! Natively and in the browser's threads build rav1e splits the frame into tiles encoded on the
//! global rayon pool, so the encode runs in parallel without starting a pool of its own.

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{ChannelLayout, Image, SampleType};
use crate::options::EncodeOptions;

const F: Format = Format::Avif;

/// AV1's largest frame side.
const MAX_SIDE: u32 = 65_536;

pub(crate) fn encode(src: &Image, plan: Plan, opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    let (w, h) = src.dimensions();
    if w > MAX_SIDE || h > MAX_SIDE {
        return Err(CodecError::encode(F, format!("AVIF holds at most {MAX_SIDE} pixels per side")));
    }
    let alpha = plan.layout.has_alpha();
    let target = if alpha { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    let img = src.converted(target, SampleType::U8);
    let quality = f32::from(opts.avif_quality.clamp(1, 100));
    let enc = ravif::Encoder::new()
        .with_quality(quality)
        .with_alpha_quality(quality)
        .with_speed(opts.avif_speed.clamp(1, 10))
        .with_bit_depth(ravif::BitDepth::Eight)
        .with_internal_color_model(ravif::ColorModel::YCbCr)
        .with_alpha_color_mode(ravif::AlphaColorMode::UnassociatedClean);
    let (wu, hu) = (w as usize, h as usize);
    let out = if alpha {
        let px: Vec<ravif::RGBA8> = img.data().chunks_exact(4).map(|c| ravif::RGBA8::new(c[0], c[1], c[2], c[3])).collect();
        if px.len() != wu * hu {
            return Err(CodecError::InvalidImage("buffer size mismatch".into()));
        }
        enc.encode_rgba(ravif::Img::new(px.as_slice(), wu, hu))
    } else {
        let px: Vec<ravif::RGB8> = img.data().chunks_exact(3).map(|c| ravif::RGB8::new(c[0], c[1], c[2])).collect();
        if px.len() != wu * hu {
            return Err(CodecError::InvalidImage("buffer size mismatch".into()));
        }
        enc.encode_rgb(ravif::Img::new(px.as_slice(), wu, hu))
    };
    out.map(|e| e.avif_file).map_err(|e| CodecError::encode(F, e))
}
