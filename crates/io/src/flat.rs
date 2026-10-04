//! Flat raster formats via `photocraft-codecs`.

use std::sync::Arc;

use photocraft_codecs::{self as codecs, ChannelLayout, Format, Image, SampleType as CSample};
use photocraft_color::{BlendMode, ColorMode, PixelFormat, SampleType};
use photocraft_doc::{Document, Layer, LayerContent};
use photocraft_geom::{Rect, Size};
use photocraft_raster::Surface;

use crate::{ExportOptions, ExportResult, ImportResult, IoError};

/// Decodes a flat image into a single-layer document.
pub fn import_flat(name: &str, bytes: &[u8]) -> Result<ImportResult, IoError> {
    let img = codecs::decode(bytes)?;
    image_to_document(name, &img)
}

/// A decoded flat image as a single-layer document.
pub(crate) fn image_to_document(name: &str, img: &Image) -> Result<ImportResult, IoError> {
    let mut warnings = Vec::new();
    let (mode, target_layout) = match img.layout() {
        ChannelLayout::Gray | ChannelLayout::GrayA => (ColorMode::Grayscale, ChannelLayout::GrayA),
        ChannelLayout::Rgb | ChannelLayout::Rgba => (ColorMode::Rgb, ChannelLayout::Rgba),
        ChannelLayout::Cmyk | ChannelLayout::CmykA => (ColorMode::Cmyk, ChannelLayout::CmykA),
    };
    let (depth, csample) = match img.sample_type() {
        CSample::U8 => (SampleType::U8, CSample::U8),
        CSample::U16 => (SampleType::U16, CSample::U16),
        CSample::F16 => {
            warnings.push("16-bit float samples are stored as 32-bit float".to_string());
            (SampleType::F32, CSample::F32)
        }
        CSample::F32 => (SampleType::F32, CSample::F32),
    };
    let (w, h) = img.dimensions();
    let mut doc = Document::new(name, Size::new(w, h), mode, depth);
    let conv = img.convert(target_layout, csample);
    let fmt = PixelFormat::new(mode, depth, true);
    let mut s = Surface::from_interleaved(fmt, Rect::new(0, 0, w as i32, h as i32), conv.data());
    s.prune();
    let mut bg = Layer::new("Background", LayerContent::Raster(s));
    if !img.layout().has_alpha() {
        bg.locks.transparency = true;
        bg.locks.position = true;
    }
    doc.layers.push(bg);
    doc.icc_profile = img.icc.clone().map(Arc::new);
    doc.metadata.exif = img.meta.exif.clone().map(Arc::new);
    doc.metadata.xmp = img.meta.xmp.clone();
    if let Some((x, _)) = img.meta.dpi {
        doc.resolution_dpi = x;
    }
    if !img.meta.text.is_empty() {
        warnings.push(format!("{} text metadata entries are not kept in the document", img.meta.text.len()));
    }
    Ok(ImportResult { document: doc, warnings })
}

/// `Some(surface)` when the document is exactly one visible, unmasked,
/// normal, fully opaque raster layer: its pixels can be written natively
/// (keeping CMYK / depth exactly) instead of going through the compositor.
fn single_layer(doc: &Document) -> Option<&Surface> {
    let [l] = &doc.layers[..] else { return None };
    let ok = l.visible
        && l.opacity >= 1.0
        && l.fill_opacity >= 1.0
        && l.mask.is_none()
        && l.effects.items.is_empty()
        && l.effects.psd_raw.is_none()
        && matches!(l.blend, BlendMode::Normal | BlendMode::PassThrough);
    match (&l.content, ok) {
        (LayerContent::Raster(s), true) if s.format() == doc.pixel_format() => Some(s),
        _ => None,
    }
}

fn layout_for(mode: ColorMode, alpha: bool) -> ChannelLayout {
    match (mode, alpha) {
        (ColorMode::Grayscale, false) => ChannelLayout::Gray,
        (ColorMode::Grayscale, true) => ChannelLayout::GrayA,
        (ColorMode::Cmyk, false) => ChannelLayout::Cmyk,
        (ColorMode::Cmyk, true) => ChannelLayout::CmykA,
        (_, false) => ChannelLayout::Rgb,
        (_, true) => ChannelLayout::Rgba,
    }
}

