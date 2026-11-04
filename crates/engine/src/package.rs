//! Capishot packages: the zip both `.theme.capishot` (a colour theme, [`crate::theme_cmds`]) and
//! `.proj.capishot` (a font project and profile, [`crate::font_projects`]) are, with the
//! `manifest.json` and the checks they share.
//!
//! The manifest names its `format` and `version` and carries optional [`Meta`] (title, author,
//! licence, tags, icon…) for listing a package. Reading one is strict, because packages come
//! from anyone: the archive's size, file count and each file's size are capped before anything
//! is decompressed, paths must be plain relative names (no `..`, no absolute or drive paths, no
//! symbolic links), only the files a format names are ever read (others are reported and left
//! alone), images must be PNGs that decode within their size limits, and nothing is fetched or
//! run: a homepage is only text.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::{EngineError, Result};

/// The name of the manifest in every package.
pub const MANIFEST: &str = "manifest.json";
/// The largest manifest read (bytes).
const MAX_MANIFEST: usize = 1 << 20;
/// A package icon's side (pixels).
pub const ICON_SIDE: u32 = 256;

/// What one package format may hold.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// The whole archive, and everything in it once decompressed (bytes).
    pub total: usize,
    /// Entries in the archive.
    pub files: usize,
    /// Any one file once decompressed (bytes).
    pub file: usize,
}

/// A package's optional description, for listing and crediting it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Meta {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub author: String,
    /// The package's own version (free text, such as `1.2`).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// Its licence (an SPDX id such as `CC-BY-4.0`, or free text).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub license: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// `icon.png` when the package has one (a [`ICON_SIDE`]-pixel square PNG).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub icon: String,
    /// An `https://` page about it: shown as text, never fetched.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub homepage: String,
    /// The oldest Capishot it works in (semantic version).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub min_capishot_version: String,
}

/// The longest title, author, version or licence (characters).
const SHORT_TEXT: usize = 120;
/// The longest description and homepage (characters).
const LONG_TEXT: usize = 2000;
const MAX_TAGS: usize = 16;
const TAG_LEN: usize = 32;

/// `s` trimmed and cut to `max` characters, without control characters (newlines and tabs kept
/// when `multiline`).
fn clean(s: &str, max: usize, multiline: bool) -> String {
    s.trim().chars().filter(|c| !c.is_control() || (multiline && matches!(c, '\n' | '\t'))).take(max).collect()
}

impl Meta {
    /// Tidies the fields (trimmed, capped, control characters dropped, tags de-duplicated, a
    /// homepage that isn't `http(s)://` or an icon that isn't `icon.png` dropped) and returns a
    /// warning for each thing dropped. An unreadable `minCapishotVersion` is dropped too.
    pub fn sanitize(&mut self) -> Vec<String> {
        let mut warnings = Vec::new();
        for (field, max) in [(&mut self.title, SHORT_TEXT), (&mut self.author, SHORT_TEXT), (&mut self.version, SHORT_TEXT), (&mut self.license, SHORT_TEXT)] {
            *field = clean(field, max, false);
        }
        self.description = clean(&self.description, LONG_TEXT, true);
        self.homepage = clean(&self.homepage, LONG_TEXT, false);
        if !self.homepage.is_empty() && !(self.homepage.starts_with("https://") || self.homepage.starts_with("http://")) {
            warnings.push(format!("homepage `{}` ignored: only http(s) links are kept", self.homepage));
            self.homepage.clear();
        }
        if !self.icon.is_empty() && self.icon != "icon.png" {
            warnings.push(format!("icon `{}` ignored: a package's icon is its icon.png", self.icon));
            self.icon.clear();
        }
        let mut tags: Vec<String> = Vec::new();
        for tag in &self.tags {
            let t = clean(tag, TAG_LEN, false);
            if !t.is_empty() && !tags.iter().any(|x| x.eq_ignore_ascii_case(&t)) {
                tags.push(t);
            }
        }
        if tags.len() > MAX_TAGS {
            warnings.push(format!("only the first {MAX_TAGS} tags kept"));
            tags.truncate(MAX_TAGS);
        }
        self.tags = tags;
        self.min_capishot_version = self.min_capishot_version.trim().trim_start_matches('v').to_string();
        if !self.min_capishot_version.is_empty() && semver::Version::parse(&self.min_capishot_version).is_err() {
            warnings.push(format!("minCapishotVersion `{}` ignored: not a version", self.min_capishot_version));
            self.min_capishot_version.clear();
        }
        warnings
    }

