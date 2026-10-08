//! Affinity container inspection, without interpreting the compressed document graph.
//!
//! The header layout was researched with MIT-licensed afread (see README). The version-12
//! thumbnail record was observed in synthetic documents saved by Affinity 3.3. Do not scan for
//! PNG signatures: an unrelated resource or an obsolete revision is not a document preview.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::fmt;

pub const MAGIC: &[u8; 4] = b"\x00\xffKA";
pub const MAX_PREVIEW_BYTES: usize = 16 << 20;
pub const MAX_PREVIEW_DIMENSION: u32 = 4096;

/// Container failure. The compressed graph is deliberately never decompressed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Malformed(&'static str),
    Unsupported(&'static str),
    Limit(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, reason) = match self {
            Self::Malformed(s) => ("damaged Affinity file", s),
            Self::Unsupported(s) => ("unsupported Affinity file", s),
            Self::Limit(s) => ("Affinity preview limit exceeded", s),
        };
        write!(f, "{kind}: {reason}; export PNG, PSD or SVG from Affinity for full-resolution artwork")
    }
}

impl std::error::Error for Error {}

/// Header identity; offsets remain private so callers cannot bypass validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub version: u16,
    thumbnail_offset: u64,
}

/// Borrowed embedded thumbnail, at its own dimensions, never the native document dimensions.
#[derive(Debug)]
pub struct Preview<'a> {
    pub png: &'a [u8],
    pub width: u32,
    pub height: u32,
}

pub fn is_affinity(bytes: &[u8]) -> bool {
    bytes.starts_with(MAGIC)
}

fn bytes_at(bytes: &[u8], offset: usize, length: usize) -> Result<&[u8], Error> {
    let end = offset.checked_add(length).ok_or(Error::Malformed("offset overflow"))?;
    bytes.get(offset..end).ok_or(Error::Malformed("truncated record"))
}

fn le16(bytes: &[u8], offset: usize) -> Result<u16, Error> {
    let b = bytes_at(bytes, offset, 2)?;
    Ok(u16::from_le_bytes(b.try_into().map_err(|_| Error::Malformed("integer"))?))
}

fn le32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let b = bytes_at(bytes, offset, 4)?;
    Ok(u32::from_le_bytes(b.try_into().map_err(|_| Error::Malformed("integer"))?))
}

fn le64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let b = bytes_at(bytes, offset, 8)?;
    Ok(u64::from_le_bytes(b.try_into().map_err(|_| Error::Malformed("integer"))?))
}

fn be32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let b = bytes_at(bytes, offset, 4)?;
    Ok(u32::from_be_bytes(b.try_into().map_err(|_| Error::Malformed("PNG integer"))?))
}

/// Recognize documents, rejecting add-ons with the same magic and unknown container versions.
pub fn inspect(bytes: &[u8]) -> Result<Header, Error> {
    if !is_affinity(bytes) {
        return Err(Error::Malformed("missing container signature"));
    }
    let version = le16(bytes, 4)?;
    if !(7..=12).contains(&version) {
        return Err(Error::Unsupported("unverified container version"));
    }
    if le16(bytes, 6)? != 0 {
        return Err(Error::Unsupported("container flags"));
    }
    // afread's Prsn document class, stored little-endian. Brushes, macros and palettes
    // share the magic but must not be opened as documents.
    if bytes_at(bytes, 8, 4)? != b"nsrP" {
        return Err(Error::Unsupported("this container is not an Affinity document"));
    }
    if bytes_at(bytes, 12, 4)? != b"#Inf" {
        return Err(Error::Malformed("missing information header"));
    }
    bytes_at(bytes, 0, if version == 7 { 64 } else { 72 })?;
    if version > 7 && bytes_at(bytes, 64, 4)? != b"Prot" {
        return Err(Error::Malformed("missing protocol header"));
    }
    Ok(Header { version, thumbnail_offset: le64(bytes, 24)? })
}

