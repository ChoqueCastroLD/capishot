// Ported from TypeR app_src/context.jsx (navigation and selection reducer cases, develop 71bc061),
// MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! The TypeR workspace: script, cursor, selected style, page files, library and settings.
//!
//! Every mutator re-derives the lines and fixes the cursor and the style the way TypeR's reducer
//! does after each action: the cursor sits on a usable line, a line change selects the line's
//! style (or the default style), and an unknown style falls back to the first one.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::library::Library;
use crate::pages;
use crate::script::{self, Line, ParseContext, UsedLineStyle};
use crate::settings::Settings;
use crate::tabs::{self, Tab};

/// Largest script accepted (TypeR has no limit; this keeps a pasted mistake from freezing the
/// app). 8 MB is several hundred chapters.
pub const MAX_SCRIPT_BYTES: usize = 8 << 20;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct State {
    /// The script.
    pub text: String,
    /// Raw index of the current line.
    pub current_line_index: usize,
    pub current_style_id: Option<Id>,
    /// Style each line was typeset with, by raw index (TypeR 3.0 `usedLineStyles`).
    pub used_line_styles: BTreeMap<usize, UsedLineStyle>,
    /// Page file names in natural order (N4).
    pub images: Vec<String>,
    pub last_opened_image: Option<String>,
    #[serde(with = "library_serde")]
    pub library: Library,
    pub settings: Settings,
    /// Work tabs (TypeR 3.0). The active tab's script, page files and cursor are the fields
    /// above; its entry here keeps only its id and name (see [`crate::tabs`]). Empty until the
    /// first tab command (or a restore) creates "Tab 1".
    pub tabs: Vec<Tab>,
    pub current_tab_id: String,
    /// Derived from the fields above; rebuilt by [`State::refresh`].
    #[serde(skip)]
    lines: Vec<Line>,
}

/// What a navigation call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moved {
    Yes,
    /// Nothing to move to (end of script, no further page, or the end-of-page lock).
    No,
}

impl State {
    /// A state with `library` and `settings`, no script.
    pub fn new(library: Library, settings: Settings) -> State {
        let mut s = State { library, settings, ..State::default() };
        s.library.normalize();
        s.ensure_tabs();
        s.refresh(false);
        s
    }

    /// Rebuild derived data after deserializing.
    pub fn restored(mut self) -> State {
        self.library.normalize();
        self.ensure_tabs();
        self.refresh(false);
        self
    }

    /// Create the first tab ("Tab 1") when there is none, and make sure the current tab exists
    /// and holds no data of its own (its data is the state's fields).
    pub fn ensure_tabs(&mut self) {
        self.tabs.truncate(tabs::MAX_TABS);
        if self.tabs.is_empty() {
            let id = tabs::next_id(&self.tabs);
            self.tabs.push(Tab { id: id.clone(), name: tabs::default_name(0), ..Tab::default() });
            self.current_tab_id = id;
        }
        let i = match self.tab_index(&self.current_tab_id) {
            Some(i) => i,
            None => {
                self.current_tab_id = self.tabs.first().map(|t| t.id.clone()).unwrap_or_default();
                0
            }
        };
        if let Some(t) = self.tabs.get_mut(i) {
            // Whatever an older file left in the active entry is stale: the fields win.
            *t = Tab { id: std::mem::take(&mut t.id), name: std::mem::take(&mut t.name), ..Tab::default() };
        }
    }

    fn tab_index(&self, id: &str) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The tabs with the active one's data filled in, for listing.
    pub fn tab_views(&self) -> Vec<Tab> {
        self.tabs.iter().map(|t| if t.id == self.current_tab_id { self.active_tab_data(t) } else { t.clone() }).collect()
    }

    fn active_tab_data(&self, t: &Tab) -> Tab {
        Tab {
            id: t.id.clone(),
            name: t.name.clone(),
            text: self.text.clone(),
            images: self.images.clone(),
            current_line_index: self.current_line_index,
            last_opened_image: self.last_opened_image.clone(),
            used_line_styles: self.used_line_styles.clone(),
        }
    }

