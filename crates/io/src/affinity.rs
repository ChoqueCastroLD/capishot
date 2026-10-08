//! Affinity documents currently open as their embedded preview, with an explicit warning.

use crate::{ImportResult, IoError};

pub use photocraft_affinity::is_affinity;

/// Extensions recognized as Affinity documents (legacy containers get an actionable error).
pub const EXTENSIONS: &[&str] = &["af", "afphoto", "afdesign", "afpub"];

pub fn has_extension(name: &str) -> bool {
    name.rsplit(['/', '\\']).next().and_then(|n| n.rsplit_once('.')).is_some_and(|(_, ext)| EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)))
}

pub(crate) fn import(name: &str, bytes: &[u8]) -> Result<ImportResult, IoError> {
    let p = photocraft_affinity::preview(bytes).map_err(|e| IoError::Unsupported(e.to_string()))?;
    let opts = photocraft_codecs::DecodeOptions {
        limits: photocraft_codecs::Limits { max_width: 4096, max_height: 4096, max_pixels: 4096 * 4096, max_alloc: 128 << 20 },
        ..Default::default()
    };
    let img = photocraft_codecs::decode_with(p.png, &opts)?;
    let mut r = crate::flat::image_to_document(name, &img)?;
    if let Some(layer) = r.document.layers.first_mut() {
        layer.name = "Affinity preview".into();
        layer.locks = Default::default();
    }
    r.source_read_only = true;
    r.warnings.insert(0, format!(
        "Opened only Affinity's embedded {}×{} PNG preview, which may be smaller than the document. Native layers, vectors, text, masks, effects, pages and the document's colour settings are not imported. Save a new copy; the Affinity source cannot be saved back. Export PSD or PNG from Affinity for full-resolution artwork.", p.width, p.height
    ));
    Ok(r)
}
