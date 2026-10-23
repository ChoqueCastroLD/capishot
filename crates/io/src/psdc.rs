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
//! Every target: on the desktop psd-compiler reads font folders and renders on all cores; in the
//! browser it gets the font bytes the app holds ([`CompileOptions::with_app_fonts`]) and its rayon
//! work runs on the calling thread unless the page set up a thread pool.

use std::path::PathBuf;

pub use photocraft_text::fonts::FontData;

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
    /// Also search the system font folders (none on the web).
    pub system_fonts: bool,
    /// Font files on disk, searched after the folders (the downloaded-fonts store).
    pub font_files: Vec<PathBuf>,
    /// Font files in memory, searched last: the bundled, craft-fonts and served fonts. The only
    /// fonts there are in the browser.
    pub font_data: Vec<FontData>,
}

impl CompileOptions {
    /// Adds the fonts the app's text engine holds ([`photocraft_text::shared`]): its in-memory
    /// files (bundled, craft-fonts, fonts served to the web build) and its downloaded-fonts
    /// store files. They come after the folders and system fonts, which keep winning name clashes.
    pub fn with_app_fonts(mut self) -> Self {
        let eng = photocraft_text::shared().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.font_files.extend(eng.fonts.managed_font_files());
        self.font_data.extend(eng.fonts.font_data());
        self
    }
}

/// psd-compiler with its fonts loaded once, for compiling many files (a chapter) with the same
/// options. Shareable between threads.
pub struct Compiler {
    fonts: psd_compiler::FontDb,
    rerender_text: bool,
}

impl Compiler {
    /// Loads the fonts of `opts` (only when type is re-rendered: the stored rasters need none).
    pub fn new(opts: &CompileOptions) -> Self {
        let mut fonts = psd_compiler::FontDb::new();
        if opts.rerender_text {
            for dir in &opts.font_dirs {
                fonts.add_dir(dir);
            }
            if opts.system_fonts {
                fonts.add_system_fonts();
            }
            for file in &opts.font_files {
                fonts.add_file(file);
            }
            for data in &opts.font_data {
                fonts.add_font_data(data.clone());
            }
        }
        Compiler { fonts, rerender_text: opts.rerender_text }
    }

    /// Number of font faces loaded.
    pub fn font_faces(&self) -> usize {
        self.fonts.len()
    }