fn csample(s: SampleType) -> CSample {
    match s {
        SampleType::U8 => CSample::U8,
        SampleType::U16 => CSample::U16,
        SampleType::F32 => CSample::F32,
    }
}

/// Renders the document to a flat codec image (native pixels when possible).
pub fn document_to_image(doc: &Document, warnings: &mut Vec<String>) -> Result<Image, IoError> {
    let (w, h) = (doc.size.width, doc.size.height);
    let canvas = doc.bounds();
    let fmt = doc.pixel_format();
    let n = (w as usize) * (h as usize);
    // Lab has no flat-format layout here: Lab documents always go through the composite.
    let native = single_layer(doc).filter(|_| fmt.mode != ColorMode::Lab);
    let mut icc = doc.icc_profile.as_ref().map(|i| i.to_vec());
    let img = if let Some(s) = native {
        // Native path: keep model and depth.
        let vals = s.read_region(canvas);
        let ch = fmt.channels();
        let opaque = vals.chunks_exact(ch).all(|p| p[ch - 1] >= 1.0);
        let layout = layout_for(fmt.mode, !opaque);
        let data: Vec<f32> = if opaque { vals.chunks_exact(ch).flat_map(|p| p[..ch - 1].to_vec()).collect() } else { vals };
        Image::from_normalized(w, h, layout, csample(fmt.sample), &data)?
    } else {
        let count = doc.layer_count();
        warnings.push(format!("{count} layer(s) flattened; layers, masks and blend modes are not kept"));
        let buf = photocraft_compose::flatten(doc);
        let opaque = buf.px.iter().all(|p| p[3] >= 1.0);
        // The compositor works in RGB; write RGB/gray.
        let gray = fmt.mode == ColorMode::Grayscale;
        if fmt.mode == ColorMode::Cmyk || fmt.mode == ColorMode::Lab {
            // The compositor renders CMYK/Lab documents in sRGB (CMYK through the built-in
            // profile), so the file is tagged sRGB.
            warnings.push(format!("{:?} composite written as sRGB RGB (colour-managed conversion)", fmt.mode));
            icc = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes().to_vec());
        }
        let layout = layout_for(if gray { ColorMode::Grayscale } else { ColorMode::Rgb }, !opaque);
        // Quantised straight from the composite in bands on all cores (the same rounding as
        // `Image::from_normalized`, without a full-size f32 copy).
        let cs = csample(fmt.sample);
        let parts = crate::pixels::par_map(crate::pixels::bands(n), |range| {
            let mut out = Vec::with_capacity(range.len() * layout.channels() * cs.bytes());
            let mut put = |v: f32| {
                let v = if v.is_nan() { 0.0 } else { v };
                match cs {
                    CSample::U8 => out.push((v.clamp(0.0, 1.0) * 255.0).round() as u8),
                    CSample::U16 => out.extend_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_ne_bytes()),
                    CSample::F16 | CSample::F32 => out.extend_from_slice(&v.to_ne_bytes()),
                }
            };
            for p in &buf.px[range] {
                if gray {
                    put(photocraft_color::convert::rgb_to_gray([p[0], p[1], p[2]]));
                } else {
                    put(p[0]);
                    put(p[1]);
                    put(p[2]);
                }
                if !opaque {
                    put(p[3]);
                }
            }
            out
        });
        let mut data = Vec::with_capacity(parts.iter().map(Vec::len).sum());
        for part in parts {
            data.extend_from_slice(&part);
        }
        Image::from_raw(w, h, layout, cs, data)?
    };
    let meta = codecs::Metadata {
        exif: doc.metadata.exif.as_ref().map(|e| e.to_vec()),
        xmp: doc.metadata.xmp.clone(),
        dpi: Some((doc.resolution_dpi, doc.resolution_dpi)),
        text: Vec::new(),
    };
    Ok(img.with_icc(icc).with_meta(meta))
}

