use super::super::css;
use super::super::{ThemeKind, apply};
use super::*;

const PATTERNED: [ThemeKind; 4] = [ThemeKind::MangaLight, ThemeKind::MangaDark, ThemeKind::CapibaraLight, ThemeKind::CapibaraDark];

fn luminance(c: Color32) -> f64 {
    let f = |v: u8| {
        let s = f64::from(v) / 255.0;
        if s <= 0.03928 { s / 12.92 } else { ((s + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
}

fn contrast(a: Color32, b: Color32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// `over` at its opacity on the opaque `under`: a pattern's full ink, the worst a label meets.
fn ink(under: Color32, over: Color32, opacity: f32) -> Color32 {
    let ch = |o: u8, u: u8| (f32::from(o) * opacity + f32::from(u) * (1.0 - opacity)).round() as u8;
    Color32::from_rgb(ch(over.r(), under.r()), ch(over.g(), under.g()), ch(over.b(), under.b()))
}

#[test]
fn the_new_themes_parse_equal_and_write_back_as_themselves() {
    for kind in PATTERNED {
        let text = super::super::builtin_css(kind);
        let read = css::parse(text);
        assert!(read.warnings.is_empty(), "{kind:?}: {:?}", read.warnings);
        assert_eq!(read.tokens, Tokens::for_kind(kind), "{kind:?}");
        assert_eq!(css::write(&read.tokens), text, "{kind:?}");
        // What the editor saves on top of a new theme reads back the same.
        let changed = css::parse(&format!("--base: {}; --pattern: lines; --bar-pattern: none; --pattern-size: 9; --pattern-color: auto;", kind.id())).tokens;
        assert_eq!(css::parse(&css::write_changes(&changed)).tokens, changed, "{kind:?}");
        assert_eq!(css::parse(&css::write(&changed)).tokens, changed, "{kind:?}");
    }
}

#[test]
fn the_new_themes_pair_up_and_keep_their_shape() {
    for (light, dark) in [(ThemeKind::MangaLight, ThemeKind::MangaDark), (ThemeKind::CapibaraLight, ThemeKind::CapibaraDark)] {
        let (l, d) = (Tokens::for_kind(light), Tokens::for_kind(dark));
        assert!(!l.dark() && light.is_light() && d.dark() && dark.is_dark(), "{light:?}/{dark:?}");
        assert!(!l.pro && !l.bevel && !d.pro && !d.bevel);
        assert_eq!((l.radius_sm, l.radius, l.radius_lg), (d.radius_sm, d.radius, d.radius_lg));
    }
    let manga = Tokens::for_kind(ThemeKind::MangaLight);
    assert_eq!((manga.radius_sm, manga.radius, manga.radius_lg), (0.0, 0.0, 0.0), "square corners");
    assert_eq!(manga.pattern, PatternKind::Dots);
    assert_eq!(manga.bar_pattern, PatternKind::Dots);
    assert!(Tokens::for_kind(ThemeKind::CapibaraLight).radius_lg >= 20.0, "very rounded");
    // Round chips stacked on the toolbar would overlap and take a tool's room.
    assert!(!Tokens::for_kind(ThemeKind::CapibaraLight).round_chips && !Tokens::for_kind(ThemeKind::CapibaraDark).round_chips);
    for (name, kind) in [
        ("manga", ThemeKind::MangaLight),
        ("Manga Dark", ThemeKind::MangaDark),
        ("capybara", ThemeKind::CapibaraLight),
        ("capivara dark", ThemeKind::CapibaraDark),
    ] {
        assert_eq!(ThemeKind::from_name(name), Some(kind), "{name}");
    }
}

/// WCAG AA: 4.5:1 for labels on every panel surface, and on a bar where a label lands right on the
/// pattern's ink.
#[test]
fn text_on_the_new_themes_is_aa_even_on_the_pattern() {
    for kind in PATTERNED {
        let t = Tokens::for_kind(kind);
        let bar = t.surface_pattern(Surface::Bar).map_or(t.chrome, |p| ink(t.chrome, p.color, p.opacity));
        for (surface, name) in
            [(t.chrome, "chrome"), (t.dock, "dock"), (t.card, "card"), (t.field, "field"), (t.hover, "hover"), (t.pressed, "pressed"), (bar, "patterned bar")]
        {
            for (colour, what) in [(t.text, "text"), (t.text_dim, "dim"), (t.icon, "icon")] {
                assert!(contrast(colour, surface) >= 4.5, "{kind:?}: {what} on {name}: {:.2}", contrast(colour, surface));
            }
        }
        for (surface, name) in [(t.chrome, "chrome"), (t.dock, "dock"), (t.card, "card"), (bar, "patterned bar")] {
            for (colour, what) in [(t.text_faint, "faint"), (t.accent_text, "accent text"), (t.danger, "danger"), (t.warning, "warning")] {
                assert!(contrast(colour, surface) >= 4.5, "{kind:?}: {what} on {name}: {:.2}", contrast(colour, surface));
            }
        }
        assert!(contrast(t.primary_text, t.primary_bg) >= 4.5, "{kind:?}: primary button");
    }
}

/// The surround stays nearly neutral, so it doesn't colour how a page's colours read. The colour
/// lives in the interface around it.
#[test]
fn the_canvas_surround_of_the_new_themes_is_close_to_neutral() {
    for kind in PATTERNED {
        let t = Tokens::for_kind(kind);
        let surround = t.surface_pattern(Surface::Canvas).map_or(t.canvas, |p| ink(t.canvas, p.color, p.opacity * 0.2));
        let (hi, lo) = (surround.r().max(surround.g()).max(surround.b()), surround.r().min(surround.g()).min(surround.b()));
        assert!(hi - lo <= 28, "{kind:?}: {surround:?}");
    }
}

#[test]
fn pattern_properties_are_strict() {
    let good = css::parse(
        "--base: studio; --pattern: LINES; --pattern-size: 12px; --pattern-opacity: 0.5; --bar-pattern: dots; --bar-pattern-opacity: 0.1; --pattern-color: #123456;",
    );
    assert!(good.warnings.is_empty(), "{:?}", good.warnings);
    assert_eq!((good.tokens.pattern, good.tokens.pattern_size, good.tokens.pattern_opacity), (PatternKind::Lines, Some(12.0), Some(0.5)));
    for bad in
        ["--pattern: url(evil.png);", "--pattern: stripes;", "--bar-pattern: 3;", "--pattern-size: big;", "--pattern-opacity: nan;", "--pattern-color: red;"]
    {
        let p = css::parse(&format!("--base: studio; {bad}"));
        assert_eq!(p.warnings.len(), 1, "{bad}: {:?}", p.warnings);
        assert_eq!(p.tokens, Tokens::for_kind(ThemeKind::Studio), "{bad} is ignored");
    }
    // Out of range: clamped, with a warning. A bar can't take a pattern that drowns its labels.
    let p = css::parse("--pattern-size: 1000; --pattern-opacity: 7; --bar-pattern-size: 0; --bar-pattern-opacity: 1;");
    assert_eq!(p.warnings.len(), 4, "{:?}", p.warnings);
    assert_eq!((p.tokens.pattern_size, p.tokens.pattern_opacity), (Some(64.0), Some(1.0)));
    assert_eq!((p.tokens.bar_pattern_size, p.tokens.bar_pattern_opacity), (Some(2.0), Some(MAX_BAR_OPACITY)));
    // `none` undoes a base's pattern, and the changes written say so.
    let plain = css::parse("--base: mangaLight; --pattern: none; --bar-pattern: none;");
    assert!(plain.warnings.is_empty(), "{:?}", plain.warnings);
    assert_eq!(plain.tokens.surface_pattern(Surface::Canvas), None);
    assert!(css::write_changes(&plain.tokens).contains("--pattern: none;"));
    assert_eq!(css::parse(&css::write_changes(&plain.tokens)).tokens, plain.tokens);
    assert!(css::write(&plain.tokens).contains("--bar-pattern: none;"));
    assert_eq!(css::parse(&css::write(&plain.tokens)).tokens, plain.tokens);
}

#[test]
fn a_tile_is_seamless_and_shows_its_pattern() {
    for kind in [PatternKind::Dots, PatternKind::Lines] {
        for side in [2usize, 5, 6, 16, 256] {
            let c = coverage(kind, side);
            assert_eq!(c.len(), side * side);
            if side >= 5 {
                assert!(c.iter().any(|v| *v > 100) && c.iter().any(|v| *v < 100), "{kind:?} {side}: flat");
            }
        }
    }
    // Hatching's wrap: the coverage repeats along the tile's edges.
    let side = 16;
    let c = coverage(PatternKind::Lines, side);
    for y in 0..side {
        for x in 0..side {
            assert_eq!(c[y * side + x], c[(y + 1) % side * side + (x + side - 1) % side], "({x},{y})");
        }
    }
    assert_eq!(coverage(PatternKind::None, 4), vec![0; 16]);
}

#[test]
fn a_tile_is_uploaded_once_and_only_when_a_theme_has_one() {
    let uploaded = |ctx: &Context| ctx.tex_manager().read().allocated().filter(|(_, meta)| meta.name == "theme-pattern").count();
    let ctx = Context::default();
    apply(&ctx, ThemeKind::Studio);
    let frame = |ctx: &Context| {
        ctx.run_ui(Default::default(), |ui| {
            paint_bar(ui);
            paint_bar(ui);
        })
        .textures_delta
        .clear();
    };
    frame(&ctx);
    assert_eq!(uploaded(&ctx), 0, "no pattern, no texture");
    apply(&ctx, ThemeKind::MangaLight);
    for _ in 0..50 {
        frame(&ctx);
    }
    assert_eq!(uploaded(&ctx), 1, "one upload across the frames");
    // Tinted at draw time: another colour and opacity reuses the texture.
    let p = Tokens::for_kind(ThemeKind::MangaLight).surface_pattern(Surface::Bar).unwrap();
    let again = tile(&ctx, &SurfacePattern { color: Color32::RED, opacity: 0.1, ..p }).unwrap();
    assert_eq!(again.texture, tile(&ctx, &p).unwrap().texture);
    // Editing the size doesn't pile textures up.
    for size in 2..64 {
        tile(&ctx, &SurfacePattern { size: size as f32, ..p });
    }
    assert!(uploaded(&ctx) <= MAX_TILES, "{}", uploaded(&ctx));
}

#[test]
fn hostile_pattern_sizes_are_harmless() {
    let ctx = Context::default();
    let p = SurfacePattern { kind: PatternKind::Dots, size: f32::NAN, opacity: 0.5, color: Color32::WHITE };
    assert_eq!(tile(&ctx, &p), None);
    assert_eq!(tile(&ctx, &SurfacePattern { size: f32::INFINITY, ..p }), None);
    assert_eq!(tile(&ctx, &SurfacePattern { kind: PatternKind::None, size: 6.0, ..p }), None);
    let huge = tile(&ctx, &SurfacePattern { size: 1e9, ..p }).unwrap();
    assert!(huge.cell <= MAX_TILE_PX);
    assert_eq!(tile(&ctx, &SurfacePattern { size: -3.0, ..p }).unwrap().cell, 2.0);
}
