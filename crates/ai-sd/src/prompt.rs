//! Prompt handling before the text encoder (no ML): the safety check and Spanish/Portuguese →
//! English translation (CLIP understands English only).
//!
//! * [`check`] refuses sexual content involving minors and a short list of clearly illegal
//!   requests (counterfeit money, forged identity documents), in English, Spanish and
//!   Portuguese. The text is normalised first (lower case, accents stripped, leetspeak digits
//!   and symbols mapped to letters, spaced-out letters joined, repeated letters collapsed), so
//!   "n i ñ 4" or "l0000li" are caught. There is no general NSFW filter.
//! * [`translate`] maps Spanish and Portuguese words and phrases through a built-in dictionary
//!   of visual vocabulary (`dict/es.txt`, `dict/pt.txt`: our own word lists), with plural and
//!   gender folding and noun–adjective reordering ("cielo azul" → "blue sky"). English prompts
//!   (more English function words than dictionary hits) pass through unchanged, as do unknown
//!   words (names, English loanwords).

use std::collections::HashMap;
use std::sync::OnceLock;

/// Why a prompt was refused (short, for the UI).
pub const REFUSED: &str = "This prompt can't be used: Generative Fill doesn't create sexual content involving minors or illegal material.";

/// Lower-cases and strips accents (Latin letters used by English, Spanish and Portuguese).
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(char::to_lowercase)
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' | 'å' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ñ' => 'n',
            'ç' => 'c',
            other => other,
        })
        .collect()
}

/// Words of the folded text (letters and digits).
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect()
}

/// The normalised token stream the safety check matches against, and the runs of single letters
/// joined together ("n i ñ a d e s n u d a" → "ninadesnuda"), searched for terms inside them.
fn normalise(s: &str) -> (Vec<String>, Vec<String>) {
    let leet: String = fold(s)
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '1' | '!' | '|' => 'i',
            '3' => 'e',
            '4' | '@' => 'a',
            '5' | '$' => 's',
            '7' | '+' => 't',
            '8' => 'b',
            other => other,
        })
        .collect();
    // Join runs of single letters ("l o l i" → "loli").
    let mut out: Vec<String> = Vec::new();
    let mut runs: Vec<String> = Vec::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut Vec<String>, runs: &mut Vec<String>| {
        if run.chars().count() > 1 {
            runs.push(collapse(run));
        }
        if !run.is_empty() {
            out.push(collapse(run));
        }
        run.clear();
    };
    for w in words(&leet) {
        if w.chars().count() == 1 {
            run.push_str(&w);
            continue;
        }
        flush(&mut run, &mut out, &mut runs);
        // Collapse repeated letters ("loooli" → "loli"); the term lists are collapsed the same way.
        out.push(collapse(&w));
    }
    flush(&mut run, &mut out, &mut runs);
    (out, runs)
}

fn collapse(w: &str) -> String {
    let mut s = String::with_capacity(w.len());
    let mut last = None;
    for c in w.chars() {
        if Some(c) != last {
            s.push(c);
        }
        last = Some(c);
    }
    s
}

/// Terms referring to children (prefix match when ending in `*`).
const MINOR: &[&str] = &[
    "child*",
    "kid",
    "kids",
    "kiddie*",
    "minor",
    "minors",
    "underage*",
    "toddler*",
    "baby",
    "babies",
    "infant*",
    "preteen*",
    "pre teen",
    "tween*",
    "teen",
    "teens",
    "teenage*",
    "loli",
    "lolis",
    "shota*",
    "schoolgirl*",
    "schoolboy*",
    "little girl*",
    "little boy*",
    "young girl*",
    "young boy*",
    "jailbait",
    "nino*",
    "nina",
    "ninas",
    "nene*",
    "nena*",
    "menor",
    "menores",
    "infantil*",
    "bebe*",
    "chiquill*",
    "adolescent*",
    "colegial*",
    "crianca*",
    "menino*",
    "menina*",
    "novinh*",
    "garotinh*",
    "pirralh*",
    "escolar*",
    "primaria",
    "kinder*",
    "jardim de infancia",
    "jardin de infantes",
];

