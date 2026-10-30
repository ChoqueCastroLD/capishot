//! Healing quality on flat manga art, where scanlation cleaners use Spot Healing and the Healing
//! Brush all day: text painted out of a white speech bubble, and out of a regular screentone.
//!
//! "No visible seam", as numbers:
//! - **White**: every healed pixel is within 3/255 of white (a grey smudge in a bubble shows).
//! - **Screentone** (dots of period `P`): the eye sees a tone's local mean, so the tone must not
//!   change: the `P × P` box mean of the result differs from the clean tone's by at most 0.035
//!   (about 9/255) at every pixel of the stroke and the ring around it (a blotch or a seam is a
//!   jump in that mean). The dots must stay dots: the healed area keeps 0.75..1.33 of the clean
//!   tone's variance (not blurred to grey, not noisier), and no text remains (no pixel darker than
//!   the tone's darkest dot by more than 0.1).

use super::*;

/// Period of the screentone dots (pixels).
const P: i32 = 6;

fn tone(x: i32, y: i32) -> f32 {
    // Round dots of radius 2 on a 6 px grid: a regular 30 % screentone.
    let (dx, dy) = ((x.rem_euclid(P) - P / 2) as f32 + 0.5, (y.rem_euclid(P) - P / 2) as f32 + 0.5);
    if dx * dx + dy * dy <= 4.5 { 0.15 } else { 1.0 }
}

/// Left half a white bubble, right half screentone.
fn clean(x: i32, y: i32) -> f32 {
    if x < 120 { 1.0 } else { tone(x, y) }
}

/// "Text": black letter strokes in a word-sized box.
fn is_text(x: i32, y: i32, x0: i32, y0: i32) -> bool {
    let (lx, ly) = (x - x0, y - y0);
    (0..40).contains(&lx) && (0..14).contains(&ly) && (lx % 7 < 2 || ly % 6 < 2)
}

fn page(depth: u64) -> Session {
    let mut s = Session::new();
    s.execute("file.new", json!({"width": 240, "height": 120, "depth": depth, "mode": "rgb"})).unwrap();
    s.edit("setup", |doc, active| {
        let b = doc.bounds();
        let surf = doc.layer_mut(active.unwrap()).unwrap().surface_mut().unwrap();
        let fmt = surf.format();
        let mut data = Vec::new();
        for y in b.y0..b.y1 {
            for x in b.x0..b.x1 {
                let v = if is_text(x, y, 40, 50) || is_text(x, y, 160, 50) { 0.0 } else { clean(x, y) };
                data.extend(photocraft_raster::from_rgba(&fmt, [v, v, v, 1.0]));
            }
        }
        surf.write_region(b, &data);
        Ok(())
    })
    .unwrap();
    s
}

fn grey(s: &Session, x: i32, y: i32) -> f32 {
    let d = s.active().unwrap();
    let p = d.doc.layer(d.active_layer.unwrap()).unwrap().surface().unwrap().rgba(x, y);
    (p[0] + p[1] + p[2]) / 3.0
}

/// A cleaner's stroke over a word at (x0, y0): along its middle, a brush a bit taller than it.
fn over_word(x0: i32, y0: i32) -> Value {
    json!([[x0 - 2, y0 + 7], [x0 + 10, y0 + 7], [x0 + 20, y0 + 7], [x0 + 30, y0 + 7], [x0 + 41, y0 + 7]])
}

fn box_mean(f: &impl Fn(i32, i32) -> f32, x: i32, y: i32) -> f32 {
    let mut sum = 0.0;
    for dy in 0..P {
        for dx in 0..P {
            sum += f(x + dx - P / 2, y + dy - P / 2);
        }
    }
    sum / (P * P) as f32
}

struct Score {
    white_worst: f32,
    tone_mean_worst: f32,
    tone_variance_ratio: f32,
    tone_darkest: f32,
}

