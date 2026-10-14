// Ported from TypeR app_src/pageMarker.js and app_src/pageImageMapping.js (develop 71bc061), MIT,
// (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Page markers in the script (`Page 12`) and the page files they map to (N2, N4).

use std::cmp::Ordering;
use std::collections::HashMap;

/// Marker words Capishot recognises besides TypeR's `Page` by default: Spanish and Portuguese
/// (`Página 12`, `Pagina 12`, `Pág. 12`, `Pag 12`). Accents and a trailing `.` are optional
/// when matching, so these two cover every spelling.
pub const DEFAULT_EXTRA_PAGE_WORDS: &[&str] = &["Página", "Pág."];

/// Longest marker word accepted (characters), and most words in the list.
pub const MAX_PAGE_WORD_CHARS: usize = 32;
pub const MAX_PAGE_WORDS: usize = 24;

/// The words a page marker may start with, folded once for matching (see [`fold`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkerWords {
    /// Folded words, `page` first; the longest first among the rest so `pag.` is tried
    /// before `pag`.
    words: Vec<Word>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Word {
    /// Folded, without a trailing `.`.
    stem: Vec<char>,
    /// The word ends in `.` (an abbreviation): the dot is optional and no space is needed
    /// after it (`Pág.12`).
    abbrev: bool,
}

impl MarkerWords {
    /// TypeR's rule: `Page` only.
    pub fn typer() -> MarkerWords {
        MarkerWords::new(std::iter::empty::<&str>())
    }

    /// `Page` plus `extra` (blank entries and entries with digits are skipped).
    pub fn new<'a>(extra: impl IntoIterator<Item = &'a str>) -> MarkerWords {
        let mut words = vec![Word { stem: "page".chars().collect(), abbrev: false }];
        let mut rest: Vec<Word> = Vec::new();
        for w in extra.into_iter().take(MAX_PAGE_WORDS) {
            let w = js_trim(w);
            if w.is_empty() || w.chars().count() > MAX_PAGE_WORD_CHARS || w.chars().any(|c| c.is_ascii_digit()) {
                continue;
            }
            let abbrev = w.ends_with('.');
            let stem: Vec<char> = fold(w.trim_end_matches('.')).chars().collect();
            if stem.is_empty() {
                continue;
            }
            let word = Word { stem, abbrev };
            if !words.contains(&word) && !rest.contains(&word) {
                rest.push(word);
            }
        }
        rest.sort_by(|a, b| b.stem.len().cmp(&a.stem.len()).then(b.abbrev.cmp(&a.abbrev)));
        words.extend(rest);
        MarkerWords { words }
    }

    /// The rest of `s` after a marker word and the space (or the abbreviation's dot) that must
    /// follow it, else `None`.
    fn strip<'a>(&self, s: &'a str) -> Option<&'a str> {
        self.words.iter().find_map(|w| w.strip(s))
    }
}

impl Word {
    fn strip<'a>(&self, s: &'a str) -> Option<&'a str> {
        let mut chars = s.char_indices();
        let mut end = 0;
        for want in &self.stem {
            let (i, c) = chars.next()?;
            let mut folded = fold_char(c);
            if folded.next() != Some(*want) || folded.next().is_some() {
                return None;
            }
            end = i + c.len_utf8();
        }
        let rest = s.get(end..)?;
        if self.abbrev
            && let Some(after_dot) = rest.strip_prefix('.')
        {
            return Some(after_dot);
        }
        // The word must end here: whitespace follows (`Page12` and `Páginas 3` are no markers).
        rest.starts_with(is_js_space).then_some(rest)
    }
}

/// Lowercase and without accents (`Página` → `pagina`), for matching marker words.
pub fn fold(s: &str) -> String {
    s.chars().flat_map(fold_char).collect()
}

/// One character lowercased with Latin diacritics removed (Spanish, Portuguese, French…).
fn fold_char(c: char) -> impl Iterator<Item = char> {
    c.to_lowercase().map(|l| match l {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' => 'a',
        'ç' | 'ć' | 'č' => 'c',
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ę' | 'ě' => 'e',
        'ì' | 'í' | 'î' | 'ï' | 'ī' => 'i',
        'ñ' | 'ń' | 'ň' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ō' | 'ø' => 'o',
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' => 'u',
        'ý' | 'ÿ' => 'y',
        other => other,
    })
}

