//! The block of lettering under a click, for a bubble that has no closed outline to flood (it
//! opens onto a panel border, the page edge or the gutter): the dark components the size of
//! glyphs (kana and kanji, Latin letters, small furigana beside a column, punctuation) gathered
//! from the one nearest the click by proximity, column by column, stopping at what isn't a glyph
//! (a panel line, the bubble's outline, an art stroke). The block's box plus a margin is where its
//! bubble's text is; the components that aren't glyphs inside it are walls never to touch.

use crate::bubble::Mask;

/// How far (px, at 1x) around the click the components are looked at.
const WINDOW: i32 = 320;
/// How far (px, at 1x) from the click the first glyph may be.
const REACH: i32 = 30;
/// A pixel darker than this (luma) is ink.
const INK: u8 = 140;
/// A block of more components than this is a texture (screentone), not lettering.
const MAX_COMPONENTS: usize = 600;

/// A block of lettering.
#[derive(Clone, Debug)]
pub struct Cluster {
    /// The block's box plus a margin, clipped to the image: `[x0, y0, x1, y1)`.
    pub rect: [i32; 4],
    /// The block's typical glyph size (px).
    pub glyph: f64,
    /// Over `rect`: true where a dark component that is not a glyph (a line, the outline, art)
    /// is, grown by two pixels.
    pub walls: Mask,
    /// How many glyph components the block holds.
    pub components: usize,
}

#[derive(Clone, Copy)]
struct Comp {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
    glyph: bool,
}

impl Comp {
    fn side(&self) -> i32 {
        (self.x1 - self.x0).max(self.y1 - self.y0)
    }
    fn gap(&self, o: &Comp) -> i32 {
        let dx = (o.x0 - self.x1).max(self.x0 - o.x1).max(0);
        let dy = (o.y0 - self.y1).max(self.y0 - o.y1).max(0);
        dx.max(dy)
    }
}

fn luma(p: [u8; 4]) -> u8 {
    if p[3] < 128 { 255 } else { ((299 * u32::from(p[0]) + 587 * u32::from(p[1]) + 114 * u32::from(p[2])) / 1000) as u8 }
}

/// The block of lettering nearest `seed` on an 8-bit RGBA image covering `area`, or `None`.
pub fn find(img: &[[u8; 4]], area: photocraft_geom::Rect, seed: (i32, i32)) -> Option<Cluster> {
    find_scaled(img, area, seed, 1.0)
}

