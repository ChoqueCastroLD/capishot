//! TextShapeR (D4): line-break variants of a text shaped like its speech bubble.
//!
//! Ported from TypeR `app_src/textShapeR.js` (develop 71bc061), MIT License, Copyright (c) 2017
//! Swirt; see `crates/typer/LICENSE-TypeR.txt`. The generator, DP split, scoring, silhouette
//! gates and bubble fit are TypeR's, with its neutral tuning (no learned style: TypeR's
//! "Learn" feedback, D5, is not ported). Capishot's changes:
//!
//! - **Widths are measured** ([`Measure`]): with a real font measurer the text's own advances
//!   replace TypeR's character-class estimates. Units stay TypeR's (about one average
//!   character), so its constants keep their meaning: one unit is the text's measured width
//!   over its character-class width ([`Measure::calibrate`]). [`CharClass`] is TypeR's own
//!   estimate (one unit per average character, 0.45 per space).
//! - **Hyphenation is language-aware**: Liang patterns for Spanish, Portuguese and English
//!   (`photocraft_text::hyphen`), TypeR's French syllable rules for French.
//! - **The bubble is measured in pixels**: the unit and line height come from the layout
//!   ([`Calibration`]) instead of being read back from a rendered layer.

use std::collections::{HashMap, HashSet};

use photocraft_text::hyphen::{self, HyphLang};
use serde::{Deserialize, Serialize};

use crate::bubble::Row;

/// The most variants returned.
pub const MAX_VARIANTS: usize = 12;
/// Longest text shaped (bytes).
pub const MAX_TEXT: usize = 4000;
/// Most words shaped: the DP grid grows with the square of the word count.
pub const MAX_WORDS: usize = 160;

/// Fraction of the bubble kept as breathing room between text and outline.
const FIT_MARGIN: f64 = 0.9;
/// Fraction of a line's leading its glyphs cover: the band the outline must hold.
const FIT_GLYPH_BAND: f64 = 0.8;
/// Lines packed at 100 % of their row capacity overflow on the slightest error.
const FIT_CAPACITY_SLACK: f64 = 1.12;
/// Blocks read best when slightly taller than the bubble's own proportions.
const FIT_ASPECT_LEAN: f64 = 1.15;
/// Lines wider than this ratio of their target never survive scoring.
const DP_WIDTH_PRUNE_RATIO: f64 = 2.5;
const CLEAN_SILHOUETTE_BADNESS: f64 = 0.6;
const BEARABLE_SILHOUETTE_BADNESS: f64 = 1.3;
/// Dehyphenation accepts every syllable boundary up to the weak vowel-vowel ones.
const CESURE_MAX_PENALTY: f64 = 8.0;

/// TypeR's shape presets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Profile {
    #[default]
    Balanced,
    Round,
    Tall,
    Wide,
}

impl Profile {
    pub fn parse(s: &str) -> Option<Profile> {
        match s {
            "balanced" => Some(Profile::Balanced),
            "round" => Some(Profile::Round),
            "tall" => Some(Profile::Tall),
            "wide" => Some(Profile::Wide),
            _ => None,
        }
    }

    fn preset(self) -> &'static Preset {
        match self {
            Profile::Balanced => &BALANCED,
            Profile::Round => &ROUND,
            Profile::Tall => &TALL,
            Profile::Wide => &WIDE,
        }
    }
}

#[derive(Debug)]
struct Preset {
    min_lines: usize,
    max_lines: usize,
    line_target: usize,
    curves: &'static [f64],
    shifts: &'static [f64],
    biases: &'static [f64],
    min_weight: f64,
    punctuation_break_bonus: f64,
    adjacent_slack: f64,
    smoothness_weight: f64,
    min_line_ratio: f64,
    score_curve: f64,
    max_line_width: f64,
}

static BALANCED: Preset = Preset {
    min_lines: 2,
    max_lines: 6,
    line_target: 4,
    curves: &[0.35, 0.45, 0.6, 0.75],
    shifts: &[0.0, -0.12, 0.12, -0.2, 0.2],
    biases: &[-1.0, 0.0, 1.0],
    min_weight: 0.35,
    punctuation_break_bonus: 14.0,
    adjacent_slack: 0.22,
    smoothness_weight: 210.0,
    min_line_ratio: 0.4,
    score_curve: 0.65,
    max_line_width: 26.0,
};
static ROUND: Preset = Preset {
    min_lines: 3,
    max_lines: 5,
    line_target: 4,
    curves: &[0.55, 0.65, 0.8, 0.95],
    shifts: &[0.0, -0.08, 0.08, -0.16, 0.16],
    biases: &[-1.0, 0.0, 1.0],
    min_weight: 0.3,
    punctuation_break_bonus: 14.0,
    adjacent_slack: 0.18,
    smoothness_weight: 240.0,
    min_line_ratio: 0.38,
    score_curve: 0.8,
    max_line_width: 22.0,
};
static TALL: Preset = Preset {
    min_lines: 4,
    max_lines: 7,
    line_target: 5,
    curves: &[0.24, 0.3, 0.42, 0.55],
    shifts: &[0.0, -0.1, 0.1, -0.18, 0.18],
    biases: &[-1.0, 0.0, 1.0],
    min_weight: 0.46,
    punctuation_break_bonus: 11.0,
    adjacent_slack: 0.26,
    smoothness_weight: 160.0,
    min_line_ratio: 0.36,
    score_curve: 0.5,
    max_line_width: 18.0,
};
static WIDE: Preset = Preset {
    min_lines: 2,
    max_lines: 4,
    line_target: 3,
    curves: &[0.18, 0.22, 0.34, 0.46],
    shifts: &[0.0, -0.08, 0.08, -0.16, 0.16],
    biases: &[-1.0, 0.0, 1.0, 2.0],
    min_weight: 0.58,
    punctuation_break_bonus: 11.0,
    adjacent_slack: 0.2,
    smoothness_weight: 220.0,
    min_line_ratio: 0.5,
    score_curve: 0.4,
    max_line_width: 34.0,
};

/// Measures text advances. Units are free: [`Measure::calibrate`] turns them into TypeR units.
pub trait Measure {
    /// Advance width of one line of plain text (Markdown markers already removed).
    fn width(&mut self, text: &str) -> f64;

    /// `(size of one TypeR unit, width of a word space in TypeR units)` for `text` (plain).
    /// The default: the text's measured width over its character-class width, so a unit is the
    /// text's own average character; the space is measured between two letters.
    fn calibrate(&mut self, text: &str) -> (f64, f64) {
        let classes = char_class_width(text);
        let measured = self.width(text);
        let unit = if classes > 0.0 && measured.is_finite() && measured > 0.0 { measured / classes } else { 1.0 };
        let space = (self.width("n n") - self.width("nn")) / unit;
        (unit, if space.is_finite() && space > 0.0 { space } else { 0.45 })
    }
}

