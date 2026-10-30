//! Capishot's additions to the layout: type in a path (`TextShape::InPath`: lines in bands of
//! the path, the block centred) and automatic hyphenation of paragraph text.

use parley::{FontData, Layout, PositionedLayoutItem};
use photocraft_doc::text::CharStyle;

use super::RunBrush;

/// What a type-in-a-path pass fitted ([`Layouter::layout`]).
#[derive(Default)]
pub(super) struct Fit {
    /// Some text had no band left.
    pub(super) overflow: bool,
    /// The top of the first line's band and the bottom of the last one's.
    pub(super) band: Option<(f32, f32)>,
}

/// The widest whitespace-separated word of a laid-out paragraph (`text` is what it lays out).
pub(super) fn longest_unbreakable(layout: &Layout<RunBrush>, text: &str) -> f32 {
    let (mut best, mut word) = (0.0f32, 0.0f32);
    for line in layout.lines() {
        for run in line.runs() {
            for c in run.clusters() {
                if text.get(c.text_range()).is_none_or(|s| s.chars().any(char::is_whitespace)) {
                    word = 0.0;
                } else {
                    word += c.advance();
                    best = best.max(word);
                }
            }
        }
    }
    best
}

/// The ink of a laid-out paragraph's glyphs: how far above and below the baseline it reaches and
/// how far past a glyph's advance it sticks out on either side, with scale, baseline shift and
/// faux styles. `None` without glyph ink.
pub(super) fn ink_extent(layout: &Layout<RunBrush>, styles: &[CharStyle], k: f32) -> Option<(f32, f32, f32)> {
    use skrifa::MetadataProvider;
    let (mut up, mut down, mut over) = (f32::NEG_INFINITY, f32::NEG_INFINITY, 0.0f32);
    for line in layout.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(gr) = item else { continue };
            let run = gr.run();
            let fd = run.font();
            let Ok(font) = skrifa::FontRef::from_index(fd.data.as_ref(), fd.index) else { continue };
            let coords: Vec<skrifa::instance::NormalizedCoord> =
                run.normalized_coords().iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
            let gm = font.glyph_metrics(skrifa::instance::Size::new(run.font_size()), skrifa::instance::LocationRef::new(&coords));
            let st = styles.get(gr.style().brush.0 as usize);
            let scale = |v: Option<f32>| v.filter(|v| *v > 0.0).unwrap_or(1.0);
            let hs = scale(st.map(|s| s.horizontal_scale));
            let vs = scale(st.map(|s| s.vertical_scale));
            let synth = run.synthesis();
            let skew_deg = if st.is_some_and(|s| s.faux_italic) { crate::render::FAUX_ITALIC_DEG } else { 0.0 } + synth.skew().unwrap_or(0.0);
            let skew = skew_deg.to_radians().tan() * vs;
            let shift = st.map_or(0.0, |s| s.baseline_shift_pt * k);
            // Faux bold draws the outline again a pixel off in every direction.
            let bold = if st.is_some_and(|s| s.faux_bold) || synth.embolden() { 1.0 } else { 0.0 };
            for g in gr.glyphs() {
                let Some(b) = gm.bounds(skrifa::GlyphId::new(g.id)) else { continue };
                if b.x_max <= b.x_min || b.y_max <= b.y_min {
                    continue;
                }
                up = up.max(b.y_max * vs + shift - g.y + bold);
                down = down.max(-b.y_min * vs - shift + g.y + bold);
                let (s0, s1) = (skew * b.y_min, skew * b.y_max);
                let left = g.x + hs * b.x_min + s0.min(s1) - bold;
                let right = g.x + hs * b.x_max + s0.max(s1) + bold;
                over = over.max(-left).max(right - g.advance * hs);
            }
        }
    }
    (up.is_finite() && down.is_finite()).then_some((up.max(0.0), down.max(0.0), over))
}

/// A line that ends inside a word with an automatic hyphen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct HyphenBreak {
    /// Line index in the paragraph layout.
    pub(super) line: usize,
    /// Clusters on the line (the line breaker's `break_next_with_length`).
    clusters: u32,
    /// Hyphen advance (px, before horizontal scale), kept free at the line end for alignment.
    advance: f32,
}

pub(super) struct HyphenParams {
    pub(super) rules: crate::hyphen::Rules,
    /// The hyphenation zone (px); unjustified lines with a smaller end gap aren't hyphenated.
    pub(super) zone_px: f32,
    pub(super) justified: bool,
}

/// One cluster of a line, logical order.
struct HCluster {
    start: usize,
    advance: f32,
    ligature_continuation: bool,
    run: usize,
}

/// Breaks the paragraph's lines again with automatic hyphenation, Photoshop's single-line
/// composer way: line by line, when the first word of the next line has a pattern break point
/// whose head plus a hyphen fits in the room left on this line (and, in unjustified text, that
/// room is at least the hyphenation zone), the head moves up. At most `rules.limit` consecutive
/// lines end in a hyphen. Left-to-right text only. Returns the hyphenated lines; without any the
/// layout's lines are exactly the plain ones.
pub(super) fn hyphenate_paragraph(
    layout: &mut Layout<RunBrush>,
    ptext: &str,
    avail: f32,
    first_indent: f32,
    params: &HyphenParams,
    lang_at: &dyn Fn(usize) -> Option<crate::hyphen::HyphLang>,
) -> Vec<HyphenBreak> {
    let mut breaks: Vec<HyphenBreak> = Vec::new();
    if layout.is_rtl() || !avail.is_finite() || avail <= 0.0 {
        return breaks;
    }
    // One pass per line; a hyphen break re-breaks the lines after it. Capped for safety.
    for li in 0..4096usize {
        if li + 1 >= layout.len() {
            break;
        }
        let consecutive = (0..li).rev().take_while(|&l| breaks.iter().any(|b| b.line == l)).count();
        let limited = params.rules.limit > 0 && consecutive >= params.rules.limit;
        let found = if limited { None } else { line_hyphen_break(layout, ptext, li, avail, first_indent, params, lang_at) };
        if let Some(b) = found {
            breaks.push(b);
            rebreak(layout, avail, &breaks);
        }
    }
    breaks
}

