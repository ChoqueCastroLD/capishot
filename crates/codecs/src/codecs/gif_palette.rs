//! The colour table of a GIF, read from its blocks without decoding pixels (GIF89a
//! specification: Logical Screen Descriptor, Global and Local Color Tables, Graphic Control
//! Extension).
//!
//! A GIF is always palette-based, so it opens in Indexed Color with the table of the frame that
//! [`crate::decode`] keeps, the first: its Local Color Table when it has one, else the Global
//! one, and the transparent entry that frame's Graphic Control Extension names.

/// Colours of the table the first frame uses, and its transparent entry. `None` for anything
/// that isn't a well-formed GIF up to its first image, or a first frame without a table.
pub(crate) fn palette(b: &[u8]) -> Option<(Vec<[u8; 3]>, Option<u8>)> {
    if !(b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")) {
        return None;
    }
    // Logical Screen Descriptor: width, height, packed fields, background, aspect.
    let packed = *b.get(10)?;
    let mut at = 13;
    let table = |at: usize, packed: u8| -> Option<Vec<[u8; 3]>> {
        let n = 2usize << (packed & 0x07);
        let bytes = b.get(at..at.checked_add(n.checked_mul(3)?)?)?;
        Some(bytes.as_chunks::<3>().0.to_vec())
    };
    let global = if packed & 0x80 != 0 {
        let t = table(at, packed)?;
        at += t.len() * 3;
        Some(t)
    } else {
        None
    };
    let mut transparent = None;
    loop {
        match *b.get(at)? {
            // Extension: label, then data sub-blocks up to a zero-length one.
            0x21 => {
                let label = *b.get(at + 1)?;
                at += 2;
                // Graphic Control Extension: block size 4, packed (bit 0: transparent colour
                // given), delay (2 bytes), transparent colour index.
                if label == 0xF9 && *b.get(at)? == 4 {
                    let flags = *b.get(at + 1)?;
                    transparent = (flags & 0x01 != 0).then_some(*b.get(at + 4)?);
                }
                loop {
                    let size = usize::from(*b.get(at)?);
                    at = at.checked_add(1 + size)?;
                    if size == 0 {
                        break;
                    }
                }
            }
            // Image Descriptor: position and size (8 bytes), packed fields, then the Local Color
            // Table if bit 7 is set. This is the first frame: its table is the one used.
            0x2C => {
                let packed = *b.get(at + 9)?;
                let colors = if packed & 0x80 != 0 { table(at + 10, packed)? } else { global? };
                let transparent = transparent.filter(|i| usize::from(*i) < colors.len());
                return Some((colors, transparent));
            }
            // Trailer, or anything else: no image.
            _ => return None,
        }
    }
}