/// TypeR's own width estimate: character classes in units of an average character.
#[derive(Clone, Copy, Debug, Default)]
pub struct CharClass;

impl Measure for CharClass {
    fn width(&mut self, text: &str) -> f64 {
        char_class_width(text)
    }

    fn calibrate(&mut self, _: &str) -> (f64, f64) {
        (1.0, 0.45)
    }
}

/// TypeR's `getCharWidth` summed over `text`.
pub fn char_class_width(text: &str) -> f64 {
    text.chars()
        .map(|c| {
            if c.is_whitespace() || "ilI.,;:!|'’".contains(c) {
                0.45
            } else if "mwMW@#%&".contains(c) {
                1.32
            } else if c.is_ascii_uppercase() || "ÀÂÄÉÈÊËÎÏÔÖÙÛÜ".contains(c) {
                1.12
            } else {
                1.0
            }
        })
        .sum()
}

/// Pixel calibration: px per TypeR unit and px per line (the leading).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Calibration {
    /// Pixels per unit; `None` = the measurer's unit (a measurer in pixels).
    pub unit_px: Option<f64>,
    pub line_px: f64,
}

/// How words may be hyphenated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hyphenation {
    /// Liang patterns (Photoshop-like): Spanish, Portuguese, English.
    Patterns(HyphLang),
    /// TypeR's French syllable rules.
    French,
}

impl Hyphenation {
    /// From a language tag (`es`, `pt-BR`, `en`, `fr`…); `None` for a language without rules.
    pub fn from_tag(tag: &str) -> Option<Hyphenation> {
        if tag.trim().to_ascii_lowercase().starts_with("fr") {
            return Some(Hyphenation::French);
        }
        HyphLang::from_tag(tag).map(Hyphenation::Patterns)
    }

    /// The tag's rules, or a guess from the text (Spanish, Portuguese or English) without one.
    pub fn for_text(tag: Option<&str>, text: &str) -> Option<Hyphenation> {
        match tag {
            Some(t) if !t.trim().is_empty() => Hyphenation::from_tag(t),
            _ => Some(Hyphenation::Patterns(hyphen::guess_language(text))),
        }
    }
}

/// What to shape and how.
#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub profile: Profile,
    pub limit: usize,
    pub max_lines: Option<usize>,
    /// Word-break rules; `None` = never hyphenate.
    pub hyphenation: Option<Hyphenation>,
    /// Merge hyphens left over from earlier breaks ("silho- uette") before shaping.
    pub dehyphenate: bool,
    /// Bubble box (px).
    pub width: Option<f64>,
    pub height: Option<f64>,
    /// Bubble outline rows, normalized to the box (D3).
    pub rows: Vec<Row>,
    pub calibration: Option<Calibration>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            profile: Profile::Balanced,
            limit: MAX_VARIANTS,
            max_lines: None,
            hyphenation: Some(Hyphenation::Patterns(HyphLang::En)),
            dehyphenate: false,
            width: None,
            height: None,
            rows: Vec::new(),
            calibration: None,
        }
    }
}

/// One proposed line breaking.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variant {
    pub lines: Vec<String>,
    /// Hyphenated words (0 or 1).
    pub hyphens: usize,
    /// Every line fits the bubble (`None` without a calibrated bubble).
    pub fits: Option<bool>,
    /// Lower is better.
    pub score: f64,
    /// Silhouette badness (TypeR's absolute quality gate; ≤ 0.6 is clean).
    pub badness: f64,
    /// Line widths in pixels with a calibrated bubble, else in TypeR units.
    pub widths: Vec<f64>,
}

impl Variant {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

// ---------------------------------------------------------------------------------------------
// Text and tokens

fn strip_markdown(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if matches!(chars.peek(), Some('\\' | '*' | '_')) => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '*' | '_' => {}
            _ => out.push(c),
        }
    }
    out
}

fn visible_len(text: &str) -> usize {
    strip_markdown(text).chars().count()
}

fn is_line_end_punct(c: char) -> bool {
    ".,;:!?…".contains(c)
}

fn ends_with_break_punctuation(text: &str) -> bool {
    let clean = strip_markdown(text);
    let clean = clean.trim_end_matches([')', '"', '\'', '’', '»', ']']).trim();
    clean.chars().next_back().is_some_and(is_line_end_punct)
}

fn is_punct_only(text: &str) -> bool {
    let s = strip_markdown(text);
    !s.is_empty() && s.chars().all(is_line_end_punct)
}

const MARKDOWN_TOKENS: [&str; 5] = ["***", "**", "__", "*", "_"];

fn is_escaped(b: &[u8], i: usize) -> bool {
    let mut n = 0;
    let mut j = i;
    while j > 0 && b.get(j - 1) == Some(&b'\\') {
        n += 1;
        j -= 1;
    }
    n % 2 == 1
}

fn find_closing(text: &str, token: &str, start: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut from = start;
    while let Some(off) = text.get(from..).and_then(|t| t.find(token)) {
        let i = from + off;
        if !is_escaped(b, i) {
            return Some(i);
        }
        from = i + token.len();
    }
    None
}

/// Words split on whitespace, with Markdown spans kept whole.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let b = text.as_bytes();
    let mut i = 0;
    while i < text.len() {
        while let Some(c) = text.get(i..).and_then(|t| t.chars().next()).filter(|c| c.is_whitespace()) {
            i += c.len_utf8();
        }
        if i >= text.len() {
            break;
        }
        let token = MARKDOWN_TOKENS.iter().find(|t| text.get(i..).is_some_and(|s| s.starts_with(**t)) && !is_escaped(b, i));
        if let Some(t) = token
            && let Some(close) = find_closing(text, t, i + t.len())
        {
            let end = close + t.len();
            if let Some(w) = text.get(i..end) {
                words.push(w.to_string());
            }
            i = end;
            continue;
        }
        let mut j = i;
        while let Some(c) = text.get(j..).and_then(|t| t.chars().next()).filter(|c| !c.is_whitespace()) {
            j += c.len_utf8();
        }
        if let Some(w) = text.get(i..j) {
            words.push(w.to_string());
        }
        i = j.max(i + 1);
    }
    words.retain(|w| !w.is_empty());
    words
}

fn closing_only(w: &str) -> bool {
    let s = strip_markdown(w);
    !s.is_empty() && s.chars().all(|c| "!?;:.,…»›)]}".contains(c))
}

fn opening_only(w: &str) -> bool {
    let s = strip_markdown(w);
    !s.is_empty() && s.chars().all(|c| "«‹¿¡([{".contains(c))
}

