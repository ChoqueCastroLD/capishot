// Ported from TypeR app_src/utils.js (parseMarkdownRuns) and app_src/markdownFormatting.js
// (develop 71bc061), MIT, (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Markdown bold/italic in script lines (S9) and the Ctrl+B / Ctrl+I toggle (S11).

use serde::{Deserialize, Serialize};

/// A styled piece of a line: the text without its Markdown markers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
}

/// A line split into runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Parsed {
    /// The text without markers and escapes.
    pub text: String,
    pub runs: Vec<Run>,
    /// Some run is bold or italic.
    pub has_formatting: bool,
}

struct Marker {
    token: &'static str,
    bold: bool,
    italic: bool,
}

/// Longest first, so `***` wins over `**` and `*` at the same index.
const MARKERS: [Marker; 6] = [
    Marker { token: "***", bold: true, italic: true },
    Marker { token: "___", bold: true, italic: true },
    Marker { token: "**", bold: true, italic: false },
    Marker { token: "__", bold: true, italic: false },
    Marker { token: "*", bold: false, italic: true },
    Marker { token: "_", bold: false, italic: true },
];

/// Nesting depth cap (each level consumes two markers, so real lines never get near it).
const MAX_DEPTH: usize = 64;

fn is_escaped(text: &str, index: usize) -> bool {
    let backslashes = text.as_bytes().get(..index).unwrap_or(&[]).iter().rev().take_while(|b| **b == b'\\').count();
    backslashes % 2 == 1
}

/// Byte index of the first unescaped `token` at or after `start`.
fn find_unescaped(text: &str, token: &str, start: usize) -> Option<usize> {
    let mut from = start;
    while from <= text.len() {
        let i = from + text.get(from..)?.find(token)?;
        if !is_escaped(text, i) {
            return Some(i);
        }
        from = i + 1;
    }
    None
}

fn next_marker(text: &str, start: usize) -> Option<(usize, &'static Marker)> {
    let mut best: Option<(usize, &'static Marker)> = None;
    for m in &MARKERS {
        let Some(i) = find_unescaped(text, m.token, start) else { continue };
        if best.is_none_or(|(bi, bm)| i < bi || (i == bi && m.token.len() > bm.token.len())) {
            best = Some((i, m));
        }
    }
    best
}

/// `\\` → `\`, `\*` → `*`, `\_` → `_`, applied in that order like TypeR.
fn unescape(s: &str) -> String {
    s.replace("\\\\", "\\").replace("\\*", "*").replace("\\_", "_")
}

/// Split `input` into bold/italic runs. Tokens: `***`/`___` (bold italic), `**`/`__` (bold),
/// `*`/`_` (italic); the longest token wins at the same index; `\*`, `\_` and `\\` escape; an
/// opener without a closer is literal text; nested runs combine their flags.
pub fn parse_runs(input: &str) -> Parsed {
    let mut runs: Vec<Run> = Vec::new();
    walk(input, false, false, &mut runs, 0);
    let text = runs.iter().map(|r| r.text.as_str()).collect();
    let has_formatting = runs.iter().any(|r| r.bold || r.italic);
    Parsed { text, runs, has_formatting }
}

fn push_run(runs: &mut Vec<Run>, segment: &str, bold: bool, italic: bool) {
    if segment.is_empty() {
        return;
    }
    let cleaned = unescape(segment);
    if cleaned.is_empty() {
        return;
    }
    match runs.last_mut() {
        Some(last) if last.bold == bold && last.italic == italic => last.text.push_str(&cleaned),
        _ => runs.push(Run { text: cleaned, bold, italic }),
    }
}

fn walk(segment: &str, bold: bool, italic: bool, runs: &mut Vec<Run>, depth: usize) {
    if depth > MAX_DEPTH {
        push_run(runs, segment, bold, italic);
        return;
    }
    let mut cursor = 0;
    while cursor < segment.len() {
        let Some((at, m)) = next_marker(segment, cursor) else {
            push_run(runs, segment.get(cursor..).unwrap_or(""), bold, italic);
            break;
        };
        if at > cursor {
            push_run(runs, segment.get(cursor..at).unwrap_or(""), bold, italic);
        }
        let after_open = at + m.token.len();
        let Some(close) = find_unescaped(segment, m.token, after_open) else {
            push_run(runs, segment.get(at..after_open).unwrap_or(""), bold, italic);
            cursor = after_open;
            continue;
        };
        walk(segment.get(after_open..close).unwrap_or(""), bold || m.bold, italic || m.italic, runs, depth + 1);
        cursor = close + m.token.len();
    }
}

/// `\`, `*` and `_` escaped so the text reads literally.
pub fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('*', "\\*").replace('_', "\\_")
}

