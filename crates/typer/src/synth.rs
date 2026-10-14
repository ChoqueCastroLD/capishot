//! Synthetic manga-like pages for tests and benchmarks: screentone panels with white gutters,
//! outlined speech bubbles (ellipses, tails, joined double bubbles), narration boxes, text-like
//! ink inside bubbles and SFX strokes over the art. Pure pixels, no I/O.

/// A speech bubble drawn on a synthetic page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ellipse {
    pub cx: f64,
    pub cy: f64,
    pub rx: f64,
    pub ry: f64,
}

impl Ellipse {
    fn inside(&self, x: f64, y: f64, grow: f64) -> bool {
        let (dx, dy) = ((x - self.cx) / (self.rx + grow), (y - self.cy) / (self.ry + grow));
        dx * dx + dy * dy <= 1.0
    }
}

/// What a synthetic page holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A bubble; `tail` hangs a tail down-right; `text` writes text-like ink inside.
    Bubble { e: Ellipse, tail: bool, text: bool },
    /// Two ellipses joined into one outlined region (a double bubble).
    Double { a: Ellipse, b: Ellipse, text: bool },
    /// A white box with a black frame (narration).
    Box { x: f64, y: f64, w: f64, h: f64 },
    /// Heavy black strokes over the art (sound effects): never a bubble.
    Sfx { x: f64, y: f64, size: f64 },
}

/// A page and what was drawn on it.
#[derive(Clone, Debug)]
pub struct SynthPage {
    pub width: usize,
    pub height: usize,
    pub px: Vec<[u8; 4]>,
    pub items: Vec<Item>,
}

const OUTLINE: f64 = 3.5;

fn tail_hit(e: &Ellipse, x: f64, y: f64, grow: f64) -> bool {
    let len = 0.6 * e.ry;
    let t = (y - (e.cy + e.ry - 8.0)) / len;
    if (0.0..=1.0).contains(&t) {
        let half = 0.08 * e.rx * (1.0 - t) + grow;
        let tx = e.cx + 0.35 * e.rx + 0.3 * e.rx * t;
        return (x - tx).abs() <= half;
    }
    false
}

/// White interior (`grow` = 0) or interior plus outline (`grow` = outline width) of an item.
fn covers(item: &Item, x: f64, y: f64, grow: f64) -> bool {
    match item {
        Item::Bubble { e, tail, .. } => e.inside(x, y, grow) || (*tail && tail_hit(e, x, y, grow)),
        Item::Double { a, b, .. } => a.inside(x, y, grow) || b.inside(x, y, grow),
        Item::Box { x: bx, y: by, w, h } => x >= bx - grow && x <= bx + w + grow && y >= by - grow && y <= by + h + grow,
        Item::Sfx { .. } => false,
    }
}

/// Text-like ink: short dark bars on lines across the middle of an ellipse.
fn text_ink(e: &Ellipse, x: f64, y: f64) -> bool {
    let (dx, dy) = ((x - e.cx) / (e.rx * 0.6), (y - e.cy) / (e.ry * 0.5));
    if dx.abs() > 1.0 || dy.abs() > 1.0 {
        return false;
    }
    let line = ((y - e.cy + e.ry) / 14.0).floor() as i64;
    let in_line = (y - e.cy + e.ry).rem_euclid(14.0) < 7.0;
    let word = ((x - e.cx + e.rx + line as f64 * 5.0) / 9.0).floor() as i64;
    in_line && word % 4 != 3 && ((x as i64 + line) % 3 != 0)
}

fn sfx_ink(cx: f64, cy: f64, size: f64, x: f64, y: f64) -> bool {
    // Three slanted heavy bars like a bold "ZAP".
    (0..3).any(|k| {
        let ox = cx + f64::from(k) * size * 0.8;
        let (u, v) = (x - ox, y - cy);
        let along = u * 0.3 + v;
        let across = u - v * 0.3;
        along.abs() <= size * 0.5 && across.abs() <= size * 0.12
    })
}

