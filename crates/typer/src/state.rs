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
        s.refresh(false);
        s
    }

    /// Rebuild derived data after deserializing.
    pub fn restored(mut self) -> State {
        self.library.normalize();
        self.refresh(false);
        self
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

    #[test]
    fn huge_scripts_are_refused_and_newlines_normalized() {
        let mut s = State::default();
        assert!(s.set_text(&"x".repeat(MAX_SCRIPT_BYTES + 1)).is_err());
        s.set_text("a\r\nb").unwrap();
        assert_eq!(s.text, "a\nb");
        assert_eq!(s.lines().len(), 2);
    }
}