    /// Renders PSD/PSB `bytes`.
    pub fn compile(&self, bytes: &[u8]) -> Result<Compiled, IoError> {
        let doc = psd_compiler::Document::parse(bytes).map_err(|e| IoError::Unsupported(format!("psd-compiler: {e}")))?;
        let options = psd_compiler::RenderOptions { keep_text_raster: !self.rerender_text, ..Default::default() };
        let out = psd_compiler::render(&doc, &self.fonts, &options);
        Ok(Compiled {
            width: out.image.width,
            height: out.image.height,
            rgba: out.image.data,
            warnings: out.warnings.iter().map(ToString::to_string).collect(),
        })
    }
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

/// Renders PSD/PSB `bytes` with psd-compiler (one file; see [`Compiler`] for many).
pub fn compile(bytes: &[u8], opts: &CompileOptions) -> Result<Compiled, IoError> {
    Compiler::new(opts).compile(bytes)
}

/// Compiles `doc`: writes it as a PSD in memory, then renders that with psd-compiler.
pub fn compile_document(doc: &Document, opts: &CompileOptions) -> Result<Compiled, IoError> {
    // psd-compiler renders the layers, so the file needs no merged image of its own (rendering
    // one would composite the whole document for nothing); only a file without layers shows it.
    let o = crate::PsdExportOptions { force_psb: !fits_psd(doc), merged_image: false, ..Default::default() };
    let (mut file, _) = crate::document_to_psd_with(doc, &o);
    if file.layer_info.as_ref().is_none_or(|l| l.layers.is_empty()) {
        file = crate::document_to_psd_with(doc, &crate::PsdExportOptions { merged_image: true, ..o }).0;
    }
    file.header.validate()?;
    compile(&file.to_bytes()?, opts)
}

fn fits_psd(doc: &Document) -> bool {
    doc.size.width <= 30_000 && doc.size.height <= 30_000
}

/// Whether a PSD/PSB save of `doc` takes its merged image from psd-compiler ([`recomposite`]):
/// 8-bit RGB documents with layers. The save then skips photocraft-compose's merged image.
pub(crate) fn renders_merged(doc: &Document) -> bool {
    let f = doc.pixel_format();
    doc.mode != photocraft_color::ColorMode::Multichannel
        && f.mode == photocraft_color::ColorMode::Rgb
        && f.sample == photocraft_color::SampleType::U8
        && !doc.layers.is_empty()
}

/// Fills in the merged image of `file`, written as `bytes` with a black placeholder (no alpha
/// channel; see `PsdExportOptions::merged_image`), with psd-compiler's render of its layers,
/// keeping its alpha channels. Returns the file's new bytes, or `None` when psd-compiler doesn't
/// apply (not 8-bit RGB, no layers, a size mismatch, no channel left for the transparency): the
/// caller then writes the compositor's merged image instead.
///
/// The merged image gets an alpha channel when psd-compiler's render is translucent anywhere,
/// with the colours matted against white as Photoshop does. Without one only the image data
/// section changes, so the bytes are patched in place of a second full serialization. The stored
/// type rasters are used, so the merged image shows the type as the canvas does.
///
/// Two shortcuts keep a save fast without changing what it writes:
/// * documents whose layers both renderers draw alike ([`compose_agrees`]: plain pixel and type
///   layers in the separable blend modes, no effects) take photocraft-compose's composite, which
///   renders in bands on every core instead of psd-compiler's whole-canvas float passes, when
///   `plain` allows it;
/// * the encoded merged image of the last few saves is kept by the file's own bytes (the layers,
///   resources and alpha channels it was rendered from), so saving an unchanged document again
///   (Save after a download, Save As another name) skips the render.
pub(crate) fn recomposite(file: &mut PsdFile, bytes: &[u8], doc: &Document, plain: bool) -> Result<Option<Vec<u8>>, IoError> {
    let h = &file.header;
    if h.depth != 8 || h.color_mode != ColorMode::Rgb || file.layer_info.as_ref().is_none_or(|l| l.layers.is_empty()) || file.merged_has_alpha() {
        return Ok(None);
    }
    let plain = plain && compose_agrees(doc);
    let key = cache::key(bytes, plain);
    let merged = match cache::get(&key) {
        Some(m) => m,
        None => {
            let Some(m) = merged_image(file, bytes, doc, plain)? else { return Ok(None) };
            let m = std::sync::Arc::new(m);
            cache::put(key, m.clone());
            m
        }
    };
    if merged.has_alpha {
        file.header.channels += 1;
        if let Some(info) = &mut file.layer_info {
            info.merged_alpha = true;
        }
        // The Quick Mask channel's index moves up past the transparency.
        if let Some(r) = file.resources.iter_mut().find(|r| r.id == crate::channel_map::QUICK_MASK_INFO)
            && let Some(id) = r.data.get(..2).map(|b| u16::from_be_bytes([b[0], b[1]]))
        {
            r.data.splice(..2, id.saturating_add(1).to_be_bytes());
        }
    }
    let old = std::mem::replace(&mut file.image_data, merged.image_data.clone());
    if merged.has_alpha {
        file.header.validate()?;
        return Ok(Some(file.to_bytes()?));
    }
    // Only the image data, the end of the file, changed: its compression code, then the data.
    let at = bytes.len().checked_sub(old.data.len() + 2);
    let tail_is_old =
        at.and_then(|at| bytes.get(at..)).is_some_and(|t| t.get(..2) == Some(&old.compression.as_u16().to_be_bytes()[..]) && t.get(2..) == Some(&old.data[..]));
    let Some(head) = at.and_then(|at| bytes.get(..at)).filter(|_| tail_is_old) else { return Ok(Some(file.to_bytes()?)) };
    let mut out = Vec::new();
    out.try_reserve_exact(head.len() + 2 + file.image_data.data.len()).map_err(|_| IoError::Unsupported("not enough memory for the PSD".into()))?;
    out.extend_from_slice(head);
    out.extend_from_slice(&file.image_data.compression.as_u16().to_be_bytes());
    out.extend_from_slice(&file.image_data.data);
    Ok(Some(out))
}

/// A merged image ready to go into a file: its encoded planes (colour, the transparency when
/// `has_alpha`, then the file's alpha channels).
pub(crate) struct Merged {
    image_data: photocraft_psd::ImageData,
    has_alpha: bool,
}

/// Renders and encodes the merged image of `file` (see [`recomposite`]); `None` when it can't.
fn merged_image(file: &PsdFile, bytes: &[u8], doc: &Document, plain: bool) -> Result<Option<Merged>, IoError> {
    let h = &file.header;
    let n = (h.width as usize).saturating_mul(h.height as usize);
    let rgba = if plain {
        let Some(rgba) = compose_rgba8(doc, h.width, h.height) else { return Ok(None) };
        rgba
    } else {
        let compiled = compile(bytes, &CompileOptions::default())?;
        if compiled.width != h.width || compiled.height != h.height {
            return Ok(None);
        }
        compiled.rgba
    };
    if rgba.len() < n.saturating_mul(4) {
        return Ok(None);
    }
    let px = rgba.as_chunks::<4>().0.get(..n).unwrap_or_default();
    let has_alpha = crate::pixels::par_map(crate::pixels::bands(n), |r| px.get(r).unwrap_or_default().iter().any(|p| p[3] < 255)).into_iter().any(|t| t);
    let extra = usize::from(h.channels).saturating_sub(3);
    // Photoshop's limit of 56 channels: the transparency would need one more.
    if has_alpha && 3 + 1 + extra > 56 {
        return Ok(None);
    }
    let merged = if has_alpha { 4 } else { 3 };
    let mut planes = vec![0u8; n.saturating_mul(merged + extra)];
    if extra > 0 {
        // The alpha channels written after the placeholder's colour planes.
        let old = file.image_data.decode(h)?;
        let (Some(src), Some(dst)) = (old.get(3 * n..), planes.get_mut(merged * n..)) else { return Ok(None) };
        if src.len() != dst.len() {
            return Ok(None);
        }
        dst.copy_from_slice(src);
    }
    // One plane per thread.
    let jobs: Vec<(usize, &mut [u8])> = planes.chunks_mut(n.max(1)).take(merged).enumerate().collect();
    crate::pixels::par_map(jobs, |(c, out)| {
        for (o, p) in out.iter_mut().zip(px) {
            *o = if c < 3 && has_alpha {
                // With an alpha channel Photoshop mattes the merged colours against white.
                crate::pixels::matte(f32::from(p[c]), f32::from(p[3]) / 255.0, 255.0).round().clamp(0.0, 255.0) as u8
            } else {
                p[c]
            };
        }
    });
    drop(rgba);
    let mut header = file.header.clone();
    if has_alpha {
        header.channels += 1;
    }
    let image_data = photocraft_psd::ImageData::encode(file.image_data.compression, &planes, &header)?;
    Ok(Some(Merged { image_data, has_alpha }))
}

/// photocraft-compose's composite of `doc` as straight 8-bit RGBA, rendered in bands (one band of
/// floats at a time, not the whole canvas); `None` when its size isn't `width`×`height`.
fn compose_rgba8(doc: &Document, width: u32, height: u32) -> Option<Vec<u8>> {
    let canvas = doc.bounds();
    if canvas.width() != width || canvas.height() != height {
        return None;
    }
    let w = width as usize;
    let mut out = vec![0u8; w.checked_mul(height as usize)?.checked_mul(4)?];
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    photocraft_compose::render_bands(doc, canvas, 0, |band| -> Result<(), ()> {
        let at = (band.rect.y0 - canvas.y0) as usize * w * 4;
        let dst = out.get_mut(at..at + band.px.len() * 4).ok_or(())?;
        let jobs: Vec<(&mut [u8], &[[f32; 4]])> = dst.chunks_mut(4 * w.max(1) * 16).zip(band.px.chunks(w.max(1) * 16)).collect();
        crate::pixels::par_map(jobs, |(d, s)| {
            for (o, p) in d.as_chunks_mut::<4>().0.iter_mut().zip(s) {
                *o = p.map(q);
            }
        });
        Ok(())
    })
    .ok()?;
    Some(out)
}

/// Whether photocraft-compose draws `doc` as psd-compiler does (within one level): 8-bit RGB with
/// only visible-or-not pixel and type layers and plain groups, in Normal, Multiply, Screen,
/// Darken or Lighten, with no layer effects, fill opacity, Blend If, advanced blending, clipping,
/// vector masks, feathered or partial-density masks, artboards, or Photoshop blocks the model
/// doesn't read. Everything else (where the two renderers can differ) goes to psd-compiler.
pub fn compose_agrees(doc: &Document) -> bool {
    use photocraft_color::BlendMode as B;
    use photocraft_doc::LayerContent as C;
    /// Photoshop blocks every layer carries that don't change how it draws.
    const INERT: &[&[u8; 4]] = &[b"luni", b"lnsr", b"lyid", b"lclr", b"shmd", b"fxrp", b"cust", b"lspf", b"cinf", b"pths", b"anFX", b"sn2P"];
    fn plain(layers: &[photocraft_doc::Layer]) -> bool {
        layers.iter().all(|l| {
            let group = matches!(&l.content, C::Group(g) if g.artboard.is_none() && plain(&g.children));
            let content = group || matches!(&l.content, C::Raster(_)) || matches!(&l.content, C::Text(t) if t.cache.is_some());
            let blend = matches!(l.blend, B::Normal | B::Multiply | B::Screen | B::Darken | B::Lighten) || (group && l.blend == B::PassThrough);
            let mask = l.mask.as_ref().is_none_or(|m| m.density >= 1.0 && m.feather <= 0.0);
            content
                && blend
                && mask
                && l.effects.items.is_empty()
                && l.effects.psd_raw.is_none()
                && l.fill_opacity >= 1.0
                && !l.clipped
                && l.vector_mask.is_none()
                && l.blend_if.is_default()
                && l.advanced == photocraft_doc::AdvancedBlending::default()
                && l.excluded_channels == 0
                && l.psd_blocks.iter().all(|(key, _)| INERT.contains(&key))
        })
    }
    renders_merged(doc) && plain(&doc.layers)
}

/// The encoded merged images of the last saves, by the bytes they were rendered from.
mod cache {
    use std::sync::{Arc, Mutex, PoisonError};