impl SynthPage {
    /// Draws `items` on a `width × height` page: two panels of screentone and gradient art
    /// separated by white gutters that touch the page edges.
    pub fn new(width: usize, height: usize, items: Vec<Item>) -> SynthPage {
        let mut px = vec![[255u8, 255, 255, 255]; width * height];
        let gutter = 24.0;
        let mid = height as f64 * 0.5;
        for y in 0..height {
            for x in 0..width {
                let (fx, fy) = (x as f64 + 0.5, y as f64 + 0.5);
                let in_panel = fx > gutter && fx < width as f64 - gutter && fy > gutter && fy < height as f64 - gutter && (fy - mid).abs() > gutter / 2.0;
                let frame = in_panel
                    && (fx < gutter + 3.0
                        || fx > width as f64 - gutter - 3.0
                        || fy < gutter + 3.0
                        || fy > height as f64 - gutter - 3.0
                        || (fy - mid).abs() < gutter / 2.0 + 3.0);
                let mut v: u8 = if !in_panel {
                    255
                } else if frame {
                    0
                } else {
                    // Screentone dots over a soft gradient.
                    let (mx, my) = ((x % 6) as f64 - 2.5, (y % 6) as f64 - 2.5);
                    let base = (180.0 + 60.0 * ((fx / width as f64) * 3.0).sin()) as u8;
                    if mx * mx + my * my < 4.0 { 60 } else { base }
                };
                for it in &items {
                    if let Item::Sfx { x: sx, y: sy, size } = it
                        && sfx_ink(*sx, *sy, *size, fx, fy)
                    {
                        v = 10;
                    }
                }
                for it in &items {
                    if covers(it, fx, fy, 0.0) {
                        v = 255;
                        let ink = match it {
                            Item::Bubble { e, text: true, .. } => text_ink(e, fx, fy),
                            Item::Double { a, b, text: true } => text_ink(a, fx, fy) || text_ink(b, fx, fy),
                            _ => false,
                        };
                        if ink {
                            v = 20;
                        }
                    } else if covers(it, fx, fy, OUTLINE) && !items.iter().any(|o| covers(o, fx, fy, 0.0)) {
                        v = 0;
                    }
                }
                if let Some(p) = px.get_mut(y * width + x) {
                    *p = [v, v, v, 255];
                }
            }
        }
        SynthPage { width, height, px, items }
    }

    /// A busy test page: six bubbles in two panels (two with tails, one double), a narration
    /// box and two SFX.
    pub fn busy(width: usize, height: usize) -> SynthPage {
        let (w, h) = (width as f64, height as f64);
        let e = |cx: f64, cy: f64, rx: f64, ry: f64| Ellipse { cx: cx * w, cy: cy * h, rx: rx * w, ry: ry * h };
        SynthPage::new(
            width,
            height,
            vec![
                Item::Bubble { e: e(0.78, 0.12, 0.12, 0.06), tail: true, text: true },
                Item::Bubble { e: e(0.42, 0.14, 0.13, 0.07), tail: false, text: true },
                Item::Double { a: e(0.2, 0.3, 0.1, 0.06), b: e(0.2, 0.41, 0.09, 0.06), text: true },
                Item::Box { x: 0.6 * w, y: 0.3 * h, w: 0.25 * w, h: 0.08 * h },
                Item::Sfx { x: 0.45 * w, y: 0.36 * h, size: 0.05 * h },
                Item::Bubble { e: e(0.7, 0.66, 0.14, 0.07), tail: true, text: true },
                Item::Bubble { e: e(0.3, 0.7, 0.12, 0.08), tail: false, text: false },
                Item::Sfx { x: 0.4 * w, y: 0.88 * h, size: 0.06 * h },
                Item::Bubble { e: e(0.75, 0.86, 0.1, 0.05), tail: false, text: true },
            ],
        )
    }

    /// Centres where text belongs, one per bubble (two for a double), narration boxes included.
    pub fn bubble_centers(&self) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for it in &self.items {
            match it {
                Item::Bubble { e, .. } => out.push((e.cx, e.cy)),
                Item::Double { a, b, .. } => {
                    out.push((a.cx, a.cy));
                    out.push((b.cx, b.cy));
                }
                Item::Box { x, y, w, h } => out.push((x + w / 2.0, y + h / 2.0)),
                Item::Sfx { .. } => {}
            }
        }
        out
    }
}
