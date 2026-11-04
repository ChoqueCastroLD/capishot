use super::*;

/// Every built-in theme is a complete stylesheet: it reads with no warnings, sets every token
/// that has no automatic value, and writes back as itself.
#[test]
fn built_in_themes_are_complete_stylesheets() {
    for kind in ThemeKind::ALL {
        let text = super::super::builtin_css(kind);
        let p = parse_builtin(text, kind);
        assert!(p.warnings.is_empty(), "{kind:?}: {:?}", p.warnings);
        let mut t = p.tokens;
        for tok in TOKENS.iter() {
            let optional = matches!((tok.slot)(&mut t), Slot::Auto(_) | Slot::AutoNum(..));
            assert!(optional || p.set.contains(&tok.name), "{kind:?} doesn't set --{}", tok.name);
        }
        assert_eq!(p.tokens.kind, kind);
        assert_eq!(write(&p.tokens), text, "{kind:?} is in canonical form");
        assert_eq!(Tokens::for_kind(kind), p.tokens);
        // A user's stylesheet with only `--base` is that theme.
        assert_eq!(parse(&format!("--base: {};", kind.id())).tokens, p.tokens, "{kind:?}");
    }
}

/// Every field of `Tokens` has a theme.css property. Adding a field without one fails to compile
/// here (the destructuring is exhaustive) or fails the lookup.
#[test]
fn every_token_field_is_reachable_from_css() {
    let Tokens {
        kind: _,
        chrome: _,
        canvas: _,
        canvas_dot: _,
        dock: _,
        card: _,
        card_border: _,
        field: _,
        field_border: _,
        hover: _,
        pressed: _,
        text: _,
        text_dim: _,
        text_faint: _,
        icon: _,
        accent: _,
        accent_soft: _,
        accent_border: _,
        accent_text: _,
        separator: _,
        shadow: _,
        primary_bg: _,
        primary_text: _,
        danger: _,
        warning: _,
        radius_sm: _,
        radius: _,
        radius_lg: _,
        bevel: _,
        pro: _,
        round_chips: _,
        tab_strip: _,
        row_selected: _,
        histogram_bg: _,
        histogram_level: _,
        caption_close: _,
        caption_close_text: _,
        scrim: _,
        dark: _,
        style:
            crate::theme::StyleTokens {
                font_small: _,
                font_body: _,
                font_button: _,
                font_heading: _,
                font_mono: _,
                item_spacing: _,
                button_padding: _,
                interact_size: _,
                slider_width: _,
                combo_width: _,
                menu_margin: _,
                window_margin: _,
                icon_width: _,
                tooltip_width: _,
                scroll_width: _,
                window_shadow: _,
                popup_shadow: _,
                selection_bg: _,
                selection_stroke: _,
                widgets: crate::theme::WidgetStates { label: _, normal: _, hover: _, active: _, open: _ },
            },
    } = Tokens::for_kind(ThemeKind::Pro);
    let crate::theme::WidgetTokens { bg: _, border: _, fg: _, border_width: _, fg_width: _, radius: _, expansion: _ } = WidgetTokens::default();
    // 38 plain fields + `dark` + 19 style values + 5 widget states × 7.
    assert_eq!(TOKENS.len(), 39 + 19 + 5 * 7);
    // Each property changes its own value and no other: no two share a field.
    let base = Tokens::for_kind(ThemeKind::Studio);
    let mut seen: Vec<Tokens> = Vec::new();
    for tok in TOKENS.iter() {
        let mut t = base;
        let v = match (tok.slot)(&mut t) {
            Slot::Color(_) | Slot::Auto(_) => "#123457".to_string(),
            Slot::Num(x, min, max) => num_text(if (*x - max).abs() < 0.5 { min } else { max }),
            Slot::Pair(p, min, max) => num_text(if (p.x - max).abs() < 0.5 { min } else { max }),
            Slot::AutoNum(_, _, max) => num_text(max),
            Slot::Byte(b) => (if *b == 7 { 8 } else { 7 }).to_string(),
            Slot::Bool(b) => (!*b).to_string(),
            Slot::Shadow(_) => "3 4 5 6 #010203".into(),
            Slot::Base(_) => "classic".into(),
        };
        let changed = parse(&format!("--base: studio; --{}: {v};", tok.name)).tokens;
        assert_ne!(changed, base, "--{}: {v} changed nothing", tok.name);
        assert!(!seen.contains(&changed) || tok.name == "base", "--{} changes the same thing as another property", tok.name);
        seen.push(changed);
    }
}