    /// Move the active tab's data from the fields into its entry.
    fn stash_active(&mut self) {
        let Some(i) = self.tab_index(&self.current_tab_id) else { return };
        let data = Tab {
            id: String::new(),
            name: String::new(),
            text: std::mem::take(&mut self.text),
            images: std::mem::take(&mut self.images),
            current_line_index: std::mem::take(&mut self.current_line_index),
            last_opened_image: self.last_opened_image.take(),
            used_line_styles: std::mem::take(&mut self.used_line_styles),
        };
        if let Some(t) = self.tabs.get_mut(i) {
            *t = Tab { id: std::mem::take(&mut t.id), name: std::mem::take(&mut t.name), ..data };
        }
    }

    /// Make tab `i` active: its data moves into the fields (TypeR's `loadTabIntoState`), and the
    /// cursor's line picks its style.
    fn load_tab(&mut self, i: usize) {
        let Some(t) = self.tabs.get_mut(i) else { return };
        self.current_tab_id = t.id.clone();
        self.text = std::mem::take(&mut t.text);
        self.images = std::mem::take(&mut t.images);
        self.current_line_index = std::mem::take(&mut t.current_line_index);
        self.last_opened_image = t.last_opened_image.take();
        self.used_line_styles = std::mem::take(&mut t.used_line_styles);
        self.refresh(true);
    }

    /// Open a new tab (TypeR's `addTab`) named `name` (default "Tab N") with `text`, and switch
    /// to it. Errors past [`tabs::MAX_TABS`] or on a script that is too big.
    pub fn new_tab(&mut self, name: Option<&str>, text: &str) -> Result<String, String> {
        self.ensure_tabs();
        if self.tabs.len() >= tabs::MAX_TABS {
            return Err(format!("at most {} tabs", tabs::MAX_TABS));
        }
        if text.len() > MAX_SCRIPT_BYTES {
            return Err(format!("the script is {} MB; the limit is {} MB", text.len() >> 20, MAX_SCRIPT_BYTES >> 20));
        }
        let name = name.and_then(tabs::clean_name).unwrap_or_else(|| tabs::default_name(self.tabs.len()));
        let id = tabs::next_id(&self.tabs);
        self.stash_active();
        self.tabs.push(Tab { id: id.clone(), name, text: script::normalize_newlines(text), ..Tab::default() });
        self.settings.multi_tab_enabled = true;
        self.load_tab(self.tabs.len() - 1);
        Ok(id)
    }

    /// Switch to tab `id`. `Moved::No` when it is already the current one.
    pub fn switch_tab(&mut self, id: &str) -> Result<Moved, String> {
        self.ensure_tabs();
        let i = self.tab_index(id).ok_or_else(|| format!("no tab \"{id}\""))?;
        if id == self.current_tab_id {
            return Ok(Moved::No);
        }
        self.stash_active();
        self.load_tab(i);
        Ok(Moved::Yes)
    }

    /// Rename tab `id` (trimmed; an empty name is refused).
    pub fn rename_tab(&mut self, id: &str, name: &str) -> Result<(), String> {
        self.ensure_tabs();
        let name = tabs::clean_name(name).ok_or("a tab needs a name")?;
        let i = self.tab_index(id).ok_or_else(|| format!("no tab \"{id}\""))?;
        if let Some(t) = self.tabs.get_mut(i) {
            t.name = name;
        }
        Ok(())
    }

    /// Close tab `id` (TypeR's `deleteTab`): the last tab can't be closed; closing the current
    /// one activates its neighbour (the next one, else the previous).
    pub fn close_tab(&mut self, id: &str) -> Result<(), String> {
        self.ensure_tabs();
        if self.tabs.len() <= 1 {
            return Err("the last tab can't be closed".into());
        }
        let i = self.tab_index(id).ok_or_else(|| format!("no tab \"{id}\""))?;
        self.tabs.remove(i);
        if id == self.current_tab_id {
            self.load_tab(i.min(self.tabs.len().saturating_sub(1)));
        }
        Ok(())
    }

    pub fn lines(&self) -> &[Line] {
        &self.lines
    }

    pub fn current_line(&self) -> Option<&Line> {
        self.lines.get(self.current_line_index).filter(|l| !l.ignore)
    }

    pub fn current_style(&self) -> Option<&crate::library::Style> {
        self.current_style_id.as_ref().and_then(|id| self.library.style(id))
    }

