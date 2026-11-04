//! Decoded documents kept for reopening (the embed API's `open(…, {cacheKey, version})`): a page
//! opened again under the same key and version comes back at once, without decoding its file.
//!
//! An entry is the document as it was opened (after the Color Settings policy), held as a shared
//! snapshot. Reopening it clones that snapshot: layers' tiles are copy-on-write, so the document
//! opened from the cache, the cache itself and any other document opened from the same entry share
//! their pixels until one of them changes a tile (a raw helper document and the page made from it
//! hold one copy of the page's pixels). The memory an entry costs is what it holds that no open
//! document shares; the budget counts its whole pixel size, the safe bound.
//!
//! Least recently used entries are dropped past the budget ([`DocCache::DEFAULT_BUDGET`]); an
//! entry whose version differs from the one asked for is stale and dropped when asked.

use std::collections::HashSet;
use std::sync::Arc;

use photocraft_doc::{DocId, Document};
use serde_json::{Value, json};

/// One decoded document.
struct Entry {
    key: String,
    version: String,
    doc: Arc<Document>,
    /// The import warnings it opened with.
    warnings: Vec<String>,
    /// Its pixel bytes (tiles and shared payloads).
    bytes: usize,
}

/// The decoded-document cache (see the module docs). Least recently used first.
#[derive(Default)]
pub struct DocCache {
    entries: Vec<Entry>,
    /// The most bytes kept; `None`: [`Self::DEFAULT_BUDGET`].
    budget: Option<usize>,
}

impl std::fmt::Debug for DocCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocCache").field("entries", &self.entries.len()).field("bytes", &self.used()).finish()
    }
}

/// The bytes of `doc`'s pixels.
fn doc_bytes(doc: &Document) -> usize {
    photocraft_ops::document_bytes(doc, &mut HashSet::new())
}

impl DocCache {
    /// 512 MB: a few dozen 2x manga pages (most of it shared with the open documents).
    pub const DEFAULT_BUDGET: usize = 512 << 20;

    /// The most bytes kept.
    pub fn budget(&self) -> usize {
        self.budget.unwrap_or(Self::DEFAULT_BUDGET)
    }

    /// Keep at most `bytes` (0 empties the cache and keeps nothing).
    pub fn set_budget(&mut self, bytes: usize) {
        self.budget = Some(bytes);
        self.evict();
    }

    /// The bytes the entries hold (pixels shared with open documents included).
    pub fn used(&self) -> usize {
        self.entries.iter().fold(0usize, |n, e| n.saturating_add(e.bytes))
    }

    /// A copy of the document cached under `key` at `version`, with a fresh id and its import
    /// warnings; its pixels are shared with the cache (copy-on-write). An entry at another version
    /// is stale: it is dropped and `None` returned.
    pub fn get(&mut self, key: &str, version: &str) -> Option<(Document, Vec<String>)> {
        let i = self.entries.iter().position(|e| e.key == key)?;
        let e = self.entries.remove(i);
        if e.version != version {
            return None;
        }
        let mut doc = (*e.doc).clone();
        doc.id = DocId::fresh();
        let warnings = e.warnings.clone();
        // Most recently used now.
        self.entries.push(e);
        Some((doc, warnings))
    }

    /// Whether `key` is cached at `version`.
    pub fn contains(&self, key: &str, version: &str) -> bool {
        self.entries.iter().any(|e| e.key == key && e.version == version)
    }

    /// Keep `doc` (shared, not copied) under `key` at `version`, replacing what was there; least
    /// recently used entries go past the budget. A document larger than the budget isn't kept.
    pub fn put(&mut self, key: &str, version: &str, doc: Arc<Document>, warnings: Vec<String>) {
        self.remove(key);
        let bytes = doc_bytes(&doc);
        if bytes > self.budget() {
            return;
        }
        self.entries.push(Entry { key: key.to_string(), version: version.to_string(), doc, warnings, bytes });
        self.evict();
    }

    /// The cached documents, least recently used first.
    pub fn documents(&self) -> impl Iterator<Item = &Document> {
        self.entries.iter().map(|e| &*e.doc)
    }

    /// Drop `key`'s entry; true when there was one.
    pub fn remove(&mut self, key: &str) -> bool {
        let n = self.entries.len();
        self.entries.retain(|e| e.key != key);
        self.entries.len() != n
    }