#[test]
fn scoped_blocks_are_the_flat_properties() {
    let scoped = parse(":root { --base: pro; }\nbutton:hover { --bg: #102030; --border-width: 2px; }\nselection { --bg: auto; }\nlabel { --radius: 3; }");
    let flat = parse("--base: pro; --button-hover-bg: #102030; --button-hover-border-width: 2; --label-radius: 3;");
    assert!(scoped.warnings.is_empty(), "{:?}", scoped.warnings);
    assert_eq!(scoped.tokens, flat.tokens);
    assert_eq!(scoped.tokens.style.widgets.hover.bg, Some(Color32::from_rgb(16, 32, 48)));
    assert_eq!(scoped.tokens.style.widgets.label.radius, Some(3.0));
}

#[test]
fn colours_numbers_and_shadows_read() {
    assert_eq!(parse_color("#fff"), Some(Color32::WHITE));
    assert_eq!(parse_color("#0008"), Some(Color32::from_rgba_unmultiplied(0, 0, 0, 0x88)));
    assert_eq!(parse_color("#11223344"), Some(Color32::from_rgba_unmultiplied(0x11, 0x22, 0x33, 0x44)));
    assert_eq!(parse_color("transparent"), Some(Color32::TRANSPARENT));
    for bad in ["red", "#12", "#gggggg", "#1234567", "rgb(1,2,3)", "", "#"] {
        assert_eq!(parse_color(bad), None, "{bad}");
    }
    let p = parse("--window-shadow: 0 -4 12 #00000080; --item-spacing: 3; --radius: 900; --font-body: 13px;");
    assert_eq!(p.tokens.style.window_shadow, ShadowTokens { offset: [0, -4], blur: 12, spread: 0, color: Some(Color32::from_rgba_unmultiplied(0, 0, 0, 128)) });
    assert_eq!(p.tokens.style.item_spacing, Vec2::splat(3.0));
    assert_eq!(p.tokens.radius, 64.0, "clamped");
    assert_eq!(p.tokens.style.font_body, 13.0);
    assert_eq!(p.warnings.len(), 1, "the clamp is reported: {:?}", p.warnings);
}

/// Anything that isn't a theme property is skipped with a warning, and nothing else changes.
#[test]
fn anything_else_is_ignored_with_a_warning() {
    let hostile = r#"
        @import url("https://example.org/evil.css");
        body { background: url(javascript:alert(1)); }
        :root { --accent: url(https://example.org/x.png); --nope: 1; color: red; --radius: abc; }
        button:hover { button { --bg: #fff; } }
        }
        --text: #ffffff
    "#;
    let p = parse(hostile);
    assert!(p.warnings.len() >= 6, "{:#?}", p.warnings);
    let mut expected = Tokens::for_kind(ThemeKind::Studio);
    expected.text = Color32::WHITE;
    assert_eq!(p.tokens, expected, "only the valid trailing property applies");
    assert!(parse("/* never closed").warnings.iter().any(|w| w.contains("comment")));
}

/// Random and mutated stylesheets never panic.
#[test]
fn hostile_stylesheets_never_panic() {
    let seed_text = write(&Tokens::for_kind(ThemeKind::Classic));
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    const ALPHABET: &[u8] = b"{}:;-#/*! \n\tabcdefxyz0123456789.()\"'@\\\xc3\xa9";
    for _ in 0..400 {
        let mut b = seed_text.clone().into_bytes();
        for _ in 0..1 + next() % 30 {
            let i = (next() as usize) % b.len();
            b[i] = ALPHABET[(next() as usize) % ALPHABET.len()];
        }
        let _ = parse(&String::from_utf8_lossy(&b));
        let random: String = (0..(next() % 400)).map(|_| ALPHABET[(next() as usize) % ALPHABET.len()] as char).collect();
        let _ = parse(&random);
    }
    // Very long and deeply repetitive input stays bounded.
    let long = "--accent: #fff;".repeat(10_000);
    assert!(parse(&long).warnings.iter().any(|w| w.contains("more than")));
    let _ = parse(&"{".repeat(50_000));
    let _ = parse(&"/*".repeat(50_000));
}

#[test]
fn only_the_changes_are_written_and_read_back() {
    let mut t = Tokens::for_kind(ThemeKind::BreezeDark);
    t.accent = Color32::from_rgb(255, 136, 0);
    t.radius = 2.0;
    t.style.widgets.active.bg = Some(Color32::from_rgb(1, 2, 3));
    t.style.popup_shadow.color = Some(Color32::BLACK);
    let text = write_changes(&t);
    assert!(text.contains("--base: breezeDark;") && text.contains("--accent: #ff8800;") && text.contains("button:active {"), "{text}");
    assert_eq!(text.lines().filter(|l| l.contains("--")).count(), 5, "{text}");
    assert_eq!(parse(&text).tokens, t);
    assert_eq!(parse(&write(&t)).tokens, t, "the full form reads back too");
}