/// [`find`] on a page `scale` times a 1x page's resolution (see
/// [`crate::bubble_find::ink_scale`]): its pixel sizes scaled by it.
pub fn find_scaled(img: &[[u8; 4]], area: photocraft_geom::Rect, seed: (i32, i32), scale: f64) -> Option<Cluster> {
    let (aw, ah) = (area.width() as usize, area.height() as usize);
    if img.len() != aw * ah || !area.contains(seed.0, seed.1) {
        return None;
    }
    let scale = if scale.is_finite() { scale.clamp(1.0, crate::bubble_find::MAX_SCALE) } else { 1.0 };
    let px = |v: i32| (f64::from(v) * scale).round() as i32;
    let (window, reach) = (px(WINDOW), px(REACH));
    let (wx0, wy0) = ((seed.0 - window).max(area.x0), (seed.1 - window).max(area.y0));
    let (wx1, wy1) = ((seed.0 + window).min(area.x1), (seed.1 + window).min(area.y1));
    let (w, h) = ((wx1 - wx0) as usize, (wy1 - wy0) as usize);
    let ink: Vec<bool> = (0..w * h)
        .map(|k| {
            let (x, y) = (wx0 + (k % w) as i32, wy0 + (k / w) as i32);
            luma(img[(y - area.y0) as usize * aw + (x - area.x0) as usize]) < INK
        })
        .collect();
    // Components (8-connected).
    let mut label = vec![u32::MAX; w * h];
    let mut comps: Vec<Comp> = Vec::new();
    let glyph_max = (0.06 * f64::from(area.width().min(area.height()))).max(48.0) as i32;
    let mut stack = Vec::new();
    for k0 in 0..w * h {
        if !ink[k0] || label[k0] != u32::MAX {
            continue;
        }
        let id = comps.len() as u32;
        let mut c = Comp { x0: i32::MAX, y0: i32::MAX, x1: i32::MIN, y1: i32::MIN, glyph: true };
        let mut touches = false;
        label[k0] = id;
        stack.push(k0);
        while let Some(k) = stack.pop() {
            let (x, y) = ((k % w) as i32, (k / w) as i32);
            c.x0 = c.x0.min(x);
            c.y0 = c.y0.min(y);
            c.x1 = c.x1.max(x + 1);
            c.y1 = c.y1.max(y + 1);
            touches |= x == 0 || y == 0 || x as usize == w - 1 || y as usize == h - 1;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx < 0 || ny < 0 || nx as usize >= w || ny as usize >= h {
                        continue;
                    }
                    let n = ny as usize * w + nx as usize;
                    if ink[n] && label[n] == u32::MAX {
                        label[n] = id;
                        stack.push(n);
                    }
                }
            }
        }
        // A glyph: no longer than a glyph can be (a panel line or the outline is), not cut by the
        // window. Thin ones count ("I", "l", "ー").
        let (cw, ch) = (c.x1 - c.x0, c.y1 - c.y0);
        c.glyph = !touches && cw.max(ch) <= glyph_max;
        comps.push(c);
    }
    // The glyph nearest the click.
    let (sx, sy) = (seed.0 - wx0, seed.1 - wy0);
    let point = Comp { x0: sx, y0: sy, x1: sx + 1, y1: sy + 1, glyph: true };
    let first = comps.iter().enumerate().filter(|(_, c)| c.glyph && c.gap(&point) <= reach).min_by_key(|(_, c)| c.gap(&point)).map(|(i, _)| i)?;
    // Grow by proximity: the gap a block's glyphs leave between them (lines of a column, columns
    // of a block, furigana beside them) scales with the glyph size.
    let mut member = vec![false; comps.len()];
    member[first] = true;
    let mut list = vec![first];
    let mut sides = vec![comps[first].side()];
    let mut i = 0;
    while i < list.len() {
        if list.len() > MAX_COMPONENTS {
            return None;
        }
        let g = {
            let mut s = sides.clone();
            s.sort_unstable();
            s[s.len() * 3 / 4]
        };
        let gap = (g * 9 / 10).max(px(8));
        let c = comps[list[i]];
        for (j, o) in comps.iter().enumerate() {
            if !member[j] && o.glyph && c.gap(o) <= gap {
                member[j] = true;
                list.push(j);
                sides.push(o.side());
            }
        }
        i += 1;
    }
    let mut s = sides.clone();
    s.sort_unstable();
    let glyph = f64::from(s[s.len() * 3 / 4]);
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &j in &list {
        let c = comps[j];
        (x0, y0, x1, y1) = (x0.min(c.x0), y0.min(c.y0), x1.max(c.x1), y1.max(c.y1));
    }
    // A block filling the window is no block of lettering.
    if (x1 - x0) as usize > w * 9 / 10 && (y1 - y0) as usize > h * 9 / 10 {
        return None;
    }
    // Wide enough that the ring the background is read from (a few pixels in from the box) is
    // clear of the lettering.
    let margin = (glyph * 0.75).max(14.0 * scale) as i32;
    let rect = [(wx0 + x0 - margin).max(area.x0), (wy0 + y0 - margin).max(area.y0), (wx0 + x1 + margin).min(area.x1), (wy0 + y1 + margin).min(area.y1)];
    let (rw, rh) = ((rect[2] - rect[0]) as usize, (rect[3] - rect[1]) as usize);
    let mut walls = Mask::new(rect[0], rect[1], rw, rh)?;
    let grow = px(2);
    for y in rect[1]..rect[3] {
        for x in rect[0]..rect[2] {
            let (lx, ly) = (x - wx0, y - wy0);
            if lx < 0 || ly < 0 || lx as usize >= w || ly as usize >= h {
                continue;
            }
            let k = ly as usize * w + lx as usize;
            if label[k] != u32::MAX && !comps[label[k] as usize].glyph {
                for dy in -grow..=grow {
                    for dx in -grow..=grow {
                        walls.set(x + dx, y + dy, true);
                    }
                }
            }
        }
    }
    Some(Cluster { rect, glyph, walls, components: list.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A white image `w × h` with black rectangles `rects` ([x0, y0, x1, y1]).
    fn image(w: i32, h: i32, rects: &[[i32; 4]]) -> Vec<[u8; 4]> {
        let mut img = vec![[255u8; 4]; (w * h) as usize];
        for r in rects {
            for y in r[1]..r[3] {
                for x in r[0]..r[2] {
                    img[(y * w + x) as usize] = [0, 0, 0, 255];
                }
            }
        }
        img
    }

    #[test]
    fn vertical_columns_with_furigana_make_one_block_and_a_panel_line_is_a_wall() {
        // Two columns of four 20 px glyphs, 6 px apart; furigana (6 px) beside the first column;
        // a panel line down the left; a big art blob far away.
        let mut rects = Vec::new();
        for (col, x) in [(0, 200), (1, 170)] {
            for row in 0..4 {
                let y = 100 + row * 26;
                rects.push([x, y, x + 20, y + 20]);
                if col == 0 {
                    rects.push([x + 23, y + 4, x + 29, y + 10]);
                }
            }
        }
        rects.push([150, 40, 152, 400]);
        rects.push([400, 300, 520, 420]);
        let img = image(600, 500, &rects);
        let area = photocraft_geom::Rect::new(0, 0, 600, 500);
        let c = find(&img, area, (210, 150)).expect("a block");
        assert_eq!(c.components, 12, "8 glyphs and 4 furigana");
        assert!(c.rect[0] <= 160 && c.rect[2] >= 229 && c.rect[1] <= 100 && c.rect[3] >= 198, "{:?}", c.rect);
        assert!(c.rect[2] < 400, "the art stays out");
        // The panel line inside the margin is a wall.
        assert!(c.rect[0] <= 151 || !c.walls.get(151, 150));
        if c.rect[0] <= 151 {
            assert!(c.walls.get(151, 150));
        }
        // A click on blank paper far from any text finds nothing.
        assert!(find(&img, area, (350, 50)).is_none());
    }

    #[test]
    fn screentone_is_no_block() {
        let mut rects = Vec::new();
        for y in (0..500).step_by(6) {
            for x in (0..600).step_by(6) {
                rects.push([x, y, x + 2, y + 2]);
            }
        }
        let img = image(600, 500, &rects);
        assert!(find(&img, photocraft_geom::Rect::new(0, 0, 600, 500), (300, 250)).is_none());
    }
}
