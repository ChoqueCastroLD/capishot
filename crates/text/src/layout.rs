//! Shaping and layout of a text layer's model into positioned glyphs (text-space pixels, y down,
//! first baseline of point text at y = 0).
//!
//! Each paragraph is shaped and line-broken by parley (HarfRust shaping, Unicode bidi and line
//! breaking); paragraphs are then stacked with Photoshop's rules: baseline-to-baseline distance =
//! the largest leading on the line (auto leading = paragraph factor × size), space before/after,
//! indents, and point-text alignment around the anchor.

use std::borrow::Cow;
use std::ops::Range;

use parley::{
    Alignment, AlignmentOptions, FontData, FontFamily, FontFeatures, FontStyle, FontVariations, FontWeight, IndentOptions, Layout, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};
use photocraft_doc::TextLayer;
use photocraft_doc::text::{Caps, CharStyle, Kerning, TextAlign, TextDirection, TextShape};

use crate::fonts::FontDb;

/// Index of the character run whose style a glyph uses.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RunBrush(pub u32);

/// A font instance used by some glyphs.
#[derive(Clone, Debug)]
pub struct GlyphFace {
    pub font: FontData,
    /// Normalised variation coordinates (F2Dot14 bits).
    pub coords: Vec<i16>,
    pub size_px: f32,
    /// Synthetic bold requested by font matching (face lacks the weight).
    pub embolden: bool,
    /// Synthetic oblique angle in degrees (face lacks italics).
    pub skew_deg: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlacedGlyph {
    pub face: u32,
    pub id: u32,
    /// Pen position (text space px): x along the baseline, y = baseline (before baseline shift).
    pub x: f32,
    pub y: f32,
    /// Character run index (into [`TextLayout::styles`]).
    pub style: u32,
}

/// Underline/strikethrough rectangle (text space px).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DecorationRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub style: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LineInfo {
    /// Byte range in the layer text (without the paragraph break).
    pub range: Range<usize>,
    pub baseline: f32,
    /// Visual extent along the baseline.
    pub x0: f32,
    pub x1: f32,
    pub ascent: f32,
    pub descent: f32,
    pub paragraph: usize,
}

/// A grapheme cluster (caret stops, hit testing, selection).
#[derive(Clone, Debug, PartialEq)]
pub struct ClusterInfo {
    pub range: Range<usize>,
    pub x: f32,
    pub advance: f32,
    pub line: usize,
    pub rtl: bool,
}

/// Result of laying out a text layer.
#[derive(Clone, Debug, Default)]
pub struct TextLayout {
    pub faces: Vec<GlyphFace>,
    pub glyphs: Vec<PlacedGlyph>,
    pub decorations: Vec<DecorationRect>,
    pub lines: Vec<LineInfo>,
    pub clusters: Vec<ClusterInfo>,
    /// Resolved character styles (one per run of the layer).
    pub styles: Vec<CharStyle>,
    /// Pixels per point used (dpi / 72).
    pub px_per_pt: f32,
}

