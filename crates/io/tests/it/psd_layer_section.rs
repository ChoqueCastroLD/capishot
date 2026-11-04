//! Saved PSDs open as layers in Photoshop (Discord report, 2026-11-04: "they open as one merged
//! image"). Photoshop falls back to the merged image when the layer and mask section is missing,
//! empty, has a bad length or a record it can't read, so every save is walked here byte by byte,
//! independently of `photocraft-psd`'s reader, the way a strict reader does: the section's
//! lengths add up exactly, every record and its blocks parse to their own lengths, every channel's
//! RLE rows decode to the layer's width, and the channel data ends where the layer info does.
//! Covered: type layers, groups, masks, effects-free styles, 8/16-bit, Grayscale, a tall
//! 1200 × 20000 strip, a strip past 30000 px (PSB), and incremental saves through
//! `PsdExportCache` (second and third saves after edits, byte-equal to full saves).

use photocraft_color::{ColorMode, SampleType};
use photocraft_doc::{Document, Layer, LayerContent, Size, TextLayer};
use photocraft_geom::Rect;
use photocraft_io::{ExportOptions, PsdExportCache, export};

use super::common::{Features, gen_doc, raster};

/// What a strict walk of a PSD/PSB found.
#[derive(Debug)]
struct Walk {
    psb: bool,
    depth: u16,
    /// Layer records (in the section, or in the global `Lr16`/`Lr32` block).
    records: usize,
    names: Vec<String>,
}

struct R<'a> {
    b: &'a [u8],
    at: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.b.len()).ok_or_else(|| format!("{n} bytes past the end at {}", self.at))?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    fn uint(&mut self, n: usize) -> Result<u64, String> {
        Ok(self.take(n)?.iter().fold(0u64, |v, &x| (v << 8) | u64::from(x)))
    }
    fn len(&mut self, n: usize) -> Result<usize, String> {
        usize::try_from(self.uint(n)?).map_err(|e| e.to_string())
    }
}

/// Keys whose length field is 8 bytes in a PSB (Adobe's specification).
fn long_key(k: &[u8]) -> bool {
    matches!(k, b"LMsk" | b"Lr16" | b"Lr32" | b"Layr" | b"Mt16" | b"Mt32" | b"Mtrn" | b"Alph" | b"FMsk" | b"lnk2" | b"FEid" | b"FXid" | b"PxSD")
}

