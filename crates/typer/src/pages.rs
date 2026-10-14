// Ported from TypeR app_src/pageMarker.js and app_src/pageImageMapping.js (develop 71bc061), MIT,
// (c) 2017 Swirt. See crates/typer/LICENSE-TypeR.txt.

//! Page markers in the script (`Page 12`) and the page files they map to (N2, N4).

use std::cmp::Ordering;
use std::collections::HashMap;

/// The page number of a `Page N` marker line (TypeR 3.0's strict rule), else `None`.
///
/// The line is trimmed; a leading Markdown heading (`#`…`######` plus whitespace), trailing `#`s
/// and one pair of surrounding brackets are stripped. What remains must be `Page`, whitespace,
/// digits and optionally `:`/`-`/`–`/`—` followed by anything, or a single `.`. Case-insensitive.
/// N must be a safe integer above 0. "Read Page 12 now", "Page 0", "Page 3 notes" are not markers.
pub fn parse_page_marker(line: &str) -> Option<u32> {
    let mut h = js_trim(line);
    h = strip_heading_prefix(h);
    h = strip_heading_suffix(h);
    h = js_trim(h);
    if let Some(inner) = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        h = js_trim(inner);
    }
    let rest = strip_prefix_ignore_case(h, "page")?;
    let after_ws = rest.trim_start_matches(is_js_space);
    if after_ws.len() == rest.len() {
        return None; // `Page` must be followed by whitespace.
    }
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

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &s[prefix.len()..])
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