    /// The page of the current line (the last marker before it).
    pub fn current_page(&self) -> Option<u32> {
        script::page_of(&self.lines, self.current_line_index)
    }

    /// Index into [`State::images`] of the current page's file.
    pub fn current_image(&self) -> Option<usize> {
        if self.images.is_empty() {
            return None;
        }
        pages::image_for_page(&self.images, self.current_page()?)
    }

    /// Re-derive lines, fix the cursor and (with `select_style`) pick the current line's style.
    pub fn refresh(&mut self, select_style: bool) {
        if self.settings.default_style_id.as_ref().is_some_and(|id| self.library.style(id).is_none()) {
            self.settings.default_style_id = None;
        }
        self.used_line_styles.retain(|_, u| self.library.style(&u.style_id).is_some());
        let folder_before = self.current_style().map(|s| s.folder.clone());
        self.parse_lines();
        // Phase 3: the cursor sits on a usable line (the first one, else 0).
        if self.lines.get(self.current_line_index).is_none_or(|l| l.ignore) {
            self.current_line_index = script::next_usable(&self.lines, 0).unwrap_or(0);
        }
        if select_style {
            // Only tag styles and continuations drive the picker; `usedStyle` doesn't (#205).
            if let Some(style) = self.lines.get(self.current_line_index).and_then(|l| l.style.clone()) {
                self.current_style_id = Some(style);
            } else if let Some(d) = self.settings.default_style_id.clone() {
                self.current_style_id = Some(d);
            }
        }
        // Phase 4: an unknown style falls back to the first style.
        if self.current_style().is_none() {
            self.current_style_id = self.library.styles.first().map(|s| s.id.clone());
        }
        // Folder isolation follows the selected style: re-match tags if its folder changed.
        if self.settings.current_folder_tag_priority && self.current_style().map(|s| s.folder.clone()) != folder_before {
            self.parse_lines();
        }
    }

    fn parse_lines(&mut self) {
        let cx = ParseContext {
            library: &self.library,
            settings: &self.settings,
            current_style: self.current_style_id.as_ref(),
            used_line_styles: &self.used_line_styles,
            has_images: !self.images.is_empty(),
        };
        self.lines = script::parse(&self.text, &cx);
    }

    /// Replace the script (newlines normalized). The cursor stays where it was if that line is
    /// still usable. Errors on a script above [`MAX_SCRIPT_BYTES`].
    pub fn set_text(&mut self, text: &str) -> Result<(), String> {
        if text.len() > MAX_SCRIPT_BYTES {
            return Err(format!("the script is {} MB; the limit is {} MB", text.len() >> 20, MAX_SCRIPT_BYTES >> 20));
        }
        self.ensure_tabs();
        let text = script::normalize_newlines(text);
        if text != self.text {
            // Recorded styles belong to the old line positions; keep the ones whose text is unchanged.
            let raw: Vec<&str> = text.split('\n').collect();
            self.used_line_styles.retain(|i, u| raw.get(*i).is_some_and(|r| *r == u.raw_text));
        }
        self.text = text;
        self.refresh(false);
        Ok(())
    }

    /// Make line `index` current (an ignored line moves the cursor to the first usable one).
    pub fn set_line(&mut self, index: usize) {
        self.current_line_index = index;
        self.refresh(true);
    }

    /// Next usable line. With `add` (after a paste or apply) the line's style is recorded as
    /// used, and the cursor doesn't leave the last line of a synced page.
    pub fn next_line(&mut self, add: bool) -> Moved {
        if self.text.is_empty() {
            return Moved::No;
        }
        let cur = self.current_line_index;
        if add && let (Some(line), Some(style)) = (self.lines.get(cur).filter(|l| !l.ignore), self.current_style_id.clone()) {
            self.used_line_styles.insert(cur, UsedLineStyle { raw_text: line.raw_text.clone(), style_id: style });
        }
        if add && self.lines.get(cur).is_some_and(|l| l.last) {
            self.refresh(false);
            return Moved::No;
        }
        let target = script::next_usable(&self.lines, cur.saturating_add(1));
        self.move_to(target)
    }

    /// Previous usable line.
    pub fn previous_line(&mut self) -> Moved {
        if self.text.is_empty() {
            return Moved::No;
        }
        let target = self.lines.iter().take(self.current_line_index).rposition(|l| !l.ignore);
        self.move_to(target)
    }