/// The layer info body starting at `r.at` and ending at `end`.
fn walk_layer_info(r: &mut R<'_>, end: usize, psb: bool, depth: u16, out: &mut Walk) -> Result<(), String> {
    let l = if psb { 8 } else { 4 };
    let count = r.uint(2)? as u16 as i16;
    let n = usize::from(count.unsigned_abs());
    let mut layers = Vec::with_capacity(n);
    for i in 0..n {
        let rect = r.take(16)?;
        let v = |k: usize| i32::from_be_bytes([rect[k], rect[k + 1], rect[k + 2], rect[k + 3]]);
        let (top, left, bottom, right) = (v(0), v(4), v(8), v(12));
        if bottom < top || right < left {
            return Err(format!("record {i}: inverted rect"));
        }
        let channels = r.len(2)?;
        if channels > 56 {
            return Err(format!("record {i}: {channels} channels"));
        }
        let mut ch = Vec::new();
        for _ in 0..channels {
            let id = r.uint(2)? as u16 as i16;
            ch.push((id, r.len(l)?));
        }
        if r.take(4)? != b"8BIM" {
            return Err(format!("record {i}: bad blend signature"));
        }
        r.take(8)?;
        let extra = r.len(4)?;
        let extra_end = r.at.checked_add(extra).ok_or("extra overflow")?;
        let mask = r.len(4)?;
        if ![0, 20, 36].contains(&mask) && mask < 20 {
            return Err(format!("record {i}: mask data of {mask} bytes"));
        }
        r.take(mask)?;
        let ranges = r.len(4)?;
        if ranges % 8 != 0 {
            return Err(format!("record {i}: blending ranges of {ranges} bytes"));
        }
        r.take(ranges)?;
        let name_len = usize::from(r.take(1)?[0]);
        let name = String::from_utf8_lossy(r.take(name_len)?).to_string();
        r.take((4 - (name_len + 1) % 4) % 4)?;
        while r.at < extra_end {
            let sig = r.take(4)?.to_vec();
            if sig != b"8BIM" && sig != b"8B64" {
                return Err(format!("record {i} ({name}): block signature {sig:?} at {}", r.at - 4));
            }
            let key = r.take(4)?.to_vec();
            let n = r.len(if psb && long_key(&key) { 8 } else { 4 })?;
            if n % 2 != 0 {
                return Err(format!("record {i} ({name}): {} block of odd length {n}", String::from_utf8_lossy(&key)));
            }
            r.take(n)?;
        }
        if r.at != extra_end {
            return Err(format!("record {i} ({name}): its blocks end at {}, the record at {extra_end}", r.at));
        }
        out.names.push(name);
        layers.push(((top, left, bottom, right), ch));
    }
    let bps = usize::from(depth / 8).max(1);
    let rows_len = if psb { 4 } else { 2 };
    for (i, ((top, left, bottom, right), ch)) in layers.iter().enumerate() {
        for &(id, len) in ch {
            let data = r.take(len)?;
            if len < 2 {
                return Err(format!("record {i} channel {id}: {len} bytes"));
            }
            // User and real masks have their own rects; the pixel channels the layer's.
            if id < -1 {
                continue;
            }
            let (w, h) = (usize::try_from(right - left).unwrap_or(0), usize::try_from(bottom - top).unwrap_or(0));
            let row = w * bps;
            match u16::from_be_bytes([data[0], data[1]]) {
                0 if w * h > 0 && len != 2 + row * h => return Err(format!("record {i} channel {id}: raw {len} bytes for {w}×{h}")),
                1 if w * h > 0 => {
                    let mut c = R { b: data, at: 2 };
                    let counts: Vec<usize> = (0..h).map(|_| c.len(rows_len)).collect::<Result<_, _>>()?;
                    for (y, n) in counts.into_iter().enumerate() {
                        let packed = c.take(n)?;
                        let (mut p, mut got) = (0, 0);
                        while p < packed.len() {
                            let k = packed[p];
                            p += 1;
                            if k < 128 {
                                got += usize::from(k) + 1;
                                p += usize::from(k) + 1;
                            } else if k > 128 {
                                got += 257 - usize::from(k);
                                p += 1;
                            }
                        }
                        if p != packed.len() || got != row {
                            return Err(format!("record {i} channel {id} row {y}: {got} of {row} bytes"));
                        }
                    }
                    if c.at != data.len() {
                        return Err(format!("record {i} channel {id}: {} bytes after the rows", data.len() - c.at));
                    }
                }
                0..=3 => {}
                c => return Err(format!("record {i} channel {id}: compression {c}")),
            }
        }
    }
    if r.at > end || end - r.at > 3 {
        return Err(format!("channel data ends at {}, the layer info at {end}", r.at));
    }
    r.at = end;
    out.records = n;
    Ok(())
}