impl TextLayout {
    /// Logical bounds (x0, y0, x1, y1) of all lines, text space.
    pub fn bounds(&self) -> Option<[f32; 4]> {
        self.lines.iter().fold(None, |acc, l| {
            let r = [l.x0, l.baseline - l.ascent, l.x1, l.baseline + l.descent];
            Some(match acc {
                None => r,
                Some(a) => [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])],
            })
        })
    }

    /// Byte offset of the caret nearest to a text-space point.
    pub fn hit_test(&self, x: f32, y: f32) -> usize {
        let Some((li, line)) = self.lines.iter().enumerate().min_by(|a, b| {
            let d = |l: &LineInfo| {
                if y < l.baseline - l.ascent {
                    l.baseline - l.ascent - y
                } else if y > l.baseline + l.descent {
                    y - l.baseline - l.descent
                } else {
                    0.0
                }
            };
            d(a.1).total_cmp(&d(b.1))
        }) else {
            return 0;
        };
        let mut best = (f32::MAX, line.range.end);
        for c in self.clusters.iter().filter(|c| c.line == li) {
            let mid = c.x + c.advance / 2.0;
            let (before, after) = if c.rtl { (c.range.end, c.range.start) } else { (c.range.start, c.range.end) };
            let (dist, off) = if x < mid { ((x - c.x).abs(), before) } else { ((x - c.x - c.advance).abs(), after) };
            if dist < best.0 {
                best = (dist, off);
            }
        }
        best.1
    }

    /// Caret geometry for a byte offset: (x, top, bottom) in text space.
    pub fn caret(&self, offset: usize) -> (f32, f32, f32) {
        for c in &self.clusters {
            if c.range.start == offset {
                let l = &self.lines[c.line];
                let x = if c.rtl { c.x + c.advance } else { c.x };
                return (x, l.baseline - l.ascent, l.baseline + l.descent);
            }
        }
        // End of a line (or empty line): after the last cluster of the line containing it.
        let li = self.lines.iter().position(|l| offset >= l.range.start && offset <= l.range.end).unwrap_or(self.lines.len().saturating_sub(1));
        match self.lines.get(li) {
            Some(l) => {
                let x = self
                    .clusters
                    .iter()
                    .filter(|c| c.line == li && c.range.end == offset)
                    .map(|c| if c.rtl { c.x } else { c.x + c.advance })
                    .next()
                    .unwrap_or(l.x1);
                (x, l.baseline - l.ascent, l.baseline + l.descent)
            }
            None => (0.0, 0.0, 0.0),
        }
    }
}

const LRM: &str = "\u{200E}";
const RLM: &str = "\u{200F}";

pub(crate) struct Layouter {
    lcx: LayoutContext<RunBrush>,
}

impl Layouter {
    pub fn new() -> Self {
        Self { lcx: LayoutContext::new() }
    }