    /// Drop every entry.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    fn evict(&mut self) {
        let budget = self.budget();
        while self.used() > budget && !self.entries.is_empty() {
            self.entries.remove(0);
        }
    }

    /// `{budgetMb, usedMb, entries: [{key, version, mb}]}`, least recently used first.
    pub fn info(&self) -> Value {
        let mb = |b: usize| (b as f64 / 1048576.0 * 10.0).round() / 10.0;
        json!({
            "budgetMb": mb(self.budget()),
            "usedMb": mb(self.used()),
            "entries": self.entries.iter().map(|e| json!({"key": e.key, "version": e.version, "mb": mb(e.bytes)})).collect::<Vec<_>>(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_doc::{Layer, LayerContent};
    use photocraft_geom::{Rect, TileCoord};
    use photocraft_raster::Surface;

    fn page(w: i32, h: i32) -> Document {
        let mut doc = Document::new("page", photocraft_doc::Size::new(w as u32, h as u32), photocraft_doc::ColorMode::Rgb, photocraft_color::SampleType::U8);
        let fmt = doc.pixel_format();
        let mut s = Surface::new(fmt);
        let px = vec![0.5f32; (w * h) as usize * fmt.channels()];
        s.write_region(Rect::new(0, 0, w, h), &px);
        doc.layers.push(Layer::new("Page", LayerContent::Raster(s)));
        doc
    }

    fn surface(doc: &Document) -> &Surface {
        match &doc.layers.last().unwrap().content {
            LayerContent::Raster(s) => s,
            _ => panic!("not a raster layer"),
        }
    }

    #[test]
    fn reopening_shares_pixels_with_a_fresh_id() {
        let mut c = DocCache::default();
        let doc = Arc::new(page(300, 200));
        c.put("p13", "v1", Arc::clone(&doc), vec!["note".into()]);
        let (a, w) = c.get("p13", "v1").unwrap();
        let (b, _) = c.get("p13", "v1").unwrap();
        assert_eq!(w, vec!["note".to_string()]);
        assert!(a.id != doc.id && b.id != doc.id && a.id != b.id);
        let t = TileCoord { tx: 0, ty: 0 };
        // The raw and the page document share the decoded tiles (copy-on-write).
        assert!(Arc::ptr_eq(surface(&a).tile(t).unwrap(), surface(&doc).tile(t).unwrap()));
        assert!(Arc::ptr_eq(surface(&a).tile(t).unwrap(), surface(&b).tile(t).unwrap()));
        // Editing one copies its tile, the others keep theirs.
        let mut a = a;
        if let LayerContent::Raster(s) = &mut a.layers.last_mut().unwrap().content {
            s.write_region(Rect::new(0, 0, 1, 1), &vec![1.0; s.format().channels()]);
        }
        assert!(!Arc::ptr_eq(surface(&a).tile(t).unwrap(), surface(&b).tile(t).unwrap()));
    }

    #[test]
    fn a_stale_version_misses_and_is_dropped() {
        let mut c = DocCache::default();
        c.put("p", "1", Arc::new(page(10, 10)), Vec::new());
        assert!(c.get("p", "2").is_none());
        assert!(c.get("p", "1").is_none(), "the stale entry is gone");
        assert!(c.get("q", "1").is_none());
    }

    #[test]
    fn least_recently_used_go_first_past_the_budget() {
        let mut c = DocCache::default();
        let one = doc_bytes(&page(256, 256));
        c.set_budget(one * 2);
        c.put("a", "1", Arc::new(page(256, 256)), Vec::new());
        c.put("b", "1", Arc::new(page(256, 256)), Vec::new());
        assert!(c.get("a", "1").is_some());
        c.put("c", "1", Arc::new(page(256, 256)), Vec::new());
        assert!(c.contains("a", "1") && c.contains("c", "1") && !c.contains("b", "1"));
        // Too big for the budget: not kept, nothing else dropped for it.
        c.put("d", "1", Arc::new(page(1024, 1024)), Vec::new());
        assert!(!c.contains("d", "1") && c.contains("a", "1"));
        assert_eq!(c.info()["entries"].as_array().unwrap().len(), 2);
        c.set_budget(0);
        assert_eq!(c.used(), 0);
        c.set_budget(one);
        c.put("e", "1", Arc::new(page(256, 256)), Vec::new());
        assert!(c.remove("e") && !c.remove("e"));
        c.put("e", "1", Arc::new(page(256, 256)), Vec::new());
        c.clear();
        assert_eq!(c.info()["usedMb"], json!(0.0));
    }
}
