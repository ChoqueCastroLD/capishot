//! Paragraph-text hyphenation in layout.

use photocraft_doc::TextLayer;
use photocraft_doc::text::{CharStyle, ParagraphRun, ParagraphStyle, TextAlign, TextRun, TextShape};

use crate::hyphen::{HyphLang, Rules, break_points};
use crate::{TextEngine, TextLayout};

fn boxed(text: &str, width: f32, align: TextAlign, hyphenate: bool, language: Option<&str>) -> TextLayer {
    let style = CharStyle { font_family: "Inter".into(), size_pt: 20.0, language: language.map(str::to_string), ..Default::default() };
    TextLayer {
        text: text.into(),
        runs: vec![TextRun { len: text.len(), style }],
        paragraphs: vec![ParagraphRun { len: text.len(), style: ParagraphStyle { align, hyphenate, ..Default::default() } }],
        shape: TextShape::Box { x: 0.0, y: 0.0, width, height: 2000.0 },
        ..Default::default()
    }
}

/// The text of each line, with a trailing `-` on hyphenated lines.
fn lines(l: &TextLayout, text: &str) -> Vec<String> {
    l.lines.iter().map(|ln| format!("{}{}", text[ln.range.clone()].trim_end(), if ln.hyphen { "-" } else { "" })).collect()
}

const ES: &str = "Entonces el murciélago desapareció rápidamente entre las montañas oscuras del horizonte";

/// Same lines, glyphs and positions.
fn same(a: &TextLayout, b: &TextLayout) -> bool {
    a.lines == b.lines && a.glyphs == b.glyphs && a.clusters == b.clusters
}

#[test]
fn off_changes_nothing() {
    let mut e = TextEngine::new();
    for w in [90.0, 140.0, 200.0, 333.0] {
        let off = e.layout(&boxed(ES, w, TextAlign::JustifyLeft, false, Some("es")), 72.0);
        assert!(off.lines.iter().all(|l| !l.hyphen));
        // Point text never hyphenates.
        let mut point = boxed(ES, w, TextAlign::Left, true, Some("es"));
        point.shape = TextShape::Point;
        let mut point_off = point.clone();
        point_off.paragraphs[0].style.hyphenate = false;
        assert!(same(&e.layout(&point, 72.0), &e.layout(&point_off, 72.0)));
    }
}

#[test]
fn hyphenated_lines_break_at_pattern_points_and_fit() {
    let mut e = TextEngine::new();
    let mut seen = 0;
    for w in (80..=320).step_by(7) {
        let w = w as f32;
        for align in [TextAlign::JustifyLeft, TextAlign::Left, TextAlign::Center] {
            let l = e.layout(&boxed(ES, w, align, true, Some("es")), 72.0);
            let plain = e.layout(&boxed(ES, w, align, false, Some("es")), 72.0);
            if l.lines.iter().all(|ln| !ln.hyphen) {
                assert!(same(&l, &plain), "no hyphen → the plain layout ({w}, {align:?})");
                continue;
            }
            seen += 1;
            assert!(l.lines.len() <= plain.lines.len(), "hyphenation never adds lines: {:?}", lines(&l, ES));
            let mut run = 0;
            for (i, ln) in l.lines.iter().enumerate() {
                if !ln.hyphen {
                    run = 0;
                    continue;
                }
                // A hyphenated line fits the box, hyphen included.
                assert!(ln.x1 - ln.x0 <= w + 0.5, "line {i} is {} wide in {w}: {:?}", ln.x1 - ln.x0, lines(&l, ES));
                run += 1;
                assert!(run <= Rules::PHOTOSHOP.limit, "too many consecutive hyphens: {:?}", lines(&l, ES));
                // The break sits at a Spanish pattern point of the word it splits.
                let at = ln.range.end;
                let start = ES[..at].rfind(|c: char| !c.is_alphabetic()).map_or(0, |i| i + 1);
                let end = ES[at..].find(|c: char| !c.is_alphabetic()).map_or(ES.len(), |i| at + i);
                let word = &ES[start..end];
                assert!(break_points(word, HyphLang::Es, &Rules::PHOTOSHOP).contains(&(at - start)), "{word} split at {}", at - start);
                // The next line starts where this one ended: no text lost or repeated.
                assert_eq!(l.lines.get(i + 1).map(|n| n.range.start), Some(at));
            }
            // One drawn hyphen per hyphenated line.
            let hyphens = l.lines.iter().filter(|ln| ln.hyphen).count();
            assert_eq!(l.glyphs.len(), plain.glyphs.len() + hyphens, "{:?}", lines(&l, ES));
            if align == TextAlign::Center {
                for ln in l.lines.iter().filter(|ln| ln.hyphen) {
                    let mid = (ln.x0 + ln.x1) / 2.0;
                    assert!((mid - w / 2.0).abs() < 1.0, "centred with its hyphen: {mid} vs {}", w / 2.0);
                }
            }
        }
    }
    assert!(seen > 10, "hyphenation happened in only {seen} layouts");
}

