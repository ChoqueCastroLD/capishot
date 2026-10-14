//! Fonts the user loads from their own files (Type › Load Fonts…), for the browser above all: a
//! page sees no installed fonts, so a typesetter brings their own. This module is the part that
//! doesn't depend on the platform:
//!
//! * [`prepare`]: a picked file → checked font data (TrueType/OpenType, collections, WOFF 1.0
//!   unpacked to its OpenType tables; WOFF2 is refused with a reason), its faces' names and the
//!   SHA-256 of the data, which is its id: the same file loaded twice is stored once.
//! * [`Store`]: the index of loaded fonts (one [`Entry`] per file), which of them are registered
//!   with the font database this session, the writes the platform has to make to keep them
//!   (the web shell keeps them in IndexedDB, in this browser only, never uploaded), and lazy
//!   restore: at start-up the shell hands over the stored index only ([`Store::restore`]); a
//!   file's bytes are read when a layout, a font menu or a PSD asks for one of its families or
//!   PostScript names ([`request`]), or in idle time ([`Store::next_loads`]).
//!
//! The process-wide store is [`store`]; layout reaches it through [`request`] (from
//! `served::request`), the font menus through [`families`] and [`generation`].

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use skrifa::raw::FileRef;
use skrifa::{MetadataProvider, string::StringId};

/// Largest font file accepted (a full CJK collection is ~100 MB).
pub const MAX_FONT_BYTES: usize = 256 << 20;
/// Extensions read as fonts, in pickers and when a folder is walked.
pub const FONT_EXTENSIONS: &[&str] = &["ttf", "otf", "ttc", "otc", "woff"];
/// Font extensions that are recognised but can't be read, so a folder load says why it skipped them.
pub const UNSUPPORTED_EXTENSIONS: &[&str] = &["woff2", "dfont", "pfb", "pfm", "fon", "fnt"];
/// Fonts read in idle time, at most this much font data: past it, a file is read when it's needed.
pub const IDLE_BUDGET_BYTES: u64 = 384 << 20;
/// Limits on what a stored index entry may hold (the store is browser storage: untrusted).
const MAX_NAME_CHARS: usize = 256;
const MAX_FACES: usize = 256;
const MAX_ENTRIES: usize = 20_000;

/// Is `name` a font file by its extension (a supported one, or one we can name a reason for)?
pub fn is_font_file(name: &str) -> bool {
    let ext = extension(name);
    FONT_EXTENSIONS.contains(&ext.as_str()) || UNSUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

fn extension(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    base.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default()
}

/// The last component of a path or relative path (`Fonts/Sub/A.ttf` → `A.ttf`).
pub fn file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

/// One face of a loaded file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Face {
    /// The family the face registers as: its typographic family name (name ID 16), else ID 1.
    pub family: String,
    /// Typographic subfamily (17), else subfamily (2).
    pub style: String,
    pub postscript: Option<String>,
    pub weight: u16,
    pub italic: bool,
}

/// One loaded font file, as the index keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Entry {
    /// Lower-case hex SHA-256 of the font data.
    pub id: String,
    /// The file's name (no folders).
    pub name: String,
    /// Size of the font data.
    pub bytes: u64,
    pub faces: Vec<Face>,
}

impl Entry {
    /// The families of its faces, without repeats, in face order.
    pub fn families(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for f in &self.faces {
            if !v.iter().any(|x| x.eq_ignore_ascii_case(&f.family)) {
                v.push(f.family.clone());
            }
        }
        v
    }

    /// Does it provide `name`, as a family (any case) or a PostScript name?
    pub fn provides(&self, name: &str) -> bool {
        self.faces.iter().any(|f| f.family.eq_ignore_ascii_case(name) || f.postscript.as_deref() == Some(name))
    }

    /// Checks an entry read back from storage: an id that is a SHA-256, bounded names, a face.
    fn valid(&self) -> Result<(), String> {
        let short = |s: &str| s.chars().count() <= MAX_NAME_CHARS;
        if self.id.len() != 64 || !self.id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
            return Err("its id is not a SHA-256".into());
        }
        if !short(&self.name) || self.faces.is_empty() || self.faces.len() > MAX_FACES {
            return Err("it lists no faces or too many".into());
        }
        if self.bytes == 0 || self.bytes > MAX_FONT_BYTES as u64 {
            return Err("its size is out of range".into());
        }
        if self.faces.iter().any(|f| f.family.is_empty() || !short(&f.family) || !short(&f.style) || !f.postscript.as_deref().is_none_or(short)) {
            return Err("a face name is empty or too long".into());
        }
        Ok(())
    }
}

