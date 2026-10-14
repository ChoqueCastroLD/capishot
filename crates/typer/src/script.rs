// Ported from TypeR app_src/context.jsx (line parsing) and app_src/lineNumbering.js (develop
// 71bc061), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! The script: one translated chapter, split into lines (S1–S8).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::id::Id;
use crate::library::Library;
use crate::pages::{js_trim, parse_page_marker_with};
use crate::settings::Settings;

/// One raw line of the script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    /// The line as typed.
    pub raw_text: String,
    /// Position in the script (0-based).
    pub raw_index: usize,
    /// The ignore prefix it starts with (`##`), or "".
    pub ignore_prefix: String,
    /// The tag that picked its style (`SFX:`, `//`, `//:`), or "".
    pub style_prefix: String,
    /// Style from its tag, or the previous line's style for a `//` continuation.
    pub style: Option<Id>,
    /// Style the line was last typeset with, while its text is unchanged (TypeR 3.0).
    pub used_style: Option<Id>,
    /// Skipped by navigation: an ignore prefix, an empty text or a page marker.
    pub ignore: bool,
    /// Line number shown in the gutter (0 for ignored lines).
    pub index: u32,
    /// Text to typeset: tag, ignore prefix and ignore tags removed, trimmed. Markdown markers are
    /// still in it (see [`crate::markdown::parse_runs`]).
    pub text: String,
    /// The page number when this line is a page marker.
    pub page: Option<u32>,
    /// Last usable line before a page marker while page files are synced: a paste doesn't
    /// advance past it.
    pub last: bool,
}

/// The style a line was typeset with, valid while the line's text is unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsedLineStyle {
    pub raw_text: String,
    pub style_id: Id,
}

/// Line number bookkeeping (`getNextLineNumberState`): returns `(counter, index)`.
pub fn next_line_number(counter: u32, is_page: bool, ignore: bool, reset_on_page: bool) -> (u32, u32) {
    let counter = if is_page && reset_on_page { 0 } else { counter };
    if ignore {
        return (counter, 0);
    }
    let next = counter.saturating_add(1);
    (next, next)
}

/// Inputs of [`parse`] besides the text.
pub struct ParseContext<'a> {
    pub library: &'a Library,
    pub settings: &'a Settings,
    /// Selected style: with folder isolation on, only its folder's tags match.
    pub current_style: Option<&'a Id>,
    pub used_line_styles: &'a BTreeMap<usize, UsedLineStyle>,
    /// Page files are synced: the last line of each page gets `last`.
    pub has_images: bool,
}

/// Split `text` on `\n` and classify each line.
///
/// Tags match with a plain `starts_with` on the raw line; the first match in style order wins
/// (unsorted styles first, then folders in tree order), or the longest with the
/// `longestPrefixMatch` setting. Styles with tags disabled are skipped. A line starting with
/// `//` (or `//:`) continues the previous usable line's style.
pub fn parse(text: &str, cx: &ParseContext<'_>) -> Vec<Line> {
    let mut prefixes: Vec<(&str, &Id)> = Vec::new();
    for style in cx.library.tag_styles(cx.current_style, cx.settings.current_folder_tag_priority) {
        if style.prefixes_disabled() {
            continue;
        }
        for p in &style.prefixes {
            if !p.is_empty() {
                prefixes.push((p.as_str(), &style.id));
            }
        }
    }
    let marker_words = cx.settings.marker_words();
    let ignore_tags: Vec<&str> = cx.settings.ignore_tags.iter().map(String::as_str).filter(|t| !t.is_empty()).collect();

    let mut lines: Vec<Line> = Vec::new();
    if text.is_empty() {
        return lines;
    }
    let mut counter = 0u32;
    let mut last_text_line: Option<usize> = None;
    let mut previous_style: Option<Id> = None;
    for (raw_index, raw) in text.split('\n').enumerate() {
        let ignore_prefix = cx.settings.ignore_line_prefixes.iter().find(|p| raw.starts_with(p.as_str())).cloned().unwrap_or_default();
        let tag = if cx.settings.longest_prefix_match {
            prefixes.iter().filter(|(p, _)| raw.starts_with(p)).max_by_key(|(p, _)| p.len())
        } else {
            prefixes.iter().find(|(p, _)| raw.starts_with(p))
        };
        let (style_prefix, style) = if raw.starts_with("//") {
            (if raw.starts_with("//:") { "//:" } else { "//" }.to_string(), previous_style.clone())
        } else if let Some((p, id)) = tag {
            (p.to_string(), Some((*id).clone()))
        } else {
            (String::new(), None)
        };
        let used_style =
            cx.used_line_styles.get(&raw_index).filter(|u| u.raw_text == raw).map(|u| u.style_id.clone()).filter(|id| cx.library.style(id).is_some());

        let mut t = raw.replacen(ignore_prefix.as_str(), "", 1).replacen(style_prefix.as_str(), "", 1);
        if !ignore_tags.is_empty() {
            t = remove_tags(&t, &ignore_tags);
        }
        let t = js_trim(&t).to_string();
        let page = parse_page_marker_with(raw, &marker_words);
        let ignore = !ignore_prefix.is_empty() || t.is_empty() || page.is_some();
        if page.is_some()
            && cx.has_images
            && let Some(l) = last_text_line.and_then(|i| lines.get_mut(i))
        {
            l.last = true;
        }
        let (c, index) = next_line_number(counter, page.is_some(), ignore, cx.settings.reset_line_counter_on_page);
        counter = c;
        if !ignore {
            last_text_line = Some(lines.len());
            if let Some(s) = used_style.clone().or_else(|| style.clone()) {
                previous_style = Some(s);
            }
        }
        lines.push(Line { raw_text: raw.to_string(), raw_index, ignore_prefix, style_prefix, style, used_style, ignore, index, text: t, page, last: false });
    }
    lines
}

