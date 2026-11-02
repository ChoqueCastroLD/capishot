//! A quick look at a PSD/PSB before it is opened: its merged (composite) image, reduced, read
//! straight from the file without parsing the layers ([`merged_preview`]). Opening shows it while
//! the layers are built; only the composite's colour planes are read.

use crate::compression::{Compression, packbits};
use crate::header::{ColorMode, Header};
use crate::io::Reader;
use crate::resources::ids;
use crate::source::ReadAt;

/// A reduced composite: 8-bit RGBA (opaque), row-major.
#[derive(Clone, PartialEq, Eq)]
pub struct Preview {
    /// Width of the reduced image.
    pub width: u32,
    /// Height of the reduced image.
    pub height: u32,
    /// The document's width.
    pub doc_width: u32,
    /// The document's height.
    pub doc_height: u32,
    /// Grayscale (else RGB).
    pub gray: bool,
    /// The file's ICC profile (resource 1039), if any: the samples are in its colours.
    pub icc: Option<Vec<u8>>,
    /// `width × height` pixels, RGBA.
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for Preview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Preview").field("size", &(self.width, self.height)).field("doc", &(self.doc_width, self.doc_height)).finish()
    }
}

/// Longest resources section read for the resources this needs (larger ones are passed over by
/// their headers alone, so this only bounds a single resource's data).
const MAX_RESOURCE: u64 = 16 << 20;

fn read(src: &dyn ReadAt, at: u64, n: u64) -> Option<Vec<u8>> {
    let n = usize::try_from(n).ok()?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(n).ok()?;
    buf.resize(n, 0);
    src.read_at(at, &mut buf).ok()?;
    Some(buf)
}

fn be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |v, b| (v << 8) | u64::from(*b))
}

fn read_be(src: &dyn ReadAt, at: u64, n: u64) -> Option<u64> {
    read(src, at, n).map(|b| be(&b))
}

/// What the image resources say about the merged image: whether it is real (1057, "has real
/// merged data"; absent counts as real), and the ICC profile (1039).
fn resources(src: &dyn ReadAt, start: u64, len: u64) -> Option<(bool, Option<Vec<u8>>)> {
    let end = start.checked_add(len)?;
    let (mut at, mut real, mut icc) = (start, true, None);
    // Each block: "8BIM", id (2), Pascal name padded to even, size (4), data padded to even.
    while at.checked_add(12)? <= end {
        let head = read(src, at, 7)?;
        let id = u16::try_from(be(head.get(4..6)?)).ok()?;
        let name_len = u64::from(*head.get(6)?);
        let name = (1 + name_len).next_multiple_of(2);
        let size_at = at.checked_add(6)?.checked_add(name)?;
        let size = read_be(src, size_at, 4)?;
        let data = size_at.checked_add(4)?;
        if data.checked_add(size)? > end {
            return None;
        }
        if size <= MAX_RESOURCE {
            if id == ids::VERSION_INFO {
                // Version (4), then the flag.
                real = read(src, data, size)?.get(4).is_none_or(|&b| b != 0);
            } else if id == ids::ICC_PROFILE {
                icc = Some(read(src, data, size)?);
            }
        }
        at = data.checked_add(size.next_multiple_of(2))?;
    }
    Some((real, icc))
}

/// Row `row` (big-endian samples, `bps` bytes each) reduced by `k`: each output sample the mean
/// of `k` neighbours (the high byte of 16-bit samples).
fn reduce_row(row: &[u8], w: usize, k: usize, bps: usize, out: &mut Vec<u8>) {
    if k == 1 && bps == 1 {
        out.extend_from_slice(row.get(..w).unwrap_or(row));
        return;
    }
    let mut x0 = 0;
    while x0 < w {
        let x1 = (x0 + k).min(w);
        let mut sum = 0u32;
        for x in x0..x1 {
            sum += u32::from(row.get(x * bps).copied().unwrap_or(0));
        }
        let n = (x1 - x0) as u32;
        out.push(((sum + n / 2) / n) as u8);
        x0 = x1;
    }
}

/// Reads `n` bytes at `at` into `buf` (reused between rows).
fn read_into(src: &dyn ReadAt, at: u64, n: u64, buf: &mut Vec<u8>) -> Option<()> {
    let n = usize::try_from(n).ok()?;
    buf.clear();
    buf.try_reserve(n).ok()?;
    buf.resize(n, 0);
    src.read_at(at, buf).ok()
}

/// Where a file's merged image is and how it is stored.
struct Merged {
    header: Header,
    /// Colour planes shown (3 RGB, 1 grayscale).
    color: usize,
    /// The ICC profile, if any.
    icc: Option<Vec<u8>>,
    compression: Compression,
    /// Where its data starts (after the compression field).
    at: u64,
}