    use super::Merged;

    /// Saves remembered: a document saved again, and the one before it (Save As a copy).
    const KEEP: usize = 2;

    pub(super) type Key = ([u8; 32], bool);
    type Saves = Mutex<Vec<(Key, Arc<Merged>)>>;
    #[cfg(not(test))]
    static SAVES: Saves = Mutex::new(Vec::new());
    // Each test thread its own: tests save in parallel.
    #[cfg(test)]
    thread_local! {
        static SAVES: Saves = const { Mutex::new(Vec::new()) };
    }

    fn with<R>(f: impl FnOnce(&mut Vec<(Key, Arc<Merged>)>) -> R) -> R {
        #[cfg(not(test))]
        return f(&mut SAVES.lock().unwrap_or_else(PoisonError::into_inner));
        #[cfg(test)]
        SAVES.with(|s| f(&mut s.lock().unwrap_or_else(PoisonError::into_inner)))
    }

    pub(super) fn key(bytes: &[u8], plain: bool) -> Key {
        (*blake3::hash(bytes).as_bytes(), plain)
    }

    pub(super) fn get(key: &Key) -> Option<Arc<Merged>> {
        with(|saves| saves.iter().find(|(k, _)| k == key).map(|(_, m)| m.clone()))
    }

    pub(super) fn put(key: Key, merged: Arc<Merged>) {
        with(|saves| {
            saves.retain(|(k, _)| *k != key);
            saves.insert(0, (key, merged));
            saves.truncate(KEEP);
        });
    }

