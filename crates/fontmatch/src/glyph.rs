//! A glyph's outline as a bitmap: flattened and filled by the nonzero rule, with no other
//! dependency than the font reader.

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::{FontRef, GlyphId, MetadataProvider};

use crate::Bitmap;

/// A segment's points, in pixels with y down.
#[derive(Default)]
struct Pen {
    lines: Vec<[f32; 4]>,
    start: [f32; 2],
    at: [f32; 2],
}

const CURVE_STEPS: usize = 8;

impl Pen {
    fn line(&mut self, x: f32, y: f32) {
        self.lines.push([self.at[0], self.at[1], x, y]);
        self.at = [x, y];
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.start = [x, y];
        self.at = [x, y];
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.line(x, y);
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let [ax, ay] = self.at;
        for k in 1..=CURVE_STEPS {
            let t = k as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            self.line(u * u * ax + 2.0 * u * t * cx + t * t * x, u * u * ay + 2.0 * u * t * cy + t * t * y);
        }
    }

    fn curve_to(&mut self, c0x: f32, c0y: f32, c1x: f32, c1y: f32, x: f32, y: f32) {
        let [ax, ay] = self.at;
        for k in 1..=CURVE_STEPS {
            let t = k as f32 / CURVE_STEPS as f32;
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            self.line(a * ax + b * c0x + c * c1x + d * x, a * ay + b * c0y + c * c1y + d * y);
        }
    }

    fn close(&mut self) {
        if self.at != self.start {
            let [x, y] = self.start;
            self.line(x, y);
        }
    }
}

/// Fills the lines at pixel centres, trimmed to the ink. `None`: nothing to draw.
fn fill(lines: &[[f32; 4]]) -> Option<Bitmap> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for [ax, ay, bx, by] in lines {
        (x0, y0, x1, y1) = (x0.min(*ax).min(*bx), y0.min(*ay).min(*by), x1.max(*ax).max(*bx), y1.max(*ay).max(*by));
    }
    if lines.is_empty() || x1 - x0 < 0.5 || y1 - y0 < 0.5 {
        return None;
    }
    let (w, h) = ((x1 - x0).ceil() as usize + 1, (y1 - y0).ceil() as usize + 1);
    let (ox, oy) = (x0.floor(), y0.floor());
    let mut out = Bitmap::new(w, h);
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..h {
        let y = oy + row as f32 + 0.5;
        crossings.clear();
        for [ax, ay, bx, by] in lines {
            if (*ay <= y) != (*by <= y) {
                let t = (y - ay) / (by - ay);
                crossings.push((ax + (bx - ax) * t, if by > ay { 1 } else { -1 }));
            }
        }
        crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut wind = 0;
        for pair in crossings.windows(2) {
            wind += pair[0].1;
            if wind != 0 {
                let from = ((pair[0].0 - ox - 0.5).ceil().max(0.0)) as usize;
                let to = ((pair[1].0 - ox - 0.5).ceil().max(0.0) as usize).min(w);
                if from < to {
                    out.data[row * w + from..row * w + to].fill(true);
                }
            }
        }
    }
    Some(trim(&out))
}

/// `b` without its empty rows and columns.
pub fn trim(b: &Bitmap) -> Bitmap {
    let (mut x0, mut y0, mut x1, mut y1) = (b.width, b.height, 0, 0);
    for y in 0..b.height {
        for x in 0..b.width {
            if b.get(x, y) {
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
        }
    }
    if x0 >= x1 {
        return Bitmap::new(1, 1);
    }
    let mut out = Bitmap::new(x1 - x0, y1 - y0);
    for y in y0..y1 {
        out.data[(y - y0) * out.width..(y - y0 + 1) * out.width].copy_from_slice(&b.data[y * b.width + x0..y * b.width + x1]);
    }
    out
}

/// The size in pixels at which `font`'s capitals are `cap` tall. `None`: it has no H to measure.
pub fn size_for_cap(font: &FontRef<'_>, cap: f32) -> Option<f32> {
    let g = font.charmap().map('H')?;
    let b = bitmap(font, g, 1000.0)?;
    Some(1000.0 * cap / b.height.max(1) as f32)
}

/// The glyph `id` at `size` pixels.
pub fn bitmap(font: &FontRef<'_>, id: GlyphId, size: f32) -> Option<Bitmap> {
    let outline = font.outline_glyphs().get(id)?;
    let mut pen = Pen::default();
    outline.draw(DrawSettings::unhinted(Size::new(size), LocationRef::default()), &mut YDown(&mut pen)).ok()?;
    pen.close();
    fill(&pen.lines)
}

/// A pen with y going down, as images do.
struct YDown<'a>(&'a mut Pen);

impl OutlinePen for YDown<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.move_to(x, -y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.line_to(x, -y);
    }
    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.0.quad_to(cx, -cy, x, -y);
    }
    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        self.0.curve_to(ax, -ay, bx, -by, x, -y);
    }
    fn close(&mut self) {
        self.0.close();
    }
}
