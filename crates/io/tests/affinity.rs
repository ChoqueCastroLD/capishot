//! Preview-only imports keep pixels and alpha, warn about native content, and never write .af.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image, SampleType};
use photocraft_io::{ExportOptions, export, import};
use proptest::prelude::*;
use std::io::Write as _;

fn png() -> Vec<u8> {
    let img = Image::from_raw(2, 1, ChannelLayout::Rgba, SampleType::U8, vec![220, 40, 60, 255, 0, 0, 0, 0]).unwrap();
    photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap()
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut tagged = kind.to_vec();
    tagged.extend(data);
    let mut result = (data.len() as u32).to_be_bytes().to_vec();
    result.extend(&tagged);
    result.extend(crc32fast::hash(&tagged).to_be_bytes());
    result
}

/// A synthetic record envelope, not an Affinity document writer (there is no object graph).
fn file(png: &[u8]) -> Vec<u8> {
    let mut b = vec![0; 72];
    b[..4].copy_from_slice(photocraft_affinity::MAGIC);
    b[4..6].copy_from_slice(&12u16.to_le_bytes());
    b[8..12].copy_from_slice(b"nsrP");
    b[12..16].copy_from_slice(b"#Inf");
    b[24..32].copy_from_slice(&72u64.to_le_bytes());
    b[64..68].copy_from_slice(b"Prot");
    b.extend(b"\xff\xff\xff\xffThmb");
    b.extend(1u32.to_le_bytes());
    b.extend((png.len() as u32 + 13).to_le_bytes());
    b.extend(29u32.to_le_bytes());
    b.extend(0u32.to_le_bytes());
    b.extend((png.len() as u32).to_le_bytes());
    b.push(1);
    b.extend(png);
    b
}

#[test]
fn preview_is_named_and_warns_at_its_actual_size() {
    let r = import("art.af", &file(&png())).unwrap();
    assert_eq!((r.document.size.width, r.document.size.height), (2, 1));
    assert_eq!(r.document.layers.len(), 1);
    assert_eq!(r.document.layers[0].name, "Affinity preview");
    assert!(r.source_read_only);
    assert!(r.warnings[0].contains("2×1"));
    assert!(r.warnings[0].contains("Native layers"));
    let out = export(&r.document, "copy.png", &ExportOptions::default()).unwrap();
    let img = photocraft_codecs::decode(&out.bytes).unwrap();
    assert_eq!(img.data(), &[220, 40, 60, 255, 0, 0, 0, 0]);
    let native = export(&r.document, "copy.pcraft", &ExportOptions::default()).unwrap();
    let back = import("copy.pcraft", &native.bytes).unwrap();
    assert_eq!(back.document.size, r.document.size);
    assert!(!back.source_read_only);
    for ext in photocraft_io::affinity::EXTENSIONS {
        assert!(export(&r.document, ext, &ExportOptions::default()).unwrap_err().to_string().contains("Affinity export"));
    }
}

#[test]
fn magic_wins_over_the_name_and_renamed_legacy_files_are_rejected() {
    assert!(import("renamed.png", &file(&png())).unwrap().source_read_only);
    assert!(import("unknown", &file(&png())).unwrap().source_read_only);
    assert!(import("fake.af", &png()).is_err());
    let mut legacy = file(&png());
    legacy[4..6].copy_from_slice(&10u16.to_le_bytes());
    assert!(import("legacy.afphoto", &legacy).unwrap_err().to_string().contains("legacy preview"));
    assert!(import("legacy.af", &legacy).is_err());
}

#[test]
fn preview_decoder_preserves_16_bit_gray_and_alpha() {
    let img = Image::from_u16(2, 1, ChannelLayout::GrayA, &[12345, 65535, 0, 0]).unwrap();
    let encoded = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap();
    let r = import("gray.af", &file(&encoded)).unwrap();
    assert_eq!(r.document.depth, photocraft_color::SampleType::U16);
    assert_eq!(r.document.mode, photocraft_color::ColorMode::Grayscale);
    let out = export(&r.document, "copy.png", &ExportOptions::default()).unwrap();
    let decoded = photocraft_codecs::decode(&out.bytes).unwrap();
    assert_eq!(decoded.sample_type(), SampleType::U16);
    assert_eq!(decoded.data(), img.data());
}

#[test]
fn previewless_current_files_return_an_export_fallback() {
    let mut bytes = file(&png());
    bytes[24..32].fill(0);
    let error = import("no-preview.af", &bytes).unwrap_err().to_string();
    assert!(error.contains("no embedded preview"), "{error}");
    assert!(error.contains("export PNG, PSD or SVG"), "{error}");
}

#[test]
fn corruption_and_every_truncation_fail_without_a_document() {
    let b = file(&png());
    for end in 0..b.len() {
        assert!(import("bad.af", &b[..end]).is_err(), "{end}");
    }
    let mut bad_crc = b.clone();
    // IHDR's CRC; the envelope reader rejects every PNG chunk's bad CRC.
    bad_crc[130] ^= 1;
    assert!(import("bad.af", &bad_crc).is_err());
}

