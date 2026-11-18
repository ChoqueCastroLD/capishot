//! AVIF export (feature `avif`): the file is a valid AVIF still image as far as its structure goes
//! (no AV1 decoder in pure Rust to check the pixels; the encoder's own tests do that). Each box
//! the image needs is found by walking the ISO-BMFF tree.
#![cfg(feature = "avif")]

use crate::common::*;
use photocraft_codecs::*;

/// The payloads of the boxes of type `kind` directly inside `body`.
fn boxes<'a>(mut body: &'a [u8], kind: &[u8; 4]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    while body.len() >= 8 {
        let size = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
        assert!(size >= 8 && size <= body.len(), "box size {size} out of bounds");
        if &body[4..8] == kind {
            out.push(&body[8..size]);
        }
        body = &body[size..];
    }
    out
}

fn one<'a>(body: &'a [u8], kind: &[u8; 4]) -> &'a [u8] {
    let found = boxes(body, kind);
    assert_eq!(found.len(), 1, "one {} box", String::from_utf8_lossy(kind));
    found[0]
}

/// `meta` and `iprp` are full boxes or containers: `meta` carries 4 bytes of version and flags.
fn properties(file: &[u8]) -> &[u8] {
    let meta = one(file, b"meta");
    one(one(&meta[4..], b"iprp"), b"ipco")
}

fn rgba_page(w: u32, h: u32, alpha: bool) -> Image {
    let layout = if alpha { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    synth(w, h, layout, SampleType::U8, 11, 0.02)
}

#[test]
fn avif_is_a_still_avif_with_its_size_depth_and_srgb() {
    let img = rgba_page(67, 41, false);
    let bytes = encode(&img, Format::Avif, &EncodeOptions::default()).unwrap();
    assert_eq!(detect(&bytes), Some(Format::Avif));
    let ftyp = one(&bytes, b"ftyp");
    assert_eq!(&ftyp[..4], b"avif", "major brand");
    assert!(ftyp[8..].chunks(4).any(|b| b == b"mif1"), "compatible with HEIF still images");
    let ipco = properties(&bytes);
    let ispe = one(ipco, b"ispe");
    let (w, h) = (u32::from_be_bytes(ispe[4..8].try_into().unwrap()), u32::from_be_bytes(ispe[8..12].try_into().unwrap()));
    assert_eq!((w, h), (67, 41));
    let av1c = one(ipco, b"av1C");
    assert_eq!(av1c[0], 0x81, "av1C marker and version 1");
    assert_eq!(av1c[2] & 0x40, 0, "8 bits, not high bit depth");
    assert_eq!(av1c[2] & 0x0c, 0, "4:4:4 (no chroma subsampling)");
    let colr = one(ipco, b"colr");
    assert_eq!(&colr[..4], b"nclx");
    let primaries = u16::from_be_bytes([colr[4], colr[5]]);
    let transfer = u16::from_be_bytes([colr[6], colr[7]]);
    assert_eq!((primaries, transfer), (1, 13), "BT.709 primaries, sRGB transfer");
    let mdat = one(&bytes, b"mdat");
    assert!(mdat.len() > 16, "AV1 payload");
    assert!(boxes(ipco, b"auxC").is_empty(), "opaque: no alpha image");
}

#[test]
fn avif_keeps_transparency_as_an_alpha_image() {
    let mut img = rgba_page(32, 24, true);
    // Half the picture fully transparent.
    for (i, px) in img.data_mut().chunks_exact_mut(4).enumerate() {
        if i % 32 < 16 {
            px[3] = 0;
        }
    }
    let bytes = encode(&img, Format::Avif, &EncodeOptions::default()).unwrap();
    let ipco = properties(&bytes);
    let aux = boxes(ipco, b"auxC");
    assert_eq!(aux.len(), 1, "an alpha auxiliary image");
    assert!(String::from_utf8_lossy(aux[0]).contains("urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"));
}

#[test]
fn avif_quality_and_speed_change_the_file() {
    let img = rgba_page(96, 96, false);
    let at = |q: u8, s: u8| encode(&img, Format::Avif, &EncodeOptions { avif_quality: q, avif_speed: s, ..Default::default() }).unwrap().len();
    assert!(at(30, 6) < at(95, 6), "lower quality is smaller");
    // Out of range values are clamped, never a panic (ravif asserts 1..=100 and 1..=10).
    for (q, s) in [(0, 0), (255, 255), (1, 1)] {
        assert!(at(q, s) > 0);
    }
}

#[test]
fn avif_writes_gray_and_16_bit_as_8_bit_rgb() {
    for (layout, sample) in [(ChannelLayout::Gray, SampleType::U8), (ChannelLayout::Rgb, SampleType::U16), (ChannelLayout::GrayA, SampleType::F32)] {
        let img = synth(20, 12, layout, sample, 3, 0.0);
        let bytes = encode(&img, Format::Avif, &EncodeOptions::default()).unwrap_or_else(|e| panic!("{layout:?} {sample:?}: {e}"));
        assert_eq!(detect(&bytes), Some(Format::Avif), "{layout:?} {sample:?}");
    }
}
