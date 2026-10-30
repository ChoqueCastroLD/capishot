//! What the browser's File › Open picker offers.

/// Everything File › Open reads: Capishot, Photoshop, OpenRaster and Affinity documents, flat images, and
/// Photoshop brushes (.abr), gradients (.grd) and swatches (.aco, .ase), which go to the preset libraries,
/// and font projects (.capiproj).
/// Camera raws stay: without the `camera-raw` feature they open as their embedded JPEG preview.
const OPEN_EXTS: &[&str] = &[
    "pcraft", "pdn", "ora", "psd", "psb", "psdt", "png", "jpg", "jpeg", "tif", "tiff", "webp", "gif", "bmp", "tga", "ico", "qoi", "exr", "hdr", "pbm", "pgm",
    "ppm", "pam", "pfm", "fits", "fit", "fts", "heic", "heif", "hif", "dng", "cr2", "cr3", "nef", "nrw", "arw", "pef", "orf", "rw2", "raf", "abr", "grd",
    "svg", "svgz", "aco", "ase", "af", "afdesign", "afphoto", "afpub", "capiproj",
];

/// [`OPEN_EXTS`] without the flat formats this build can't read (OpenEXR and HEIF in the default
/// web build), so the picker doesn't offer files that would only fail to open.
pub fn open_exts() -> Vec<&'static str> {
    OPEN_EXTS.iter().copied().filter(|e| photocraft_codecs::from_extension(e).is_none_or(|f| f.caps().read)).collect()
}

#[cfg(test)]
mod tests {
    use photocraft_codecs::Format;

    #[test]
    fn picker_offers_only_formats_this_build_reads() {
        let exts = super::open_exts();
        for always in ["psd", "png", "jpg", "webp", "pcraft", "dng", "svg", "abr"] {
            assert!(exts.contains(&always), "{always} missing from {exts:?}");
        }
        assert_eq!(exts.contains(&"exr"), Format::OpenExr.caps().read);
        assert_eq!(exts.contains(&"heic"), Format::Heif.caps().read);
        assert_eq!(exts.contains(&"hif"), Format::Heif.caps().read);
    }
}