    pub fn layout(&mut self, fonts: &mut FontDb, t: &TextLayer, dpi: f32) -> TextLayout {
        let k = if dpi > 0.0 { dpi / 72.0 } else { 1.0 };
        let runs = t.char_runs();
        let paras = t.paragraph_runs();
        let mut out = TextLayout { px_per_pt: k, ..Default::default() };
        // Resolve families (PostScript names from PSDs, unknown families).
        for r in &runs {
            let mut s = r.style.clone();
            if let Some(ps) = s.postscript_name.clone() {
                let f = fonts.resolve_postscript(&ps);
                // An exact face match wins; a guessed family only fills a missing family.
                if f.exact || (!fonts.has_family(&s.font_family) && fonts.has_family(&f.family)) {
                    s.font_family = f.family;
                    s.weight = f.weight;
                    s.italic = f.italic;
                }
            }
            out.styles.push(s);
        }
        let run_starts: Vec<usize> = runs
            .iter()
            .scan(0, |a, r| {
                let s = *a;
                *a += r.len;
                Some(s)
            })
            .collect();
        let style_at = |off: usize| run_starts.iter().rposition(|&s| s <= off).unwrap_or(0);
        let para_starts: Vec<usize> = paras
            .iter()
            .scan(0, |a, r| {
                let s = *a;
                *a += r.len;
                Some(s)
            })
            .collect();
        let para_style_at = |off: usize| &paras[para_starts.iter().rposition(|&s| s <= off).unwrap_or(0)].style;

        let text = &t.text;
        let (box_rect, is_box) = match t.shape {
            TextShape::Box { x, y, width, height } => ((x, y, width, height), true),
            TextShape::Point => ((0.0, 0.0, 0.0, 0.0), false),
        };
        let mut prev_baseline: Option<f32> = None;
        let mut pending_space = 0.0f32;
        let mut stop = false;
        for (pi, prange) in split_paragraphs(text).into_iter().enumerate() {
            if stop {
                break;
            }
            let ps = para_style_at(prange.start).clone();
            let content_end = strip_break(text, &prange);
            let content = &text[prange.start..content_end];
            let prefix = match ps.direction {
                TextDirection::Auto => "",
                TextDirection::Ltr => LRM,
                TextDirection::Rtl => RLM,
            };
            let mut ptext = String::with_capacity(prefix.len() + content.len());
            ptext.push_str(prefix);
            for (i, ch) in content.char_indices() {
                let caps = out.styles[style_at(prange.start + i)].caps;
                if caps == Caps::AllCaps {
                    let up: String = ch.to_uppercase().collect();
                    if up.len() == ch.len_utf8() {
                        ptext.push_str(&up);
                        continue;
                    }
                }
                ptext.push(ch);
            }
            let first_style = &out.styles[style_at(prange.start)];
            let first_px = first_style.size_pt * k;
            let fallback: Vec<String> = fonts.fallback_stack().map(str::to_string).collect();
            let mut layout: Layout<RunBrush> = {
                let mut b = self.lcx.ranged_builder(&mut fonts.fcx, &ptext, 1.0, false);
                // Paragraph-start style as the default (covers the direction mark and empty
                // paragraphs), then every run piece intersecting this paragraph.
                let si0 = style_at(prange.start);
                for p in style_props(&out.styles[si0], k, &fallback, si0 as u32) {
                    b.push_default(p);
                }
                for (ri, st) in out.styles.iter().enumerate() {
                    let rs = run_starts[ri];
                    let (a, z) = (rs.max(prange.start), (rs + runs[ri].len).min(content_end));
                    if a >= z {
                        continue;
                    }
                    let range = (a - prange.start + prefix.len())..(z - prange.start + prefix.len());
                    for p in style_props(st, k, &fallback, ri as u32) {
                        b.push(p, range.clone());
                    }
                }
                b.build(&ptext)
            };
            let indent_start = ps.start_indent_pt * k;
            let indent_end = ps.end_indent_pt * k;
            if ps.first_line_indent_pt != 0.0 {
                layout.set_text_indent(ps.first_line_indent_pt * k, IndentOptions::default());
            }
            let avail = if is_box { Some((box_rect.2 - indent_start - indent_end).max(1.0)) } else { None };
            layout.break_all_lines(avail);
            let alignment = if is_box {
                match ps.align {
                    TextAlign::Left => Alignment::Left,
                    TextAlign::Center => Alignment::Center,
                    TextAlign::Right => Alignment::Right,
                    _ => Alignment::Justify,
                }
            } else {
                Alignment::Left
            };
            layout.align(alignment, AlignmentOptions { align_when_overflowing: !is_box });

            // Stack lines.
            if prev_baseline.is_some() {
                pending_space += ps.space_before_pt * k;
            }
            let nlines = layout.len();
            for (li, line) in layout.lines().enumerate() {
                let m = *line.metrics();
                // Largest leading among the glyph runs of the line.
                let mut leading = 0.0f32;
                for item in line.items() {
                    if let PositionedLayoutItem::GlyphRun(gr) = item {
                        let st = &out.styles[gr.style().brush.0 as usize];
                        let px = gr.run().font_size();
                        leading = leading.max(st.leading_pt.map_or(ps.auto_leading * px, |l| l * k));
                    }
                }
                if leading == 0.0 {
                    let st = &out.styles[style_at(prange.start)];
                    leading = st.leading_pt.map_or(ps.auto_leading * st.size_pt * k, |l| l * k);
                }
                let (ascent, descent) = if m.ascent > 0.0 || m.descent > 0.0 { (m.ascent, m.descent) } else { (first_px * 0.8, first_px * 0.2) };
                let baseline = match prev_baseline {
                    // Photoshop's "first baseline: ascent": the top of the tallest ascender
                    // (height of 'd') touches the box top, not the font's hhea ascent.
                    None if is_box => box_rect.1 + first_ascent(&line).unwrap_or(ascent),
                    None => 0.0,
                    Some(b) => b + leading + pending_space,
                };
                pending_space = 0.0;
                if is_box && baseline + descent > box_rect.1 + box_rect.3 + 0.5 {
                    stop = true;
                    break;
                }
                prev_baseline = Some(baseline);
                let last_line = li + 1 == nlines;
                let adv = m.advance - m.trailing_whitespace;
                let dx = if is_box {
                    let base = box_rect.0 + indent_start;
                    let slack = avail.unwrap_or(0.0) - adv;
                    base + if last_line {
                        match ps.align {
                            TextAlign::JustifyCenter => slack * 0.5 - m.offset,
                            TextAlign::JustifyRight => slack - m.offset,
                            _ => 0.0,
                        }
                    } else {
                        0.0
                    }
                } else {
                    let target = match ps.align {
                        TextAlign::Center | TextAlign::JustifyCenter => -adv / 2.0,
                        TextAlign::Right | TextAlign::JustifyRight => -adv,
                        _ => indent_start,
                    };
                    target - m.offset
                };
                let justify_all = is_box && last_line && ps.align == TextAlign::JustifyAll;
                let line_index = out.lines.len();
                let map = |o: usize| (prange.start + o.saturating_sub(prefix.len())).min(content_end);
                let lr = line.text_range();
                let g0 = out.glyphs.len();
                let c0 = out.clusters.len();
                let mut extra = 0.0f32; // horizontal-scale growth along the line
                let mut seen_runs: Vec<usize> = Vec::new();
                for item in line.items() {
                    let PositionedLayoutItem::GlyphRun(gr) = item else {
                        continue;
                    };
                    let run = gr.run();
                    let si = gr.style().brush.0;
                    let st = &out.styles[si as usize];
                    let hs = if st.horizontal_scale > 0.0 { st.horizontal_scale } else { 1.0 };
                    let synth = run.synthesis();
                    let face = out.faces.len() as u32;
                    out.faces.push(GlyphFace {
                        font: run.font().clone(),
                        coords: run.normalized_coords().to_vec(),
                        size_px: run.font_size(),
                        embolden: synth.embolden(),
                        skew_deg: synth.skew().unwrap_or(0.0),
                    });
                    let run_x0 = dx + gr.offset() + extra;
                    if !seen_runs.contains(&run.index()) {
                        seen_runs.push(run.index());
                        let mut cx = run_x0;
                        for c in run.visual_clusters() {
                            let a = c.advance() * hs;
                            let r = c.text_range();
                            if r.end > prefix.len() || prefix.is_empty() {
                                out.clusters.push(ClusterInfo { range: map(r.start)..map(r.end), x: cx, advance: a, line: line_index, rtl: c.is_rtl() });
                            }
                            cx += a;
                        }
                    }
                    let mut pen = gr.offset();
                    for g in gr.glyphs() {
                        out.glyphs.push(PlacedGlyph { face, id: g.id, x: dx + pen + g.x + extra, y: baseline + g.y, style: si });
                        extra += g.advance * (hs - 1.0);
                        pen += g.advance;
                    }
                    let run_x1 = dx + gr.offset() + gr.advance() + extra;
                    let rm = run.metrics();
                    let shift = st.baseline_shift_pt * k;
                    if gr.style().underline.is_some() {
                        let y0 = baseline - rm.underline_offset - shift;
                        out.decorations.push(DecorationRect { x0: run_x0, y0, x1: run_x1, y1: y0 + rm.underline_size.max(1.0), style: si });
                    }
                    if gr.style().strikethrough.is_some() {
                        let y0 = baseline - rm.strikethrough_offset - shift;
                        out.decorations.push(DecorationRect { x0: run_x0, y0, x1: run_x1, y1: y0 + rm.strikethrough_size.max(1.0), style: si });
                    }
                }
                if justify_all {
                    let slack = avail.unwrap_or(0.0) - (adv + extra);
                    let n = out.clusters.len() - c0;
                    if n > 1 && slack > 0.0 {
                        let step = slack / (n - 1) as f32;
                        let starts: Vec<f32> = out.clusters[c0..].iter().map(|c| c.x).collect();
                        for (i, c) in out.clusters[c0..].iter_mut().enumerate() {
                            c.x += step * i as f32;
                        }
                        for g in &mut out.glyphs[g0..] {
                            let i = starts.iter().rposition(|&s| s <= g.x + 1e-3).unwrap_or(0);
                            g.x += step * i as f32;
                        }
                        extra += slack;
                    }
                }
                let x0 = dx + m.offset;
                out.lines.push(LineInfo {
                    range: map(lr.start)..map(lr.end).min(content_end),
                    baseline,
                    x0,
                    x1: x0 + adv + extra,
                    ascent,
                    descent,
                    paragraph: pi,
                });
            }
            pending_space += ps.space_after_pt * k;
        }
        out
    }
}

