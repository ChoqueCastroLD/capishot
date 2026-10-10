//! [psd-compiler](https://github.com/ChoqueCastroLD/psd-compiler-cli): Capishot's PSD renderer.
//!
//! psd-compiler matches the composite Photoshop stores on 508 of 515 real Photoshop files
//! (photocraft-compose: 395), so Capishot uses it for what other programs see:
//!
//! * the merged image of every 8-bit RGB PSD/PSB it saves ([`recomposite`]), so viewers, web
//!   readers and psd-tools show what Photoshop would;
//! * File › Export › Compile PSD and `capishot-cli compile` ([`compile`]), which can also
//!   re-render type layers from their text with the given fonts.
//!
//! Desktop only: psd-compiler reads fonts from disk and renders on all cores.

use std::path::PathBuf;

use photocraft_doc::Document;
use photocraft_psd::{ColorMode, PsdFile};

use crate::IoError;

/// What [`compile`] renders with.
#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    /// Re-render type layers from their text (needs the fonts) instead of using the raster
    /// stored with the layer.
    pub rerender_text: bool,
    /// Extra font folders, searched before the system fonts.
    pub font_dirs: Vec<PathBuf>,
    /// Also search the system font folders.
    pub system_fonts: bool,
}

/// A compiled image: straight 8-bit RGBA, row-major.
#[derive(Debug, Clone)]
pub struct Compiled {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Features psd-compiler skipped or approximated (missing fonts, unsupported filters…).
    pub warnings: Vec<String>,
}

impl Compiled {
    /// Encodes as `ext` (`png`, `jpg`/`jpeg`, `webp`, `tif`/`tiff`); JPEG flattens onto white.
    pub fn encode(&self, ext: &str, quality: Option<u8>) -> Result<Vec<u8>, IoError> {
        let format = psd_compiler::Format::from_extension(ext).ok_or_else(|| IoError::UnknownFormat(ext.to_string()))?;
        if format == psd_compiler::Format::Avif {
            return Err(IoError::Unsupported("AVIF is not in this build of psd-compiler".into()));
        }
        let mut options = psd_compiler::EncodeOptions::default();
        if let Some(q) = quality {
            options.quality = q.clamp(1, 100);
        }
        let image = psd_compiler::Image { width: self.width, height: self.height, data: self.rgba.clone() };
        image.encode(format, &options).map_err(|e| IoError::Unsupported(format!("psd-compiler: {e}")))
    }
}

/// Renders PSD/PSB `bytes` with psd-compiler.
pub fn compile(bytes: &[u8], opts: &CompileOptions) -> Result<Compiled, IoError> {
    let doc = psd_compiler::Document::parse(bytes).map_err(|e| IoError::Unsupported(format!("psd-compiler: {e}")))?;
    let mut fonts = psd_compiler::FontDb::new();
    if opts.rerender_text {
        for dir in &opts.font_dirs {
            fonts.add_dir(dir);
        }
        if opts.system_fonts {
            fonts.add_system_fonts();
        }
    }
    let options = psd_compiler::RenderOptions { keep_text_raster: !opts.rerender_text, ..Default::default() };
    let out = psd_compiler::render(&doc, &fonts, &options);
    Ok(Compiled {
        width: out.image.width,
        height: out.image.height,
        rgba: out.image.data,
        warnings: out.warnings.iter().map(ToString::to_string).collect(),
    })
}

/// Compiles `doc`: writes it as a PSD in memory, then renders that with psd-compiler.
pub fn compile_document(doc: &Document, opts: &CompileOptions) -> Result<Compiled, IoError> {
    let (file, _) = crate::document_to_psd_with(doc, &crate::PsdExportOptions { force_psb: !fits_psd(doc), ..Default::default() });
    file.header.validate()?;
    compile(&file.to_bytes()?, opts)
}

fn fits_psd(doc: &Document) -> bool {
    doc.size.width <= 30_000 && doc.size.height <= 30_000
}

/// Replaces the merged image of `file` (already written as `bytes`) with psd-compiler's render,
/// keeping its alpha channels. Only 8-bit RGB files are rewritten; returns whether `file` changed.
/// The stored type rasters are used, so the merged image shows the type as the canvas does.
pub(crate) fn recomposite(file: &mut PsdFile, bytes: &[u8]) -> Result<bool, IoError> {
    let h = &file.header;
    if h.depth != 8 || h.color_mode != ColorMode::Rgb || file.layer_info.as_ref().is_none_or(|l| l.layers.is_empty()) {
        return Ok(false);
    }
    let compiled = compile(bytes, &CompileOptions::default())?;
    if compiled.width != h.width || compiled.height != h.height {
        return Ok(false);
    }
    let n = (h.width as usize).saturating_mul(h.height as usize);
    let mut planes = file.image_data.decode(h)?;
    let has_alpha = file.merged_has_alpha();
    let replaced = if has_alpha { 4 } else { 3 };
    if planes.len() < n.saturating_mul(replaced) || compiled.rgba.len() < n.saturating_mul(4) {
        return Ok(false);
    }
    for (i, px) in compiled.rgba.as_chunks::<4>().0.iter().take(n).enumerate() {
        let a = f32::from(px[3]) / 255.0;
        for (c, &v) in px.iter().take(3).enumerate() {
            // With an alpha channel Photoshop mattes the merged colours against white.
            let v = if has_alpha { crate::pixels::matte(f32::from(v), a, 255.0).round().clamp(0.0, 255.0) as u8 } else { v };
            if let Some(slot) = planes.get_mut(c * n + i) {
                *slot = v;
            }
        }
        if has_alpha && let Some(slot) = planes.get_mut(3 * n + i) {
            *slot = px[3];
        }
    }
    file.image_data = photocraft_psd::ImageData::encode(file.image_data.compression, &planes, h)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::Layer;
    use photocraft_geom::{Rect, Size};

    /// (51, 102, 204) under 50 % red: (153, 51, 102) wherever both cover.
    fn two_layer_doc() -> Document {
        let mut doc = Document::new("t", Size::new(64, 48), ColorMode::Rgb, SampleType::U8);
        let fmt = doc.pixel_format();
        let mut bg = Layer::raster("blue", fmt);
        bg.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &[0.2, 0.4, 0.8, 1.0]);
        let mut top = Layer::raster("red", fmt);
        top.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &[1.0, 0.0, 0.0, 1.0]);
        top.opacity = 0.5;
        doc.layers.push(bg);
        doc.layers.push(top);
        doc
    }

    #[test]
    fn compiled_psd_matches_the_compositor() {
        let doc = two_layer_doc();
        let c = compile_document(&doc, &CompileOptions::default()).unwrap();
        assert_eq!((c.width, c.height), (64, 48));
        // 50 % red over (51, 102, 204): (153, 51, 102).
        let p = &c.rgba[..4];
        for (got, want) in p.iter().zip([153u8, 51, 102, 255]) {
            assert!(got.abs_diff(want) <= 1, "{p:?}");
        }
    }

    #[test]
    fn saved_psd_carries_the_compiled_composite() {
        let doc = two_layer_doc();
        let out = crate::export(&doc, "x.psd", &Default::default()).unwrap();
        let file = PsdFile::from_bytes(&out.bytes).unwrap();
        let rgba = file.composite_rgba8().unwrap();
        assert!(rgba.data[..4].iter().zip([153u8, 51, 102, 255]).all(|(g, w)| g.abs_diff(w) <= 1), "{:?}", &rgba.data[..4]);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(compile(b"8BPS garbage", &CompileOptions::default()).is_err());
        assert!(compile(&[], &CompileOptions::default()).is_err());
    }
}