/// Sexual terms.
const SEXUAL: &[&str] = &[
    "nude*",
    "naked",
    "nudity",
    "nsfw",
    "sex",
    "sexy",
    "sexual*",
    "porn*",
    "hentai",
    "erotic*",
    "lewd*",
    "explicit",
    "topless",
    "bottomless",
    "genital*",
    "nipple*",
    "penis*",
    "vagina*",
    "pussy",
    "cum",
    "fuck*",
    "orgasm*",
    "masturbat*",
    "undress*",
    "lingerie",
    "underwear",
    "panties",
    "striptease",
    "stripper*",
    "seduc*",
    "boob*",
    "desnud*",
    "sexo",
    "porno*",
    "erotic*",
    "follar",
    "tetas",
    "pechos",
    "pene",
    "lenceria",
    "calzon*",
    "bragas",
    "masturb*",
    "violar",
    "violacion",
    "nu",
    "nua",
    "nus",
    "nuas",
    "transar",
    "foder",
    "fodendo",
    "peitos",
    "seios",
    "buceta",
    "calcinha*",
    "estupr*",
    "rape*",
    "molest*",
    "abus*",
];

/// Requests refused on their own (child sexual abuse material, counterfeit money, forged IDs).
const ALWAYS: &[&str] = &[
    "csam",
    "child porn*",
    "childporn*",
    "pedo*",
    "pedofil*",
    "lolicon",
    "shotacon",
    "jailbait",
    "pornografia infantil",
    "porno infantil",
    "child abuse",
    "abuso infantil",
    "counterfeit money",
    "counterfeit banknote*",
    "counterfeit bill*",
    "fake banknote*",
    "fake money",
    "billete* falso*",
    "dinero falso",
    "nota* falsa*",
    "dinheiro falso",
    "cedula* falsa*",
    "fake passport*",
    "forged passport*",
    "fake id",
    "fake driver* license*",
    "pasaporte* falso*",
    "documento* falso*",
    "passaporte* falso*",
    "identidade falsa",
    "cnh falsa",
    "dni falso",
];

/// Whether `term` (one or more words, `*` = prefix on its last word) occurs in `toks`, or (3+
/// letters) inside one of the spelled-out `runs`.
fn has((toks, runs): (&[String], &[String]), term: &str) -> bool {
    let squashed: String = term.split(' ').map(|p| collapse(p.trim_end_matches('*'))).collect();
    if squashed.chars().count() >= 3 && runs.iter().any(|r| r.contains(&squashed)) {
        return true;
    }
    // Each word may end in `*` (a prefix: "billete* falso*" matches "billetes falsos").
    let parts: Vec<(String, bool)> = term.split(' ').map(|p| (collapse(p.trim_end_matches('*')), p.ends_with('*'))).collect();
    let n = parts.len();
    if n == 0 || toks.len() < n {
        return false;
    }
    let fits = |t: &str, (p, prefix): &(String, bool)| if *prefix { t.starts_with(p.as_str()) } else { t == p };
    (0..=toks.len() - n).any(|i| parts.iter().enumerate().all(|(j, p)| fits(toks.get(i + j).map(String::as_str).unwrap_or_default(), p)))
        || (n > 1 && {
            // The phrase written without spaces ("childporn").
            let joined: String = parts.iter().map(|(p, _)| p.as_str()).collect();
            let prefix = parts.last().is_some_and(|(_, pre)| *pre);
            toks.iter().any(|t| if prefix { t.starts_with(&joined) } else { *t == joined })
        })
}

/// `Err(REFUSED)` when the prompt asks for something Generative Fill must not make.
pub fn check(prompt: &str) -> Result<(), &'static str> {
    let (toks, runs) = normalise(prompt);
    let toks = (toks.as_slice(), runs.as_slice());
    // Ages under 18 next to "year(s) old" / "yo" / "años" / "anos" (before the leetspeak map,
    // which turns digits into letters).
    let plain = words(&fold(prompt));
    let young_age = plain.windows(2).any(|w| match w {
        [a, b] => a.parse::<u32>().is_ok_and(|n| n < 18) && ["yo", "year", "years", "ano", "anos", "anito", "aninhos"].contains(&b.as_str()),
        _ => false,
    });
    if ALWAYS.iter().any(|t| has(toks, t)) {
        return Err(REFUSED);
    }
    let minor = young_age || MINOR.iter().any(|t| has(toks, t));
    if minor && SEXUAL.iter().any(|t| has(toks, t)) {
        return Err(REFUSED);
    }
    Ok(())
}

// ------------------------------------------------------------------ translation

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pos {
    Noun,
    Adj,
    Other,
}

struct Dict {
    /// Folded key (one to three words) → (English, part of speech).
    map: HashMap<String, (String, Pos)>,
}

