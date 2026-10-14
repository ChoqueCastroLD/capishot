//! TextShapeR tests: TypeR's own `scripts/testTextShapeR.js` expectations (neutral tuning,
//! character-class widths, French rules where the JS used them), then language-aware
//! hyphenation and real-font measuring.

use super::*;

fn variants_of(text: &str, opts: Options) -> Vec<Variant> {
    shape(text, &opts, &mut CharClass)
}

fn fr(limit: usize) -> Options {
    Options { hyphenation: Some(Hyphenation::French), limit, ..Default::default() }
}

fn texts(v: &[Variant]) -> Vec<String> {
    v.iter().map(Variant::text).collect()
}

fn visible_lens(v: &Variant) -> Vec<usize> {
    v.lines.iter().map(|l| visible_len(l)).collect()
}

fn ellipse_rows() -> Vec<Row> {
    (0..=12)
        .map(|i| {
            let y = f64::from(i) / 12.0;
            let w = (1.0 - (2.0 * y - 1.0).powi(2)).max(0.0).sqrt();
            Row { y, left: 0.5 - w / 2.0, right: 0.5 + w / 2.0, width: w }
        })
        .collect()
}

#[test]
fn a_sentence_gets_a_convex_block() {
    let v = variants_of("This sentence needs a pleasant bubble shaped manga layout today.", fr(12));
    assert!(!v.is_empty() && v.len() <= 12);
    let unique: HashSet<String> = texts(&v).into_iter().collect();
    assert_eq!(unique.len(), v.len());
    assert!(v[0].lines.len() >= 2, "{:?}", v[0].lines);
    let l = visible_lens(&v[0]);
    let mid = l[(l.len() - 1) / 2];
    if l.len() > 2 {
        assert!(mid >= l[0] && mid >= l[l.len() - 1], "{:?}", v[0].lines);
    } else {
        assert!(l[0].abs_diff(l[1]) <= 6, "{:?}", v[0].lines);
    }
}

#[test]
fn hyphenation_can_be_turned_off() {
    let text = "extraordinarily shaped lettering can fit better";
    assert!(texts(&variants_of(text, fr(10))).iter().any(|t| t.contains("-\n")));
    let off = variants_of(text, Options { hyphenation: None, limit: 10, ..Default::default() });
    assert!(!off.is_empty());
    assert!(texts(&off).iter().all(|t| !t.contains("-\n")));
}

#[test]
fn markdown_spans_stay_whole() {
    let v = variants_of("A **bold sentence** should keep markdown markers safe", fr(10));
    assert!(!v.is_empty());
    for t in texts(&v) {
        assert!(!t.contains("**bo\nld") && !t.contains("sent\nence**"), "{t}");
        assert_eq!(t.matches("**").count(), 2, "{t}");
    }
}

#[test]
fn one_short_word_is_itself() {
    assert_eq!(texts(&variants_of("Bonjour", fr(12))), ["Bonjour"]);
}

#[test]
fn tall_profiles_give_more_lines_than_wide_ones() {
    let text = "A longer sentence can choose different bubble silhouettes for the same lettering.";
    let tall = variants_of(text, Options { profile: Profile::Tall, ..fr(10) });
    let wide = variants_of(text, Options { profile: Profile::Wide, ..fr(10) });
    assert!(tall[0].lines.len() > wide[0].lines.len(), "{:?} vs {:?}", tall[0].lines, wide[0].lines);
    assert!(char_class_width("minimum") < char_class_width("maximum"));
}

#[test]
fn lines_like_to_end_on_punctuation() {
    let v = variants_of("Mais attends, je voulais juste te parler de ce qui est arrive hier soir.", fr(10));
    assert!(v[0].text().contains("attends,\n"), "{}", v[0].text());
}

#[test]
fn no_lone_name_line_nor_abrupt_jumps() {
    let v = variants_of("CLYDE, JE PEUX UTILISER UN MÉDAILLON ?", fr(10));
    assert!(!v[0].text().starts_with("CLYDE,\nJE PEUX UTILISER"), "{}", v[0].text());
    for x in v.iter().take(5) {
        if x.lines.len() <= 2 {
            continue;
        }
        let w: Vec<f64> = x.lines.iter().map(|l| char_class_width(l)).collect();
        let max = w.iter().copied().fold(0.0, f64::max);
        assert!(w.windows(2).all(|p| (p[1] - p[0]).abs() / max <= 0.55), "{:?}", x.lines);
    }
}

#[test]
fn punctuation_never_starts_or_ends_a_line_wrongly() {
    let v = variants_of("salut ! comment ça va aujourd'hui mes amis ?", fr(12));
    assert!(!v.is_empty());
    for x in &v {
        for l in &x.lines {
            let t = l.trim();
            assert!(!t.starts_with(|c| "!?;:.,…»›)]}".contains(c)), "{t:?} in {:?}", x.lines);
            assert!(!t.ends_with(|c| "«‹¿¡([{".contains(c)), "{t:?} in {:?}", x.lines);
        }
    }
}