    /// An error when the package needs a newer Capishot than this one.
    pub fn check_version(&self) -> Result<()> {
        let (Ok(need), Ok(have)) = (semver::Version::parse(&self.min_capishot_version), semver::Version::parse(crate::build_info::VERSION)) else {
            return Ok(());
        };
        if need > have {
            return Err(EngineError::Other(format!("this package needs Capishot {need} or newer (this is {have}); update to open it")));
        }
        Ok(())
    }
}

/// Is `name` a plain relative path inside a package (no `..`, no absolute or drive path, no
/// backslashes or control characters)?
pub fn safe_path(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.starts_with('/')
        && !name.contains('\\')
        && !name.chars().any(char::is_control)
        && !name.split('/').any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
}

fn bad(kind: &str, msg: impl std::fmt::Display) -> EngineError {
    EngineError::Other(format!("not a valid {kind}: {msg}"))
}

/// An opened package: its manifest checked, its entries vetted, nothing else read yet.
pub struct Package<'a> {
    zip: photocraft_format::zip::ZipReader<'a>,
    /// What kind of package this is, for messages ("Capishot theme").
    kind: &'static str,
    limits: Limits,
    allowed: fn(&str) -> bool,
    pub format: String,
    pub version: u32,
    pub meta: Meta,
    /// The whole manifest, for the format's own fields.
    pub manifest: Map<String, Value>,
    /// Files present that the format doesn't use: listed, never read.
    pub ignored: Vec<String>,
    /// What [`Meta::sanitize`] dropped.
    pub warnings: Vec<String>,
}

impl<'a> Package<'a> {
    /// Opens `bytes` as a package of one of `formats` (`(format, newest version read)`), whose
    /// files are those `allowed` accepts. `kind` names it in errors.
    pub fn open(bytes: &'a [u8], kind: &'static str, formats: &[(&str, u32)], allowed: fn(&str) -> bool, limits: Limits) -> Result<Self> {
        if bytes.len() > limits.total {
            return Err(bad(kind, format!("larger than {} MB", limits.total >> 20)));
        }
        let zip = photocraft_format::zip::ZipReader::new(bytes).map_err(|e| bad(kind, e))?;
        if zip.entries.len() > limits.files {
            return Err(bad(kind, format!("more than {} files", limits.files)));
        }
        let mut total = 0usize;
        let mut ignored = Vec::new();
        for e in &zip.entries {
            if e.name.ends_with('/') && safe_path(e.name.trim_end_matches('/')) && e.declared_size() == 0 {
                continue; // a directory entry
            }
            if !safe_path(&e.name) {
                return Err(bad(kind, format!("unsafe path `{}`", e.name.escape_debug())));
            }
            if e.is_symlink() {
                return Err(bad(kind, format!("`{}` is a symbolic link", e.name)));
            }
            if e.declared_size() > limits.file {
                return Err(bad(kind, format!("`{}` is larger than {} MB", e.name, limits.file >> 20)));
            }
            total = total.saturating_add(e.declared_size());
            if e.name != MANIFEST && !allowed(&e.name) {
                ignored.push(e.name.clone());
            }
        }
        if total > limits.total {
            return Err(bad(kind, format!("unpacks to more than {} MB", limits.total >> 20)));
        }
        let manifest = zip.read_by_name(MANIFEST, MAX_MANIFEST).map_err(|_| bad(kind, "no manifest.json"))?;
        let Ok(Value::Object(manifest)) = serde_json::from_slice::<Value>(&manifest) else {
            return Err(bad(kind, "manifest.json is not a JSON object"));
        };
        let format = manifest.get("format").and_then(Value::as_str).unwrap_or_default().to_string();
        let Some(&(_, newest)) = formats.iter().find(|(f, _)| *f == format) else {
            return Err(bad(kind, "unknown format"));
        };
        let version = manifest.get("version").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| bad(kind, "no format version"))?;
        if version > newest {
            return Err(EngineError::Other(format!("this {kind} was made by a newer Capishot; update to open it")));
        }
        let mut meta: Meta = match manifest.get("meta") {
            Some(m) => serde_json::from_value(m.clone()).map_err(|e| bad(kind, format!("meta: {e}")))?,
            None => Meta::default(),
        };
        let warnings = meta.sanitize();
        meta.check_version()?;
        Ok(Package { zip, kind, limits, allowed, format, version, meta, manifest, ignored, warnings })
    }

    /// Is `name` in the package (and one of its format's files)?
    pub fn has(&self, name: &str) -> bool {
        (self.allowed)(name) && self.zip.find(name).is_some()
    }

    /// The names of the package's files under `dir/` its format reads.
    pub fn files_in(&self, dir: &str) -> Vec<String> {
        self.zip.entries.iter().map(|e| e.name.clone()).filter(|n| n.strip_prefix(dir).is_some_and(|r| r.starts_with('/')) && (self.allowed)(n)).collect()
    }

    /// One of its format's files, decompressed (at most the per-file limit).
    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        if !(self.allowed)(name) || !safe_path(name) {
            return Err(bad(self.kind, format!("`{name}` is not part of this format")));
        }
        self.zip.read_by_name(name, self.limits.file).map_err(|e| bad(self.kind, e))
    }

    /// One of its files as JSON, if it's there.
    pub fn read_json(&self, name: &str) -> Result<Option<Value>> {
        if !self.has(name) {
            return Ok(None);
        }
        serde_json::from_slice(&self.read(name)?).map(Some).map_err(|e| bad(self.kind, format!("{name}: {e}")))
    }

    /// One of its files as UTF-8 text, if it's there.
    pub fn read_text(&self, name: &str) -> Result<Option<String>> {
        if !self.has(name) {
            return Ok(None);
        }
        String::from_utf8(self.read(name)?).map(Some).map_err(|_| bad(self.kind, format!("{name} is not UTF-8 text")))
    }

    /// One of its files as a PNG at most `max` pixels on a side, if it's there.
    pub fn read_png(&self, name: &str, max: u32) -> Result<Option<Vec<u8>>> {
        if !self.has(name) {
            return Ok(None);
        }
        let bytes = self.read(name)?;
        check_png(&bytes, max).map_err(|e| bad(self.kind, format!("{name}: {e}")))?;
        Ok(Some(bytes))
    }
}