/// Byte ranges of paragraphs (each including its `\r`, `\n` or `\r\n` terminator). Text ending
/// with a break yields a final empty paragraph, like Photoshop's trailing empty line.
pub fn split_paragraphs(text: &str) -> Vec<Range<usize>> {
    let b = text.as_bytes();
    let mut v = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\r' => {
                let end = if b.get(i + 1) == Some(&b'\n') { i + 2 } else { i + 1 };
                v.push(start..end);
                start = end;
                i = end;
            }
            b'\n' => {
                v.push(start..i + 1);
                start = i + 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    v.push(start..b.len());
    v
}

fn strip_break(text: &str, r: &Range<usize>) -> usize {
    let s = &text[r.clone()];
    r.start + s.trim_end_matches(['\r', '\n']).len()
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace(['\\', '"'], ""))
}

fn style_props(st: &CharStyle, k: f32, fallback: &[String], idx: u32) -> Vec<StyleProperty<'static, RunBrush>> {
    let px = (st.size_pt * k).max(0.01);
    let mut fam: Vec<String> = Vec::new();
    if !st.font_family.is_empty() {
        fam.push(quote(&st.font_family));
    }
    fam.extend(fallback.iter().map(|f| quote(f)));
    fam.push("sans-serif".into());
    let mut feats: Vec<String> = Vec::new();
    if st.kerning == Kerning::Off {
        feats.push("\"kern\" 0".into());
    }
    if !st.ligatures {
        feats.push("\"liga\" 0".into());
        feats.push("\"clig\" 0".into());
    }
    if st.discretionary_ligatures {
        feats.push("\"dlig\" 1".into());
    }
    if st.caps == Caps::SmallCaps {
        feats.push("\"smcp\" 1".into());
    }
    for f in &st.features {
        if f.tag.len() == 4 && f.tag.is_ascii() {
            feats.push(format!("\"{}\" {}", f.tag, f.value));
        }
    }
    let vars: Vec<String> = st.variations.iter().filter(|v| v.axis.len() == 4 && v.axis.is_ascii()).map(|v| format!("\"{}\" {}", v.axis, v.value)).collect();
    vec![
        StyleProperty::FontFamily(FontFamily::Source(Cow::Owned(fam.join(", ")))),
        StyleProperty::FontSize(px),
        StyleProperty::FontWeight(FontWeight::new(st.weight.clamp(1, 1000) as f32)),
        StyleProperty::FontStyle(if st.italic { FontStyle::Italic } else { FontStyle::Normal }),
        StyleProperty::FontFeatures(FontFeatures::Source(Cow::Owned(feats.join(", ")))),
        StyleProperty::FontVariations(FontVariations::Source(Cow::Owned(vars.join(", ")))),
        StyleProperty::LetterSpacing(st.tracking / 1000.0 * px),
        StyleProperty::Underline(st.underline),
        StyleProperty::Strikethrough(st.strikethrough),
        StyleProperty::Brush(RunBrush(idx)),
        StyleProperty::Locale(st.language.as_deref().and_then(|l| parley::fontique::Language::parse(l).ok())),
    ]
}

/// Height of the lowercase ascender ('d') of the tallest run on the line.
fn first_ascent(line: &parley::Line<'_, RunBrush>) -> Option<f32> {
    use skrifa::MetadataProvider;
    let mut best: Option<f32> = None;
    for run in line.runs() {
        let fd = run.font();
        let Ok(font) = skrifa::FontRef::from_index(fd.data.as_ref(), fd.index) else {
            continue;
        };
        let coords: Vec<skrifa::instance::NormalizedCoord> = run.normalized_coords().iter().map(|&c| skrifa::instance::NormalizedCoord::from_bits(c)).collect();
        let loc = skrifa::instance::LocationRef::new(&coords);
        let size = skrifa::instance::Size::new(run.font_size());
        let h = font.charmap().map('d').and_then(|g| font.glyph_metrics(size, loc).bounds(g)).map(|b| b.y_max).or_else(|| {
            let m = font.metrics(size, loc);
            m.cap_height.or(Some(m.ascent * 0.75))
        });
        if let Some(h) = h {
            best = Some(best.map_or(h, |b: f32| b.max(h)));
        }
    }
    best
}