/// The page number of a `Page N` marker line (TypeR 3.0's strict rule), else `None`.
///
/// The line is trimmed; a leading Markdown heading (`#`…`######` plus whitespace), trailing `#`s
/// and one pair of surrounding brackets are stripped. What remains must be `Page`, whitespace,
/// digits and optionally `:`/`-`/`–`/`—` followed by anything, or a single `.`. Case-insensitive.
/// N must be a safe integer above 0. "Read Page 12 now", "Page 0", "Page 3 notes" are not markers.
pub fn parse_page_marker(line: &str) -> Option<u32> {
    parse_page_marker_with(line, &MarkerWords::typer())
}

/// [`parse_page_marker`] with other marker words (`Página 12`, `Pág. 12`): the same whole-line
/// rule, case- and accent-insensitive.
pub fn parse_page_marker_with(line: &str, words: &MarkerWords) -> Option<u32> {
    let mut h = js_trim(line);
    h = strip_heading_prefix(h);
    h = strip_heading_suffix(h);
    h = js_trim(h);
    if let Some(inner) = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        h = js_trim(inner);
    }
    let rest = words.strip(h)?;
    let after_ws = rest.trim_start_matches(is_js_space);
    let digits_len = after_ws.bytes().take_while(u8::is_ascii_digit).count();
    if digits_len == 0 {
        return None;
    }
    let (digits, tail) = after_ws.split_at(digits_len);
    let tail = tail.trim_start_matches(is_js_space);
    let tail_ok = tail.is_empty() || tail == "." || tail.starts_with([':', '-', '\u{2013}', '\u{2014}']);
    if !tail_ok {
        return None;
    }
    // Number.isSafeInteger(page) && page > 0; anything above u32 is no real page either.
    let n: u64 = digits.parse().ok()?;
    if n == 0 || n > 9_007_199_254_740_991 {
        return None;
    }
    u32::try_from(n).ok()
}

/// `^#{1,6}\s+` removed.
fn strip_heading_prefix(s: &str) -> &str {
    let hashes = s.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return s;
    }
    let rest = &s[hashes..];
    let trimmed = rest.trim_start_matches(is_js_space);
    if trimmed.len() == rest.len() { s } else { trimmed }
}

/// `\s+#+$` removed.
fn strip_heading_suffix(s: &str) -> &str {
    let without_hashes = s.trim_end_matches('#');
    if without_hashes.len() == s.len() {
        return s;
    }
    let trimmed = without_hashes.trim_end_matches(is_js_space);
    if trimmed.len() == without_hashes.len() { s } else { trimmed }
}

/// JavaScript's `\s` / `trim()` whitespace: Unicode White_Space plus the BOM.
pub fn is_js_space(c: char) -> bool {
    c.is_whitespace() || c == '\u{feff}'
}