/// Closing punctuation never starts a line; opening punctuation never ends one.
fn merge_punctuation(words: Vec<String>) -> Vec<String> {
    let mut closed: Vec<String> = Vec::new();
    for w in words {
        if let Some(last) = closed.last_mut()
            && closing_only(&w)
        {
            last.push(' ');
            last.push_str(&w);
            continue;
        }
        closed.push(w);
    }
    let mut merged = Vec::new();
    let mut it = closed.into_iter().peekable();
    while let Some(mut w) = it.next() {
        while opening_only(&w) {
            match it.next() {
                Some(n) => {
                    w.push(' ');
                    w.push_str(&n);
                }
                None => break,
            }
        }
        merged.push(w);
    }
    merged
}

fn normalize(text: &str, opts: &Options) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if opts.dehyphenate { dehyphenate(&collapsed, opts.hyphenation) } else { collapsed }
}

/// "silho- uette" → "silhouette" when the junction is a plausible syllable boundary of the
/// merged word; compounds ("court- circuit", "Jean- Paul") keep their hyphen.
fn dehyphenate(text: &str, rules: Option<Hyphenation>) -> String {
    if !text.contains("- ") {
        return text.to_string();
    }
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while let Some(w) = words.get(i) {
        let mut cur = (*w).to_string();
        while let (Some(stem), Some(next)) = (cur.strip_suffix('-'), words.get(i + 1)) {
            let before_tail: String = stem.chars().rev().take_while(|c| c.is_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
            let after_head: String = next.chars().take_while(|c| c.is_alphabetic()).collect();
            if before_tail.is_empty() || after_head.is_empty() || after_head.chars().next().is_some_and(char::is_uppercase) {
                break;
            }
            let merged = format!("{before_tail}{after_head}");
            let at = before_tail.chars().count();
            let plausible = match rules {
                Some(Hyphenation::French) | None => french_split_penalty(&merged, at) < CESURE_MAX_PENALTY,
                Some(Hyphenation::Patterns(l)) => {
                    let byte = merged.char_indices().nth(at).map_or(merged.len(), |(b, _)| b);
                    let r = hyphen::Rules { min_word: 2, before: 1, after: 1, limit: 0, zone_pt: 0.0 };
                    hyphen::break_points(&merged, l, &r).contains(&byte)
                }
            };
            if !plausible {
                break;
            }
            cur = format!("{stem}{next}");
            i += 1;
        }
        out.push(cur);
        i += 1;
    }
    out.join(" ")
}

// ---------------------------------------------------------------------------------------------
// Hyphenation candidates

const FRENCH_VOWELS: &str = "aeiouyAEIOUYàâäéèêëîïôöùûüÀÂÄÉÈÊËÎÏÔÖÙÛÜ";
const FRENCH_ONSETS: [&str; 25] =
    ["bl", "br", "ch", "cl", "cr", "dr", "fl", "fr", "gl", "gn", "gr", "kl", "kr", "ph", "pl", "pr", "rh", "th", "tr", "vr", "chl", "chr", "phl", "phr", "thr"];

fn fr_vowel(c: Option<char>) -> bool {
    c.is_some_and(|c| FRENCH_VOWELS.contains(c))
}

fn fr_letter(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_ascii_alphabetic() || "àâäéèêëîïôöùûüÀÂÄÉÈÊËÎÏÔÖÙÛÜ".contains(c))
}

fn fr_consonant(c: Option<char>) -> bool {
    fr_letter(c) && !fr_vowel(c)
}

/// TypeR's French syllable split penalty at char `index` of `body` (0–1.5 clean, 6–7 merge
/// only, 10+ implausible, infinite without a vowel on a side).
fn french_split_penalty(body: &str, index: usize) -> f64 {
    let lower: Vec<char> = body.to_lowercase().chars().collect();
    let at = |i: usize| lower.get(i).copied();
    let (left, right) = (lower.get(..index).unwrap_or(&[]), lower.get(index..).unwrap_or(&[]));
    if !left.iter().any(|c| fr_vowel(Some(*c))) || !right.iter().any(|c| fr_vowel(Some(*c))) {
        return f64::INFINITY;
    }
    let prev = index.checked_sub(1).and_then(at);
    let cur = at(index);
    if fr_vowel(prev) && fr_vowel(cur) {
        return 7.0;
    }
    if fr_consonant(prev) && fr_vowel(cur) {
        return 16.0;
    }
    if cur == Some('x') && fr_vowel(at(index + 1)) {
        return 12.0;
    }
    let pair: String = [prev, cur].iter().flatten().collect();
    if fr_consonant(prev) && FRENCH_ONSETS.contains(&pair.as_str()) {
        return 7.0;
    }
    let mut start = index;
    while start > 0 && fr_consonant(at(start - 1)) {
        start -= 1;
    }
    let mut end = index;
    while end < lower.len() && fr_consonant(at(end)) {
        end += 1;
    }
    let onset: String = lower.get(index..end).unwrap_or(&[]).iter().collect();
    let coda = index - start;
    let rest: String = lower.get(end..).unwrap_or(&[]).iter().collect();
    if rest == "e" || rest == "es" {
        return 6.0;
    }
    if onset == "h" {
        return 6.0;
    }
    if coda >= 2 {
        return 10.0;
    }
    if onset.chars().count() == 1 {
        return if coda == 0 { 0.0 } else { 1.0 };
    }
    if FRENCH_ONSETS.contains(&onset.as_str()) {
        return if coda == 0 { 0.5 } else { 1.5 };
    }
    10.0
}

/// A hyphenated split of a word: the head (with its hyphen) and the tail.
#[derive(Clone, Debug, PartialEq)]
struct Split {
    prefix: String,
    suffix: String,
}