#[test]
fn crc_failures_after_idat_cannot_be_tolerated_as_partial_pngs() {
    let mut bad_iend = file(&png());
    *bad_iend.last_mut().unwrap() ^= 1;
    assert!(import("bad-iend.af", &bad_iend).unwrap_err().to_string().contains("PNG chunk CRC"));

    let mut encoded = png();
    let tagged = b"tEXtnote\0preview";
    let mut chunk = ((tagged.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend(tagged);
    chunk.extend(crc32fast::hash(tagged).to_be_bytes());
    let end = encoded.len() - 12;
    encoded.splice(end..end, chunk.clone());
    assert!(import("valid-ancillary.af", &file(&encoded)).is_ok());
    encoded[end + chunk.len() - 1] ^= 1;
    assert!(import("bad-ancillary.af", &file(&encoded)).unwrap_err().to_string().contains("PNG chunk CRC"));
}

#[test]
fn compressed_metadata_and_apng_fail_before_reaching_the_decoder() {
    for kind in [b"zTXt", b"iTXt", b"iCCP", b"acTL", b"fcTL", b"fdAT"] {
        let mut encoded = png();
        // A tiny declared payload stands in for arbitrarily large inflated metadata. It must
        // return the format's unsupported error, not be passed to a decompressor.
        let mut tagged = kind.to_vec();
        tagged.extend(b"preview\0\0\x78\x9c");
        let mut chunk = ((tagged.len() - 4) as u32).to_be_bytes().to_vec();
        chunk.extend(&tagged);
        chunk.extend(crc32fast::hash(&tagged).to_be_bytes());
        let end = encoded.len() - 12;
        encoded.splice(end..end, chunk);
        let error = import("unsupported.af", &file(&encoded)).unwrap_err().to_string();
        assert!(error.contains("unsupported Affinity file"), "{kind:?}: {error}");
        assert!(error.contains("PNG metadata") || error.contains("animated PNG"), "{error}");
    }
}

#[test]
fn many_valid_compressed_text_chunks_cannot_bypass_the_total_memory_budget() {
    let mut compressor = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    for _ in 0..1024 {
        compressor.write_all(&[b'x'; 1024]).unwrap();
    }
    let compressed = compressor.finish().unwrap();
    assert!(compressed.len() < 2048);
    let mut tagged = b"zTXtnote\0\0".to_vec();
    tagged.extend(compressed);
    let mut chunk = ((tagged.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend(&tagged);
    chunk.extend(crc32fast::hash(&tagged).to_be_bytes());
    let mut encoded = png();
    let end = encoded.len() - 12;
    // A few MiB of encoded PNG would otherwise carry 1 GiB of aggregate text.
    encoded.splice(end..end, chunk.repeat(1024));
    let error = import("text-bomb.af", &file(&encoded)).unwrap_err().to_string();
    assert!(error.contains("compressed PNG metadata"), "{error}");
}

#[test]
fn invalid_critical_chunks_after_idat_fail_before_png_finish() {
    let original = png();
    for trailing in [chunk(b"IHDR", &original[16..29]), chunk(b"PLTE", &[0, 0, 0]), chunk(b"ABCD", &[])] {
        let mut encoded = original.clone();
        let end = encoded.len() - 12;
        encoded.splice(end..end, trailing);
        let error = import("bad-order.af", &file(&encoded)).unwrap_err().to_string();
        assert!(error.contains("Affinity file"), "{error}");
    }
    let mut encoded = original;
    let end = encoded.len() - 12;
    let mut interrupted = chunk(b"tEXt", b"note\0preview");
    interrupted.extend(chunk(b"IDAT", &[]));
    encoded.splice(end..end, interrupted);
    assert!(import("split-idat.af", &file(&encoded)).unwrap_err().to_string().contains("IDAT chunks must be contiguous"));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn preview_and_decoder_survive_mutations(edits in prop::collection::vec((0usize..300, any::<u8>()), 0..16)) {
        let mut b = file(&png());
        for (i, v) in edits { if let Some(byte) = b.get_mut(i) { *byte = v; } }
        let _ = import("mutated.af", &b);
    }
}

/// Optional local oracle; authored by Affinity 3.3, never committed as a binary fixture.
#[test]
#[ignore = "set AFFINITY_ORACLE_DIR to synthetic Affinity .af and exported PNG files"]
fn native_affinity_oracle() {
    let dir = std::env::var("AFFINITY_ORACLE_DIR").unwrap();
    for (name, size) in [("synthetic-rectangle", (64, 48)), ("synthetic-large", (1024, 768))] {
        let bytes = std::fs::read(format!("{dir}/{name}.af")).unwrap();
        let r = import(&format!("{name}.af"), &bytes).unwrap();
        assert_eq!((r.document.size.width, r.document.size.height), size);
        let preview = photocraft_affinity::preview(&bytes).unwrap();
        let source = photocraft_codecs::decode(&std::fs::read(format!("{dir}/{name}.png")).unwrap()).unwrap();
        let decoded = photocraft_codecs::decode(preview.png).unwrap();
        assert_eq!(decoded.data().len(), size.0 as usize * size.1 as usize * 4);
        if name == "synthetic-rectangle" {
            assert_eq!(decoded.data(), source.data());
        }
    }
}
