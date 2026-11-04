//! A fingerprint of a laid-out text layer (`type.layoutDigest`): every line, glyph and decoration
//! position bit for bit, and the font instance each glyph uses. Two builds (the browser and the
//! CLI) lay a layer out identically exactly when their digests match.

use super::{GlyphOrient, TextLayout};

/// Lower-case hex SHA-256 of the layout's lines, glyphs, decorations and faces (font size,
/// variation coordinates, synthetic bold and oblique, and the font file's length and face
/// index). Floats are hashed by their bits, so any difference in the last bit shows.
pub fn layout_digest(layout: &TextLayout) -> String {
    let mut b: Vec<u8> = Vec::with_capacity(64 + layout.glyphs.len() * 17 + layout.lines.len() * 41);
    let f = |v: f32, b: &mut Vec<u8>| b.extend_from_slice(&v.to_bits().to_le_bytes());
    let u = |v: usize, b: &mut Vec<u8>| b.extend_from_slice(&(v as u64).to_le_bytes());
    b.extend_from_slice(b"photocraft-layout-1");
    f(layout.px_per_pt, &mut b);
    b.push(u8::from(layout.vertical));
    u(layout.faces.len(), &mut b);
    for face in &layout.faces {
        u(face.font.data.as_ref().len(), &mut b);
        b.extend_from_slice(&face.font.index.to_le_bytes());
        u(face.coords.len(), &mut b);
        for c in &face.coords {
            b.extend_from_slice(&c.to_le_bytes());
        }
        f(face.size_px, &mut b);
        b.push(u8::from(face.embolden));
        f(face.skew_deg, &mut b);
    }
    u(layout.lines.len(), &mut b);
    for ln in &layout.lines {
        u(ln.range.start, &mut b);
        u(ln.range.end, &mut b);
        for v in [ln.baseline, ln.x0, ln.x1, ln.ascent, ln.descent] {
            f(v, &mut b);
        }
        u(ln.paragraph, &mut b);
        b.push(u8::from(ln.hyphen));
    }
    u(layout.glyphs.len(), &mut b);
    for g in &layout.glyphs {
        b.extend_from_slice(&g.face.to_le_bytes());
        b.extend_from_slice(&g.id.to_le_bytes());
        f(g.x, &mut b);
        f(g.y, &mut b);
        b.extend_from_slice(&g.style.to_le_bytes());
        b.push(match g.orient {
            GlyphOrient::Horizontal => 0,
            GlyphOrient::Upright => 1,
            GlyphOrient::Rotated => 2,
        });
    }
    u(layout.decorations.len(), &mut b);
    for d in &layout.decorations {
        for v in [d.x0, d.y0, d.x1, d.y1] {
            f(v, &mut b);
        }
        b.extend_from_slice(&d.style.to_le_bytes());
    }
    crate::sha256::sha256_hex(&b)
}