/// Up to three hyphen splits of `word`, best first (TypeR: body of 8+ characters, split 3..len-3,
/// ranked by syllable quality then closeness to the middle).
fn hyphen_splits(word: &str, rules: Hyphenation) -> Vec<Split> {
    // Body and trailing [.,;:!?] punctuation; leading opening punctuation stays on the head.
    let trimmed = word.trim_end_matches(['.', ',', ';', ':', '!', '?']);
    let punctuation = word.get(trimmed.len()..).unwrap_or("");
    let (lead, body) = match rules {
        Hyphenation::French => ("", trimmed),
        Hyphenation::Patterns(_) => {
            let i = trimmed.char_indices().find(|(_, c)| c.is_alphabetic()).map_or(trimmed.len(), |(i, _)| i);
            (trimmed.get(..i).unwrap_or(""), trimmed.get(i..).unwrap_or(""))
        }
    };
    let n = body.chars().count();
    if n < 8 || body.contains(['*', '_', '\\', '-', '\'', '’']) || body.chars().any(char::is_whitespace) || lead.contains(['*', '_', '\\']) {
        return Vec::new();
    }
    let byte = |ci: usize| body.char_indices().nth(ci).map_or(body.len(), |(b, _)| b);
    let mut positions: Vec<(f64, usize)> = Vec::new();
    match rules {
        Hyphenation::French => {
            for i in 3..=n.saturating_sub(3) {
                let p = french_split_penalty(body, i);
                if p.is_finite() && p < 3.0 {
                    positions.push((p * 10.0 + (i as f64 - n as f64 / 2.0).abs(), i));
                }
            }
        }
        Hyphenation::Patterns(lang) => {
            let r = hyphen::Rules { min_word: 8, before: 3, after: 3, limit: 0, zone_pt: 0.0 };
            for b in hyphen::break_points(body, lang, &r) {
                let i = body.get(..b).map_or(0, |s| s.chars().count());
                positions.push(((i as f64 - n as f64 / 2.0).abs(), i));
            }
        }
    }
    positions.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    positions
        .into_iter()
        .take(3)
        .filter_map(|(_, i)| {
            let (h, t) = (body.get(..byte(i))?, body.get(byte(i)..)?);
            Some(Split { prefix: format!("{lead}{h}-"), suffix: format!("{t}{punctuation}") })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Measuring

struct Meter<'a> {
    m: &'a mut dyn Measure,
    unit: f64,
    space: f64,
    cache: HashMap<String, f64>,
}

impl Meter<'_> {
    /// Width of `text` (Markdown allowed) in TypeR units.
    fn units(&mut self, text: &str) -> f64 {
        if let Some(w) = self.cache.get(text) {
            return *w;
        }
        let w = self.m.width(&strip_markdown(text)) / self.unit;
        let w = if w.is_finite() { w.max(0.0) } else { 0.0 };
        self.cache.insert(text.to_string(), w);
        w
    }
}

#[derive(Clone, Debug)]
struct Token {
    text: String,
    width: f64,
    break_after: bool,
}

/// Prefix sums and flags over a token set.
struct TokenSet {
    id: usize,
    tokens: Vec<Token>,
    prefix: Vec<f64>,
    ends_break: Vec<bool>,
    punct_only: Vec<bool>,
    break_count: Vec<usize>,
}

impl TokenSet {
    fn new(id: usize, tokens: Vec<Token>) -> TokenSet {
        let mut prefix = vec![0.0];
        let mut break_count = vec![0];
        let mut ends_break = Vec::with_capacity(tokens.len());
        let mut punct_only = Vec::with_capacity(tokens.len());
        for t in &tokens {
            prefix.push(prefix.last().copied().unwrap_or(0.0) + t.width);
            break_count.push(break_count.last().copied().unwrap_or(0) + usize::from(t.break_after));
            ends_break.push(ends_with_break_punctuation(&t.text));
            punct_only.push(is_punct_only(&t.text));
        }
        TokenSet { id, tokens, prefix, ends_break, punct_only, break_count }
    }

    fn len(&self) -> usize {
        self.tokens.len()
    }

    fn total(&self) -> f64 {
        self.prefix.last().copied().unwrap_or(0.0)
    }
}

// ---------------------------------------------------------------------------------------------
// Targets and the DP split

fn build_weights(n: usize, curve: f64, shift: f64, min_weight: f64) -> Vec<f64> {
    if n <= 1 {
        return vec![1.0];
    }
    let center = (n as f64 - 1.0) / 2.0 + shift;
    let max_distance = center.max(n as f64 - 1.0 - center);
    let max_distance = if max_distance == 0.0 { 1.0 } else { max_distance };
    (0..n).map(|i| (1.0 - (i as f64 - center).abs() / max_distance * curve).max(min_weight)).collect()
}

fn build_targets(set: &TokenSet, n: usize, curve: f64, shift: f64, bias: f64, min_weight: f64) -> Vec<f64> {
    let total = set.total() + set.len().saturating_sub(n) as f64;
    let w = build_weights(n, curve, shift, min_weight);
    let wt: f64 = w.iter().sum();
    let wt = if wt == 0.0 { 1.0 } else { wt };
    w.iter().map(|w| (total * w / wt + bias).max(1.0)).collect()
}

/// The token ranges of the best split of `set` into `n` lines aiming at `targets`.
fn build_candidate(set: &TokenSet, n: usize, targets: &[f64], space: f64) -> Option<Vec<(usize, usize)>> {
    let tc = set.len();
    if tc == 0 || n < 1 || tc < n {
        return None;
    }
    let stride = tc + 1;
    let mut dp = vec![f64::INFINITY; (n + 1) * stride];
    let mut prev = vec![usize::MAX; (n + 1) * stride];
    *dp.get_mut(0)? = 0.0;
    for li in 0..n {
        let target = targets.get(li).copied().filter(|t| *t != 0.0 && !t.is_nan()).unwrap_or(1.0);
        let prune = target * (1.0 + DP_WIDTH_PRUNE_RATIO);
        let last = li == n - 1;
        let row = li * stride;
        let next_row = row + stride;
        let max_to = tc - (n - li - 1);
        for from in li..=tc {
            let base = *dp.get(row + from)?;
            if base == f64::INFINITY {
                continue;
            }
            let fp = *set.prefix.get(from)?;
            let fb = *set.break_count.get(from)?;
            for to in from + 1..=max_to {
                let width = set.prefix.get(to)? - fp + space * (to - from - 1) as f64;
                if width > prune && to > from + 1 {
                    break;
                }
                if set.break_count.get(to - 1)? - fb > 0 {
                    continue;
                }
                let ratio = (width - target) / target;
                let mut cost = ratio * ratio;
                if !last && *set.ends_break.get(to - 1)? {
                    cost = if cost > 0.09 { cost - 0.09 } else { 0.0 };
                }
                if to - from == 1 && *set.punct_only.get(from)? {
                    cost += 3.0;
                }
                let next = base + cost;
                if next < *dp.get(next_row + to)? {
                    *dp.get_mut(next_row + to)? = next;
                    *prev.get_mut(next_row + to)? = from;
                }
            }
        }
    }
    if *dp.get(n * stride + tc)? == f64::INFINITY {
        return None;
    }
    let mut ranges = Vec::with_capacity(n);
    let mut cursor = tc;
    for li in (1..=n).rev() {
        let from = *prev.get(li * stride + cursor)?;
        if from == usize::MAX {
            return None;
        }
        ranges.push((from, cursor));
        cursor = from;
    }
    ranges.reverse();
    Some(ranges)
}

// ---------------------------------------------------------------------------------------------
// Bubble fit

#[derive(Clone, Debug)]
struct Fit {
    unit_px: f64,
    line_px: f64,
    width: f64,
    height: f64,
    rows: Option<Vec<Row>>,
}

fn normalize_rows(rows: &[Row]) -> Vec<Row> {
    let mut out: Vec<Row> = rows
        .iter()
        .filter(|r| r.y.is_finite() && r.width.is_finite())
        .map(|r| Row { y: r.y.clamp(0.0, 1.0), left: r.left.clamp(0.0, 1.0), right: r.right.clamp(0.0, 1.0), width: r.width.clamp(0.0, 1.0) })
        .collect();
    out.sort_by(|a, b| a.y.total_cmp(&b.y));
    out
}

fn width_at(rows: &[Row], y: f64) -> Option<f64> {
    let first = rows.first()?;
    let last = rows.last()?;
    if y <= first.y {
        return Some(first.width);
    }
    if y >= last.y {
        return Some(last.width);
    }
    for w in rows.windows(2) {
        let (p, n) = (w.first()?, w.get(1)?);
        if y > n.y {
            continue;
        }
        let span = if n.y - p.y == 0.0 { 1.0 } else { n.y - p.y };
        return Some(p.width + (n.width - p.width) * (y - p.y) / span);
    }
    Some(last.width)
}

fn fit_line_y(i: usize, n: usize, fit: &Fit) -> f64 {
    0.5 + (i as f64 + 0.5 - n as f64 / 2.0) * (fit.line_px / fit.height)
}

/// Units available to line `i` of `n` inside the outline (the narrowest point of its band).
fn fit_available(i: usize, n: usize, fit: &Fit) -> f64 {
    let Some(rows) = &fit.rows else { return fit.width * FIT_MARGIN / fit.unit_px };
    let yc = fit_line_y(i, n, fit);
    let half = fit.line_px / fit.height * (FIT_GLYPH_BAND / 2.0);
    let mut row = f64::INFINITY;
    for y in [yc - half, yc, yc + half] {
        if let Some(w) = width_at(rows, y.clamp(0.0, 1.0))
            && w < row
        {
            row = w;
        }
    }
    if !row.is_finite() {
        row = 0.0;
    }
    fit.width * row.max(0.0) * FIT_MARGIN / fit.unit_px
}

fn estimate_fit_line_count(total: f64, fit: &Fit) -> usize {
    let by_height = ((fit.height * FIT_MARGIN / fit.line_px).floor().max(1.0)) as usize;
    let limit = by_height.min(8);
    let bubble_aspect = fit.height / fit.width.max(1.0);
    let (mut best, mut best_cost, mut tightest) = (0usize, f64::INFINITY, 0usize);
    for n in 1..=limit {
        let (mut capacity, mut widest) = (0.0f64, 0.0f64);
        for i in 0..n {
            let u = fit_available(i, n, fit);
            capacity += u;
            widest = widest.max(u);
        }
        if capacity < total {
            continue;
        }
        if tightest == 0 {
            tightest = n;
        }
        let fill = total / capacity;
        let widest_px = (widest * fill * fit.unit_px).max(1.0);
        let block_aspect = n as f64 * fit.line_px / widest_px;
        let mut cost = (block_aspect / (bubble_aspect * FIT_ASPECT_LEAN)).ln().abs();
        cost += (fill * FIT_CAPACITY_SLACK - 1.0).max(0.0) * 6.0;
        if cost < best_cost {
            best_cost = cost;
            best = n;
        }
    }
    if best > 0 {
        best
    } else if tightest > 0 {
        tightest
    } else {
        limit
    }
}

fn estimate_manual_line_count(word_count: usize, width: f64, height: f64) -> usize {
    if word_count <= 2 {
        return 1;
    }
    let aspect = (height.max(1.0) / width.max(1.0)).clamp(0.35, 2.4);
    let max_lines = word_count.clamp(1, 8);
    ((word_count as f64 * aspect * 1.5).sqrt().round() as usize).min(max_lines).max(2)
}

/// Manual "selection" targets: the outline width at each line's height (softness 1, floor 0.14).
fn selection_targets(set: &TokenSet, n: usize, rows: &[Row], space: f64) -> Vec<f64> {
    let floor = 0.14;
    let w: Vec<f64> = (0..n)
        .map(|i| {
            let y = if n <= 1 { 0.5 } else { (i as f64 + 0.5) / n as f64 };
            width_at(rows, y).unwrap_or(0.0).max(floor)
        })
        .collect();
    let total = set.total() + set.len().saturating_sub(n) as f64 * space;
    let wt: f64 = w.iter().sum();
    let peak = total / if wt == 0.0 { 1.0 } else { wt };
    w.iter().map(|w| (w * peak).max(1.0)).collect()
}

// ---------------------------------------------------------------------------------------------
// Scoring

/// The profile a candidate is scored against.
#[derive(Clone, Debug)]
struct Scoring {
    line_target: f64,
    line_target_weight: f64,
    max_line_width: f64,
    min_weight: f64,
    score_curve: f64,
    punctuation_break_bonus: f64,
    adjacent_slack: f64,
    smoothness_weight: f64,
    min_line_ratio: f64,
    shape_rows: Option<Vec<Row>>,
    fit: Option<Fit>,
}

fn max_of(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::NEG_INFINITY, f64::max)
}

