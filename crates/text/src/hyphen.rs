//! Automatic hyphenation of paragraph (box) text: Liang patterns (TeX `hyph-utf8`, through the
//! `hypher` tries) for English, Spanish and Portuguese, with Photoshop's paragraph defaults.
//!
//! Photoshop's Hyphenation dialog defaults (also what its PSDs store in `EngineData`):
//! words of at least 6 letters, at least 2 letters before and 2 after the hyphen, at most 2
//! consecutive hyphenated lines, and a 36 pt (3 pica) hyphenation zone for unjustified text. The
//! document model only has the on/off switch ([`photocraft_doc::text::ParagraphStyle::hyphenate`]),
//! so [`Rules::PHOTOSHOP`] holds the rest.
//!
//! The pattern licences (MIT, BSD-3-Clause and an all-permissive notice) are in
//! `crates/text/LICENSE-hyphenation.txt`.

/// A language with bundled hyphenation patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HyphLang {
    /// American English (`hyph-en-us`, Gerard D.C. Kuiken).
    En,
    /// Spanish (`hyph-es`, Javier Bezos, CervanTeX).
    Es,
    /// Portuguese (`hyph-pt`, Pedro J. de Rezende, J. Joao Dias Almeida and others).
    Pt,
}

impl HyphLang {
    /// The language of a BCP-47 tag (`es`, `es-MX`, `pt-BR`, `en_US`, …); `None` for languages
    /// without bundled patterns.
    pub fn from_tag(tag: &str) -> Option<Self> {
        let primary = tag.trim().split(['-', '_']).next().unwrap_or("").to_ascii_lowercase();
        match primary.as_str() {
            "en" | "eng" | "english" => Some(Self::En),
            "es" | "spa" | "spanish" => Some(Self::Es),
            "pt" | "por" | "portuguese" => Some(Self::Pt),
            _ => None,
        }
    }

    /// The two-letter code.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Es => "es",
            Self::Pt => "pt",
        }
    }

    fn hypher(self) -> hypher::Lang {
        match self {
            Self::En => hypher::Lang::English,
            Self::Es => hypher::Lang::Spanish,
            Self::Pt => hypher::Lang::Portuguese,
        }
    }

    /// The fewest letters the patterns need after the last break. The English patterns were made
    /// for TeX's `righthyphenmin` 3 and give bad splits closer to the end ("look-ed"); Spanish and
    /// Portuguese syllables are regular, so Photoshop's 2 holds.
    fn pattern_right_min(self) -> usize {
        match self {
            Self::Es | Self::Pt => 2,
            Self::En => 3,
        }
    }
}

/// Hyphenation limits (Photoshop's paragraph Hyphenation settings).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rules {
    /// Only words with at least this many letters are hyphenated.
    pub min_word: usize,
    /// Letters kept before the hyphen.
    pub before: usize,
    /// Letters carried to the next line.
    pub after: usize,
    /// Most consecutive lines that may end in a hyphen (0 = no limit).
    pub limit: usize,
    /// Unjustified text: a line whose gap at the end is narrower than this (points) is left
    /// alone. Justified text ignores it.
    pub zone_pt: f32,
}

impl Rules {
    /// Photoshop's defaults.
    pub const PHOTOSHOP: Rules = Rules { min_word: 6, before: 2, after: 2, limit: 2, zone_pt: 36.0 };
}

impl Default for Rules {
    fn default() -> Self {
        Self::PHOTOSHOP
    }
}

/// Whether `c` is part of a hyphenatable word.
pub fn is_word_char(c: char) -> bool {
    c.is_alphabetic()
}

