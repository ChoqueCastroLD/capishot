//! Save to device: where the browser has the File System Access API (`showSaveFilePicker`:
//! Chrome and Edge on the desktop, and Android where it ships), Save As asks for a file and the
//! handle is kept by the file's name, so File › Save of that document writes the same file in
//! place, streamed from the encoder's chunks, instead of downloading another copy. Elsewhere, and
//! when a write fails, saves download as before. See `save_handles.js`.

use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/save_handles.js")]
extern "C" {
    fn sh_supported() -> bool;
    #[wasm_bindgen(catch)]
    async fn sh_pick(suggested: &str, description: &str, mime: &str, ext: &str) -> Result<JsValue, JsValue>;
    fn sh_has(name: &str) -> bool;
    fn sh_forget(name: &str);
    #[wasm_bindgen(catch)]
    async fn sh_write(name: &str, parts: &js_sys::Array) -> Result<JsValue, JsValue>;
}

/// The browser can save to a file the user picks.
pub fn supported() -> bool {
    sh_supported()
}

/// What Save As answered.
pub enum Picked {
    /// The file's name (its handle is kept).
    Name(String),
    /// The user closed the picker.
    Cancelled,
    /// The browser refused (no user gesture left, a sandboxed frame): download instead.
    Unavailable,
}

/// Ask for a file to save `suggested` (a file name) to.
pub async fn pick(suggested: &str) -> Picked {
    let ext = std::path::Path::new(suggested).extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let mime = crate::web::mime_for(suggested);
    let description = if ext.is_empty() { String::new() } else { format!("{} file", ext.to_ascii_uppercase()) };
    match sh_pick(suggested, &description, mime, &ext).await {
        Ok(v) => v.as_string().map_or(Picked::Cancelled, Picked::Name),
        Err(e) => {
            log::warn!("Save to device unavailable, downloading instead: {e:?}");
            Picked::Unavailable
        }
    }
}

/// A file picked for `path`'s name is kept: saves to `path` write it.
pub fn has(path: &str) -> bool {
    supported() && sh_has(&name_of(path))
}

/// Write `parts` (Uint8Arrays) to the file kept for `path`. On failure the handle is dropped
/// (the next save downloads, or asks again with Save As).
pub async fn write(path: &str, parts: &js_sys::Array) -> Result<(), String> {
    let name = name_of(path);
    sh_write(&name, parts).await.map(|_| ()).map_err(|e| {
        sh_forget(&name);
        e.as_string().or_else(|| js_sys::Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string())).unwrap_or_else(|| format!("{e:?}"))
    })
}

fn name_of(path: &str) -> String {
    std::path::Path::new(path).file_name().map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}