/// Checks `bytes` is a PNG at most `max` pixels on a side that decodes; returns its size.
pub fn check_png(bytes: &[u8], max: u32) -> std::result::Result<(u32, u32), String> {
    if photocraft_codecs::detect(bytes) != Some(photocraft_codecs::Format::Png) {
        return Err("not a PNG image".into());
    }
    // IHDR, the first chunk, holds the size (big-endian width and height at bytes 16..24).
    let be = |at: usize| bytes.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    let (Some(w), Some(h)) = (be(16), be(20)) else { return Err("not a PNG image".into()) };
    if bytes.get(12..16) != Some(b"IHDR") {
        return Err("not a PNG image".into());
    }
    if w == 0 || h == 0 || w > max || h > max {
        return Err(format!("{w}×{h} pixels (at most {max}×{max})"));
    }
    photocraft_codecs::decode_as(photocraft_codecs::Format::Png, bytes).map_err(|e| e.to_string())?;
    Ok((w, h))
}

/// Writes a package: `manifest` (with `format`, `version` and `meta` set here) and its files.
pub struct Writer {
    zip: photocraft_format::zip::ZipWriter,
    manifest: Map<String, Value>,
}

impl Writer {
    pub fn new(format: &str, version: u32, meta: &Meta, mut manifest: Map<String, Value>) -> Self {
        manifest.insert("format".into(), Value::from(format));
        manifest.insert("version".into(), Value::from(version));
        let mut meta = meta.clone();
        meta.sanitize();
        if let Ok(m) = serde_json::to_value(&meta)
            && m.as_object().is_some_and(|o| !o.is_empty())
        {
            manifest.insert("meta".into(), m);
        }
        Self { zip: photocraft_format::zip::ZipWriter::new(), manifest }
    }

    pub fn manifest_mut(&mut self) -> &mut Map<String, Value> {
        &mut self.manifest
    }

    pub fn add(&mut self, name: &str, bytes: &[u8]) -> Result<()> {
        if !safe_path(name) || name == MANIFEST {
            return Err(EngineError::Other(format!("can't pack `{name}`")));
        }
        self.zip.add(name, bytes).map_err(|e| EngineError::Other(e.to_string()))
    }

    pub fn add_json(&mut self, name: &str, value: &Value) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|e| EngineError::Other(e.to_string()))?;
        self.add(name, &bytes)
    }

    pub fn finish(mut self) -> Result<Vec<u8>> {
        let text = serde_json::to_vec_pretty(&Value::Object(std::mem::take(&mut self.manifest))).map_err(|e| EngineError::Other(e.to_string()))?;
        self.zip.add(MANIFEST, &text).map_err(|e| EngineError::Other(e.to_string()))?;
        self.zip.finish().map_err(|e| EngineError::Other(e.to_string()))
    }
}

#[cfg(test)]
#[path = "package_tests.rs"]
pub(crate) mod tests;