/// Byte offsets inside `word` (all letters) where it may break with a hyphen, in order. Empty
/// when the word is shorter than `rules.min_word` letters or isn't all letters.
pub fn break_points(word: &str, lang: HyphLang, rules: &Rules) -> Vec<usize> {
    let letters = word.chars().count();
    if letters < rules.min_word.max(2) || !word.chars().all(is_word_char) || word.len() > 256 {
        return Vec::new();
    }
    let before = rules.before.max(1);
    let after = rules.after.max(lang.pattern_right_min()).max(1);
    if letters < before + after {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut at = 0usize;
    for syllable in hypher::hyphenate_bounded(word, lang.hypher(), before, after) {
        at += syllable.len();
        if at < word.len() {
            out.push(at);
        }
    }
    out
}

/// `word` with a `-` at every break point (tests and diagnostics).
pub fn hyphenated(word: &str, lang: HyphLang, rules: &Rules) -> String {
    let points = break_points(word, lang, rules);
    let mut s = String::with_capacity(word.len() + points.len());
    let mut last = 0;
    for p in points {
        if let Some(part) = word.get(last..p) {
            s.push_str(part);
            s.push('-');
            last = p;
        }
    }
    s.push_str(word.get(last..).unwrap_or(""));
    s
}

/// A guess at the language of `text` for hyphenation when the style names none: Spanish or
/// Portuguese by their distinctive letters and common words, otherwise English (Photoshop's
/// default dictionary).
pub fn guess_language(text: &str) -> HyphLang {
    const ES: &[&str] = &[
        "el", "los", "las", "y", "un", "una", "es", "por", "con", "lo", "pero", "qué", "cómo", "yo", "eso", "esto", "ya", "muy", "hay", "aquí", "del", "al",
        "su", "mi", "tu", "le", "les", "está", "estás", "también", "sí", "nada", "algo", "puedo", "quiero", "tiene", "ahora", "bien", "porque", "cuando",
        "donde", "dónde", "ese", "esa", "este", "esta",
    ];
    const PT: &[&str] = &[
        "o", "os", "e", "um", "uma", "é", "não", "você", "com", "mas", "eu", "isso", "isto", "meu", "minha", "seu", "sua", "já", "muito", "ele", "ela", "do",
        "da", "dos", "das", "na", "no", "nós", "também", "sim", "agora", "bem", "porque", "quando", "onde", "esse", "essa", "este", "esta", "tem", "estou",
        "vai", "pra", "aqui", "então",
    ];
    const EN: &[&str] = &[
        "the", "and", "of", "to", "you", "i", "is", "it", "that", "what", "this", "we", "are", "for", "in", "on", "my", "your", "me", "be", "not", "have",
        "do", "don't", "i'm", "it's", "with", "was", "he", "she", "they", "but", "can", "just", "so", "if", "there", "here", "now", "will", "all",
    ];
    let lower = text.to_lowercase();
    let (mut es, mut pt, mut en) = (0i32, 0i32, 0i32);
    for c in lower.chars() {
        match c {
            'ñ' | '¿' | '¡' => es += 3,
            'ã' | 'õ' => pt += 3,
            'ç' | 'ê' | 'ô' | 'â' | 'à' => pt += 2,
            _ => {}
        }
    }
    for w in lower.split(|c: char| !(c.is_alphabetic() || c == '\'' || c == '’')) {
        let w = w.trim_matches(|c| c == '\'' || c == '’');
        if w.is_empty() {
            continue;
        }
        es += i32::from(ES.contains(&w));
        pt += i32::from(PT.contains(&w));
        en += i32::from(EN.contains(&w));
        if w.ends_with("ção") || w.ends_with("ções") {
            pt += 2;
        }
        if w.ends_with("ción") || w.ends_with("ciones") {
            es += 2;
        }
    }
    if es > pt && es > en {
        HyphLang::Es
    } else if pt > es && pt > en {
        HyphLang::Pt
    } else {
        HyphLang::En
    }
}

/// The hyphenation language for a style's language tag; a missing tag falls back to
/// [`guess_language`] over `text`.
pub fn language_for(tag: Option<&str>, text: &str) -> Option<HyphLang> {
    match tag {
        Some(t) if !t.trim().is_empty() => HyphLang::from_tag(t),
        _ => Some(guess_language(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(word: &str, lang: HyphLang) -> String {
        hyphenated(word, lang, &Rules::PHOTOSHOP)
    }

    #[test]
    fn english_words() {
        assert_eq!(h("hyphenation", HyphLang::En), "hy-phen-ation");
        assert_eq!(h("extensive", HyphLang::En), "ex-ten-sive");
        assert_eq!(h("computer", HyphLang::En), "com-puter");
        assert_eq!(h("looked", HyphLang::En), "looked");
        assert_eq!(h("Something", HyphLang::En), "Some-thing");
    }

    #[test]
    fn spanish_words() {
        assert_eq!(h("murciélago", HyphLang::Es), "mur-cié-la-go");
        assert_eq!(h("entonces", HyphLang::Es), "en-ton-ces");
        assert_eq!(h("problema", HyphLang::Es), "pro-ble-ma");
        assert_eq!(h("rápidamente", HyphLang::Es), "rá-pi-da-men-te");
    }

    #[test]
    fn portuguese_words() {
        assert_eq!(h("computador", HyphLang::Pt), "com-pu-ta-dor");
        assert_eq!(h("problema", HyphLang::Pt), "pro-ble-ma");
        assert_eq!(h("obrigado", HyphLang::Pt), "obri-ga-do");
    }

    #[test]
    fn limits() {
        // Under six letters: never.
        assert_eq!(h("hello", HyphLang::En), "hello");
        assert_eq!(h("casa", HyphLang::Es), "casa");
        // Not a word.
        assert!(break_points("abc-def", HyphLang::En, &Rules::PHOTOSHOP).is_empty());
        assert!(break_points("", HyphLang::Es, &Rules::PHOTOSHOP).is_empty());
        // Every point keeps two letters before and two after.
        for (w, l) in [("murciélago", HyphLang::Es), ("hyphenation", HyphLang::En), ("computador", HyphLang::Pt)] {
            for p in break_points(w, l, &Rules::PHOTOSHOP) {
                assert!(w[..p].chars().count() >= 2 && w[p..].chars().count() >= 2, "{w} at {p}");
            }
        }
    }

    #[test]
    fn languages() {
        assert_eq!(HyphLang::from_tag("es-MX"), Some(HyphLang::Es));
        assert_eq!(HyphLang::from_tag("pt_BR"), Some(HyphLang::Pt));
        assert_eq!(HyphLang::from_tag("en-GB"), Some(HyphLang::En));
        assert_eq!(HyphLang::from_tag("fr"), None);
        assert_eq!(language_for(Some("de"), "the"), None);
        assert_eq!(guess_language("¿Qué estás haciendo aquí?"), HyphLang::Es);
        assert_eq!(guess_language("Você não sabe o que está fazendo."), HyphLang::Pt);
        assert_eq!(guess_language("What are you doing here?"), HyphLang::En);
        assert_eq!(guess_language("Entonces el problema es que no hay tiempo"), HyphLang::Es);
        assert_eq!(guess_language(""), HyphLang::En);
    }
}