/// A picked file, checked and ready to register and store.
#[derive(Clone, Debug)]
pub struct Prepared {
    pub entry: Entry,
    /// The font data (OpenType; a WOFF file arrives unpacked).
    pub data: Arc<Vec<u8>>,
}

/// Checks a picked file and reads its faces. The error says why the file can't be used.
pub fn prepare(name: &str, bytes: Vec<u8>) -> Result<Prepared, String> {
    if bytes.len() > MAX_FONT_BYTES {
        return Err(format!("larger than {} MB", MAX_FONT_BYTES >> 20));
    }
    let data = match bytes.get(..4) {
        Some(b"wOFF") => woff_to_sfnt(&bytes)?,
        Some(b"wOF2") => return Err("WOFF2 fonts are not supported yet; use the TTF or OTF file".into()),
        _ if UNSUPPORTED_EXTENSIONS.contains(&extension(name).as_str()) => return Err("this font format is not supported; use TTF, OTF or WOFF".into()),
        _ => bytes,
    };
    crate::fonts::validate_font(&data)?;
    let faces = faces_of(&data);
    if faces.is_empty() {
        return Err("the font has no named faces".into());
    }
    let entry = Entry { id: crate::sha256::sha256_hex(&data), name: file_name(name).chars().take(MAX_NAME_CHARS).collect(), bytes: data.len() as u64, faces };
    Ok(Prepared { entry, data: Arc::new(data) })
}

/// The faces of OpenType data (every face of a collection), named as fontique registers them.
pub fn faces_of(data: &[u8]) -> Vec<Face> {
    let Ok(file) = FileRef::new(data) else { return Vec::new() };
    let mut out = Vec::new();
    for font in file.fonts().flatten() {
        let name =
            |id| font.localized_strings(id).english_or_first().map(|s| s.chars().take(MAX_NAME_CHARS).collect::<String>()).filter(|s| !s.trim().is_empty());
        let Some(family) = name(StringId::TYPOGRAPHIC_FAMILY_NAME).or_else(|| name(StringId::FAMILY_NAME)) else { continue };
        let attrs = font.attributes();
        let italic = attrs.style != skrifa::attribute::Style::Normal;
        let weight = attrs.weight.value().round().clamp(1.0, 1000.0) as u16;
        let style = name(StringId::TYPOGRAPHIC_SUBFAMILY_NAME)
            .or_else(|| name(StringId::SUBFAMILY_NAME))
            .unwrap_or_else(|| if italic { "Italic".into() } else { "Regular".into() });
        out.push(Face { family, style, postscript: name(StringId::POSTSCRIPT_NAME), weight, italic });
        if out.len() >= MAX_FACES {
            break;
        }
    }
    out
}

