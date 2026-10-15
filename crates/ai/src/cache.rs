//! Verified model files: checksums and the native on-disk cache.
//!
//! A model file is only ever used after its sha256 matched the catalog's. Natively the cache is
//! a folder (`<config dir>/ai-models/`, see [`DiskCache`]); writes are atomic (temp file, then
//! rename) and a corrupt file is deleted and fetched again. In the browser the AI worker keeps
//! files in Cache Storage under the same rule (`apps/photocraft-ai-worker`).

use sha2::{Digest, Sha256};

use crate::catalog::ModelInfo;
use crate::{AiError, Result};

/// The browser's Cache Storage cache holding model files (keyed by their URL).
pub const WEB_CACHE: &str = "capishot-ai-models-v1";

/// The response header holding a cached file's sha256 (written by the AI worker's downloads;
/// files cached before it existed have none).
pub const WEB_SHA_HEADER: &str = "x-capishot-sha256";

/// Lowercase hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in d {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Checks a downloaded or cached file against the catalog.
pub fn verify(model: &ModelInfo, bytes: &[u8]) -> Result<()> {
    if bytes.len() as u64 != model.bytes {
        return Err(AiError::Checksum { expected: format!("{} bytes", model.bytes), actual: format!("{} bytes", bytes.len()) });
    }
    let actual = sha256_hex(bytes);
    if actual != model.sha256 {
        return Err(AiError::Checksum { expected: model.sha256.to_string(), actual });
    }
    Ok(())
}

/// The native model cache: one folder of verified weight files, plus read-only extra folders
/// searched first (a developer's local conversions, `PHOTOCRAFT_AI_MODELS`).
#[cfg(not(target_family = "wasm"))]
#[derive(Clone, Debug)]
pub struct DiskCache {
    pub dir: std::path::PathBuf,
    pub extra: Vec<std::path::PathBuf>,
}

#[cfg(not(target_family = "wasm"))]
impl DiskCache {
    /// The cache under an app config dir (`<config>/ai-models`), plus `PHOTOCRAFT_AI_MODELS`
    /// (a `:`/`;`-separated list of folders) when set.
    pub fn in_config_dir(config_dir: &std::path::Path) -> Self {
        let extra = std::env::var_os("PHOTOCRAFT_AI_MODELS").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
        Self { dir: config_dir.join("ai-models"), extra }
    }

    fn candidates(&self, model: &ModelInfo) -> impl Iterator<Item = std::path::PathBuf> + '_ {
        let file = model.file;
        self.extra.iter().map(move |d| d.join(file)).chain(std::iter::once(self.dir.join(file)))
    }

    /// Is a file present for `model` (not verified: cheap, for the UI)?
    pub fn contains(&self, model: &ModelInfo) -> bool {
        self.candidates(model).any(|p| p.metadata().is_ok_and(|m| m.len() == model.bytes))
    }

    /// The verified file, if cached. A corrupt file in the cache folder is deleted.
    pub fn load(&self, model: &ModelInfo) -> Option<Vec<u8>> {
        for p in self.candidates(model) {
            let Ok(meta) = p.metadata() else { continue };
            if meta.len() != model.bytes {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            if verify(model, &bytes).is_ok() {
                return Some(bytes);
            }
            if p.starts_with(&self.dir) {
                let _ = std::fs::remove_file(&p);
            }
        }
        None
    }

    /// Verifies and stores a downloaded file atomically.
    pub fn store(&self, model: &ModelInfo, bytes: &[u8]) -> Result<()> {
        verify(model, bytes)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| AiError::Cache(format!("{}: {e}", self.dir.display())))?;
        let tmp = self.dir.join(format!(".{}.part", model.file));
        std::fs::write(&tmp, bytes).map_err(|e| AiError::Cache(format!("{}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, self.dir.join(model.file)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            AiError::Cache(e.to_string())
        })
    }

    /// Deletes every catalog model's file from the cache folder (not the extra folders').
    pub fn clear(&self) -> Result<()> {
        for m in crate::catalog::MODELS {
            self.remove(m)?;
        }
        Ok(())
    }

    /// Deletes a cached file (not the extra folders'). Missing files are not an error.
    pub fn remove(&self, model: &ModelInfo) -> Result<()> {
        match std::fs::remove_file(self.dir.join(model.file)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(AiError::Cache(e.to_string())),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Arch, Content};

    fn fake(bytes: &[u8]) -> ModelInfo {
        let sha: &'static str = Box::leak(sha256_hex(bytes).into_boxed_str());
        ModelInfo {
            id: "t",
            title: "T",
            file: "t.safetensors",
            arch: Arch::Esrgan,
            scale: 2,
            content: Content::Color,
            height: None,
            bytes: bytes.len() as u64,
            sha256: sha,
            license: "MIT",
            author: "",
            source: "",
            lite: false,
        }
    }

    #[test]
    fn sha_and_verify() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        let m = fake(b"hello");
        assert!(verify(&m, b"hello").is_ok());
        assert!(verify(&m, b"hellO").is_err());
        assert!(verify(&m, b"hell").is_err());
    }

    #[test]
    fn disk_cache_round_trip_and_corruption() {
        let dir = std::env::temp_dir().join(format!("photocraft-ai-cache-{}", std::process::id()));
        let c = DiskCache { dir: dir.clone(), extra: vec![] };
        let m = fake(b"weights!");
        assert!(c.load(&m).is_none());
        assert!(c.store(&m, b"weights?").is_err());
        c.store(&m, b"weights!").unwrap();
        assert!(c.contains(&m));
        assert_eq!(c.load(&m).as_deref(), Some(&b"weights!"[..]));
        std::fs::write(dir.join(m.file), b"weights?").unwrap();
        assert!(c.load(&m).is_none());
        assert!(!dir.join(m.file).exists());
        c.remove(&m).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