/// Read only the indexed version-12 `Thmb` record and validate every PNG chunk CRC.
/// Compressed pixels must still be validated by the consuming image decoder, with allocation limits.
pub fn preview(bytes: &[u8]) -> Result<Preview<'_>, Error> {
    let h = inspect(bytes)?;
    if h.version != 12 {
        return Err(Error::Unsupported("legacy preview records have not been verified"));
    }
    if h.thumbnail_offset == 0 {
        return Err(Error::Unsupported("no embedded preview; native layer decoding is not implemented"));
    }
    let offset = usize::try_from(h.thumbnail_offset).map_err(|_| Error::Malformed("thumbnail offset overflow"))?;
    if offset < 72 {
        return Err(Error::Malformed("thumbnail overlaps the file header"));
    }
    let record = bytes.get(offset..).ok_or(Error::Malformed("thumbnail offset outside file"))?;
    if bytes_at(record, 0, 8)? != b"\xff\xff\xff\xffThmb" {
        return Err(Error::Malformed("missing indexed thumbnail"));
    }
    if le32(record, 8)? != 1 || bytes_at(record, 28, 1)? != [1] {
        return Err(Error::Unsupported("thumbnail record version or encoding"));
    }
    let size = usize::try_from(le32(record, 24)?).map_err(|_| Error::Limit("encoded size"))?;
    if size > MAX_PREVIEW_BYTES {
        return Err(Error::Limit("encoded preview exceeds 16 MiB"));
    }
    if le32(record, 16)? != 29 || le32(record, 20)? != 0 || u64::from(le32(record, 12)?) != size as u64 + 13 {
        return Err(Error::Malformed("inconsistent thumbnail record lengths"));
    }
    let png = bytes_at(record, 29, size)?;
    if bytes_at(png, 0, 8)? != b"\x89PNG\r\n\x1a\n" || be32(png, 8)? != 13 || bytes_at(png, 12, 4)? != b"IHDR" {
        return Err(Error::Malformed("thumbnail is not a PNG"));
    }
    let width = be32(png, 16)?;
    let height = be32(png, 20)?;
    if width == 0 || height == 0 || width > MAX_PREVIEW_DIMENSION || height > MAX_PREVIEW_DIMENSION {
        return Err(Error::Limit("preview dimensions must be 1..4096"));
    }
    // Bound work by bytes, not a file-supplied chunk count, and require exactly one complete PNG.
    let mut pos = 8usize;
    let mut seen_ihdr = false;
    let mut seen_plte = false;
    let mut seen_idat = false;
    let mut idat_ended = false;
    loop {
        let length = usize::try_from(be32(png, pos)?).map_err(|_| Error::Limit("PNG chunk length"))?;
        let chunk_size = length.checked_add(12).ok_or(Error::Malformed("PNG chunk overflow"))?;
        let chunk = bytes_at(png, pos, chunk_size)?;
        let crc_offset = chunk_size.checked_sub(4).ok_or(Error::Malformed("PNG CRC offset"))?;
        let protected = chunk.get(4..crc_offset).ok_or(Error::Malformed("PNG CRC span"))?;
        if crc32fast::hash(protected) != be32(chunk, crc_offset)? {
            return Err(Error::Malformed("PNG chunk CRC mismatch"));
        }
        let kind = bytes_at(chunk, 4, 4)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) {
            return Err(Error::Malformed("invalid PNG chunk name"));
        }
        // Native oracles have no compressed metadata or animation. Reject before any decoder
        // can inflate many individually capped text/profile chunks or silently pick a frame.
        if matches!(kind, b"zTXt" | b"iTXt" | b"iCCP") {
            return Err(Error::Unsupported("compressed PNG metadata in preview"));
        }
        if matches!(kind, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(Error::Unsupported("animated PNG preview"));
        }
        match kind {
            b"IHDR" => {
                if seen_ihdr || pos != 8 {
                    return Err(Error::Malformed("PNG IHDR must be first and unique"));
                }
                seen_ihdr = true;
            }
            b"PLTE" => {
                if seen_plte || seen_idat {
                    return Err(Error::Malformed("PNG PLTE must be unique and precede IDAT"));
                }
                seen_plte = true;
            }
            b"IDAT" => {
                if idat_ended {
                    return Err(Error::Malformed("PNG IDAT chunks must be contiguous"));
                }
                seen_idat = true;
            }
            b"IEND" => {
                if !seen_idat {
                    return Err(Error::Malformed("PNG has no image data"));
                }
            }
            _ => {
                if kind.first().is_some_and(|b| b.is_ascii_uppercase()) {
                    return Err(Error::Unsupported("unknown critical PNG chunk"));
                }
            }
        }
        if seen_idat && kind != b"IDAT" {
            idat_ended = true;
        }
        pos = pos.checked_add(chunk_size).ok_or(Error::Malformed("PNG offset overflow"))?;
        if kind == b"IEND" {
            if length != 0 || pos != png.len() {
                return Err(Error::Malformed("PNG end or trailing data"));
            }
            break;
        }
    }
    Ok(Preview { png, width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn fixture() -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend(13u32.to_be_bytes());
        png.extend(b"IHDR");
        png.extend(2u32.to_be_bytes());
        png.extend(1u32.to_be_bytes());
        png.extend([8, 6, 0, 0, 0]);
        let ihdr_crc = crc32fast::hash(&png[12..]);
        png.extend(ihdr_crc.to_be_bytes());
        png.extend([0; 4]);
        png.extend(b"IDAT");
        png.extend(crc32fast::hash(b"IDAT").to_be_bytes());
        png.extend([0; 4]);
        png.extend(b"IEND");
        png.extend(crc32fast::hash(b"IEND").to_be_bytes());
        // Valid CRCs, but no compressed pixels: consuming decoders still validate those.
        file(&png)
    }

    fn file(png: &[u8]) -> Vec<u8> {
        let mut b = vec![0; 72];
        b[..4].copy_from_slice(MAGIC);
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

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut tagged = kind.to_vec();
        tagged.extend(data);
        let mut result = (data.len() as u32).to_be_bytes().to_vec();
        result.extend(&tagged);
        result.extend(crc32fast::hash(&tagged).to_be_bytes());
        result
    }

    #[test]
    fn indexed_preview_and_header() {
        let b = fixture();
        assert_eq!(inspect(&b).unwrap().version, 12);
        let p = preview(&b).unwrap();
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(p.png, &b[101..]);
    }

    #[test]
    fn every_truncation_is_rejected() {
        let b = fixture();
        for end in 0..b.len() {
            assert!(preview(&b[..end]).is_err(), "{end}");
        }
    }

    #[test]
    fn rejects_assets_versions_offsets_lengths_and_bombs() {
        let b = fixture();
        for (offset, value) in [
            (4, 13u64.to_le_bytes().to_vec()),
            (8, b"urBR".to_vec()),
            (24, u64::MAX.to_le_bytes().to_vec()),
            (24, 1u64.to_le_bytes().to_vec()),
            (24, 0u64.to_le_bytes().to_vec()),
            (84, u32::MAX.to_le_bytes().to_vec()),
            (88, 30u32.to_le_bytes().to_vec()),
            (96, u32::MAX.to_le_bytes().to_vec()),
            (117, 5000u32.to_be_bytes().to_vec()),
        ] {
            let mut broken = b.clone();
            broken[offset..offset + value.len()].copy_from_slice(&value);
            assert!(preview(&broken).is_err(), "offset {offset}");
        }
        for version in 7..12 {
            let mut legacy = b.clone();
            legacy[4..6].copy_from_slice(&(version as u16).to_le_bytes());
            assert!(inspect(&legacy).is_ok());
            assert!(matches!(preview(&legacy), Err(Error::Unsupported(_))));
        }
    }

    #[test]
    fn an_unindexed_png_or_resource_is_never_used() {
        let mut b = fixture();
        b[24..32].copy_from_slice(&0u64.to_le_bytes());
        assert!(preview(&b).is_err());
        b[24..32].copy_from_slice(&72u64.to_le_bytes());
        b[76..80].copy_from_slice(b"Meta");
        assert!(preview(&b).is_err());
    }

    #[test]
    fn every_chunk_crc_including_iend_and_ancillary_is_checked() {
        let mut bad_iend = fixture();
        *bad_iend.last_mut().unwrap() ^= 1;
        assert_eq!(preview(&bad_iend).unwrap_err(), Error::Malformed("PNG chunk CRC mismatch"));

        let b = fixture();
        let mut png = preview(&b).unwrap().png.to_vec();
        let tagged = b"tEXtnote\0preview";
        let mut chunk = ((tagged.len() - 4) as u32).to_be_bytes().to_vec();
        chunk.extend(tagged);
        chunk.extend(crc32fast::hash(tagged).to_be_bytes());
        let end = png.len() - 12;
        png.splice(end..end, chunk.clone());
        assert!(preview(&file(&png)).is_ok());
        png[end + chunk.len() - 1] ^= 1;
        assert_eq!(preview(&file(&png)).unwrap_err(), Error::Malformed("PNG chunk CRC mismatch"));
    }

    #[test]
    fn compressed_metadata_and_animation_are_rejected_before_pixel_decoding() {
        let b = fixture();
        for kind in [b"zTXt", b"iTXt", b"iCCP", b"acTL", b"fcTL", b"fdAT"] {
            let mut png = preview(&b).unwrap().png.to_vec();
            // The chunk claims compressed metadata without making the parser inflate it.
            let mut tagged = kind.to_vec();
            tagged.extend(b"preview\0\0\x78\x9c");
            let mut chunk = ((tagged.len() - 4) as u32).to_be_bytes().to_vec();
            chunk.extend(&tagged);
            chunk.extend(crc32fast::hash(&tagged).to_be_bytes());
            let end = png.len() - 12;
            png.splice(end..end, chunk);
            assert!(matches!(preview(&file(&png)), Err(Error::Unsupported(_))), "{kind:?}");
        }
    }

    #[test]
    fn critical_chunk_order_is_checked_even_after_image_data() {
        let b = fixture();
        let original = preview(&b).unwrap().png.to_vec();
        for trailing in [chunk(b"IHDR", &original[16..29]), chunk(b"PLTE", &[0, 0, 0]), chunk(b"ABCD", &[])] {
            let mut png = original.clone();
            let end = png.len() - 12;
            png.splice(end..end, trailing);
            assert!(preview(&file(&png)).is_err());
        }
        let mut no_image = original.clone();
        no_image.drain(33..45);
        assert!(preview(&file(&no_image)).is_err());
        let mut duplicate_palette = original.clone();
        duplicate_palette.splice(33..33, chunk(b"PLTE", &[0, 0, 0]).repeat(2));
        assert!(preview(&file(&duplicate_palette)).is_err());
        let mut split_image = original;
        let end = split_image.len() - 12;
        let mut interrupted = chunk(b"tEXt", b"note\0preview");
        interrupted.extend(chunk(b"IDAT", &[]));
        split_image.splice(end..end, interrupted);
        assert!(preview(&file(&split_image)).is_err());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        #[test]
        fn arbitrary_input_never_panics(b in prop::collection::vec(any::<u8>(), 0..2048)) {
            let _ = inspect(&b);
            let _ = preview(&b);
        }
        #[test]
        fn mutations_never_panic(edits in prop::collection::vec((0usize..200, any::<u8>()), 0..32)) {
            let mut b = fixture();
            for (i, value) in edits { if let Some(v) = b.get_mut(i) { *v = value; } }
            let _ = preview(&b);
        }
    }
}
