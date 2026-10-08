//! Content hashes of installed LUTs, so installing the same LUTs twice (a pack and its parent
//! folder, or a pack downloaded again) can be noticed. Each pack keeps its hashes in a hidden
//! `.hashes` file written at install time; packs installed without one are hashed once, on demand.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use super::{LutLibrary, collect};

const HASH_FILE: &str = ".hashes";

pub(super) fn hash_bytes(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

pub(super) fn write_sidecar(dir: &Path, hashes: &[(String, String)]) {
    let text: String = hashes.iter().map(|(h, f)| format!("{h}\t{f}\n")).collect();
    let _ = super::write_atomic(&dir.join(HASH_FILE), text.as_bytes());
}

fn read_sidecar(dir: &Path) -> Option<Vec<(String, String)>> {
    let text = fs::read_to_string(dir.join(HASH_FILE)).ok()?;
    Some(text.lines().filter_map(|l| l.split_once('\t')).map(|(h, f)| (h.to_string(), f.to_string())).collect())
}

/// Hash every LUT in `dir` (pack-relative paths) and keep the result next to them.
fn rebuild(dir: &Path) -> Vec<(String, String)> {
    let Ok(found) = collect(dir) else { return Vec::new() };
    let hashes: Vec<(String, String)> = found.iter().filter_map(|f| fs::read(&f.path).ok().map(|b| (hash_bytes(&b), f.rel.join("/")))).collect();
    write_sidecar(dir, &hashes);
    hashes
}

/// `(content hashes, deepest folder level)` of the pack in `dir`.
fn pack_hashes(dir: &Path) -> (HashSet<String>, usize) {
    let present: HashSet<String> = collect(dir).map(|f| f.iter().map(|x| x.rel.join("/")).collect()).unwrap_or_default();
    let mut hashes = read_sidecar(dir).unwrap_or_default();
    if hashes.len() != present.len() || hashes.iter().any(|(_, f)| !present.contains(f)) {
        hashes = rebuild(dir);
    }
    let depth = hashes.iter().map(|(_, f)| f.matches('/').count()).max().unwrap_or(0);
    (hashes.into_iter().map(|(h, _)| h).collect(), depth)
}

impl LutLibrary {
    /// Packs whose every LUT is also in one other pack, as `pack → the pack that covers it`. Of two
    /// packs with identical content the one with deeper folders (then the later name) is flagged, so
    /// a parent folder installed over its own sub-folder is the duplicate. Nothing is deleted.
    pub fn redundant_packs(&self) -> HashMap<String, String> {
        let Ok(entries) = fs::read_dir(&self.root) else { return HashMap::new() };
        let mut packs: Vec<(String, HashSet<String>, usize)> = Vec::new();
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !e.file_type().is_ok_and(|k| k.is_dir()) {
                continue;
            }
            let (hashes, depth) = pack_hashes(&e.path());
            if !hashes.is_empty() {
                packs.push((name, hashes, depth));
            }
        }
        let key = |p: &(String, HashSet<String>, usize)| (p.2, p.0.to_lowercase());
        let mut out = HashMap::new();
        for a in &packs {
            let cover = packs.iter().filter(|b| b.0 != a.0 && a.1.is_subset(&b.1) && (a.1 != b.1 || key(a) > key(b))).min_by_key(|b| key(b));
            if let Some(b) = cover {
                out.insert(a.0.clone(), b.0.clone());
            }
        }
        out
    }
    /// `content hash → (pack, path in pack)` of every installed LUT outside the pack `exclude`.
    pub(super) fn known_hashes(&self, exclude: Option<&str>) -> HashMap<String, (String, String)> {
        let mut out = HashMap::new();
        let Ok(entries) = fs::read_dir(&self.root) else { return out };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || exclude == Some(name.as_str()) || !entry.file_type().is_ok_and(|k| k.is_dir()) {
                continue;
            }
            let dir = entry.path();
            let present: HashSet<String> = collect(&dir).map(|f| f.iter().map(|x| x.rel.join("/")).collect()).unwrap_or_default();
            let mut hashes = read_sidecar(&dir).unwrap_or_default();
            // A pack edited by hand (or installed by an older build) no longer matches its sidecar.
            if hashes.len() != present.len() || hashes.iter().any(|(_, f)| !present.contains(f)) {
                hashes = rebuild(&dir);
            }
            for (hash, file) in hashes {
                out.entry(hash).or_insert_with(|| (name.clone(), file));
            }
        }
        out
    }
}