    /// First usable line after the next page marker.
    pub fn next_page(&mut self) -> Moved {
        if self.text.is_empty() {
            return Moved::No;
        }
        let from = self.current_line_index.saturating_add(1);
        let marker = self.lines.iter().skip(from).position(|l| l.page.is_some()).map(|p| p + from);
        let target = marker.and_then(|m| script::next_usable(&self.lines, m + 1));
        self.move_to(target)
    }

    /// First usable line after the second-to-last marker before the cursor (the start of the
    /// previous page), not crossing the next marker.
    pub fn previous_page(&mut self) -> Moved {
        if self.text.is_empty() {
            return Moved::No;
        }
        let markers: Vec<usize> = self.lines.iter().take(self.current_line_index).enumerate().filter(|(_, l)| l.page.is_some()).map(|(i, _)| i).collect();
        let Some(&target_marker) = markers.len().checked_sub(2).and_then(|i| markers.get(i)) else { return Moved::No };
        let mut target = None;
        for (i, l) in self.lines.iter().enumerate().skip(target_marker + 1) {
            if l.page.is_some() {
                break;
            }
            if !l.ignore {
                target = Some(i);
                break;
            }
        }
        self.move_to(target)
    }

    fn move_to(&mut self, target: Option<usize>) -> Moved {
        match target {
            Some(i) => {
                self.current_line_index = i;
                self.refresh(true);
                Moved::Yes
            }
            None => {
                self.refresh(false);
                Moved::No
            }
        }
    }

    /// Select a style by id. Errors if it doesn't exist.
    pub fn select_style(&mut self, id: &Id) -> Result<(), String> {
        if self.library.style(id).is_none() {
            return Err(format!("no style {id}"));
        }
        self.current_style_id = Some(id.clone());
        self.refresh(false);
        Ok(())
    }

    /// Select the `index`-th (0-based) style of the current style's folder (numpad 1–9).
    pub fn select_style_in_folder(&mut self, index: usize) -> Result<(), String> {
        let id = self
            .library
            .style_in_current_folder(self.current_style_id.as_ref(), index)
            .map(|s| s.id.clone())
            .ok_or_else(|| format!("the current folder has no style {}", index + 1))?;
        self.select_style(&id)
    }

    /// Next (`step` 1) or previous (`step` -1) style in display order, wrapping around.
    pub fn step_style(&mut self, forward: bool) -> Moved {
        let n = self.library.styles.len();
        if n == 0 {
            return Moved::No;
        }
        let cur = self.current_style_id.as_ref().and_then(|id| self.library.styles.iter().position(|s| &s.id == id)).unwrap_or(0);
        let next = if forward { (cur + 1) % n } else { (cur + n - 1) % n };
        self.current_style_id = self.library.styles.get(next).map(|s| s.id.clone());
        self.refresh(false);
        Moved::Yes
    }

    /// Set the page files (names or paths), sorted naturally.
    pub fn set_images(&mut self, mut images: Vec<String>) {
        self.ensure_tabs();
        images.sort_by(|a, b| pages::natural_cmp(a, b));
        self.images = images;
        self.last_opened_image = None;
        self.refresh(false);
    }

    /// Change the library through `f`, then normalize and refresh.
    pub fn edit_library<R>(&mut self, f: impl FnOnce(&mut Library) -> R) -> R {
        let r = f(&mut self.library);
        self.library.normalize();
        self.refresh(false);
        r
    }

    /// Change the settings through `f`, then refresh.
    pub fn edit_settings<R>(&mut self, f: impl FnOnce(&mut Settings) -> R) -> R {
        let r = f(&mut self.settings);
        self.refresh(false);
        r
    }
}