/// Score the two healed words (the stroke area plus a 4 px ring).
fn score(s: &Session) -> Score {
    let mut white_worst = 0.0f32;
    for y in 40..74 {
        for x in 30..92 {
            white_worst = white_worst.max((1.0 - grey(s, x, y)).abs());
        }
    }
    let healed = |x: i32, y: i32| grey(s, x, y);
    let (mut tone_mean_worst, mut darkest) = (0.0f32, 1.0f32);
    let (mut var_h, mut var_c) = (0.0f32, 0.0f32);
    // Means of the clean tone and of the healed area, for the variances.
    let region = || (44..70).flat_map(|y| (154..206).map(move |x| (x, y)));
    let (mh, mc) = region().fold((0.0, 0.0), |(a, b), (x, y)| (a + healed(x, y), b + tone(x, y)));
    let count = region().count() as f32;
    let (mh, mc) = (mh / count, mc / count);
    for (x, y) in region() {
        tone_mean_worst = tone_mean_worst.max((box_mean(&healed, x, y) - box_mean(&tone, x, y)).abs());
        darkest = darkest.min(healed(x, y));
        var_h += (healed(x, y) - mh).powi(2);
        var_c += (tone(x, y) - mc).powi(2);
    }
    Score { white_worst, tone_mean_worst, tone_variance_ratio: var_h / var_c.max(1e-9), tone_darkest: darkest }
}

fn assert_seamless(s: &Session, what: &str) {
    let sc = score(s);
    assert!(sc.white_worst <= 3.0 / 255.0 + 1e-4, "{what}: white bubble off by {:.4} (> 3/255)", sc.white_worst);
    assert!(sc.tone_mean_worst <= 0.035, "{what}: screentone local mean off by {:.4} (a seam or blotch)", sc.tone_mean_worst);
    assert!((0.75..=1.33).contains(&sc.tone_variance_ratio), "{what}: screentone texture ratio {:.3} (blurred or noisy)", sc.tone_variance_ratio);
    assert!(sc.tone_darkest >= 0.15 - 0.1, "{what}: text left in the tone (darkest {:.3})", sc.tone_darkest);
}

#[test]
fn the_page_before_healing_fails_the_criterion() {
    // The criterion sees the text: it is not trivially satisfied.
    let sc = score(&page(8));
    assert!(sc.white_worst > 0.5 && sc.tone_mean_worst > 0.1 && sc.tone_darkest < 0.05);
}

#[test]
fn spot_healing_cleans_text_out_of_white_and_screentone_without_a_seam() {
    for (depth, kind) in [(8u64, "contentAware"), (16, "contentAware"), (8, "proximityMatch")] {
        let mut s = page(depth);
        for (x0, y0) in [(40, 50), (160, 50)] {
            s.execute("paint.spotHealing", json!({"points": over_word(x0, y0), "size": 20, "hardness": 100, "type": kind})).unwrap();
        }
        assert_seamless(&s, &format!("Spot Healing {kind}, {depth}-bit"));
    }
}

#[test]
fn healing_brush_cleans_text_from_a_tone_aligned_source_without_a_seam() {
    // The cleaner ⌥-clicks clean art: white beside the word, and the same screentone a whole
    // number of dot periods away (as they line up the dots by eye).
    for depth in [8u64, 16] {
        let mut s = page(depth);
        s.execute("paint.healingBrush", json!({"points": over_word(40, 50), "offset": [0, -36], "size": 20, "hardness": 100})).unwrap();
        s.execute("paint.healingBrush", json!({"points": over_word(160, 50), "offset": [0, -36], "size": 20, "hardness": 100})).unwrap();
        assert_seamless(&s, &format!("Healing Brush {depth}-bit"));
    }
}

/// The live Healing Brush meets the same criterion while it is drawn, so what the cleaner sees
/// mid-stroke is already clean.
#[test]
fn live_healing_is_seamless_on_manga_art() {
    let mut s = page(8);
    for (x0, y0) in [(40, 50), (160, 50)] {
        let all: Vec<[f64; 2]> = over_word(x0, y0).as_array().unwrap().iter().map(|v| [v[0].as_f64().unwrap(), v[1].as_f64().unwrap()]).collect();
        let mut live = LiveRetouch::begin(&s, "paint.healingBrush", &json!({"points": [all[0]], "offset": [0, -36], "size": 20, "hardness": 100})).unwrap();
        for q in &all[1..] {
            live.push(&[StrokePoint::new(q[0], q[1], 1.0)]).unwrap();
        }
        // Keep what was shown, as if released there, for the next word.
        let shown = (*live.doc).clone();
        s.edit("shown", |doc, _| {
            *doc = shown;
            Ok(())
        })
        .unwrap();
    }
    assert_seamless(&s, "live Healing Brush");
}