#[test]
fn french_syllables() {
    let v = texts(&variants_of("utiliser", fr(10)));
    assert!(v.iter().any(|t| t.to_lowercase().contains("uti-\nliser")), "{v:?}");
    assert!(v.iter().all(|t| !t.to_lowercase().contains("util-\niser")), "{v:?}");
}

#[test]
fn a_calibrated_round_bubble_holds_its_variants() {
    let rows = ellipse_rows();
    let opts = Options {
        rows: rows.clone(),
        width: Some(300.0),
        height: Some(300.0),
        calibration: Some(Calibration { unit_px: Some(10.0), line_px: 40.0 }),
        ..fr(12)
    };
    let v = variants_of("Je crois que nous devrions vraiment partir avant que la nuit tombe sur la ville.", opts);
    assert!(!v.is_empty());
    let best = &v[0];
    let w: Vec<f64> = best.lines.iter().map(|l| char_class_width(l)).collect();
    assert!(w.len() > 1, "{:?}", best.lines);
    assert!(w.len() as f64 * 40.0 <= 300.0);
    let ellipse = |y: f64| (1.0 - (2.0 * y.clamp(0.0, 1.0) - 1.0).powi(2)).max(0.0).sqrt();
    let n = w.len() as f64;
    for (i, width) in w.iter().enumerate() {
        let yc = 0.5 + (i as f64 + 0.5 - n / 2.0) * (40.0 / 300.0);
        let half = 40.0 / 300.0 * 0.4;
        let avail = 300.0 * ellipse(yc - half).min(ellipse(yc + half));
        assert!(width * 10.0 <= avail * 1.05, "line {i} escapes: {:?}", best.lines);
    }
    if w.len() > 2 {
        let interior = w[1..w.len() - 1].iter().copied().fold(0.0, f64::max);
        assert!(interior >= w[0] && interior >= w[w.len() - 1], "{:?}", best.lines);
    }
    let max = w.iter().copied().fold(0.0, f64::max);
    assert!(w.windows(2).all(|p| (p[1] - p[0]).abs() / max <= 0.6), "{:?}", best.lines);
    assert_eq!(best.fits, Some(true));
}

#[test]
fn spanish_portuguese_and_english_split_at_pattern_points() {
    for (text, lang) in [
        ("El murciélago desapareció rápidamente entre las montañas", HyphLang::Es),
        ("O computador processava informações extraordinariamente complicadas", HyphLang::Pt),
        ("The extraordinary hyphenation algorithm demonstrates sophistication", HyphLang::En),
    ] {
        let opts = Options { hyphenation: Some(Hyphenation::Patterns(lang)), limit: 64, ..Default::default() };
        // Every hyphen set competes; look at all of them through a generous limit.
        let mut seen = 0;
        for w in split_words(text) {
            for s in hyphen_splits(&w, Hyphenation::Patterns(lang)) {
                seen += 1;
                let head = s.prefix.trim_end_matches('-');
                let r = hyphen::Rules { min_word: 8, before: 3, after: 3, limit: 0, zone_pt: 0.0 };
                assert!(hyphen::break_points(&w, lang, &r).contains(&head.len()), "{w}: {head}");
                assert!(head.chars().count() >= 3 && s.suffix.chars().count() >= 3, "{w}: {s:?}");
            }
        }
        assert!(seen > 3, "{lang:?}: {seen}");
        let v = variants_of(text, opts);
        assert!(!v.is_empty());
        for x in v.iter().filter(|x| x.hyphens > 0) {
            assert_eq!(x.text().matches("-\n").count(), 1, "{:?}", x.lines);
        }
    }
    // Opening punctuation stays on the head: "¿Desapareció" → "¿Desa-" + "pareció?".
    let s = hyphen_splits("¿Desapareció?", Hyphenation::Patterns(HyphLang::Es));
    assert!(!s.is_empty() && s.iter().all(|s| s.prefix.starts_with('¿') && s.suffix.ends_with('?')), "{s:?}");
    // Short words, Markdown and compounds never split.
    assert!(hyphen_splits("casa", Hyphenation::Patterns(HyphLang::Es)).is_empty());
    assert!(hyphen_splits("**extraordinario**", Hyphenation::Patterns(HyphLang::Es)).is_empty());
    assert!(hyphen_splits("bien-intencionado", Hyphenation::Patterns(HyphLang::Es)).is_empty());
}

#[test]
fn languages_come_from_tags_or_the_text() {
    assert_eq!(Hyphenation::from_tag("fr-FR"), Some(Hyphenation::French));
    assert_eq!(Hyphenation::from_tag("es"), Some(Hyphenation::Patterns(HyphLang::Es)));
    assert_eq!(Hyphenation::from_tag("ja"), None);
    assert_eq!(Hyphenation::for_text(None, "¿Qué pasa aquí?"), Some(Hyphenation::Patterns(HyphLang::Es)));
}

#[test]
fn dehyphenation_merges_cesures_but_keeps_compounds() {
    let o = Options { dehyphenate: true, hyphenation: Some(Hyphenation::French), ..Default::default() };
    assert_eq!(normalize("une silho- uette dans le court- circuit avec Jean- Paul", &o), "une silhouette dans le court- circuit avec Jean- Paul");
    let es = Options { dehyphenate: true, hyphenation: Some(Hyphenation::Patterns(HyphLang::Es)), ..Default::default() };
    assert_eq!(normalize("el murcié- lago", &es), "el murciélago");
}