/// JavaScript's `String.prototype.trim`.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// A page file's number: the **last** digit run of its base name (`chap016_023.psd` → 23), else
/// `None`. Folders and the extension are ignored.
pub fn image_page_number(name: &str) -> Option<u32> {
    let file = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let base = match file.rfind('.') {
        Some(dot) => &file[..dot],
        None => file,
    };
    let mut last: Option<&str> = None;
    let mut start: Option<usize> = None;
    for (i, c) in base.char_indices() {
        match (c.is_ascii_digit(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                last = base.get(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        last = base.get(s..);
    }
    let n: u64 = last?.parse().ok()?;
    if n == 0 || n > 9_007_199_254_740_991 {
        return None;
    }
    u32::try_from(n).ok()
}

/// Page number → index into `images`, by the number in each file name. A number used by two
/// files is ambiguous and maps to nothing (the positional fallback then applies).
pub fn page_image_lookup(images: &[String]) -> HashMap<u32, Option<usize>> {
    let mut map: HashMap<u32, Option<usize>> = HashMap::new();
    for (i, name) in images.iter().enumerate() {
        let Some(n) = image_page_number(name) else { continue };
        map.entry(n).and_modify(|e| *e = None).or_insert(Some(i));
    }
    map
}

/// The index of the file for `page`: the file whose name carries that number, else the
/// `page`-th file (TypeR 2.5's positional rule), else `None`.
pub fn image_for_page(images: &[String], page: u32) -> Option<usize> {
    image_for_page_with(images, page, &page_image_lookup(images))
}

/// [`image_for_page`] with a lookup built once by [`page_image_lookup`].
pub fn image_for_page_with(images: &[String], page: u32, lookup: &HashMap<u32, Option<usize>>) -> Option<usize> {
    if page == 0 {
        return None;
    }
    if let Some(Some(i)) = lookup.get(&page) {
        return Some(*i);
    }
    let i = usize::try_from(page - 1).ok()?;
    (i < images.len()).then_some(i)
}

/// Natural ("numeric") order of file names, as TypeR sorts the picked page files
/// (`localeCompare(…, {numeric: true})`): digit runs compare by value, other text
/// case-insensitively, ties by the raw string.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let take = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut s = String::new();
                    while let Some(ch) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(ch);
                        it.next();
                    }
                    s
                };
                let (m, n) = (take(&mut x), take(&mut y));
                let (m, n) = (m.trim_start_matches('0'), n.trim_start_matches('0'));
                let ord = m.len().cmp(&n.len()).then_with(|| m.cmp(n));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(c), Some(d)) => {
                let ord = c.to_lowercase().cmp(d.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                x.next();
                y.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // scripts/testWorkspaceNavigation.js
    #[test]
    fn page_marker_accept_and_reject_lists() {
        assert_eq!(parse_page_marker(" Page 12 "), Some(12));
        assert_eq!(parse_page_marker("page\t003"), Some(3));
        for heading in [
            "Page 12 :",
            "Page 12:",
            "Page 12 -",
            "Page 12.",
            "[Page 12]",
            "## Page 12",
            "### Page 12 ###",
            "Page 12 : Title",
            "Page 12 - End",
            "Page 12 \u{2013} Title",
        ] {
            assert_eq!(parse_page_marker(heading), Some(12), "{heading:?}");
        }
        for text in ["Read Page 12 now", "Page 0", "Page 3 notes", "Page 9007199254740992"] {
            assert_eq!(parse_page_marker(text), None, "{text:?}");
        }
    }

    #[test]
    fn page_marker_edge_cases() {
        assert_eq!(parse_page_marker("PAGE 7 — fin"), Some(7));
        assert_eq!(parse_page_marker("####### Page 2"), None, "seven hashes are not a heading");
        assert_eq!(parse_page_marker("#Page 2"), None, "a heading needs a space");
        assert_eq!(parse_page_marker("Page12"), None);
        assert_eq!(parse_page_marker("Page 4.."), None);
        assert_eq!(parse_page_marker(""), None);
        assert_eq!(parse_page_marker("Página 3"), None);
        assert_eq!(parse_page_marker("[ Page 5 ]"), Some(5));
    }

    fn es_pt() -> MarkerWords {
        MarkerWords::new(DEFAULT_EXTRA_PAGE_WORDS.iter().copied())
    }

    // TypeR's accept list in Spanish and Portuguese, plus the abbreviations.
    #[test]
    fn spanish_and_portuguese_markers() {
        let w = es_pt();
        for (line, n) in [
            ("Página 12", 12),
            ("PÁGINA 12", 12),
            ("pagina\t003", 3),
            ("Pagina 12", 12),
            ("Pág. 12", 12),
            ("pág.12", 12),
            ("PAG 12", 12),
            ("Pag. 12", 12),
            ("Pág 12", 12),
            ("[Página 12]", 12),
            ("[ Pág. 12 ]", 12),
            ("## Página 12", 12),
            ("### Pág. 12 ###", 12),
            ("Página 12:", 12),
            ("Página 12 : Título", 12),
            ("Página 12 - Fin", 12),
            ("Página 12 \u{2014} Final", 12),
            ("Página 12.", 12),
            ("  Página 7  ", 7),
            ("Page 5", 5),
            ("page 5", 5),
        ] {
            assert_eq!(parse_page_marker_with(line, &w), Some(n), "{line:?}");
        }
        for line in [
            "Lee la página 12 ahora",
            "Leia a página 12 agora",
            "Read Page 12 now",
            "Página 0",
            "Pág. 0",
            "Página 3 notas",
            "Pág. 3 notas",
            "Página 3 de 20",
            "Página 9007199254740992",
            "Página12",
            "Pag12",
            "Páginas 3",
            "Paginación 3",
            "Página",
            "Pág.",
            "Pág..3",
            "#Página 2",
            "####### Página 2",
            "Página 4..",
        ] {
            assert_eq!(parse_page_marker_with(line, &w), None, "{line:?}");
        }
        // TypeR's own rule knows only "Page".
        assert_eq!(parse_page_marker("Página 12"), None);
        assert_eq!(parse_page_marker_with("Página 12", &MarkerWords::typer()), None);
    }

    #[test]
    fn marker_word_lists() {
        let w = MarkerWords::new(["  Folha ", "", "p4", "Pg.", "Página", "PÁGINA"]);
        assert_eq!(parse_page_marker_with("Folha 3", &w), Some(3));
        assert_eq!(parse_page_marker_with("folha 3", &w), Some(3));
        assert_eq!(parse_page_marker_with("Pg.3", &w), Some(3));
        assert_eq!(parse_page_marker_with("Pg 3", &w), Some(3));
        assert_eq!(parse_page_marker_with("Page 3", &w), Some(3), "Page is always a marker word");
        assert_eq!(parse_page_marker_with("p4 3", &w), None, "words with digits are skipped");
        assert_eq!(w.words.len(), 4, "duplicates after folding count once");
        assert_eq!(fold("PÁGINA Ñandú"), "pagina nandu");
        let many: Vec<String> = (0..100).map(|i| format!("w{}", "x".repeat(i))).collect();
        assert!(MarkerWords::new(many.iter().map(String::as_str)).words.len() <= MAX_PAGE_WORDS + 1);
        assert_eq!(MarkerWords::new(["x".repeat(MAX_PAGE_WORD_CHARS + 1).as_str()]), MarkerWords::typer());
    }

    // scripts/testPageImageMapping.js
    #[test]
    fn image_page_numbers_and_lookup() {
        assert_eq!(image_page_number("chap016_023.psd"), Some(23));
        assert_eq!(image_page_number("page-24-final.psd"), Some(24));
        assert_eq!(image_page_number("cover.psd"), None);
        assert_eq!(image_page_number("C:\\chapter\\12\\cover.psd"), None, "folders don't count");

        let mut pages: Vec<String> = (1..=20).map(|i| format!("chap016_{i:03}.psd")).collect();
        pages.push("chap016_023.psd".into());
        pages.push("chap016_024.psd".into());
        assert_eq!(image_for_page(&pages, 23).map(|i| pages[i].as_str()), Some("chap016_023.psd"));
        assert_eq!(image_for_page(&pages, 24).map(|i| pages[i].as_str()), Some("chap016_024.psd"));

        let unnamed = vec!["cover.psd".to_string(), "credits.psd".to_string()];
        assert_eq!(image_for_page(&unnamed, 2), Some(1), "names without numbers keep positional matching");

        let dup = vec!["page-001.psd".to_string(), "page-001-final.psd".to_string()];
        assert_eq!(image_for_page(&dup, 1), Some(0), "duplicate numbers keep positional matching");
        assert_eq!(image_for_page(&dup, 0), None);
        assert_eq!(image_for_page(&dup, 9), None);
    }

    #[test]
    fn natural_sort_orders_numbers_by_value() {
        let mut v = vec!["p10.psd", "p2.psd", "P1.psd", "p02b.psd"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["P1.psd", "p2.psd", "p02b.psd", "p10.psd"]);
    }
}