/// Unpacks a WOFF 1.0 file (W3C WOFF File Format 1.0: zlib-compressed OpenType tables) into the
/// OpenType file it wraps. Every offset and length comes from the file and is checked.
pub fn woff_to_sfnt(woff: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let bad = |why: &str| format!("damaged WOFF file ({why})");
    let u16_at = |at: usize| woff.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |at: usize| woff.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    if woff.get(..4) != Some(b"wOFF") {
        return Err(bad("signature"));
    }
    let flavor = u32_at(4).ok_or_else(|| bad("header"))?;
    let num_tables = usize::from(u16_at(12).ok_or_else(|| bad("header"))?);
    let total = u32_at(16).ok_or_else(|| bad("header"))? as usize;
    if num_tables == 0 || num_tables > 1024 || total > MAX_FONT_BYTES {
        return Err(bad("table count or size"));
    }
    struct Table {
        tag: u32,
        checksum: u32,
        data: Vec<u8>,
    }
    let mut tables = Vec::with_capacity(num_tables);
    let mut sum: usize = 0;
    for i in 0..num_tables {
        let at = 44 + i * 20;
        let (tag, offset, comp, orig, checksum) = (u32_at(at), u32_at(at + 4), u32_at(at + 8), u32_at(at + 12), u32_at(at + 16));
        let (Some(tag), Some(offset), Some(comp), Some(orig), Some(checksum)) = (tag, offset, comp, orig, checksum) else {
            return Err(bad("table directory"));
        };
        let (offset, comp, orig) = (offset as usize, comp as usize, orig as usize);
        sum = sum.saturating_add(orig);
        if comp > orig || sum > MAX_FONT_BYTES {
            return Err(bad("table size"));
        }
        let raw = woff.get(offset..offset.checked_add(comp).ok_or_else(|| bad("table offset"))?).ok_or_else(|| bad("table outside the file"))?;
        let data = if comp < orig {
            let mut out = Vec::with_capacity(orig);
            flate2::read::ZlibDecoder::new(raw).take(orig as u64 + 1).read_to_end(&mut out).map_err(|_| bad("compressed table"))?;
            if out.len() != orig {
                return Err(bad("table length"));
            }
            out
        } else {
            raw.to_vec()
        };
        tables.push(Table { tag, checksum, data });
    }
    tables.sort_by_key(|t| t.tag);
    let n = tables.len() as u16;
    let mut entry_selector = 0u16;
    while (2u32 << entry_selector) <= u32::from(n) {
        entry_selector += 1;
    }
    let search_range = (1u16 << entry_selector).saturating_mul(16);
    let range_shift = n.saturating_mul(16).saturating_sub(search_range);
    let mut out = Vec::with_capacity(12 + 16 * tables.len() + sum + 3 * tables.len());
    out.extend_from_slice(&flavor.to_be_bytes());
    for v in [n, search_range, entry_selector, range_shift] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    let mut offset = 12 + 16 * tables.len();
    for t in &tables {
        out.extend_from_slice(&t.tag.to_be_bytes());
        out.extend_from_slice(&t.checksum.to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(t.data.len() as u32).to_be_bytes());
        offset += t.data.len().div_ceil(4) * 4;
    }
    for t in &tables {
        out.extend_from_slice(&t.data);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    Ok(out)
}

/// Where loaded fonts are kept.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Persistence {
    /// For this session only (the desktop: fonts are files there anyway; tests).
    #[default]
    Session,
    /// A store is opening (the web build at start-up): writes wait for it.
    Pending,
    /// Kept by the platform (IndexedDB in the browser).
    Stored,
    /// The store could not be opened: session only, and why.
    Unavailable(String),
}

/// A write the platform has to make to its store.
#[derive(Clone, Debug)]
pub enum Op {
    Put { entry: Entry, data: Arc<Vec<u8>> },
    Delete(String),
    Clear,
}

/// The outcome of [`Store::add`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Added {
    New {
        id: String,
        families: Vec<String>,
    },
    /// The same font data is already loaded (under this file name).
    Duplicate {
        name: String,
    },
}

/// Why a store write failed, as the user is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WriteError {
    /// The browser's storage quota is used up.
    Quota,
    Other(String),
}

/// Is a store error the browser's quota (`QuotaExceededError`, Firefox's `NS_ERROR_DOM_QUOTA_REACHED`)?
pub fn is_quota_error(e: &str) -> bool {
    let l = e.to_ascii_lowercase();
    l.contains("quotaexceeded") || l.contains("quota_reached") || l.contains("quota exceeded") || l.contains("quota")
}

/// The index of loaded fonts and the work around it (see the module docs).
#[derive(Debug, Default)]
pub struct Store {
    entries: Vec<Entry>,
    /// Registered with the font database in this session.
    loaded: BTreeSet<String>,
    /// Kept for the session only: the store write failed (quota) or the store is unavailable.
    unsaved: BTreeSet<String>,
    /// To read from the store, oldest first.
    queued: Vec<String>,
    in_flight: BTreeSet<String>,
    ops: Vec<Op>,
    persistence: Persistence,
    generation: u64,
    /// Bytes in use and the quota of the origin's storage, as the browser estimates them.
    pub estimate: Option<(u64, u64)>,
    /// Did the browser grant persistent storage (`navigator.storage.persist()`)?
    pub persisted: Option<bool>,
    /// The Manage Fonts dialog wants a fresh [`Store::estimate`].
    pub want_estimate: bool,
    /// Ask the browser for persistent storage (set when the first font is added).
    pub want_persist: bool,
}

impl Store {
    pub const fn new() -> Self {
        Store {
            entries: Vec::new(),
            loaded: BTreeSet::new(),
            unsaved: BTreeSet::new(),
            queued: Vec::new(),
            in_flight: BTreeSet::new(),
            ops: Vec::new(),
            persistence: Persistence::Session,
            generation: 0,
            estimate: None,
            persisted: None,
            want_estimate: false,
            want_persist: false,
        }
    }