#[test]
fn hostile_input_is_harmless() {
    assert!(variants_of("", Options::default()).is_empty());
    assert!(variants_of("   \n\t ", Options::default()).is_empty());
    assert!(variants_of(&"a ".repeat(3000), Options::default()).is_empty(), "too long");
    let weird = Options {
        width: Some(f64::NAN),
        height: Some(-3.0),
        rows: vec![Row { y: f64::NAN, left: 0.0, right: 1.0, width: 2.0 }, Row { y: 0.5, left: -1.0, right: 9.0, width: f64::INFINITY }],
        calibration: Some(Calibration { unit_px: Some(0.0), line_px: f64::NAN }),
        max_lines: Some(0),
        limit: 0,
        ..Default::default()
    };
    let v = variants_of("Some text that would normally be shaped into lines", weird);
    assert!(v.len() <= 1);
    for t in ["***", "* * *", "\\* \\_ \\\\", "¿¡«", "a\u{0}b", "😀 😀 😀 😀"] {
        let _ = variants_of(t, Options::default());
    }
    // Zero-width measurer.
    struct Zero;
    impl Measure for Zero {
        fn width(&mut self, _: &str) -> f64 {
            0.0
        }
    }
    let _ = shape("A few words to shape here", &Options::default(), &mut Zero);
}

#[test]
fn real_font_widths_keep_lines_inside_the_bubble() {
    let mut engine = photocraft_text::TextEngine::new();
    let style = photocraft_doc::text::CharStyle { font_family: "Inter".into(), size_pt: 20.0, ..Default::default() };
    let rows = ellipse_rows();
    let text = "Nunca pensé que llegaríamos tan lejos, pero aquí estamos todavía juntos.";
    let line_px = 24.0;
    let opts = Options {
        hyphenation: Some(Hyphenation::Patterns(HyphLang::Es)),
        rows,
        width: Some(360.0),
        height: Some(300.0),
        calibration: Some(Calibration { unit_px: None, line_px }),
        ..Default::default()
    };
    let v = shape(text, &opts, &mut TextMeasure { engine: &mut engine, style: style.clone(), dpi: 72.0 });
    assert!(!v.is_empty());
    let mut m = TextMeasure { engine: &mut engine, style, dpi: 72.0 };
    let ellipse = |y: f64| (1.0 - (2.0 * y.clamp(0.0, 1.0) - 1.0).powi(2)).max(0.0).sqrt();
    for x in v.iter().filter(|x| x.fits == Some(true)) {
        let n = x.lines.len() as f64;
        for (i, l) in x.lines.iter().enumerate() {
            let px = m.width(l);
            assert!((px - x.widths[i]).abs() < 0.5, "reported width {} vs {px}", x.widths[i]);
            let yc = 0.5 + (i as f64 + 0.5 - n / 2.0) * (line_px / 300.0);
            let half = line_px / 300.0 * 0.4;
            let avail = 360.0 * ellipse(yc - half).min(ellipse(yc)).min(ellipse(yc + half)) * 0.9;
            assert!(px <= avail * 1.02 + 0.5, "{l:?} {px} > {avail} in {:?}", x.lines);
        }
    }
    assert_eq!(v[0].fits, Some(true), "{:?}", v[0].lines);
    assert!(v[0].lines.len() >= 3, "{:?}", v[0].lines);
}

#[test]
#[ignore]
fn print_samples() {
    let mut engine = photocraft_text::TextEngine::new();
    let style = photocraft_doc::text::CharStyle { font_family: "Inter".into(), size_pt: 20.0, ..Default::default() };
    for (text, w, h) in [
        ("Nunca pensé que llegaríamos tan lejos, pero aquí estamos todavía juntos.", 360.0, 300.0),
        ("¿De verdad crees que el murciélago desapareció rápidamente entre las montañas?", 300.0, 340.0),
        ("I never thought we would make it this far, but here we are, still together.", 380.0, 260.0),
    ] {
        let opts = Options {
            hyphenation: Hyphenation::for_text(None, text),
            rows: ellipse_rows(),
            width: Some(w),
            height: Some(h),
            calibration: Some(Calibration { unit_px: None, line_px: 24.0 }),
            ..Default::default()
        };
        let t = std::time::Instant::now();
        let v = shape(text, &opts, &mut TextMeasure { engine: &mut engine, style: style.clone(), dpi: 72.0 });
        println!("--- {:?} {} variants in {:?}", opts.hyphenation, v.len(), t.elapsed());
        for x in v.iter().take(4) {
            println!("  score {:.1} fits {:?} hy {} : {:?}", x.score, x.fits, x.hyphens, x.lines);
        }
        let v = shape(text, &Options { hyphenation: Hyphenation::for_text(None, text), ..Default::default() }, &mut CharClass);
        println!("  (no bubble) {:?}", v.first().map(|x| &x.lines));
    }
}
