//! Page detection: TypeR's `scripts/testBubbleDetection.js` cases (prior weights only), the neck
//! split and reading order, and synthetic manga pages.

use super::*;
use crate::synth::{Ellipse, Item, SynthPage};

struct Img {
    w: usize,
    h: usize,
    px: Vec<[u8; 4]>,
}

impl Img {
    fn new(w: usize, h: usize, gray: u8) -> Img {
        Img { w, h, px: vec![[gray, gray, gray, 255]; w * h] }
    }

    fn rect(&mut self, l: usize, t: usize, r: usize, b: usize, g: u8) {
        for y in t..b {
            for x in l..r {
                self.px[y * self.w + x] = [g, g, g, 255];
            }
        }
    }

    fn ellipse(&mut self, cx: f64, cy: f64, rx: f64, ry: f64, g: u8) {
        for y in (cy - ry).floor() as usize..=(cy + ry).ceil() as usize {
            for x in (cx - rx).floor() as usize..=(cx + rx).ceil() as usize {
                if ((x as f64 - cx) / rx).powi(2) + ((y as f64 - cy) / ry).powi(2) <= 1.0 {
                    self.px[y * self.w + x] = [g, g, g, 255];
                }
            }
        }
    }

    fn page(&self) -> Page<'_> {
        Page { px: &self.px, width: self.w, height: self.h }
    }
}

fn opts(s: u8) -> DetectOptions {
    DetectOptions { sensitivity: s, rtl: false, ..Default::default() }
}

/// TypeR's stable detector thresholds (`detectBubbles`), for the cases its test checks with it.
fn stable(img: &Img, s: u8) -> Vec<Candidate> {
    let min_area = 20usize.max(((img.w * img.h) as f64 * (0.0025 / f64::from(s))).floor() as usize);
    candidates(&img.px, img.w, img.h, &opts(s))
        .into_iter()
        .filter(|c| {
            let (w, h) = (c.bounds.width() as f64, c.bounds.height() as f64);
            c.area >= min_area && w >= 12.0 && h >= 12.0 && (w / h).max(h / w) <= 8.0 && c.fill >= 0.45 && c.runs_per_line <= 1.8 && c.outline_dark >= 0.55
        })
        .collect()
}

#[test]
fn two_bubbles_not_slivers_or_margins() {
    let mut p = Img::new(200, 150, 90);
    p.rect(20, 20, 70, 60, 255);
    p.rect(120, 25, 180, 70, 255);
    p.rect(20, 100, 180, 104, 255);
    p.rect(0, 130, 200, 150, 255);
    p.rect(30, 30, 40, 34, 0);
    p.rect(45, 40, 60, 44, 0);
    let mut d = stable(&p, 5);
    d.sort_by_key(|c| c.bounds.left);
    assert_eq!(d.len(), 2);
    assert_eq!((d[0].bounds.left, d[0].bounds.right), (20, 70));
    assert_eq!(d[1].bounds.top, 25);
    assert!(d[0].fill > 0.9);
    // The learned pipeline finds the same two.
    assert_eq!(detect(&p.page(), (0.0, 0.0), &opts(5)).len(), 2);
}

#[test]
fn jagged_and_unsealed_whites_are_refused() {
    let mut comb = Img::new(200, 150, 90);
    comb.rect(20, 20, 30, 50, 255);
    comb.rect(40, 20, 50, 50, 255);
    comb.rect(60, 20, 70, 50, 255);
    comb.rect(20, 50, 70, 60, 255);
    assert!(stable(&comb, 5).is_empty());
    let mut unsealed = Img::new(200, 150, 90);
    unsealed.rect(110, 80, 175, 130, 210);
    unsealed.rect(120, 90, 165, 120, 255);
    assert!(stable(&unsealed, 5).is_empty());
    assert!(detect(&unsealed.page(), (0.0, 0.0), &opts(5)).is_empty());
}

#[test]
fn sensitivity_admits_greyer_whites() {
    let mut p = Img::new(200, 150, 90);
    p.rect(40, 40, 100, 90, 218);
    assert!(stable(&p, 3).is_empty());
    assert_eq!(stable(&p, 8).len(), 1);
}

#[test]
fn prior_confidence_and_features() {
    let mut p = Img::new(200, 150, 90);
    p.rect(40, 35, 90, 80, 255);
    let c = candidates(&p.px, p.w, p.h, &opts(5));
    assert_eq!(c.len(), 1);
    assert!(c[0].features.iter().all(|f| (-1.0..=1.0).contains(f)));
    assert!((c[0].confidence - confidence(&c[0].features)).abs() < 1e-12);
    assert_eq!(detect(&p.page(), (0.0, 0.0), &opts(5)).len(), 1);
}