/// Reads the sections before the merged image: `None` unless it can be previewed.
fn locate(src: &dyn ReadAt) -> Option<Merged> {
    let head = read(src, 0, 26)?;
    let header = Header::read(&mut Reader::new(&head)).ok()?;
    let color = match header.color_mode {
        ColorMode::Rgb => 3usize,
        ColorMode::Grayscale => 1,
        _ => return None,
    };
    if !(header.depth == 8 || header.depth == 16) || usize::from(header.channels) < color || header.width == 0 || header.height == 0 {
        return None;
    }
    let mut at = 26u64;
    at = at.checked_add(4)?.checked_add(read_be(src, at, 4)?)?;
    let res_len = read_be(src, at, 4)?;
    let (real, icc) = resources(src, at.checked_add(4)?, res_len)?;
    if !real {
        return None;
    }
    at = at.checked_add(4)?.checked_add(res_len)?;
    let lf = if header.version.is_psb() { 8 } else { 4 };
    at = at.checked_add(lf)?.checked_add(read_be(src, at, lf)?)?;
    let compression = Compression::from_u16(u16::try_from(read_be(src, at, 2)?).ok()?);
    Some(Merged { header, color, icc, compression, at: at.checked_add(2)? })
}

/// The colour planes of `m`, rows `rows` each reduced by `k` ([`reduce_row`]).
fn read_planes(src: &dyn ReadAt, m: &Merged, rows: &[usize], k: usize) -> Option<Vec<Vec<u8>>> {
    let (w, h) = (m.header.width as usize, m.header.height as usize);
    let bps = usize::from(m.header.depth / 8);
    let row = w.checked_mul(bps)?;
    let out_len = w.div_ceil(k).checked_mul(rows.len())?;
    let mut planes: Vec<Vec<u8>> = Vec::with_capacity(m.color);
    let (mut enc, mut buf) = (Vec::new(), Vec::new());
    match m.compression {
        Compression::Raw => {
            for c in 0..m.color as u64 {
                let mut out = Vec::with_capacity(out_len);
                let plane = m.at.checked_add(c.checked_mul(h as u64)?.checked_mul(row as u64)?)?;
                for &y in rows {
                    read_into(src, plane.checked_add((y as u64).checked_mul(row as u64)?)?, row as u64, &mut buf)?;
                    reduce_row(&buf, w, k, bps, &mut out);
                }
                planes.push(out);
            }
        }
        Compression::Rle => {
            // The colour planes' row byte counts come first in the table (every plane's rows),
            // the rows' data after the whole table.
            let count = if m.header.version.is_psb() { 4 } else { 2 };
            let n_rows = m.color.checked_mul(h)?;
            let table = read(src, m.at, (n_rows as u64).checked_mul(count)?)?;
            let all_rows = usize::from(m.header.channels).checked_mul(h)? as u64;
            let mut offsets = Vec::with_capacity(n_rows + 1);
            let mut offset = m.at.checked_add(all_rows.checked_mul(count)?)?;
            offsets.push(offset);
            for n in table.chunks_exact(count as usize).map(be) {
                offset = offset.checked_add(n)?;
                offsets.push(offset);
            }
            if offset > src.len() {
                return None;
            }
            for c in 0..m.color {
                let mut out = Vec::with_capacity(out_len);
                for &y in rows {
                    let i = c.checked_mul(h)?.checked_add(y)?;
                    let (start, end) = (*offsets.get(i)?, *offsets.get(i + 1)?);
                    read_into(src, start, end.checked_sub(start)?, &mut enc)?;
                    buf.clear();
                    packbits::decode_into(&enc, row, &mut buf).ok()?;
                    reduce_row(&buf, w, k, bps, &mut out);
                }
                planes.push(out);
            }
        }
        _ => return None,
    }
    Some(planes)
}