    /// Saves remembered (tests).
    #[cfg(test)]
    pub(super) fn len() -> usize {
        with(|saves| saves.len())
    }

    /// Forgets every remembered save (tests).
    #[cfg(test)]
    pub(super) fn clear() {
        with(Vec::clear);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_color::{ColorMode, SampleType};
    use photocraft_doc::Layer;
    use photocraft_geom::{Rect, Size};
    use photocraft_raster::Surface;

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
    fn app_fonts_reach_psd_compiler_only_when_type_is_rerendered() {
        let bare = CompileOptions { rerender_text: true, ..Default::default() };
        assert_eq!(Compiler::new(&bare).font_faces(), 0);
        let app = bare.clone().with_app_fonts();
        // The bundled Inter and JetBrains Mono at least; in the browser these are all there is.
        assert!(app.font_data.len() >= photocraft_text::fonts::BUNDLED.len());
        assert!(Compiler::new(&app).font_faces() >= photocraft_text::fonts::BUNDLED.len());
        let stored = CompileOptions { rerender_text: false, ..app };
        assert_eq!(Compiler::new(&stored).font_faces(), 0);
    }

    #[test]
    fn one_compiler_compiles_many_files() {
        let doc = two_layer_doc();
        let (file, _) = crate::document_to_psd_with(&doc, &crate::PsdExportOptions::default());
        let bytes = file.to_bytes().unwrap();
        let compiler = Compiler::new(&CompileOptions::default());
        for _ in 0..2 {
            let c = compiler.compile(&bytes).unwrap();
            assert!(c.rgba[..4].iter().zip([153u8, 51, 102, 255]).all(|(g, w)| g.abs_diff(w) <= 1), "{:?}", &c.rgba[..4]);
        }
        assert!(compiler.compile(b"nope").is_err());
    }

    /// `doc` saved the way it was before psd-compiler filled in a placeholder: the compositor's
    /// merged image, then psd-compiler's render over it. The reference the saves are held to.
    fn compiled_over_compositor(doc: &Document) -> (PsdFile, Compiled) {
        let (file, _) = crate::document_to_psd_with(doc, &crate::PsdExportOptions::default());
        let c = compile(&file.to_bytes().unwrap(), &CompileOptions::default()).unwrap();
        (file, c)
    }

    /// Opaque, translucent (no background), with alpha channels and a Quick Mask.
    fn save_cases() -> Vec<(&'static str, Document)> {
        let opaque = two_layer_doc();
        let mut translucent = two_layer_doc();
        translucent.layers.remove(0);
        translucent.layers[0].surface_mut().unwrap().fill_rect(Rect::new(10, 10, 20, 12), &[0.0; 4]);
        let mut channels = two_layer_doc();
        let mut alpha = Surface::new(photocraft_color::PixelFormat::GRAY8);
        alpha.fill_rect(Rect::new(0, 0, 30, 20), &[1.0, 1.0]);
        channels.channels.push(photocraft_doc::AlphaChannel::new("Alpha 1", alpha.clone()));
        channels.quick_mask = Some(photocraft_doc::AlphaChannel::new("Quick Mask", alpha));
        let mut both = channels.clone();
        both.layers.remove(0);
        vec![("opaque", opaque), ("translucent", translucent), ("channels", channels), ("translucent with channels", both)]
    }

    /// The merged image of a save is psd-compiler's render, as when it was rendered over the
    /// compositor's merged image (before that was skipped): same channels, same pixels within
    /// one level, the alpha channels and the Quick Mask kept in their places.
    #[test]
    fn saved_merged_image_matches_the_compiled_composite() {
        for (name, doc) in save_cases() {
            let out = crate::export(&doc, "x.psd", &Default::default()).unwrap();
            assert!(out.warnings.is_empty(), "{name}: {:?}", out.warnings);
            let file = PsdFile::from_bytes(&out.bytes).unwrap();
            let (old, c) = compiled_over_compositor(&doc);
            let translucent = c.rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
            assert_eq!(file.merged_has_alpha(), translucent, "{name}");
            assert_eq!(usize::from(file.header.channels), 3 + usize::from(translucent) + doc.channels.len() + usize::from(doc.quick_mask.is_some()), "{name}");
            let n = 64 * 48;
            let planes = file.image_data.decode(&file.header).unwrap();
            for (i, p) in c.rgba.as_chunks::<4>().0.iter().enumerate() {
                let a = f32::from(p[3]) / 255.0;
                for ch in 0..3 {
                    let want = if translucent { crate::pixels::matte(f32::from(p[ch]), a, 255.0).round() as u8 } else { p[ch] };
                    assert!(planes[ch * n + i].abs_diff(want) <= 1, "{name}: pixel {i} channel {ch}");
                }
                if translucent {
                    assert_eq!(planes[3 * n + i], p[3], "{name}: alpha {i}");
                }
            }
            // The alpha channels follow the merged image, as the compositor's file had them.
            let old_planes = old.image_data.decode(&old.header).unwrap();
            let extra = doc.channels.len() + usize::from(doc.quick_mask.is_some());
            let at = |has_alpha: bool| (3 + usize::from(has_alpha)) * n;
            assert_eq!(planes[at(translucent)..], old_planes[at(old.merged_has_alpha())..], "{name}");
            assert_eq!(planes.len(), at(translucent) + extra * n, "{name}");
            // And they read back: the alpha channel and the Quick Mask in their places.
            let back = crate::import("x.psd", &out.bytes).unwrap().document;
            assert_eq!(back.channels.len(), doc.channels.len(), "{name}");
            assert_eq!(back.quick_mask.is_some(), doc.quick_mask.is_some(), "{name}");
            if let (Some(a), Some(b)) = (&back.quick_mask, &doc.quick_mask) {
                assert_eq!(a.surface.pixel(5, 5), b.surface.pixel(5, 5), "{name}");
                assert_eq!(a.surface.pixel(40, 30), b.surface.pixel(40, 30), "{name}");
            }
            assert_eq!(back.layers.len(), doc.layers.len(), "{name}");
        }
    }

    /// The placeholder never reaches a file: the bytes `recomposite` patches are the file's own.
    #[test]
    fn patched_bytes_are_the_serialized_file() {
        for (name, doc) in save_cases() {
            let o = crate::PsdExportOptions { merged_image: false, ..Default::default() };
            let (mut file, _) = crate::document_to_psd_with(&doc, &o);
            assert!(!file.merged_has_alpha());
            let bytes = file.to_bytes().unwrap();
            let out = recomposite(&mut file, &bytes, &doc, false).unwrap().expect("8-bit RGB with layers");
            assert_eq!(out, file.to_bytes().unwrap(), "{name}");
        }
    }

    /// No layers (psd-compiler would show the placeholder), not 8-bit RGB, or psd-compiler off:
    /// the compositor's merged image, exactly as before.
    #[test]
    fn saves_psd_compiler_does_not_render_keep_the_compositor_image() {
        let empty = Document::new("t", Size::new(8, 8), ColorMode::Rgb, SampleType::U8);
        let mut deep = two_layer_doc();
        let f16 = photocraft_color::PixelFormat::new(ColorMode::Rgb, SampleType::U16, true);
        deep.layers.iter_mut().for_each(|l| *l.surface_mut().unwrap() = l.surface().unwrap().convert(f16));
        deep.depth = SampleType::U16;
        let off = crate::ExportOptions { psdc_composite: false, ..Default::default() };
        for (name, doc, opts) in [("no layers", empty, Default::default()), ("16-bit", deep, Default::default()), ("off", two_layer_doc(), off)] {
            assert!(!renders_merged(&doc) || !opts.psdc_composite, "{name}");
            let out = crate::export(&doc, "x.psd", &opts).unwrap();
            let file = PsdFile::from_bytes(&out.bytes).unwrap();
            let (want, _) = crate::document_to_psd_with(&doc, &crate::PsdExportOptions::default());
            assert_eq!(file.header, want.header, "{name}");
            assert_eq!(file.image_data, want.image_data, "{name}");
        }
    }

    /// Plain documents in every blend mode `compose_agrees` takes, with masks, hidden layers,
    /// groups and transparency: the merged image of a save that takes photocraft-compose's
    /// composite is psd-compiler's within one level, the layers byte for byte the same.
    #[test]
    fn plain_documents_save_the_compiled_composite_within_one_level() {
        use photocraft_color::BlendMode;
        let mut cases = save_cases();
        for (i, mode) in [BlendMode::Multiply, BlendMode::Screen, BlendMode::Darken, BlendMode::Lighten].into_iter().enumerate() {
            let mut doc = two_layer_doc();
            doc.layers[1].blend = mode;
            doc.layers[1].opacity = 0.7;
            let fmt = doc.pixel_format();
            let mut mask = photocraft_doc::LayerMask::reveal_all();
            mask.surface.fill_rect(Rect::new(0, 0, 20 + i as i32 * 5, 48), &[0.25]);
            doc.layers[1].mask = Some(mask);
            let mut hidden = Layer::raster("hidden", fmt);
            hidden.surface_mut().unwrap().fill_rect(Rect::new(0, 0, 64, 48), &[0.0, 1.0, 0.0, 1.0]);
            hidden.visible = false;
            let mut inner = Layer::raster("inner", fmt);
            inner.surface_mut().unwrap().fill_rect(Rect::new(30, 5, 60, 40), &[0.9, 0.8, 0.1, 0.6]);
            doc.layers.push(hidden);
            doc.layers.push(Layer::group("group", vec![inner]));
            cases.push(("blend", doc));
        }
        for (name, doc) in cases {
            assert!(compose_agrees(&doc), "{name}");
            cache::clear();
            let fast = crate::export(&doc, "x.psd", &Default::default()).unwrap();
            cache::clear();
            let slow = crate::export(&doc, "x.psd", &crate::ExportOptions { psdc_plain_compose: false, ..Default::default() }).unwrap();
            let (a, b) = (PsdFile::from_bytes(&fast.bytes).unwrap(), PsdFile::from_bytes(&slow.bytes).unwrap());
            assert_eq!(a.header, b.header, "{name}");
            assert_eq!(a.layer_info, b.layer_info, "{name}");
            let (pa, pb) = (a.image_data.decode(&a.header).unwrap(), b.image_data.decode(&b.header).unwrap());
            assert_eq!(pa.len(), pb.len(), "{name}");
            let worst = pa.iter().zip(&pb).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
            assert!(worst <= 1, "{name}: merged image off by {worst}");
        }
    }

    /// Effects, clipping, other blend modes or Photoshop blocks the model doesn't read: the save
    /// renders with psd-compiler.
    #[test]
    fn documents_the_renderers_may_draw_apart_go_to_psd_compiler() {
        let mut fx = two_layer_doc();
        fx.layers[1].effects.items.push(photocraft_doc::Effect::ColorOverlay {
            common: photocraft_doc::FxCommon { enabled: true, blend: photocraft_color::BlendMode::Normal, opacity: 0.5 },
            color: photocraft_color::Color::BLACK,
        });
        let mut clipped = two_layer_doc();
        clipped.layers[1].clipped = true;
        let mut overlay = two_layer_doc();
        overlay.layers[1].blend = photocraft_color::BlendMode::Overlay;
        let mut unknown = two_layer_doc();
        unknown.layers[1].psd_blocks.push((*b"vogk", std::sync::Arc::new(vec![0; 4])));
        for (name, doc) in [("effects", fx), ("clipped", clipped), ("overlay", overlay), ("unknown block", unknown)] {
            assert!(!compose_agrees(&doc), "{name}");
        }
        assert!(compose_agrees(&two_layer_doc()));
    }

    /// Saving an unchanged document again reuses the merged image: the same bytes.
    #[test]
    fn saving_again_reuses_the_merged_image() {
        cache::clear();
        let mut doc = two_layer_doc();
        doc.layers[1].effects.items.push(photocraft_doc::Effect::ColorOverlay {
            common: photocraft_doc::FxCommon { enabled: true, blend: photocraft_color::BlendMode::Normal, opacity: 0.5 },
            color: photocraft_color::Color::BLACK,
        });
        let first = crate::export(&doc, "x.psd", &Default::default()).unwrap();
        assert_eq!(cache::len(), 1, "the save is remembered");
        let again = crate::export(&doc, "x.psd", &Default::default()).unwrap();
        assert_eq!(first.bytes, again.bytes);
        // A change renders anew.
        doc.layers[0].surface_mut().unwrap().fill_rect(Rect::new(0, 0, 8, 8), &[0.0, 0.0, 0.0, 1.0]);
        let changed = crate::export(&doc, "x.psd", &Default::default()).unwrap();
        assert_ne!(first.bytes, changed.bytes);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(compile(b"8BPS garbage", &CompileOptions::default()).is_err());
        assert!(compile(&[], &CompileOptions::default()).is_err());
    }
}