fn parse(text: &str) -> Dict {
    let mut map = HashMap::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let mut f = line.split('|');
        let (Some(k), Some(v)) = (f.next(), f.next()) else { continue };
        let pos = match f.next().map(str::trim) {
            Some("a") => Pos::Adj,
            Some("x") => Pos::Other,
            _ => Pos::Noun,
        };
        map.insert(fold(k.trim()), (v.trim().to_string(), pos));
    }
    Dict { map }
}

fn es() -> &'static Dict {
    static D: OnceLock<Dict> = OnceLock::new();
    D.get_or_init(|| parse(include_str!("dict/es.txt")))
}

fn pt() -> &'static Dict {
    static D: OnceLock<Dict> = OnceLock::new();
    D.get_or_init(|| parse(include_str!("dict/pt.txt")))
}

/// Common English words: a prompt with more of these than dictionary hits is English.
const ENGLISH: &[&str] = &[
    "the",
    "a",
    "an",
    "of",
    "with",
    "and",
    "in",
    "on",
    "at",
    "by",
    "for",
    "from",
    "to",
    "into",
    "over",
    "under",
    "behind",
    "is",
    "are",
    "background",
    "sky",
    "tree",
    "trees",
    "water",
    "cloud",
    "clouds",
    "light",
    "dark",
    "white",
    "black",
    "blue",
    "green",
    "yellow",
    "big",
    "small",
    "old",
    "new",
    "girl",
    "boy",
    "man",
    "woman",
    "house",
    "street",
    "city",
    "night",
    "day",
    "sun",
    "moon",
    "flower",
    "flowers",
    "cat",
    "dog",
    "bird",
    "fire",
    "snow",
    "rain",
    "wall",
    "window",
    "door",
    "room",
    "style",
    "anime",
    "manga",
    "detailed",
    "beautiful",
    "photo",
    "realistic",
    "painting",
    "drawing",
    "sketch",
    "shadow",
    "glowing",
    "wearing",
    "holding",
    "standing",
    "sitting",
    "looking",
    "smiling",
    "red",
    "pink",
    "purple",
    "orange",
    "brown",
    "gray",
    "grey",
    "golden",
    "silver",
    "wooden",
    "stone",
    "glass",
    "metal",
    "empty",
];

/// [`ENGLISH`] plus every word the dictionaries translate to: evidence that a prompt is English.
fn english_vocab() -> &'static std::collections::HashSet<String> {
    static V: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    V.get_or_init(|| {
        let mut v: std::collections::HashSet<String> = ENGLISH.iter().map(|s| (*s).to_string()).collect();
        for d in [es(), pt()] {
            for (en, _) in d.map.values() {
                v.extend(en.split(' ').filter(|w| !w.is_empty()).map(str::to_string));
            }
        }
        v
    })
}

impl Dict {
    /// The entry for a folded word, folding plurals and gender; `true` when it was a plural.
    fn word(&self, w: &str, portuguese: bool) -> Option<(&(String, Pos), bool)> {
        if let Some(e) = self.map.get(w) {
            return Some((e, false));
        }
        let mut cands: Vec<String> = Vec::new();
        if portuguese {
            for (suf, rep) in [
                ("oes", "ao"),
                ("aes", "ao"),
                ("ais", "al"),
                ("eis", "el"),
                ("ois", "ol"),
                ("is", "il"),
                ("ns", "m"),
                ("res", "r"),
                ("zes", "z"),
                ("ses", "s"),
                ("s", ""),
            ] {
                if let Some(stem) = w.strip_suffix(suf) {
                    cands.push(format!("{stem}{rep}"));
                }
            }
        } else {
            for (suf, rep) in [("ces", "z"), ("es", ""), ("s", "")] {
                if let Some(stem) = w.strip_suffix(suf) {
                    cands.push(format!("{stem}{rep}"));
                }
            }
        }
        let mut gendered = Vec::new();
        for (c, plural) in std::iter::once((w.to_string(), false)).chain(cands.iter().map(|c| (c.clone(), true))) {
            if let Some(stem) = c.strip_suffix('a') {
                gendered.push((format!("{stem}o"), plural));
            }
        }
        cands.into_iter().map(|c| (c, true)).chain(gendered).find_map(|(c, plural)| self.map.get(&c).map(|e| (e, plural)))
    }
}