/// Flattens and encodes as `format`.
pub fn export_flat(doc: &Document, format: Format, opts: &ExportOptions) -> Result<ExportResult, IoError> {
    if let Some(r) = export_mode_specific(doc, format, opts)? {
        return Ok(r);
    }
    let mut warnings = Vec::new();
    let mut img = document_to_image(doc, &mut warnings)?;
    if img.layout().is_cmyk() && !format.caps().layouts.iter().any(|l| l.is_cmyk()) {
        img = cmyk_image_to_srgb(&img)?;
        warnings.push(format!("CMYK converted to sRGB for {format:?} through the document's colour profile"));
    }
    for w in codecs::fidelity_warnings_with(&img, format, &opts.encode) {
        if w.is_fatal() {
            return Err(IoError::Unsupported(w.to_string()));
        }
        warnings.push(w.to_string());
    }
    let bytes = codecs::encode(&img, format, &opts.encode)?;
    Ok(ExportResult { bytes, warnings })
}

/// Colour-managed CMYK → sRGB for formats that cannot store CMYK (the document's embedded
/// CMYK profile when it parses, else the built-in coated CMYK; relative colorimetric + BPC).
fn cmyk_image_to_srgb(img: &Image) -> Result<Image, IoError> {
    use photocraft_cms::{Builtin, ColorSpace, Intent, Profile, Transform};
    let src = img
        .icc
        .as_ref()
        .and_then(|b| Profile::parse(b).ok())
        .filter(|p| p.color_space == ColorSpace::Cmyk)
        .unwrap_or_else(|| Builtin::CoatedCmyk.profile().clone());
    let dst = Builtin::Srgb.profile();
    let t = Transform::new(&src, dst, Intent::RelativeColorimetric, true).map_err(|e| IoError::Unsupported(e.to_string()))?;
    let alpha = img.layout().has_alpha();
    let (ss, ds) = (if alpha { 5 } else { 4 }, if alpha { 4 } else { 3 });
    let vals = img.to_normalized();
    let mut out = vec![0.0f32; img.pixel_count() * ds];
    t.convert_f32(&vals, ss, &mut out, ds, true);
    let (w, h) = img.dimensions();
    let layout = if alpha { ChannelLayout::Rgba } else { ChannelLayout::Rgb };
    let sample = match img.sample_type() {
        CSample::F16 => CSample::F32,
        s => s,
    };
    Ok(Image::from_normalized(w, h, layout, sample, &out)?.with_icc(Some(dst.to_bytes().to_vec())).with_meta(img.meta.clone()))
}

/// Indexed Color → PNG-8 with its colour table; Duotone → the inks rendered as RGB.
fn export_mode_specific(doc: &Document, format: Format, opts: &ExportOptions) -> Result<Option<ExportResult>, IoError> {
    match doc.mode {
        ColorMode::Indexed if format == Format::Png => {
            let Some(table) = doc.color_table.as_ref().filter(|t| !t.colors.is_empty() && t.colors.len() <= 256) else { return Ok(None) };
            let buf = photocraft_compose::flatten(doc);
            let idx: Vec<u8> = buf
                .px
                .iter()
                .map(|p| match table.transparent {
                    Some(t) if p[3] < 0.5 => t,
                    _ => table.nearest([p[0], p[1], p[2]]) as u8,
                })
                .collect();
            let bytes = codecs::encode_png_indexed(doc.size.width, doc.size.height, &idx, &table.colors, table.transparent)?;
            Ok(Some(ExportResult { bytes, warnings: vec![format!("written as an 8-bit palette PNG ({} colours)", table.colors.len())] }))
        }
        ColorMode::Duotone => {
            let Some(d) = doc.duotone.as_ref() else { return Ok(None) };
            let mut shown = doc.clone();
            shown.layers.push(photocraft_doc::Layer::new("Duotone", photocraft_doc::LayerContent::Adjustment(d.display_adjustment())));
            shown.mode = ColorMode::Rgb;
            shown.icc_profile = None;
            shown.duotone = None;
            let mut r = export_flat(&shown, format, opts)?;
            r.warnings.retain(|w| !w.contains("flattened"));
            r.warnings.push(format!("Duotone ({} inks) written as RGB", d.inks.len()));
            Ok(Some(r))
        }
        _ => Ok(None),
    }
}