    fn changed(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn persistence(&self) -> &Persistence {
        &self.persistence
    }

    pub fn set_persistence(&mut self, p: Persistence) {
        if matches!(p, Persistence::Unavailable(_) | Persistence::Session) {
            // Nothing will write the queued fonts: they are this session's only.
            for op in std::mem::take(&mut self.ops) {
                if let Op::Put { entry, .. } = op {
                    self.unsaved.insert(entry.id);
                }
            }
        }
        self.persistence = p;
        self.changed();
    }

    fn writes(&self) -> bool {
        matches!(self.persistence, Persistence::Pending | Persistence::Stored)
    }

    /// The stored index, read back at start-up (one JSON [`Entry`] per stored file). Entries that
    /// don't read or check are skipped, with why; entries already known are kept as they are.
    pub fn restore(&mut self, records: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut warnings = Vec::new();
        for text in records {
            if self.entries.len() >= MAX_ENTRIES {
                warnings.push(format!("more than {MAX_ENTRIES} stored fonts; the rest were skipped"));
                break;
            }
            match serde_json::from_str::<Entry>(&text).map_err(|e| e.to_string()).and_then(|e| e.valid().map(|()| e)) {
                Ok(e) if self.entries.iter().any(|x| x.id == e.id) => {}
                Ok(e) => self.entries.push(e),
                Err(why) => {
                    if warnings.len() < 8 {
                        warnings.push(format!("a stored font was skipped: {why}"));
                    }
                }
            }
        }
        self.changed();
        warnings
    }

    /// Adds a prepared font: registered by the caller (it's counted loaded), written to the store
    /// when there is one. The same data twice is a [`Added::Duplicate`].
    pub fn add(&mut self, p: &Prepared) -> Added {
        if let Some(e) = self.entries.iter().find(|e| e.id == p.entry.id) {
            let name = e.name.clone();
            // Known from the index but not read yet: these bytes are as good.
            if !self.loaded.contains(&p.entry.id) {
                self.loaded.insert(p.entry.id.clone());
                self.queued.retain(|q| *q != p.entry.id);
                self.changed();
                return Added::New { id: p.entry.id.clone(), families: p.entry.families() };
            }
            return Added::Duplicate { name };
        }
        self.entries.push(p.entry.clone());
        self.loaded.insert(p.entry.id.clone());
        if self.writes() {
            self.ops.push(Op::Put { entry: p.entry.clone(), data: p.data.clone() });
            self.want_persist = true;
        } else {
            self.unsaved.insert(p.entry.id.clone());
        }
        self.changed();
        Added::New { id: p.entry.id.clone(), families: p.entry.families() }
    }

    /// Forgets a font (the caller unregisters it) and deletes it from the store.
    pub fn remove(&mut self, id: &str) -> Option<Entry> {
        let i = self.entries.iter().position(|e| e.id == id)?;
        let e = self.entries.remove(i);
        self.loaded.remove(id);
        self.in_flight.remove(id);
        self.queued.retain(|q| q != id);
        // A write still queued is dropped instead of being made and undone.
        let before = self.ops.len();
        self.ops.retain(|op| !matches!(op, Op::Put { entry, .. } if entry.id == id));
        if self.writes() && self.ops.len() == before && !self.unsaved.contains(id) {
            self.ops.push(Op::Delete(id.to_string()));
        }
        self.unsaved.remove(id);
        self.changed();
        Some(e)
    }

    /// Forgets every font and empties the store. Returns what was loaded (to unregister).
    pub fn clear(&mut self) -> Vec<Entry> {
        let all = std::mem::take(&mut self.entries);
        self.loaded.clear();
        self.queued.clear();
        self.unsaved.clear();
        self.ops.clear();
        if self.writes() {
            self.ops.push(Op::Clear);
        }
        self.changed();
        all
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn entry(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    pub fn is_loaded(&self, id: &str) -> bool {
        self.loaded.contains(id)
    }

    /// Kept for this session only (its write failed, or there is no store).
    pub fn is_unsaved(&self, id: &str) -> bool {
        self.unsaved.contains(id)
    }

    /// Bytes of font data in the index.
    pub fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|e| e.bytes).sum()
    }

    /// Every family the user fonts provide, loaded or not, sorted.
    pub fn families(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for e in &self.entries {
            for f in e.families() {
                if !v.iter().any(|x| x.eq_ignore_ascii_case(&f)) {
                    v.push(f);
                }
            }
        }
        v.sort_by_key(|s| s.to_lowercase());
        v
    }

    pub fn is_user_family(&self, family: &str) -> bool {
        self.entries.iter().any(|e| e.faces.iter().any(|f| f.family.eq_ignore_ascii_case(family)))
    }

    /// Asks for the files that provide `name` (a family or a PostScript name) to be read, if they
    /// aren't loaded yet. True when a user font provides it (loaded or not).
    pub fn request(&mut self, name: &str) -> bool {
        let mut any = false;
        for e in &self.entries {
            if e.provides(name) {
                any = true;
                if !self.loaded.contains(&e.id) && !self.in_flight.contains(&e.id) && !self.queued.contains(&e.id) {
                    self.queued.push(e.id.clone());
                }
            }
        }
        any
    }

    /// The files to read now: the ones asked for, else (when `idle`) the next unloaded one while
    /// the loaded fonts stay under [`IDLE_BUDGET_BYTES`]. Each id is handed out once until
    /// [`Store::loaded`] or [`Store::load_failed`].
    pub fn next_loads(&mut self, idle: bool) -> Vec<String> {
        let mut out: Vec<String> =
            std::mem::take(&mut self.queued).into_iter().filter(|id| !self.loaded.contains(id) && !self.in_flight.contains(id)).collect();
        if out.is_empty() && idle && self.in_flight.is_empty() {
            let used: u64 = self.entries.iter().filter(|e| self.loaded.contains(&e.id)).map(|e| e.bytes).sum();
            if let Some(e) = self.entries.iter().find(|e| !self.loaded.contains(&e.id) && !self.unsaved.contains(&e.id))
                && used.saturating_add(e.bytes) <= IDLE_BUDGET_BYTES
            {
                out.push(e.id.clone());
            }
        }
        self.in_flight.extend(out.iter().cloned());
        out
    }

    /// Is a stored file being read, or waiting to be?
    pub fn loading(&self) -> bool {
        !self.in_flight.is_empty() || !self.queued.is_empty()
    }

    /// Is there a file [`Store::next_loads`] would read in idle time?
    pub fn idle_work(&self) -> bool {
        let used: u64 = self.entries.iter().filter(|e| self.loaded.contains(&e.id)).map(|e| e.bytes).sum();
        self.entries
            .iter()
            .find(|e| !self.loaded.contains(&e.id) && !self.unsaved.contains(&e.id))
            .is_some_and(|e| used.saturating_add(e.bytes) <= IDLE_BUDGET_BYTES)
    }

    /// Unloaded fonts left to read in idle time.
    pub fn unloaded(&self) -> usize {
        self.entries.iter().filter(|e| !self.loaded.contains(&e.id)).count()
    }

    /// A stored file was read and registered.
    pub fn loaded(&mut self, id: &str) {
        self.in_flight.remove(id);
        if self.entries.iter().any(|e| e.id == id) {
            self.loaded.insert(id.to_string());
        }
        self.changed();
    }

    /// A stored file could not be read (or is no longer a font): it is dropped from the index
    /// and the store. Returns its entry, to tell the user.
    pub fn load_failed(&mut self, id: &str) -> Option<Entry> {
        self.in_flight.remove(id);
        self.remove(id)
    }

    /// The store writes to make, oldest first.
    pub fn take_ops(&mut self) -> Vec<Op> {
        if self.persistence == Persistence::Pending {
            return Vec::new();
        }
        std::mem::take(&mut self.ops)
    }

    /// A [`Op::Put`] of `id` failed with `error`: the font stays loaded for this session.
    pub fn write_failed(&mut self, id: &str, error: &str) -> WriteError {
        if self.entries.iter().any(|e| e.id == id) {
            self.unsaved.insert(id.to_string());
            self.changed();
        }
        if is_quota_error(error) { WriteError::Quota } else { WriteError::Other(error.to_string()) }
    }

    /// Changes whenever the index or what is loaded changes (menus and dialogs refresh).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

static STORE: Mutex<Store> = Mutex::new(Store::new());

/// The process-wide store. Never lock the shared text engine while holding it: layout (under
/// the engine's lock) calls [`request`].
pub fn store() -> MutexGuard<'static, Store> {
    STORE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// [`Store::request`] on the process-wide store (layout and font menus call it).
pub fn request(name: &str) -> bool {
    store().request(name)
}

/// [`Store::families`] on the process-wide store.
pub fn families() -> Vec<String> {
    store().families()
}

/// [`Store::generation`] on the process-wide store.
pub fn generation() -> u64 {
    store().generation()
}

#[cfg(test)]
#[path = "user_fonts_tests.rs"]
mod tests;