/// The hyphen break that pulls the head of line `li + 1`'s first word onto line `li`, if any.
fn line_hyphen_break(
    layout: &Layout<RunBrush>,
    ptext: &str,
    li: usize,
    avail: f32,
    first_indent: f32,
    params: &HyphenParams,
    lang_at: &dyn Fn(usize) -> Option<crate::hyphen::HyphLang>,
) -> Option<HyphenBreak> {
    let line = layout.get(li)?;
    let next = layout.get(li + 1)?;
    if line.break_reason() != parley::BreakReason::Regular || line.runs().any(|r| r.is_rtl()) || next.runs().any(|r| r.is_rtl()) {
        return None;
    }
    let m = line.metrics();
    let line_avail = avail - if li == 0 { first_indent } else { 0.0 };
    let gap = line_avail - (m.advance - m.trailing_whitespace);
    if !params.justified && gap < params.zone_px {
        return None;
    }
    // The next line's first word: after opening punctuation, a run of letters that isn't glued to
    // digits or more letters through other characters.
    let nr = next.text_range();
    let s = ptext.get(nr.clone())?;
    let lead = s.char_indices().find(|(_, c)| crate::hyphen::is_word_char(*c) || c.is_whitespace() || c.is_numeric()).map_or(s.len(), |(i, _)| i);
    let rest = s.get(lead..)?;
    let wlen = rest.char_indices().find(|(_, c)| !crate::hyphen::is_word_char(*c)).map_or(rest.len(), |(i, _)| i);
    if wlen == 0 || rest.get(wlen..).and_then(|t| t.chars().next()).is_some_and(char::is_alphanumeric) {
        return None;
    }
    let word = rest.get(..wlen)?;
    let word_start = nr.start + lead;
    let lang = lang_at(word_start)?;
    let points = crate::hyphen::break_points(word, lang, &params.rules);
    if points.is_empty() {
        return None;
    }
    let mut clusters: Vec<HCluster> = Vec::new();
    let mut runs: Vec<parley::Run<'_, RunBrush>> = Vec::new();
    for run in next.runs() {
        let ri = runs.len();
        for c in run.clusters() {
            clusters.push(HCluster { start: c.text_range().start, advance: c.advance(), ligature_continuation: c.is_ligature_continuation(), run: ri });
        }
        runs.push(run);
    }
    let on_line: usize = line.runs().map(|r| r.cluster_range().len()).sum();
    for &p in points.iter().rev() {
        let at = word_start + p;
        let Some(idx) = clusters.iter().position(|c| c.start == at) else { continue };
        if clusters.get(idx).is_none_or(|c| c.ligature_continuation) || idx == 0 {
            continue;
        }
        let Some(before) = clusters.get(idx - 1) else { continue };
        let Some(run) = runs.get(before.run) else { continue };
        let Some((_, hyphen)) = hyphen_glyph(run.font(), run.normalized_coords(), run.font_size()) else { continue };
        let head: f32 = clusters.get(..idx).map_or(0.0, |c| c.iter().map(|c| c.advance).sum());
        if m.advance + head + hyphen <= line_avail + 1e-3 {
            let count = u32::try_from(on_line + idx).ok()?;
            return Some(HyphenBreak { line: li, clusters: count, advance: hyphen });
        }
    }
    None
}

/// Breaks the layout's lines at `avail`, ending the lines in `breaks` after their cluster count.
fn rebreak(layout: &mut Layout<RunBrush>, avail: f32, breaks: &[HyphenBreak]) {
    let mut bl = layout.break_lines();
    let st = bl.state_mut();
    st.set_layout_max_advance(avail);
    st.set_line_max_advance(avail);
    let mut li = 0usize;
    loop {
        let more = match breaks.iter().find(|b| b.line == li) {
            Some(b) => {
                let r = bl.break_next_with_length(b.clusters).is_some();
                // Align the line as if the box were narrower by the hyphen it ends with.
                bl.set_prior_line_width((avail - b.advance).max(0.0));
                r
            }
            None => bl.break_next().is_some(),
        };
        if !more || bl.is_done() {
            break;
        }
        li += 1;
    }
    bl.finish();
}

/// The hyphen glyph of a font instance and its advance (px): `-`, else U+2010.
pub(super) fn hyphen_glyph(font: &FontData, coords: &[i16], size: f32) -> Option<(u32, f32)> {
    use skrifa::MetadataProvider;
    let f = skrifa::FontRef::from_index(font.data.as_ref(), font.index).ok()?;
    let gid = f.charmap().map('-').or_else(|| f.charmap().map('\u{2010}'))?;
    let coords: Vec<skrifa::instance::NormalizedCoord> = coords.iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
    let advance = f.glyph_metrics(skrifa::instance::Size::new(size), skrifa::instance::LocationRef::new(&coords)).advance_width(gid)?;
    advance.is_finite().then_some((gid.to_u32(), advance))
}