#[test]
fn justified_text_hyphenates_a_known_split() {
    let mut e = TextEngine::new();
    let text = "Hola murciélago";
    // Wide enough for "Hola murcié-" but not the whole word.
    let full = e.layout(&boxed(text, 2000.0, TextAlign::Left, false, Some("es")), 72.0);
    let x_at = |byte: usize| full.clusters.iter().find(|c| c.range.start == byte).map(|c| c.x).unwrap();
    let cut = text.find("lago").unwrap();
    let w = x_at(cut) - full.lines[0].x0 + 14.0;
    let l = e.layout(&boxed(text, w, TextAlign::JustifyLeft, true, Some("es")), 72.0);
    assert_eq!(lines(&l, text), ["Hola murcié-", "lago"]);
    // Caret offsets stay in the text and cover it.
    assert!(l.clusters.iter().all(|c| c.range.end <= text.len()));
    assert_eq!(l.clusters.iter().map(|c| c.range.len()).sum::<usize>(), text.len());
}

#[test]
fn language_comes_from_the_style_or_the_text() {
    let mut e = TextEngine::new();
    let text = "Hola murciélago";
    let full = e.layout(&boxed(text, 2000.0, TextAlign::Left, false, None), 72.0);
    let x = full.clusters.iter().find(|c| c.range.start == text.find("lago").unwrap()).map(|c| c.x).unwrap();
    let w = x - full.lines[0].x0 + 14.0;
    // No language: guessed (Spanish here).
    let l = e.layout(&boxed(text, w, TextAlign::JustifyLeft, true, None), 72.0);
    assert!(l.lines[0].hyphen, "{:?}", lines(&l, text));
    // A language without patterns: plain breaks.
    let fr = e.layout(&boxed(text, w, TextAlign::JustifyLeft, true, Some("fr")), 72.0);
    assert!(fr.lines.iter().all(|ln| !ln.hyphen));
}

#[test]
fn unjustified_lines_respect_the_hyphenation_zone() {
    let mut e = TextEngine::new();
    // At 300 dpi the 36 pt zone is 150 px: wider than " mur-" in 10 pt type.
    let text = "Hola murciélago";
    let mut small = |w: f32, align: TextAlign, hyphenate: bool| {
        let mut t = boxed(text, w, align, hyphenate, Some("es"));
        t.runs[0].style.size_pt = 10.0;
        e.layout(&t, 300.0)
    };
    let full = small(4000.0, TextAlign::Left, false);
    let x = |b: usize| full.clusters.iter().find(|c| c.range.start == b).map(|c| c.x - full.lines[0].x0).unwrap();
    let w = x(text.find("ciélago").unwrap()) + 25.0;
    let gap = w - x(text.find(' ').unwrap());
    assert!(gap < 150.0, "{gap}");
    let left = small(w, TextAlign::Left, true);
    assert!(left.lines.iter().all(|ln| !ln.hyphen), "{:?}", lines(&left, text));
    // Justified text ignores the zone.
    let j = small(w, TextAlign::JustifyLeft, true);
    assert_eq!(lines(&j, text), ["Hola mur-", "ciélago"]);
}

#[test]
fn english_and_portuguese_paragraphs() {
    let mut e = TextEngine::new();
    for (text, lang) in [
        ("The extraordinary hyphenation algorithm demonstrates incredible typographical sophistication", HyphLang::En),
        ("O computador processava informações extraordinariamente complicadas durante a madrugada", HyphLang::Pt),
    ] {
        let mut hits = 0;
        for w in (90..=300).step_by(9) {
            let l = e.layout(&boxed(text, w as f32, TextAlign::JustifyLeft, true, Some(lang.code())), 72.0);
            for ln in l.lines.iter().filter(|ln| ln.hyphen) {
                hits += 1;
                let at = ln.range.end;
                let start = text[..at].rfind(|c: char| !c.is_alphabetic()).map_or(0, |i| i + 1);
                let end = text[at..].find(|c: char| !c.is_alphabetic()).map_or(text.len(), |i| at + i);
                assert!(break_points(&text[start..end], lang, &Rules::PHOTOSHOP).contains(&(at - start)), "{}", &text[start..end]);
            }
        }
        assert!(hits > 5, "{lang:?}: {hits}");
    }
}