/// Remove every occurrence of the tags (first listed tag wins at a position), like TypeR's
/// alternation regex with the `g` flag.
fn remove_tags(s: &str, tags: &[&str]) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    'outer: while !rest.is_empty() {
        for t in tags {
            if let Some(after) = rest.strip_prefix(t) {
                rest = after;
                continue 'outer;
            }
        }
        let mut chars = rest.chars();
        if let Some(c) = chars.next() {
            out.push(c);
        }
        rest = chars.as_str();
    }
    out
}

/// Normalize pasted script text: `\r\n` and lone `\r` become `\n`.
pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// The page a line belongs to: the last page marker at or before it.
pub fn page_of(lines: &[Line], index: usize) -> Option<u32> {
    lines.get(..=index.min(lines.len().saturating_sub(1)))?.iter().rev().find_map(|l| l.page)
}

/// First usable line at or after `from`.
pub fn next_usable(lines: &[Line], from: usize) -> Option<usize> {
    lines.iter().skip(from).position(|l| !l.ignore).map(|p| p + from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Style;
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

    fn run(text: &str, lib: &Library, settings: &Settings, current: Option<&str>) -> Vec<Line> {
        let current = current.map(Id::str);
        let used = BTreeMap::new();
        parse(text, &ParseContext { library: lib, settings, current_style: current.as_ref(), used_line_styles: &used, has_images: false })
    }

    fn numbers(items: &[&str], reset: bool) -> Vec<u32> {
        let mut counter = 0;
        items
            .iter()
            .map(|item| {
                let (c, i) = next_line_number(counter, *item == "page", *item == "page" || *item == "ignore", reset);
                counter = c;
                i
            })
            .collect()
    }

    // scripts/testLineNumbering.js
    #[test]
    fn line_numbering_resets_per_page() {
        assert_eq!(numbers(&["line", "line", "page", "line", "line"], true), [1, 2, 0, 1, 2]);
        assert_eq!(numbers(&["line", "line", "page", "line", "line"], false), [1, 2, 0, 3, 4]);
        assert_eq!(numbers(&["line", "ignore", "line"], true), [1, 0, 2]);
    }

    #[test]
    fn numbering_through_the_parser() {
        let lib = Library::default();
        let mut s = Settings::default();
        let text = "a\nb\nPage 2\nc\nd";
        let idx: Vec<u32> = run(text, &lib, &s, None).iter().map(|l| l.index).collect();
        assert_eq!(idx, [1, 2, 0, 1, 2]);
        s.reset_line_counter_on_page = false;
        let idx: Vec<u32> = run(text, &lib, &s, None).iter().map(|l| l.index).collect();
        assert_eq!(idx, [1, 2, 0, 3, 4]);
    }

    // scripts/testWorkspaceNavigation.js (reducer half): "Read Page 12 now" is dialogue.
    #[test]
    fn only_strict_markers_are_ignored() {
        let lines = run("Read Page 12 now\nPage 2\nDialogue", &Library::default(), &Settings::default(), None);
        assert!(!lines[0].ignore);
        assert!(lines[1].ignore);
        assert_eq!(lines[1].page, Some(2));
        assert_eq!(page_of(&lines, 2), Some(2));
        assert_eq!(page_of(&lines, 0), None);
    }

    #[test]
    fn spanish_markers_unless_turned_off() {
        let mut s = Settings::default();
        let text = "Página 1\nhola\nPág. 2\nLee la página 3 ahora";
        let pages: Vec<Option<u32>> = run(text, &Library::default(), &s, None).iter().map(|l| l.page).collect();
        assert_eq!(pages, [Some(1), None, Some(2), None]);
        s.extra_page_markers = false;
        let lines = run(text, &Library::default(), &s, None);
        assert!(lines.iter().all(|l| l.page.is_none() && !l.ignore), "with TypeR's rule they are dialogue");
    }

    #[test]
    fn ignore_prefixes_tags_and_empty_lines() {
        let s = Settings { ignore_tags: vec!["**".into(), "[TL]".into()], ..Settings::default() };
        let lines = run("## note\n   \nHello **there** [TL]\n##", &Library::default(), &s, None);
        assert!(lines[0].ignore && lines[0].ignore_prefix == "##");
        assert!(lines[1].ignore, "blank");
        assert!(!lines[2].ignore);
        assert_eq!(lines[2].text, "Hello there");
        assert!(lines[3].ignore);
    }

    // 2.5 behaviour: first match in style order, not the longest.
    #[test]
    fn first_prefix_in_style_order_wins() {
        let mut lib = Library { folders: vec![], styles: vec![st("short", None, &["S"]), st("long", None, &["SFX:"])] };
        lib.normalize();
        let mut s = Settings::default();
        let lines = run("SFX: BOOM", &lib, &s, Some("short"));
        assert_eq!(lines[0].style, Some(Id::str("short")));
        assert_eq!(lines[0].style_prefix, "S");
        assert_eq!(lines[0].text, "FX: BOOM");
        s.longest_prefix_match = true;
        let lines = run("SFX: BOOM", &lib, &s, Some("short"));
        assert_eq!(lines[0].style, Some(Id::str("long")));
        assert_eq!(lines[0].text, "BOOM");
    }

    #[test]
    fn disabled_prefixes_and_folder_isolation() {
        let mut lib = Library {
            folders: vec![
                crate::library::Folder { id: Id::str("a"), name: "A".into(), parent_id: None, order: 0.0 },
                crate::library::Folder { id: Id::str("b"), name: "B".into(), parent_id: None, order: 1.0 },
            ],
            styles: vec![st("a-reg", Some("a"), &["B:"]), st("b-reg", Some("b"), &["B:"])],
        };
        lib.normalize();
        let s = Settings::default();
        assert_eq!(run("B: hi", &lib, &s, Some("b-reg"))[0].style, Some(Id::str("b-reg")), "project B's tag wins in project B (#208)");
        assert_eq!(run("B: hi", &lib, &s, Some("a-reg"))[0].style, Some(Id::str("a-reg")));
        lib.toggle_prefixes(&Id::str("a-reg"));
        assert_eq!(run("B: hi", &lib, &s, Some("a-reg"))[0].style, None);
    }

    // 2.5 behaviour: `//` and `//:` continue the previous usable line's style.
    #[test]
    fn continuation_lines_take_the_previous_style() {
        let lib = Library { folders: vec![], styles: vec![st("sfx", None, &["SFX:"])] };
        let lines = run("SFX: BOOM\n## note\n//: and more\n// again\nplain", &lib, &Settings::default(), None);
        assert_eq!(lines[2].style, Some(Id::str("sfx")));
        assert_eq!(lines[2].text, "and more");
        assert_eq!(lines[3].style_prefix, "//");
        assert_eq!(lines[3].style, Some(Id::str("sfx")));
        assert_eq!(lines[4].style, None);
        let first = run("// orphan", &lib, &Settings::default(), None);
        assert_eq!(first[0].style, None);
        assert_eq!(first[0].text, "orphan");
    }

    #[test]
    fn used_styles_drive_continuations_while_text_is_unchanged() {
        let lib = Library { folders: vec![], styles: vec![st("a", None, &[]), st("b", None, &[])] };
        let mut used = BTreeMap::new();
        used.insert(0, UsedLineStyle { raw_text: "hello".into(), style_id: Id::str("b") });
        let s = Settings::default();
        let cx = ParseContext { library: &lib, settings: &s, current_style: None, used_line_styles: &used, has_images: false };
        let lines = parse("hello\n// more", &cx);
        assert_eq!(lines[0].used_style, Some(Id::str("b")));
        assert_eq!(lines[1].style, Some(Id::str("b")));
        let lines = parse("hello!\n// more", &cx);
        assert_eq!(lines[0].used_style, None, "edited line drops its used style");
    }

    // 2.5 behaviour: the last line of a page is marked when page files are synced.
    #[test]
    fn last_line_of_each_page_is_marked_with_images() {
        let lib = Library::default();
        let s = Settings::default();
        let used = BTreeMap::new();
        let cx = ParseContext { library: &lib, settings: &s, current_style: None, used_line_styles: &used, has_images: true };
        let lines = parse("Page 1\na\nb\n## x\nPage 2\nc", &cx);
        let last: Vec<bool> = lines.iter().map(|l| l.last).collect();
        assert_eq!(last, [false, false, true, false, false, false]);
        let cx = ParseContext { has_images: false, ..cx };
        assert!(parse("Page 1\na\nPage 2\nc", &cx).iter().all(|l| !l.last));
    }

    #[test]
    fn helpers() {
        assert_eq!(normalize_newlines("a\r\nb\rc"), "a\nb\nc");
        assert_eq!(remove_tags("a**b**c", &["**", "*"]), "abc");
        assert!(run("", &Library::default(), &Settings::default(), None).is_empty());
        let lines = run("## x\n\na", &Library::default(), &Settings::default(), None);
        assert_eq!(next_usable(&lines, 0), Some(2));
        assert_eq!(next_usable(&lines, 3), None);
    }
}