/// Walks a PSD/PSB strictly; an error says where a strict reader would give up on the layers.
fn walk(b: &[u8]) -> Result<Walk, String> {
    let mut r = R { b, at: 0 };
    if r.take(4)? != b"8BPS" {
        return Err("signature".into());
    }
    let version = r.uint(2)?;
    let psb = version == 2;
    r.take(6)?;
    let _channels = r.uint(2)?;
    let (height, width, depth) = (r.uint(4)?, r.uint(4)?, r.uint(2)? as u16);
    if !psb && (height > 30000 || width > 30000) {
        return Err(format!("a PSD of {width}×{height}: past 30000 px it must be a PSB"));
    }
    r.take(2)?;
    let n = r.len(4)?;
    r.take(n)?;
    let n = r.len(4)?;
    r.take(n)?;
    let l = if psb { 8 } else { 4 };
    let section = r.len(l)?;
    if section == 0 {
        return Err("no layer and mask section".into());
    }
    let section_end = r.at.checked_add(section).filter(|&e| e <= b.len()).ok_or("the layer and mask section runs past the file")?;
    let mut out = Walk { psb, depth, records: 0, names: Vec::new() };
    let info = r.len(l)?;
    if info % 2 != 0 {
        return Err(format!("layer info of odd length {info}"));
    }
    if info > 0 {
        let end = r.at + info;
        walk_layer_info(&mut r, end, psb, depth, &mut out)?;
    }
    if r.at < section_end {
        let n = r.len(4)?;
        r.take(n)?;
    }
    while r.at + 12 <= section_end {
        let sig = r.take(4)?.to_vec();
        if sig != b"8BIM" && sig != b"8B64" {
            return Err(format!("global block signature {sig:?} at {}", r.at - 4));
        }
        let key = r.take(4)?.to_vec();
        let n = r.len(if psb && long_key(&key) { 8 } else { 4 })?;
        let end = r.at + n;
        if matches!(&key[..], b"Lr16" | b"Lr32") {
            if out.records > 0 {
                return Err("layers in the section and in a global block".into());
            }
            walk_layer_info(&mut r, end, psb, depth, &mut out)?;
        } else {
            r.take(n)?;
        }
        // Global blocks are padded to 4, after their length (zeros).
        let pad = (4 - n % 4) % 4;
        if b.get(r.at..r.at + pad).is_some_and(|p| p.iter().all(|&x| x == 0)) && r.at + pad <= section_end {
            r.at += pad;
        }
    }
    if r.at != section_end {
        return Err(format!("the layer and mask section ends at {}, its blocks at {}", section_end, r.at));
    }
    if out.records == 0 {
        return Err("no layer records".into());
    }
    Ok(out)
}

fn type_layer(name: &str, text: &str, x: f32, y: f32) -> Layer {
    let mut t = TextLayer { text: text.into(), size_pt: 24.0, ..TextLayer::default() };
    t.transform = photocraft_geom::Affine::translate(f64::from(x), f64::from(y));
    Layer::new(name, LayerContent::Text(t))
}

/// A scanlation page: the raw, a cleaning layer, a group of type layers with long lines.
fn page(w: u32, h: u32, mode: ColorMode, depth: SampleType) -> Document {
    let mut d = Document::new("page", Size::new(w, h), mode, depth);
    let fmt = d.pixel_format();
    let mut bg = raster("Background", fmt, d.bounds(), 1, false);
    bg.locks.transparency = true;
    d.layers.push(bg);
    d.layers.push(raster("limpieza", fmt, Rect::from_xywh(10, 10, w / 3, h / 5), 2, true));
    let long = "¿QUÉ ESTÁS HACIENDO AQUÍ? ¡TE DIJE QUE NO VINIERAS! ESTE LUGAR ES PELIGROSO, HAY MONSTRUOS POR TODAS PARTES Y NO QUIERO QUE TE PASE NADA MALO, ASÍ QUE VUELVE A CASA AHORA MISMO.";
    let texts = (0..4).map(|i| type_layer(&format!("texto {i}"), if i == 0 { long } else { "HOLA" }, 20.0, 60.0 + 200.0 * i as f32)).collect();
    d.layers.push(Layer::group("Textos", texts));
    d
}

fn save(doc: &Document, cache: Option<&PsdExportCache>) -> Vec<u8> {
    let opts = ExportOptions { psd_cache: cache.cloned(), ..Default::default() };
    export(doc, "page.psd", &opts).unwrap().bytes
}