/// Bring a stored state object (`TyperSession::to_json`'s `state`, or a TypeR storage file) to the
/// tab schema before it is read: TypeR's tab migration ([`tabs::migrate`]), then the active tab's
/// data goes into the state's fields and its entry keeps only id and name. Page files given as
/// TypeR's `{path}` objects become names.
pub fn migrate_tabs(state: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::Value;
    let mut data = serde_json::Map::new();
    for k in tabs::TAB_FIELDS.iter().copied().chain(["lastOpenedImagePath", "tabs", "currentTabId"]) {
        if let Some(v) = state.get(k) {
            data.insert(k.to_string(), v.clone());
        }
    }
    let m = tabs::migrate(&Value::Object(data), &tabs::default_name(0), || "tab-1".to_string());
    let mut out: Vec<Tab> = Vec::new();
    let mut active: Option<Tab> = None;
    for (i, t) in m.tabs.iter().enumerate() {
        let id = t.get("id").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("tab-x{i}"));
        let name = t.get("name").and_then(Value::as_str).and_then(tabs::clean_name).unwrap_or_else(|| tabs::default_name(i));
        // Duplicate ids would make a tab unreachable: later copies get new ids.
        let id = if out.iter().any(|o| o.id == id) || active.as_ref().is_some_and(|a| a.id == id) { format!("{id}-{i}") } else { id };
        let tab = Tab::from_value(id, name, t);
        if active.is_none() && tab.id == m.current_tab_id {
            out.push(Tab { id: tab.id.clone(), name: tab.name.clone(), ..Tab::default() });
            active = Some(tab);
        } else {
            out.push(tab);
        }
    }
    out.truncate(tabs::MAX_TABS);
    if let Some(a) = active {
        state.insert("text".into(), Value::String(a.text));
        state.insert("images".into(), serde_json::to_value(a.images).unwrap_or(Value::Null));
        state.insert("currentLineIndex".into(), a.current_line_index.into());
        state.insert("lastOpenedImage".into(), a.last_opened_image.map_or(Value::Null, Value::String));
        state.insert("usedLineStyles".into(), serde_json::to_value(a.used_line_styles).unwrap_or(Value::Null));
        state.insert("currentTabId".into(), Value::String(a.id));
    }
    state.remove("lastOpenedImagePath");
    state.insert("tabs".into(), serde_json::to_value(out).unwrap_or(Value::Null));
}