fn min_of(v: &[f64]) -> f64 {
    v.iter().copied().fold(f64::INFINITY, f64::min)
}

fn first_index_of(v: &[f64], x: f64) -> usize {
    v.iter().position(|y| *y == x).unwrap_or(0)
}

fn silhouette_badness(lengths: &[f64], width_cap: f64) -> f64 {
    let n = lengths.len();
    let max = max_of(lengths);
    let mut bad = 0.0;
    if width_cap > 0.0 {
        for l in lengths {
            bad += (l / width_cap - 1.0).max(0.0) * 3.0;
        }
    }
    if n < 2 || max.is_nan() || max <= 0.0 {
        return bad;
    }
    let (slack, weight) = if n == 2 { (0.32, 6.0) } else { (0.48, 5.0) };
    for w in lengths.windows(2) {
        let (a, b) = (w[0], w[1]);
        let longer = a.max(b).max(1.0);
        bad += ((b - a).abs() / longer - slack).max(0.0) * weight;
    }
    if n >= 3 {
        let interior = max_of(&lengths[1..n - 1]);
        let edge = lengths[0].max(lengths[n - 1]);
        if edge > interior {
            bad += (edge - interior) / max * 2.5;
        }
        let peak = first_index_of(lengths, max);
        for i in 1..=peak {
            let drop = (lengths[i - 1] - lengths[i]) / max;
            if drop > 0.18 {
                bad += (drop - 0.18) * 2.0;
            }
        }
        for i in peak + 1..n {
            let rise = (lengths[i] - lengths[i - 1]) / max;
            if rise > 0.18 {
                bad += (rise - 0.18) * 2.0;
            }
        }
        let min_ratio = min_of(lengths) / max;
        if min_ratio < 0.3 {
            bad += (0.3 - min_ratio) * 2.0;
        }
    }
    bad
}