#[test]
fn a_peanut_splits_and_an_oval_does_not() {
    let mut p = Img::new(500, 700, 50);
    p.ellipse(250.0, 220.0, 70.0, 78.0, 255);
    p.ellipse(250.0, 355.0, 62.0, 82.0, 255);
    let c = candidates(&p.px, p.w, p.h, &opts(5));
    let s = c[0].split.as_ref().expect("a joined double bubble offers a split");
    assert!(s.confidence() >= SPLIT_THRESHOLD, "{}", s.confidence());
    let d = detect(&p.page(), (0.0, 0.0), &opts(5));
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d[0].center.1 < d[1].center.1, "top lobe first");
    assert!(d.iter().all(|b| b.split));
    // Each lobe's centre is near its ellipse's.
    assert!((d[0].center.1 - 220.0).abs() < 25.0 && (d[1].center.1 - 355.0).abs() < 25.0, "{:?}", d.iter().map(|b| b.center).collect::<Vec<_>>());
    let mut oval = Img::new(500, 700, 50);
    oval.ellipse(250.0, 290.0, 70.0, 140.0, 255);
    let c = candidates(&oval.px, oval.w, oval.h, &opts(5));
    assert!(c[0].split.is_none());
    assert_eq!(detect(&oval.page(), (0.0, 0.0), &opts(5)).len(), 1);
    // Side by side lobes split left/right.
    let mut wide = Img::new(700, 500, 50);
    wide.ellipse(220.0, 250.0, 78.0, 70.0, 255);
    wide.ellipse(355.0, 250.0, 82.0, 62.0, 255);
    let d = detect(&wide.page(), (0.0, 0.0), &DetectOptions { rtl: true, ..opts(5) });
    assert_eq!(d.len(), 2);
    assert!(d[0].center.0 > d[1].center.0, "manga order reads the right lobe first");
}

#[test]
fn reading_order_rows_then_direction() {
    let b = |x: f64, y: f64, w: f64, h: f64| Rect::new(x, y, w, h);
    let boxes = [b(25.0, 20.0, 40.0, 40.0), b(130.0, 25.0, 40.0, 45.0), b(80.0, 90.0, 40.0, 40.0)];
    assert_eq!(reading_order(&boxes, true), [1, 0, 2]);
    assert_eq!(reading_order(&boxes, false), [0, 1, 2]);
    assert!(reading_order(&[], true).is_empty());
}

#[test]
fn a_busy_synthetic_page() {
    let page = SynthPage::busy(1000, 1500);
    let d = detect(&Page { px: &page.px, width: page.width, height: page.height }, (0.0, 0.0), &DetectOptions::default());
    let want = page.bubble_centers();
    // Every bubble (both lobes of the double, and the narration box) is found, centred on its body.
    for c in &want {
        let hit = d.iter().find(|b| b.opened.x <= c.0 && c.0 <= b.opened.x + b.opened.width && b.opened.y <= c.1 && c.1 <= b.opened.y + b.opened.height);
        let hit = hit.unwrap_or_else(|| panic!("no bubble at {c:?}: {:?}", d.iter().map(|b| b.bbox).collect::<Vec<_>>()));
        let err = ((hit.center.0 - c.0).powi(2) + (hit.center.1 - c.1).powi(2)).sqrt();
        assert!(err <= 0.12 * hit.opened.width.min(hit.opened.height), "{c:?} vs {:?}", hit.center);
    }
    assert_eq!(d.len(), want.len(), "no false positives (SFX, screentone, gutters): {:?}", d.iter().map(|b| b.bbox).collect::<Vec<_>>());
    // Manga order: top row right to left first.
    assert!(d[0].center.0 > d[1].center.0 && (d[0].center.1 - d[1].center.1).abs() < 100.0);
    for b in &d {
        assert!(!b.rows.is_empty() && b.rows.iter().all(|r| (0.0..=1.0).contains(&r.width)));
        assert!(b.confidence >= 0.78 - 4.0 * 0.055);
    }
}

#[test]
fn downscaling_keeps_the_page_and_maps_back() {
    let page = SynthPage::new(3000, 2000, vec![Item::Bubble { e: Ellipse { cx: 900.0, cy: 600.0, rx: 260.0, ry: 160.0 }, tail: true, text: true }]);
    let p = Page { px: &page.px, width: page.width, height: page.height };
    let (_, w, h, f) = downscale(&p).unwrap();
    assert_eq!((w, h, f), (1500, 1000, 2));
    let d = detect(&p, (10.0, 20.0), &DetectOptions::default());
    assert_eq!(d.len(), 1, "{d:?}");
    assert!((d[0].center.0 - 910.0).abs() < 12.0 && (d[0].center.1 - 620.0).abs() < 12.0, "{:?}", d[0].center);
}

#[test]
fn hostile_input_is_harmless() {
    assert!(detect(&Page { px: &[], width: 0, height: 0 }, (0.0, 0.0), &DetectOptions::default()).is_empty());
    assert!(detect(&Page { px: &[[0; 4]; 3], width: 10, height: 10 }, (0.0, 0.0), &DetectOptions::default()).is_empty());
    let white = Img::new(64, 64, 255);
    assert!(detect(&white.page(), (0.0, 0.0), &DetectOptions { sensitivity: 0, rows: 0, ..Default::default() }).is_empty());
    let black = Img::new(64, 64, 0);
    assert!(detect(&black.page(), (0.0, 0.0), &DetectOptions { sensitivity: 200, ..Default::default() }).is_empty());
    assert!(neck_split(&[true; 10], 30, 30).is_none());
    assert!(fill_holes(&[], 0, 0).is_empty());
}

#[test]
#[ignore = "timing; run in release"]
fn perf_2000x3000() {
    let page = SynthPage::busy(2000, 3000);
    let p = Page { px: &page.px, width: page.width, height: page.height };
    let t = std::time::Instant::now();
    let d = detect(&p, (0.0, 0.0), &DetectOptions::default());
    println!("detect 2000x3000: {} bubbles in {:?}", d.len(), t.elapsed());
}