/// The library is stored as TypeR JSON (`{folders, styles}`).
mod library_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    use crate::library::{Folder, Library, Style};

    pub fn serialize<S: Serializer>(lib: &Library, s: S) -> Result<S::Ok, S::Error> {
        serde_json::json!({
            "folders": lib.folders.iter().map(Folder::to_json).collect::<Vec<_>>(),
            "styles": lib.styles.iter().map(Style::to_json).collect::<Vec<_>>(),
        })
        .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Library, D::Error> {
        let v = Value::deserialize(d)?;
        let list = |k: &str| v.get(k).and_then(Value::as_array).cloned().unwrap_or_default();
        Ok(Library {
            folders: list("folders").iter().filter_map(Folder::from_json).collect(),
            styles: list("styles").iter().filter_map(Style::from_json).collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::{Folder, Style};
    use serde_json::Map;

    fn st(id: &str, folder: Option<&str>, prefixes: &[&str]) -> Style {
        Style {
            id: Id::str(id),
            name: id.into(),
            folder: folder.map(Id::str),
            edited: None,
            prefixes: prefixes.iter().map(|p| p.to_string()).collect(),
            props: Map::new(),
        }
    }

    fn lib() -> Library {
        Library {
            folders: vec![Folder { id: Id::str("a"), name: "A".into(), parent_id: None, order: 0.0 }],
            styles: vec![st("reg", None, &[]), st("sfx", None, &["SFX:"]), st("a1", Some("a"), &["A:"])],
        }
    }

    fn text_of(s: &State) -> &str {
        s.current_line().map_or("", |l| l.text.as_str())
    }

    #[test]
    fn cursor_skips_ignored_lines_and_selects_tag_styles() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("## title\nPage 1\nhello\nSFX: boom\n\nbye").unwrap();
        assert_eq!(text_of(&s), "hello", "the cursor starts on the first usable line");
        assert_eq!(s.current_style_id, Some(Id::str("reg")), "unknown style falls back to the first");
        assert_eq!(s.next_line(false), Moved::Yes);
        assert_eq!(text_of(&s), "boom");
        assert_eq!(s.current_style_id, Some(Id::str("sfx")));
        assert_eq!(s.next_line(false), Moved::Yes);
        assert_eq!(text_of(&s), "bye");
        assert_eq!(s.current_style_id, Some(Id::str("sfx")), "untagged line keeps the style without a default");
        assert_eq!(s.next_line(false), Moved::No);
        assert_eq!(s.previous_line(), Moved::Yes);
        assert_eq!(text_of(&s), "boom");
        s.set_line(0);
        assert_eq!(text_of(&s), "hello", "an ignored target moves to the first usable line");
        assert_eq!(s.current_page(), Some(1));
    }

    #[test]
    fn default_style_applies_to_untagged_lines_and_is_validated() {
        let settings = Settings { default_style_id: Some(Id::str("a1")), ..Settings::default() };
        let mut s = State::new(lib(), settings);
        s.set_text("SFX: x\nplain").unwrap();
        s.next_line(false);
        assert_eq!(s.current_style_id, Some(Id::str("a1")));
        s.edit_library(|l| l.delete_style(&Id::str("a1")));
        assert_eq!(s.settings.default_style_id, None, "a deleted default style is reset");
    }

    #[test]
    fn pages_navigate_by_markers() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("Page 1\na\nb\nPage 2\n## n\nc\nd\nPage 3\ne").unwrap();
        assert_eq!(s.next_page(), Moved::Yes);
        assert_eq!(text_of(&s), "c");
        assert_eq!(s.next_page(), Moved::Yes);
        assert_eq!(text_of(&s), "e");
        assert_eq!(s.next_page(), Moved::No);
        assert_eq!(s.previous_page(), Moved::Yes);
        assert_eq!(text_of(&s), "c");
        assert_eq!(s.previous_page(), Moved::Yes);
        assert_eq!(text_of(&s), "a");
        assert_eq!(s.previous_page(), Moved::No);
    }

    #[test]
    fn paste_advance_stops_at_the_end_of_a_synced_page() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("Page 1\na\nb\nPage 2\nc").unwrap();
        s.set_images(vec!["p2.psd".into(), "p1.psd".into()]);
        assert_eq!(s.images, ["p1.psd", "p2.psd"]);
        assert_eq!(s.current_image(), Some(0));
        s.next_line(true);
        assert_eq!(text_of(&s), "b");
        assert_eq!(s.next_line(true), Moved::No, "paste doesn't leave the page");
        assert_eq!(text_of(&s), "b");
        assert_eq!(s.used_line_styles.len(), 2, "both pasted lines recorded their style");
        assert_eq!(s.next_line(false), Moved::Yes, "plain next ignores the lock");
        assert_eq!(text_of(&s), "c");
        assert_eq!(s.current_image(), Some(1));
    }

    #[test]
    fn style_stepping_wraps_and_numpad_uses_the_current_folder() {
        let mut s = State::new(lib(), Settings::default());
        assert_eq!(s.current_style_id, Some(Id::str("reg")));
        s.step_style(false);
        assert_eq!(s.current_style_id, Some(Id::str("a1")));
        s.step_style(true);
        assert_eq!(s.current_style_id, Some(Id::str("reg")));
        s.select_style_in_folder(1).unwrap();
        assert_eq!(s.current_style_id, Some(Id::str("sfx")));
        assert!(s.select_style_in_folder(5).is_err());
        assert!(s.select_style(&Id::str("ghost")).is_err());
    }

    #[test]
    fn selecting_a_style_in_another_folder_rematches_tags() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("A: x\nSFX: y").unwrap();
        assert_eq!(s.lines()[0].style, None, "A: belongs to folder a, not the current Unsorted group");
        assert_eq!(s.lines()[1].style, Some(Id::str("sfx")));
        s.select_style(&Id::str("a1")).unwrap();
        assert_eq!(s.lines()[0].style, Some(Id::str("a1")));
        assert_eq!(s.lines()[1].style, None);
    }

    #[test]
    fn serde_round_trip_rebuilds_lines() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("hello\nSFX: boom").unwrap();
        s.next_line(true);
        let json = serde_json::to_string(&s).unwrap();
        let back: State = serde_json::from_str::<State>(&json).unwrap().restored();
        assert_eq!(back, s);
        assert_eq!(text_of(&back), "boom");
    }

    // TypeR 3.0's tab reducer: add, switch, rename, close.
    #[test]
    fn tabs_keep_their_own_script_cursor_and_page_files() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("Page 1\nhello\nSFX: boom").unwrap();
        s.set_images(vec!["01.psd".into()]);
        s.next_line(true);
        assert_eq!(s.tabs.len(), 1);
        let first = s.current_tab_id.clone();
        let second = s.new_tab(None, "A: other\nmore").unwrap();
        assert_eq!(s.tabs[1].name, "Tab 2");
        assert_eq!(s.current_tab_id, second);
        assert_eq!(text_of(&s), "A: other", "A: belongs to folder a, the current style's folder is Unsorted");
        assert!(s.images.is_empty() && s.used_line_styles.is_empty() && s.last_opened_image.is_none());
        s.next_line(false);
        assert_eq!(text_of(&s), "more");
        // Back: the first tab's cursor, page files and used styles are where they were.
        assert_eq!(s.switch_tab(&first).unwrap(), Moved::Yes);
        assert_eq!(text_of(&s), "boom");
        assert_eq!(s.images, ["01.psd"]);
        assert_eq!(s.used_line_styles.len(), 1);
        assert_eq!(s.switch_tab(&first).unwrap(), Moved::No);
        assert!(s.switch_tab("nope").is_err());
        // The active entry holds no copy; the views fill it in.
        assert!(s.tabs[0].text.is_empty());
        assert_eq!(s.tab_views()[0].text, s.text);
        assert_eq!(s.tab_views()[1].text, "A: other\nmore");
        s.rename_tab(&second, "  Ch. 13 ").unwrap();
        assert_eq!(s.tabs[1].name, "Ch. 13");
        assert!(s.rename_tab(&second, " ").is_err());
        // Closing the current tab activates its neighbour; the last tab stays.
        s.switch_tab(&second).unwrap();
        s.close_tab(&second).unwrap();
        assert_eq!(s.current_tab_id, first);
        assert_eq!(text_of(&s), "boom");
        assert!(s.close_tab(&first).is_err(), "the last tab can't be closed");
        assert!(s.new_tab(Some("x"), &"y".repeat(MAX_SCRIPT_BYTES + 1)).is_err());
    }

    #[test]
    fn tabs_survive_a_save_and_old_files_migrate() {
        let mut s = State::new(lib(), Settings::default());
        s.set_text("one").unwrap();
        s.new_tab(Some("B"), "two\nthree").unwrap();
        s.next_line(false);
        let mut v = serde_json::to_value(&s).unwrap();
        assert!(v["tabs"][1]["text"].as_str().is_some_and(str::is_empty), "nothing stored twice");
        migrate_tabs(v.as_object_mut().unwrap());
        let back: State = serde_json::from_value::<State>(v).unwrap().restored();
        assert_eq!(back, s);
        // A workspace from before tabs: its script becomes "Tab 1".
        let mut old = serde_json::json!({"text": "legacy", "images": [{"path": "p1.psd"}], "lastOpenedImagePath": "p1.psd", "currentLineIndex": 0});
        migrate_tabs(old.as_object_mut().unwrap());
        let back: State = serde_json::from_value::<State>(old).unwrap().restored();
        assert_eq!((back.tabs.len(), back.tabs[0].name.as_str(), back.text.as_str()), (1, "Tab 1", "legacy"));
        assert_eq!(back.images, ["p1.psd"]);
        assert_eq!(back.last_opened_image.as_deref(), Some("p1.psd"));
        // Duplicate ids and an unknown current tab are repaired.
        let mut dup = serde_json::json!({"tabs": [{"id": "a", "name": "A", "text": "x"}, {"id": "a", "name": "B", "text": "y"}], "currentTabId": "q"});
        migrate_tabs(dup.as_object_mut().unwrap());
        let back: State = serde_json::from_value::<State>(dup).unwrap().restored();
        assert_eq!(back.text, "x");
        assert_ne!(back.tabs[0].id, back.tabs[1].id);
        assert_eq!(back.tab_views()[1].text, "y");
    }

    #[test]
    fn huge_scripts_are_refused_and_newlines_normalized() {
        let mut s = State::default();
        assert!(s.set_text(&"x".repeat(MAX_SCRIPT_BYTES + 1)).is_err());
        s.set_text("a\r\nb").unwrap();
        assert_eq!(s.text, "a\nb");
        assert_eq!(s.lines().len(), 2);
    }
}