fn silhouette_aesthetics(lengths: &[f64], adjacent_slack: Option<f64>, smoothness: f64, min_line_ratio: f64) -> f64 {
    let n = lengths.len();
    let max = max_of(lengths);
    let min = min_of(lengths);
    if max.is_nan() || max <= 0.0 || n < 2 {
        return 0.0;
    }
    if n == 2 {
        let slack = adjacent_slack.unwrap_or(0.24);
        let excess = ((lengths[0] - lengths[1]).abs() / max - slack).max(0.0);
        return excess * excess * smoothness;
    }
    let slack = adjacent_slack.unwrap_or(0.3);
    let min_ratio = min / max;
    let interior = max_of(&lengths[1..n - 1]);
    let mut score = 0.0;
    let edge = lengths[0].max(lengths[n - 1]);
    if edge > interior {
        score += ((edge - interior) / max).powi(2) * 420.0 + 20.0;
    }
    if interior > 0.0 {
        for e in [lengths[0], lengths[n - 1]] {
            let x = (e / interior - 0.85).max(0.0);
            score += x * x * 900.0;
        }
    }
    let peak = first_index_of(lengths, max);
    for i in 1..=peak {
        let drop = lengths[i - 1] - lengths[i];
        if drop > 0.0 {
            score += (drop / max).powi(2) * 300.0;
        }
    }
    for i in peak + 1..n {
        let rise = lengths[i] - lengths[i - 1];
        if rise > 0.0 {
            score += (rise / max).powi(2) * 300.0;
        }
    }
    let center = (n as f64 - 1.0) / 2.0;
    if center > 0.0 {
        let off = (peak as f64 - center).abs() / center;
        score += off * off * 60.0;
    }
    for w in lengths.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = (b - a).abs() / max;
        let excess = (d - slack).max(0.0);
        score += excess * excess * smoothness;
        score += (d - 0.38).max(0.0) * 280.0;
        let local = (b - a).abs() / a.max(b).max(1.0);
        let le = (local - 0.45).max(0.0);
        score += le * le * 700.0;
    }
    let ed = (lengths[0] - lengths[n - 1]).abs() / max;
    score += (ed - 0.22).max(0.0).powi(2) * 240.0;
    if min_ratio < min_line_ratio {
        score += (min_line_ratio - min_ratio).powi(2) * 260.0;
    }
    score
}

/// A candidate's lines: their text, widths (units) and visible lengths.
struct Lines {
    texts: Vec<String>,
    widths: Vec<f64>,
    chars: Vec<usize>,
}

fn score_candidate(lines: &Lines, hyphens: usize, p: &Scoring) -> f64 {
    let lengths = &lines.widths;
    let n = lengths.len();
    let total: f64 = lengths.iter().sum();
    let shape_rows = p.shape_rows.as_ref().filter(|r| !r.is_empty());
    let targets: Vec<f64> = if let Some(rows) = shape_rows {
        let w: Vec<f64> = (0..n)
            .map(|i| match &p.fit {
                Some(f) => fit_available(i, n, f).max(0.12),
                None => width_at(rows, (i as f64 + 0.5) / n as f64).unwrap_or(0.0).max(0.12),
            })
            .collect();
        let wt: f64 = w.iter().sum();
        let wt = if wt == 0.0 { 1.0 } else { wt };
        w.iter().map(|w| total * w / wt).collect()
    } else {
        let w = build_weights(n, p.score_curve, 0.0, p.min_weight);
        let wt: f64 = w.iter().sum();
        let wt = if wt == 0.0 { 1.0 } else { wt };
        w.iter().map(|w| total * w / wt).collect()
    };
    let max = max_of(lengths);
    let delta = n as f64 - p.line_target;
    let factor = if delta < 0.0 { 3.0 } else { 1.0 };
    let mut score = hyphens as f64 * 130.0 + delta.abs().powf(1.5) * p.line_target_weight * factor;
    if let Some(fit) = &p.fit {
        let he = (n as f64 * fit.line_px / (fit.height * FIT_MARGIN) - 1.0).max(0.0);
        if he > 0.0 {
            score += 80.0 + he * he * 900.0;
        }
        for (i, l) in lengths.iter().enumerate() {
            let avail = fit_available(i, n, fit);
            if avail <= 0.0 {
                score += 120.0;
                continue;
            }
            let ex = (l / avail - 1.0).max(0.0);
            if ex > 0.0 {
                score += 60.0 + ex * ex * 1600.0;
            }
        }
    }
    for (i, l) in lengths.iter().enumerate() {
        let t = targets.get(i).copied().filter(|t| *t != 0.0 && !t.is_nan()).unwrap_or(1.0);
        let rel = (l - t) / t;
        score += rel * rel * 120.0;
        let we = (l - p.max_line_width).max(0.0) / p.max_line_width;
        score += we * we * 320.0;
        if *l <= 1.0 {
            score += 30.0;
        }
        if n > 3 && lines.chars.get(i).is_some_and(|c| *c <= 4) {
            score += 8.0;
        }
        if lines.texts.get(i).is_some_and(|t| !t.is_empty() && t.chars().all(|c| ".,;:!?".contains(c))) {
            score += 40.0;
        }
        if i + 1 < n && lines.texts.get(i).is_some_and(|t| ends_with_break_punctuation(t)) {
            score -= p.punctuation_break_bonus;
        }
    }
    if shape_rows.is_some() && max > 0.0 && n > 1 {
        for i in 1..n {
            let step_err = ((lengths[i] - lengths[i - 1]) - (targets[i] - targets[i - 1])) / max;
            score += step_err * step_err * p.smoothness_weight;
        }
        score += silhouette_aesthetics(lengths, Some(p.adjacent_slack), p.smoothness_weight, p.min_line_ratio * 0.85);
        return score;
    }
    score + silhouette_aesthetics(lengths, Some(p.adjacent_slack), p.smoothness_weight, p.min_line_ratio)
}

// ---------------------------------------------------------------------------------------------
// The generator

struct Candidate {
    lines: Vec<String>,
    score: f64,
    hyphens: usize,
    fits: Option<bool>,
    badness: f64,
}

struct Gen<'a, 'm> {
    meter: &'a mut Meter<'m>,
    results: HashMap<String, Candidate>,
    seen: HashSet<String>,
}

impl Gen<'_, '_> {
    fn lines_of(&mut self, set: &TokenSet, ranges: &[(usize, usize)]) -> Lines {
        let mut texts = Vec::with_capacity(ranges.len());
        let mut widths = Vec::with_capacity(ranges.len());
        let mut chars = Vec::with_capacity(ranges.len());
        for &(a, b) in ranges {
            let toks = set.tokens.get(a..b).unwrap_or(&[]);
            let text = toks.iter().map(|t| t.text.as_str()).collect::<Vec<_>>().join(" ");
            let text = text.trim().to_string();
            let w: f64 = toks.iter().map(|t| t.width).sum::<f64>() + self.meter.space * toks.len().saturating_sub(1) as f64;
            chars.push(visible_len(&text));
            texts.push(text);
            widths.push(w);
        }
        Lines { texts, widths, chars }
    }