/// English plural of a (last) word: `-s`, `-es` after s/x/z/ch/sh, `-ies` after consonant + y.
fn pluralize(en: &str) -> String {
    let (head, last) = match en.rsplit_once(' ') {
        Some((h, l)) => (format!("{h} "), l),
        None => (String::new(), en),
    };
    let p = if ["s", "x", "z", "ch", "sh"].iter().any(|s| last.ends_with(s)) {
        format!("{last}es")
    } else if last.ends_with('y') && !last.ends_with("ay") && !last.ends_with("ey") && !last.ends_with("oy") {
        format!("{}ies", &last[..last.len() - 1])
    } else {
        format!("{last}s")
    };
    format!("{head}{p}")
}

/// Translates a Spanish or Portuguese prompt to English (English and unknown words unchanged).
pub fn translate(prompt: &str) -> String {
    let folded = fold(prompt);
    let toks = words(&folded);
    if toks.is_empty() {
        return String::new();
    }
    let en = english_vocab();
    let english = toks.iter().filter(|t| en.contains(t.as_str())).count();
    // A dictionary hit counts only when it isn't also an English word ("panda", "metal").
    let hits = |d: &Dict, pt: bool| toks.iter().filter(|t| !en.contains(t.as_str()) && d.word(t, pt).is_some()).count();
    let (h_es, h_pt) = (hits(es(), false), hits(pt(), true));
    // Ties go to translation unless an English function word shows up ("panda rojo" is Spanish).
    let function_words = toks.iter().any(|t| ["the", "a", "an", "of", "with", "and", "in", "on", "at", "is", "are"].contains(&t.as_str()));
    let best = h_es.max(h_pt);
    if best == 0 || english > best || (english == best && function_words) {
        return prompt.trim().to_string();
    }
    let (dict, portuguese) = if h_pt > h_es { (pt(), true) } else { (es(), false) };
    // Greedy longest phrase (up to three words), then words.
    let mut out: Vec<(String, Pos)> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        let mut matched = false;
        for n in (2..=3).rev() {
            if i + n <= toks.len() {
                let key = toks[i..i + n].join(" ");
                if let Some((en, pos)) = dict.map.get(&key) {
                    out.push((en.clone(), *pos));
                    i += n;
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }
        let t = &toks[i];
        match dict.word(t, portuguese) {
            Some(((en, pos), plural)) => out.push((if plural && *pos == Pos::Noun { pluralize(en) } else { en.clone() }, *pos)),
            None => out.push((t.clone(), Pos::Other)),
        }
        i += 1;
    }
    // Romance order "noun adjective" → English "adjective noun".
    let mut k = 0;
    while k + 1 < out.len() {
        if out[k].1 == Pos::Noun && out[k + 1].1 == Pos::Adj {
            out.swap(k, k + 1);
            k += 2;
        } else {
            k += 1;
        }
    }
    out.into_iter().map(|(w, _)| w).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_minors_in_sexual_prompts_with_obfuscation() {
        for p in [
            "naked child",
            "n4k3d ch1ld",
            "niña desnuda",
            "n i ñ a  d e s n u d a",
            "menina nua na praia",
            "sexy 12 year old",
            "loooli hentai",
            "chica de 15 años en lencería",
            "lolicon",
            "child porn",
            "billetes falsos de 100 dolares",
            "fake passport photo page",
        ] {
            assert_eq!(check(p), Err(REFUSED), "{p}");
        }
        for p in [
            "a child flying a kite in the park",
            "niña jugando con un perro",
            "beach at sunset",
            "nude statue in a museum",
            "woman in lingerie",
            "menino andando de bicicleta",
            "",
            "teenage mutant robot in a city",
        ] {
            assert_eq!(check(p), Ok(()), "{p}");
        }
    }

    #[test]
    fn translates_spanish_and_portuguese() {
        assert_eq!(translate("cielo azul con nubes"), "blue sky with clouds");
        assert_eq!(translate("un gato negro sobre el tejado"), "black cat on roof");
        assert_eq!(translate("céu azul com nuvens"), "blue sky with clouds");
        assert_eq!(translate("flores rojas"), "red flowers");
        assert_eq!(translate("a red car in the street"), "a red car in the street");
        assert_eq!(translate("Totoro"), "Totoro");
        assert_eq!(translate("panda rojo"), "red panda");
        assert_eq!(translate("chica anime en un hospital"), "girl anime in hospital");
        assert_eq!(translate("dog with a bone"), "dog with a bone");
    }
}