/// Which toggle [`format_selection`] applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Bold,
    Italic,
}

/// Result of [`format_selection`]: the new text and selection, in **character** indices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Formatted {
    pub text: String,
    pub selection_start: usize,
    pub selection_end: usize,
}

fn marker_count_around(text: &[char], start: usize, end: usize, ch: char) -> usize {
    let mut before = 0;
    let mut after = 0;
    while before < 3 && start > before && text.get(start - before - 1) == Some(&ch) {
        before += 1;
    }
    while after < 3 && text.get(end + after) == Some(&ch) {
        after += 1;
    }
    before.min(after)
}

fn marker_count_inside(text: &[char], start: usize, end: usize, ch: char) -> usize {
    let mut opening = 0;
    let mut closing = 0;
    while opening < 3 && start + opening < end && text.get(start + opening) == Some(&ch) {
        opening += 1;
    }
    while closing < 3 && end > closing && end - closing > start && text.get(end - closing - 1) == Some(&ch) {
        closing += 1;
    }
    let count = opening.min(closing);
    if end - start > count * 2 { count } else { 0 }
}

/// Toggle bold or italic Markdown around the selection `start..end` (character indices) of
/// `text`, detecting `*`/`_` markers just outside or just inside the selection. An empty
/// selection changes nothing. Indices are clamped to the text.
pub fn format_selection(text: &str, start: usize, end: usize, format: Format) -> Formatted {
    let chars: Vec<char> = text.chars().collect();
    let start = start.min(chars.len());
    let end = end.min(chars.len()).max(start);
    if start == end {
        return Formatted { text: text.to_string(), selection_start: start, selection_end: end };
    }

    let mut ch = '*';
    let mut count = marker_count_around(&chars, start, end, ch);
    let mut inside = false;
    if count == 0 {
        let underscores = marker_count_around(&chars, start, end, '_');
        if underscores > 0 {
            ch = '_';
            count = underscores;
        }
    }
    if count == 0 {
        count = marker_count_inside(&chars, start, end, ch);
        inside = count > 0;
        if count == 0 {
            let underscores = marker_count_inside(&chars, start, end, '_');
            if underscores > 0 {
                ch = '_';
                count = underscores;
                inside = true;
            }
        }
    }

    let (bold, italic) = (count >= 2, count == 1 || count >= 3);
    let (bold, italic) = match format {
        Format::Bold => (!bold, italic),
        Format::Italic => (bold, !italic),
    };
    let next_count = match (bold, italic) {
        (true, true) => 3,
        (true, false) => 2,
        (false, true) => 1,
        (false, false) => 0,
    };
    let marker: String = std::iter::repeat_n(ch, next_count).collect();
    let slice = |a: usize, b: usize| -> String { chars.get(a..b).map(|s| s.iter().collect()).unwrap_or_default() };
    let selected = if inside { slice(start + count, end - count) } else { slice(start, end) };
    let selected_len = selected.chars().count();

    if count > 0 {
        let outer_start = if inside { start } else { start - count };
        let outer_end = if inside { end } else { end + count };
        let text = format!("{}{marker}{selected}{marker}{}", slice(0, outer_start), slice(outer_end, chars.len()));
        return Formatted { text, selection_start: outer_start + next_count, selection_end: outer_start + next_count + selected_len };
    }
    let text = format!("{}{marker}{selected}{marker}{}", slice(0, start), slice(end, chars.len()));
    Formatted { text, selection_start: start + next_count, selection_end: end + next_count }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(s: &str) -> Vec<(String, bool, bool)> {
        parse_runs(s).runs.into_iter().map(|r| (r.text, r.bold, r.italic)).collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        let p = parse_runs("Hello there");
        assert_eq!(p.text, "Hello there");
        assert!(!p.has_formatting);
        assert_eq!(runs("Hello"), [("Hello".into(), false, false)]);
        assert!(parse_runs("").runs.is_empty());
    }

    #[test]
    fn bold_italic_and_longest_token() {
        assert_eq!(runs("a **b** c"), [("a ".into(), false, false), ("b".into(), true, false), (" c".into(), false, false)]);
        assert_eq!(runs("*i*"), [("i".into(), false, true)]);
        assert_eq!(runs("***bi***"), [("bi".into(), true, true)]);
        assert_eq!(runs("__b__ _i_"), [("b".into(), true, false), (" ".into(), false, false), ("i".into(), false, true)]);
    }

    #[test]
    fn nested_runs_or_their_flags() {
        assert_eq!(runs("**bold *both* bold**"), [("bold ".into(), true, false), ("both".into(), true, true), (" bold".into(), true, false)]);
    }

    #[test]
    fn unmatched_openers_are_literal_and_escapes_work() {
        assert_eq!(parse_runs("2 * 3").text, "2 * 3");
        assert_eq!(parse_runs("a \\*b\\* c").text, "a *b* c");
        assert!(!parse_runs("a \\*b\\* c").has_formatting);
        assert_eq!(parse_runs("back\\\\slash").text, "back\\slash");
        assert_eq!(parse_runs("snake\\_case").text, "snake_case");
        let p = parse_runs("¡**Hola**, niño!");
        assert_eq!(p.text, "¡Hola, niño!");
        assert!(p.has_formatting);
    }

    #[test]
    fn escape_round_trips() {
        let s = "a*b_c\\d";
        assert_eq!(parse_runs(&escape(s)).text, s);
    }

    #[test]
    fn deep_nesting_does_not_overflow() {
        let s = format!("{}x{}", "*_".repeat(2000), "_*".repeat(2000));
        let _ = parse_runs(&s);
    }

    fn f(text: &str, s: usize, e: usize, fmt: Format) -> (String, usize, usize) {
        let r = format_selection(text, s, e, fmt);
        (r.text, r.selection_start, r.selection_end)
    }

    // scripts/testMarkdownFormatting.js
    #[test]
    fn format_selection_matches_typer() {
        assert_eq!(f("hello", 0, 5, Format::Bold), ("**hello**".into(), 2, 7));
        assert_eq!(f("**hello**", 2, 7, Format::Italic), ("***hello***".into(), 3, 8));
        assert_eq!(f("***hello***", 3, 8, Format::Bold), ("*hello*".into(), 1, 6));
        assert_eq!(f("_hello_", 1, 6, Format::Italic), ("hello".into(), 0, 5));
        assert_eq!(f("**hello**", 0, 9, Format::Italic), ("***hello***".into(), 3, 8));
        assert_eq!(f("*hello*", 0, 7, Format::Italic), ("hello".into(), 0, 5));
        assert_eq!(f("hello", 2, 2, Format::Bold), ("hello".into(), 2, 2));
    }

    #[test]
    fn format_selection_clamps_and_handles_unicode() {
        assert_eq!(f("niño", 0, 99, Format::Bold), ("**niño**".into(), 2, 6));
        assert_eq!(f("abc", 9, 1, Format::Italic), ("abc".into(), 3, 3));
    }
}