    fn add(&mut self, set: &TokenSet, n: usize, targets: Vec<f64>, hyphens: usize, scoring: &Scoring) {
        if set.len() == 0 || set.len() < n {
            return;
        }
        let mut sig = format!("{}:{}", set.id, n);
        for t in &targets {
            sig.push(',');
            sig.push_str(&((t * 4.0).round() as i64).to_string());
        }
        if !self.seen.insert(sig) {
            return;
        }
        let Some(ranges) = build_candidate(set, n, &targets, self.meter.space) else { return };
        let lines = self.lines_of(set, &ranges);
        let key = lines.texts.join("\n");
        if key.is_empty() || self.results.contains_key(&key) {
            return;
        }
        let score = score_candidate(&lines, hyphens, scoring);
        self.results.insert(key, Candidate { lines: lines.texts, score, hyphens, fits: None, badness: 0.0 });
    }
}

/// TextShapeR variants for `text`, best first (at most `opts.limit`).
pub fn shape(text: &str, opts: &Options, measure: &mut dyn Measure) -> Vec<Variant> {
    if text.len() > MAX_TEXT {
        return Vec::new();
    }
    let normalized = normalize(text, opts);
    if normalized.is_empty() {
        return Vec::new();
    }
    let words = merge_punctuation(split_words(&normalized));
    if words.is_empty() || words.len() > MAX_WORDS {
        return Vec::new();
    }
    let (unit, space) = measure.calibrate(&strip_markdown(&normalized));
    let unit = if unit.is_finite() && unit > 0.0 { unit } else { 1.0 };
    let mut meter = Meter { m: measure, unit, space, cache: HashMap::new() };
    let preset = opts.profile.preset();
    let limit = opts.limit.clamp(1, 64);
    let total_units = meter.units(&normalized);
    let mut widest_word = 1.0f64;
    for w in &words {
        widest_word = widest_word.max(meter.units(w));
    }
    let granularity_max = ((total_units / (widest_word * 0.72)).floor().clamp(1.0, 8.0)) as usize;
    let shape_rows: Vec<Row> = normalize_rows(&opts.rows).into_iter().filter(|r| r.width > 0.0).collect();
    let (bw, bh) = (opts.width.filter(|w| w.is_finite() && *w > 0.0), opts.height.filter(|h| h.is_finite() && *h > 0.0));
    let fit = match (opts.calibration, bw, bh) {
        (Some(c), Some(w), Some(h)) => {
            let unit_px = c.unit_px.unwrap_or(unit);
            (unit_px.is_finite() && unit_px > 0.0 && c.line_px.is_finite() && c.line_px > 0.0).then(|| Fit {
                unit_px,
                line_px: c.line_px,
                width: w,
                height: h,
                rows: (shape_rows.len() > 1).then(|| shape_rows.clone()),
            })
        }
        _ => None,
    };
    let aspect = match (bw, bh) {
        (Some(w), Some(h)) => Some((h / w).clamp(0.25, 3.0)),
        _ => None,
    };
    let tall = aspect.is_some_and(|a| a >= 1.15);
    let wide = aspect.is_some_and(|a| a <= 0.85);
    let stretch = aspect.map_or(1.0, |a| a.sqrt().clamp(0.7, 1.45));
    let mut scoring = Scoring {
        line_target: preset.line_target as f64,
        line_target_weight: 16.0,
        max_line_width: preset.max_line_width,
        min_weight: preset.min_weight,
        score_curve: preset.score_curve,
        punctuation_break_bonus: preset.punctuation_break_bonus,
        adjacent_slack: preset.adjacent_slack,
        smoothness_weight: preset.smoothness_weight,
        min_line_ratio: preset.min_line_ratio,
        shape_rows: None,
        fit: None,
    };
    if aspect.is_none() && shape_rows.len() <= 1 {
        let volume = (total_units / 2.6).sqrt().round();
        let lo = preset.min_lines.min(granularity_max) as f64;
        let hi = preset.max_lines.min(granularity_max.max(1)) as f64;
        scoring.line_target = lo.max(hi.min(volume));
    }
    if aspect.is_some() && shape_rows.len() <= 1 {
        scoring.line_target = 1f64.max((granularity_max.clamp(1, 8) as f64).min((preset.line_target as f64 * stretch).round()));
        scoring.max_line_width = (preset.max_line_width / stretch).clamp(12.0, 40.0);
        scoring.line_target_weight = 12.0;
    }
    let manual_rows = shape_rows.len() > 1;
    if manual_rows {
        let estimated = match &fit {
            Some(f) => estimate_fit_line_count(total_units, f),
            None => estimate_manual_line_count(words.len(), bw.unwrap_or(320.0), bh.unwrap_or(280.0)),
        };
        scoring.shape_rows = Some(shape_rows.clone());
        scoring.line_target = (estimated.clamp(1, preset.max_lines.max(estimated))) as f64;
        scoring.line_target_weight = 24.0;
        scoring.max_line_width = (preset.max_line_width / stretch).clamp(12.0, 40.0);
    }
    if let Some(f) = &fit {
        let widest_row = f.rows.as_ref().map_or(1.0, |r| r.iter().map(|r| r.width).fold(0.0, f64::max));
        let absolute = f.width * widest_row * FIT_MARGIN / f.unit_px;
        scoring.max_line_width = scoring.max_line_width.min(absolute).clamp(6.0, 60.0);
        scoring.fit = Some(f.clone());
    }
    let base_min = if words.len() <= 2 { 1 } else { preset.min_lines };
    let mut min_lines = base_min.max(1).min(words.len().max(1));
    if manual_rows && fit.is_some() {
        min_lines = min_lines.min((scoring.line_target as usize).max(1));
    }
    let profile_max = if manual_rows { preset.max_lines.max(8.min(scoring.line_target as usize + 1)) } else { preset.max_lines };
    let mut max_lines = opts.max_lines.unwrap_or(profile_max).min(words.len().max(1));
    if tall {
        let extra = if aspect.is_some_and(|a| a >= 1.6) { 2 } else { 1 };
        max_lines = words.len().max(1).min((max_lines + extra).min(8));
    }
    if wide {
        min_lines = min_lines.saturating_sub(1).max(1);
    }
    max_lines = min_lines.max(max_lines.min(granularity_max));
    let mut params: Vec<&Preset> = vec![preset];
    if tall && opts.profile != Profile::Tall {
        params.push(&TALL);
    }
    if wide && opts.profile != Profile::Wide {
        params.push(&WIDE);
    }
    let base_tokens: Vec<Token> = words.iter().map(|w| Token { text: w.clone(), width: meter.units(w), break_after: false }).collect();
    let base = TokenSet::new(0, base_tokens.clone());
    let space = meter.space;
    let mut g = Gen { meter: &mut meter, results: HashMap::new(), seen: HashSet::new() };

    let add_shape = |g: &mut Gen, set: &TokenSet, n: usize, hyphens: usize, scoring: &Scoring| {
        if !manual_rows {
            return;
        }
        let targets: Vec<f64> = match &fit {
            Some(f) => {
                let avail: Vec<f64> = (0..n).map(|i| fit_available(i, n, f).max(1.0)).collect();
                let total = set.total() + set.len().saturating_sub(n) as f64 * space;
                let at: f64 = avail.iter().sum();
                let scale = (total / if at == 0.0 { 1.0 } else { at }).min(1.0);
                avail.iter().map(|u| (u * scale).max(1.0)).collect()
            }
            None => selection_targets(set, n, &shape_rows, space),
        };
        for bias in [-1.0, 0.0, 1.0] {
            let t: Vec<f64> = if bias == 0.0 { targets.clone() } else { targets.iter().map(|t| (t + bias).max(1.0)).collect() };
            g.add(set, n, t, hyphens, scoring);
        }
    };

    for n in min_lines..=max_lines {
        for p in &params {
            for &curve in p.curves {
                for &shift in p.shifts {
                    for &bias in p.biases {
                        let t = build_targets(&base, n, curve, shift, bias, p.min_weight);
                        g.add(&base, n, t, 0, &scoring);
                    }
                }
            }
        }
        add_shape(&mut g, &base, n, 0, &scoring);
    }

    if let Some(rules) = opts.hyphenation {
        let mut next_id = 1;
        for (wi, w) in words.iter().enumerate() {
            for split in hyphen_splits(w, rules) {
                let mut toks = Vec::with_capacity(base_tokens.len() + 1);
                for (i, t) in base_tokens.iter().enumerate() {
                    if i == wi {
                        toks.push(Token { width: g.meter.units(&split.prefix), text: split.prefix.clone(), break_after: true });
                        toks.push(Token { width: g.meter.units(&split.suffix), text: split.suffix.clone(), break_after: false });
                    } else {
                        toks.push(t.clone());
                    }
                }
                let set = TokenSet::new(next_id, toks);
                next_id += 1;
                let widest = set.tokens.iter().map(|t| t.width).fold(1.0, f64::max);
                let set_gran = ((total_units / (widest * 0.72)).floor().clamp(2.0, 8.0)) as usize;
                let hmax = opts.max_lines.unwrap_or(profile_max.max(max_lines)).min(set.len().max(1)).min(set_gran);
                for n in min_lines.max(2)..=hmax {
                    for p in &params {
                        let curves: Vec<f64> = if p.curves.len() > 2 { vec![p.curves[0], p.curves[2]] } else { p.curves.to_vec() };
                        for curve in curves {
                            for &shift in p.shifts.iter().take(3) {
                                let t = build_targets(&set, n, curve, shift, 0.0, p.min_weight);
                                g.add(&set, n, t, 1, &scoring);
                            }
                        }
                    }
                    add_shape(&mut g, &set, n, 1, &scoring);
                }
            }
        }
    }

    let mut variants: Vec<Candidate> = g.results.into_values().collect();
    if let Some(f) = &fit {
        for v in &mut variants {
            v.fits = Some(fits_bubble(&v.lines, f, &mut meter));
        }
        if variants.iter().filter(|v| v.fits == Some(true)).count() >= 2 {
            variants.retain(|v| v.fits == Some(true));
        }
    }
    let width_cap = scoring.max_line_width * 1.05;
    for v in &mut variants {
        let w: Vec<f64> = v.lines.iter().map(|l| meter.units(l)).collect();
        v.badness = silhouette_badness(&w, width_cap);
    }
    if variants.iter().any(|v| v.badness <= BEARABLE_SILHOUETTE_BADNESS) {
        variants.retain(|v| v.badness <= BEARABLE_SILHOUETTE_BADNESS);
    }
    if variants.iter().filter(|v| v.badness <= CLEAN_SILHOUETTE_BADNESS).count() >= 3.min(limit) {
        variants.retain(|v| v.badness <= CLEAN_SILHOUETTE_BADNESS);
    }
    let fitted = fit.is_some();
    let cmp = |a: &Candidate, b: &Candidate| {
        if fitted && a.fits != b.fits {
            return if a.fits == Some(true) { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater };
        }
        a.score.total_cmp(&b.score).then_with(|| a.lines.join("\n").cmp(&b.lines.join("\n")))
    };
    variants.sort_by(cmp);
    // Diversity: the best of each line count first, then the rest by score.
    let mut picked: Vec<usize> = Vec::new();
    let mut counts: HashSet<usize> = HashSet::new();
    for (i, v) in variants.iter().enumerate() {
        if picked.len() >= limit {
            break;
        }
        if counts.contains(&v.lines.len()) || (fitted && v.fits != Some(true)) {
            continue;
        }
        counts.insert(v.lines.len());
        picked.push(i);
    }
    for i in 0..variants.len() {
        if picked.len() >= limit {
            break;
        }
        if !picked.contains(&i) {
            picked.push(i);
        }
    }
    picked.sort_unstable();
    let unit_px = fit.as_ref().map(|f| f.unit_px);
    picked
        .into_iter()
        .filter_map(|i| variants.get(i))
        .map(|v| Variant {
            lines: v.lines.clone(),
            hyphens: v.hyphens,
            fits: v.fits,
            score: v.score,
            badness: v.badness,
            widths: v.lines.iter().map(|l| meter.units(l) * unit_px.unwrap_or(1.0)).collect(),
        })
        .collect()
}

/// Measures with Capishot's text layout: point text in `style` at `dpi`, in pixels.
pub struct TextMeasure<'a> {
    pub engine: &'a mut photocraft_text::TextEngine,
    pub style: photocraft_doc::text::CharStyle,
    pub dpi: f32,
}

impl Measure for TextMeasure<'_> {
    fn width(&mut self, text: &str) -> f64 {
        if text.is_empty() {
            return 0.0;
        }
        let layer = photocraft_doc::TextLayer {
            text: text.to_string(),
            runs: vec![photocraft_doc::text::TextRun { len: text.len(), style: self.style.clone() }],
            ..Default::default()
        };
        let l = self.engine.layout(&layer, self.dpi);
        f64::from(l.lines.iter().map(|ln| ln.x1 - ln.x0).fold(0.0f32, f32::max))
    }
}

fn fits_bubble(lines: &[String], fit: &Fit, meter: &mut Meter) -> bool {
    let n = lines.len();
    if n as f64 * fit.line_px > fit.height * FIT_MARGIN * 1.02 {
        return false;
    }
    lines.iter().enumerate().all(|(i, l)| meter.units(l) <= fit_available(i, n, fit) * 1.02)
}

#[cfg(test)]
#[path = "shaper_tests.rs"]
mod tests;
