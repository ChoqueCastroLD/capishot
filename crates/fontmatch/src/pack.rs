//! A library of faces whose files aren't here: their alphabets, and where to get each file. It's
//! how a search can look through a whole catalog of fonts without having them, and it's optional:
//! the app fetches [`URL`] only when a person asks, and each font only when it's an answer.
//!
//! The file is gzip over this, little endian: `CSFP1\n`, the base URL, the number of faces, then
//! each face's family, style and file path (strings: u32 length, UTF-8), `u32` index in the file,
//! `f32` size for the cap height, `u64` key, `u8` glyphs, each glyph its `u32` character, `u16`
//! width and height and its bits. Written by `examples/pack.rs`.

use std::collections::HashMap;
use std::io::{Read, Write};

use crate::index::{Face, Glyph, Packed};

/// The Google Fonts pack, and how much is expected of it.
pub const URL: &str = "https://r2.capichan.com/capishot/fontmatch/google-shapes-v1.bin.gz";
pub const SHA256: &str = "4d9bd276ec7b1b490bfc834c10dc541b67fd9fccd38f39e96d6e93564b1a10e9";
pub const BYTES: u64 = 16_371_008;

const MAGIC: &[u8] = b"CSFP1\n";
/// Larger than this isn't a pack.
const MAX_UNPACKED: u64 = 1 << 30;

/// Faces and where their files are.
#[derive(Clone, Debug, Default)]
pub struct Pack {
    pub base_url: String,
    pub faces: Vec<Face>,
    files: HashMap<(String, String), String>,
}

impl Pack {
    /// Where the file of a face in the pack is.
    pub fn url(&self, family: &str, style: &str) -> Option<String> {
        self.files.get(&(family.to_string(), style.to_string())).map(|p| format!("{}{p}", self.base_url))
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

/// `faces` with their file paths, as a pack file.
pub fn write(base_url: &str, faces: &[(Face, String, u32)]) -> std::io::Result<Vec<u8>> {
    let mut raw = MAGIC.to_vec();
    put_str(&mut raw, base_url);
    raw.extend_from_slice(&(faces.len() as u32).to_le_bytes());
    for (f, path, index) in faces {
        put_str(&mut raw, &f.family);
        put_str(&mut raw, &f.style);
        put_str(&mut raw, path);
        raw.extend_from_slice(&index.to_le_bytes());
        raw.extend_from_slice(&f.size_for_cap.to_le_bytes());
        raw.extend_from_slice(&f.key.to_le_bytes());
        raw.push(u8::try_from(f.glyphs.len()).unwrap_or(u8::MAX));
        for g in f.glyphs.iter() {
            raw.extend_from_slice(&(g.ch as u32).to_le_bytes());
            raw.extend_from_slice(&(g.bitmap.width as u16).to_le_bytes());
            raw.extend_from_slice(&(g.bitmap.height as u16).to_le_bytes());
            raw.extend_from_slice(&g.bitmap.to_bytes());
        }
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&raw)?;
    gz.finish()
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)?.try_into().ok().map(u32::from_le_bytes)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2)?.try_into().ok().map(u16::from_le_bytes)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        String::from_utf8(self.take(n)?.to_vec()).ok()
    }
}

fn parse(raw: &[u8]) -> Option<Pack> {
    let mut r = Reader(raw);
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let base_url = r.string()?;
    let n = r.u32()? as usize;
    let mut pack = Pack { base_url, faces: Vec::with_capacity(n.min(1 << 16)), files: HashMap::with_capacity(n.min(1 << 16)) };
    for _ in 0..n {
        let (family, style, path) = (r.string()?, r.string()?, r.string()?);
        let _index = r.u32()?;
        let size = f32::from_le_bytes(r.take(4)?.try_into().ok()?);
        let key = u64::from_le_bytes(r.take(8)?.try_into().ok()?);
        let count = usize::from(r.take(1)?[0]);
        let mut glyphs = Vec::with_capacity(count);
        for _ in 0..count {
            let ch = char::from_u32(r.u32()?)?;
            let (width, height) = (usize::from(r.u16()?), usize::from(r.u16()?));
            let bits = r.take((width * height).div_ceil(8))?;
            glyphs.push(Glyph { ch, bitmap: Packed::from_bytes(width, height, bits) });
        }
        pack.files.insert((family.clone(), style.clone()), path);
        pack.faces.push(Face::from_glyphs(family, style, size, glyphs, key));
    }
    Some(pack)
}

/// A pack file's faces; `None` when it isn't one.
pub fn read(file: &[u8]) -> Option<Pack> {
    let mut raw = Vec::new();
    flate2::read::GzDecoder::new(file).take(MAX_UNPACKED).read_to_end(&mut raw).ok()?;
    parse(&raw)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::Bitmap;

    fn glyph(ch: char, on: usize) -> Glyph {
        let mut b = Bitmap::new(9, 7);
        b.data[on] = true;
        Glyph { ch, bitmap: Packed::new(&b) }
    }

    #[test]
    fn a_pack_reads_back_what_was_written() {
        let face = Face::from_glyphs("Fam".into(), "Bold".into(), 41.5, vec![glyph('A', 3), glyph('b', 62)], 77);
        let file = write("https://x/", &[(face.clone(), "ofl/fam/Fam-Bold.ttf".into(), 0)]).unwrap();
        let pack = read(&file).unwrap();
        assert_eq!(pack.url("Fam", "Bold").as_deref(), Some("https://x/ofl/fam/Fam-Bold.ttf"));
        assert_eq!(pack.url("Fam", "Regular"), None);
        let back = &pack.faces[0];
        assert_eq!((back.family.as_str(), back.style.as_str(), back.size_for_cap, back.key), ("Fam", "Bold", 41.5, 77));
        assert_eq!(back.glyphs[0].bitmap, face.glyphs[0].bitmap);
        assert_eq!(back.glyphs[1].ch, 'b');
        assert_eq!(back.look, face.look, "the same drawing is the same look");
        assert!(read(b"not a pack").is_none());
        assert!(read(&write("u", &[]).unwrap()).is_some_and(|p| p.faces.is_empty()));
    }
}