fn checked(doc: &Document, bytes: &[u8], what: &str) -> Walk {
    let w = walk(bytes).unwrap_or_else(|e| panic!("{what}: {e}"));
    // Every layer is a record; a group adds its divider.
    fn records(layers: &[Layer]) -> usize {
        layers.iter().map(|l| if let LayerContent::Group(g) = &l.content { 2 + records(&g.children) } else { 1 }).sum()
    }
    assert_eq!(w.records, records(&doc.layers), "{what}: layer records {:?}", w.names);
    w
}

#[test]
fn the_walker_rejects_a_flat_or_broken_layer_section() {
    let doc = page(300, 400, ColorMode::Rgb, SampleType::U8);
    let bytes = save(&doc, None);
    assert!(walk(&bytes).is_ok());
    // The section a flat file has, and one whose length is off by two.
    let file = photocraft_psd::PsdFile::from_bytes(&bytes).unwrap();
    let flat = photocraft_psd::PsdFile { layer_info: None, global_blocks: Vec::new(), global_layer_mask: None, ..file.clone() };
    assert!(walk(&flat.to_bytes().unwrap()).is_err());
    let at = 26 + 4 + 4 + u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
    let mut bad = bytes.clone();
    let n = u32::from_be_bytes(bad[at..at + 4].try_into().unwrap()) + 2;
    bad[at..at + 4].copy_from_slice(&n.to_be_bytes());
    assert!(walk(&bad).is_err());
}

#[test]
fn saved_pages_keep_their_layer_section() {
    for (mode, depth) in [(ColorMode::Rgb, SampleType::U8), (ColorMode::Rgb, SampleType::U16), (ColorMode::Grayscale, SampleType::U8)] {
        let doc = page(900, 1300, mode, depth);
        let w = checked(&doc, &save(&doc, None), &format!("{mode:?} {depth:?}"));
        assert!(!w.psb && w.names.iter().any(|n| n.starts_with("¿QUÉ") || n.starts_with("texto")), "{:?}", w.names);
        let generated = gen_doc(mode, depth, Features::ALL);
        walk(&save(&generated, None)).unwrap_or_else(|e| panic!("generated {mode:?} {depth:?}: {e}"));
    }
}

#[test]
fn incremental_saves_keep_their_layer_section() {
    for depth in [SampleType::U8, SampleType::U16] {
        let cache = PsdExportCache::default();
        let mut doc = page(900, 1300, ColorMode::Rgb, depth);
        let first = save(&doc, Some(&cache));
        assert_eq!(first, save(&doc, None), "{depth:?}: first save");
        checked(&doc, &first, "first save");
        // Second save: a new type layer and a stroke on the cleaning layer.
        doc.layers.push(type_layer("nuevo", "SEGUNDO", 40.0, 900.0));
        if let Some(s) = doc.layers[1].surface_mut() {
            s.fill_rect(Rect::from_xywh(30, 30, 40, 40), &[1.0, 1.0, 1.0, 1.0]);
        }
        let second = save(&doc, Some(&cache));
        assert_eq!(second, save(&doc, None), "{depth:?}: second save");
        checked(&doc, &second, "second save");
        // Third: a layer removed and the rest unchanged.
        doc.layers.remove(1);
        let third = save(&doc, Some(&cache));
        assert_eq!(third, save(&doc, None), "{depth:?}: third save");
        checked(&doc, &third, "third save");
        // And again with nothing changed.
        assert_eq!(save(&doc, Some(&cache)), third, "{depth:?}: unchanged save");
    }
}

#[test]
fn webtoon_strips_keep_their_layer_section() {
    // Within PSD's 30000 px: a PSD.
    let doc = page(1200, 20000, ColorMode::Rgb, SampleType::U8);
    let w = checked(&doc, &save(&doc, None), "1200×20000");
    assert!(!w.psb && w.depth == 8);
    // Past it: written as PSB (Photoshop's Large Document Format), still layered.
    let doc = page(300, 30500, ColorMode::Rgb, SampleType::U8);
    let w = checked(&doc, &save(&doc, None), "300×30500");
    assert!(w.psb);
}