/// The merged image of the PSD/PSB in `src`, reduced by a whole factor `k` so its long side is at
/// most `max_side` (each pixel the mean of `k` neighbours along the middle row of its `k`×`k`
/// box: only a `k`-th of the rows is decoded), or `None` when it can't stand for the document:
/// not a PSD, a colour mode or depth it doesn't show (RGB and grayscale at 8 or 16 bits), a
/// merged image that isn't real (saved without Maximize Compatibility, resource 1057) or is one
/// flat colour (the placeholder some writers leave), a ZIP-compressed composite, or anything
/// unreadable.
pub fn merged_preview(src: &dyn ReadAt, max_side: u32) -> Option<Preview> {
    let m = locate(src)?;
    let (w, h) = (m.header.width as usize, m.header.height as usize);
    let k = w.max(h).div_ceil(max_side.clamp(1, 8192) as usize).max(1);
    let (pw, ph) = (w.div_ceil(k), h.div_ceil(k));
    // One row per output row (the middle of its `k`), its columns averaged `k` at a time: a
    // `k`-th of the composite is decoded.
    let rows: Vec<usize> = (0..ph).map(|oy| (oy * k + k / 2).min(h - 1)).collect();
    let planes = read_planes(src, &m, &rows, k)?;
    // One flat colour is a placeholder, not the document.
    if planes.iter().all(|p| p.first().is_none_or(|f| p.iter().all(|v| v == f))) {
        return None;
    }
    let n = pw.checked_mul(ph)?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(n.checked_mul(4)?).ok()?;
    for i in 0..n {
        let px = |c: usize| planes.get(c.min(m.color - 1)).and_then(|p| p.get(i)).copied().unwrap_or(0);
        rgba.extend_from_slice(&[px(0), px(1), px(2), 255]);
    }
    Some(Preview { width: pw as u32, height: ph as u32, doc_width: m.header.width, doc_height: m.header.height, gray: m.color == 1, icc: m.icc, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::Version;

    #[test]
    fn the_merged_image_is_previewed_reduced_for_raw_and_rle() {
        for c in [Compression::Raw, Compression::Rle] {
            for v in [Version::Psd, Version::Psb] {
                let file = crate::testgen::merged_only(v, ColorMode::Rgb, 8, c, 300, 200);
                let bytes = file.to_bytes().unwrap();
                let full = file.decode_merged().unwrap();
                let p = merged_preview(&bytes, 100).unwrap_or_else(|| panic!("{c:?} {v:?}"));
                assert_eq!((p.width, p.height, p.doc_width, p.doc_height), (100, 67, 300, 200));
                // Pixel (x, y) of the preview averages the document's 3 pixels from (3x, 3y + 1).
                let (x, y) = (10usize, 20usize);
                let avg = |plane: usize| {
                    let s: u32 = (0..3).map(|dx| u32::from(full[plane * 300 * 200 + (3 * y + 1) * 300 + 3 * x + dx])).sum();
                    ((s + 1) / 3) as u8
                };
                assert_eq!(&p.rgba[(y * 100 + x) * 4..(y * 100 + x) * 4 + 4], &[avg(0), avg(1), avg(2), 255], "{c:?} {v:?}");
                // At full size it is the merged image.
                let p = merged_preview(&bytes, 300).unwrap();
                assert_eq!(p.rgba[(5 * 300 + 7) * 4], full[5 * 300 + 7]);
            }
        }
    }

    #[test]
    fn no_preview_when_it_could_not_stand_for_the_document() {
        assert!(merged_preview(&b"nope".to_vec(), 100).is_none());
        let cmyk = crate::testgen::merged_only(Version::Psd, ColorMode::Cmyk, 8, Compression::Raw, 8, 8).to_bytes().unwrap();
        assert!(merged_preview(&cmyk, 100).is_none(), "CMYK isn't shown");
        let zip = crate::testgen::merged_only(Version::Psd, ColorMode::Rgb, 8, Compression::Zip, 8, 8).to_bytes().unwrap();
        assert!(merged_preview(&zip, 100).is_none(), "ZIP composites aren't read");
        // A merged image marked as not real (saved without Maximize Compatibility).
        let mut file = crate::testgen::merged_only(Version::Psd, ColorMode::Rgb, 8, Compression::Rle, 30, 20);
        file.resources.push(crate::resources::version_info_resource(false));
        assert!(merged_preview(&file.to_bytes().unwrap(), 100).is_none());
        // A flat placeholder.
        let mut file = crate::testgen::merged_only(Version::Psd, ColorMode::Rgb, 8, Compression::Raw, 30, 20);
        let n = file.image_data.data.len();
        file.image_data.data = vec![255; n];
        assert!(merged_preview(&file.to_bytes().unwrap(), 100).is_none());
    }

    /// Every truncation of a file gives a preview or none, never a panic.
    #[test]
    fn truncated_files_never_panic() {
        for c in [Compression::Raw, Compression::Rle] {
            let bytes = crate::testgen::merged_only(Version::Psb, ColorMode::Rgb, 16, c, 40, 30).to_bytes().unwrap();
            for cut in (0..bytes.len()).step_by(7) {
                let _ = merged_preview(&bytes[..cut].to_vec(), 16);
            }
            // Garbage counts and sizes.
            let mut bad = bytes.clone();
            for b in bad.iter_mut().skip(26).step_by(11) {
                *b = 0xFF;
            }
            let _ = merged_preview(&bad, 16);
        }
    }
}
