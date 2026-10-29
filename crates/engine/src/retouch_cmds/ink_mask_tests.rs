use super::*;

const W: usize = 200;
const H: usize = 120;

/// A white page with black rectangles (x0, y0, x1, y1), anti-aliased by a grey 1 px rim.
fn page(rects: &[(usize, usize, usize, usize)]) -> Vec<f32> {
    let mut v = vec![1.0f32; W * H];
    for &(x0, y0, x1, y1) in rects {
        for y in y0.saturating_sub(1)..(y1 + 1).min(H) {
            for x in x0.saturating_sub(1)..(x1 + 1).min(W) {
                let inside = (x0..x1).contains(&x) && (y0..y1).contains(&y);
                v[y * W + x] = v[y * W + x].min(if inside { 0.0 } else { 0.7 });
            }
        }
    }
    v
}

fn painted(x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<bool> {
    (0..W * H).map(|i| (x0..x1).contains(&(i % W)) && (y0..y1).contains(&(i / W))).collect()
}

const G: Grow = Grow { dilate: 4, reach: 24, feather: 2 };

#[test]
fn the_page_sets_the_growth() {
    assert_eq!(Grow::for_page(1644, 42.0), Grow { dilate: 5, reach: 42, feather: 2 });
    assert_eq!(Grow::for_page(720, 10.0), Grow { dilate: 4, reach: 16, feather: 2 });
    assert_eq!(Grow::for_page(6000, 500.0), Grow { dilate: 10, reach: 128, feather: 2 });
    assert_eq!(Grow::for_page(1000, f32::NAN).reach, 16);
}

#[test]
fn local_max_is_the_brightest_around() {
    let v: Vec<f32> = (0..W * H).map(|i| ((i * 7919) % 101) as f32 / 100.0).collect();
    let got = local_max(W, H, &v, 3);
    for (i, g) in got.iter().enumerate() {
        let (x, y) = ((i % W) as i64, (i / W) as i64);
        let mut m = 0.0f32;
        for yy in (y - 3).max(0)..=(y + 3).min(H as i64 - 1) {
            for xx in (x - 3).max(0)..=(x + 3).min(W as i64 - 1) {
                m = m.max(v[yy as usize * W + xx as usize]);
            }
        }
        assert_eq!(*g, m, "at {x},{y}");
    }
}

#[test]
fn a_letter_the_stroke_grazes_goes_whole_and_its_neighbours_and_the_outline_stay() {
    // A letter (40..52 × 40..70) the stroke covers only its left half of, a letter 6 px to the
    // right of the stroke's grown edge it never touches, and a long outline 3 px below the stroke.
    let lum = page(&[(40, 40, 52, 70), (62, 40, 70, 70), (0, 76, W, 79)]);
    let hole = expand(W, H, &lum, &painted(20, 38, 46, 73), G);
    let at = |x: usize, y: usize| hole.mask[y * W + x];
    assert!(at(51, 55) && at(51, 41) && at(52, 55), "the grazed letter and its soft edge go");
    assert!(!at(62, 55) && !at(66, 55), "the untouched letter stays");
    assert!((0..W).all(|x| !at(x, 76) && !at(x, 77) && !at(x, 78)), "the outline stays whole");
    assert!(at(18, 55) && at(16, 55), "the stroke grows by the dilation");
    // Weights: 1 over the core, then 2/3 and 1/3 over the feather ring.
    let wt = |x: usize, y: usize| hole.weight[y * W + x];
    assert_eq!(wt(30, 55), 1.0);
    assert_eq!(wt(16, 55), 1.0);
    assert!((wt(15, 55) - 2.0 / 3.0).abs() < 1e-6 && (wt(14, 55) - 1.0 / 3.0).abs() < 1e-6, "{} {}", wt(15, 55), wt(14, 55));
    assert_eq!(wt(13, 55), 0.0);
    assert!(!at(13, 55));
    // The untouched letter is hidden from the model (not changed); the outline isn't.
    let hidden = |x: usize, y: usize| hole.hide[y * W + x];
    assert!(hidden(66, 55) && hidden(61, 55), "the letter beside, with its soft edge");
    assert!(!hidden(100, 77) && !hidden(51, 55), "not the outline, not the hole");
}

#[test]
fn ink_that_runs_past_the_reach_is_not_taken_on_screentone_too() {
    // A grey screentone field (dots on 0.85) with a long dark stroke through it: the stroke's
    // touched run reaches past the reach, so only the painted part and the dilation go, and the
    // dilation keeps off the run.
    let mut lum = vec![0.85f32; W * H];
    for y in (0..H).step_by(4) {
        for x in (0..W).step_by(4) {
            lum[y * W + x] = 0.3;
        }
    }
    for y in 50..56 {
        for x in 0..W {
            lum[y * W + x] = 0.0;
        }
    }
    let hole = expand(W, H, &lum, &painted(89, 40, 110, 66), G);
    let at = |x: usize, y: usize| hole.mask[y * W + x];
    assert!(!at(60, 52) && !at(140, 52), "the long run stays outside the stroke");
    assert!(!at(114, 52), "the dilation keeps off the run it doesn't take");
    assert!(at(114, 45), "the dilation grows over the tone between");
    // An isolated dot the stroke touches is taken.
    assert!(at(88, 44), "the dot at the stroke's edge goes");
    assert!(hole.hide.iter().all(|h| !h), "screentone and the long stroke are no letters to hide");
}

#[test]
fn the_fill_takes_the_papers_white_where_the_hole_meets_paper() {
    // A white page, a hole (60..120 × 30..90) the model filled a shade grey with a dark blob in
    // the middle (art it drew), and screentone to the right of x = 140.
    // Below y = 100, a light grey that is as flat (a fine screentone), with a hole of its own.
    let mut orig: Vec<f32> = (0..W * H * 3).map(|k| if k / 3 / W >= 100 { 0.8 } else { 1.0 }).collect();
    for y in 0..H {
        for x in 140..W {
            if x % 3 == 0 && y % 3 == 0 {
                orig[(y * W + x) * 3..(y * W + x) * 3 + 3].copy_from_slice(&[0.2; 3]);
            }
        }
    }
    let hole: Vec<bool> = (0..W * H).map(|i| (60..120).contains(&(i % W)) && (30..90).contains(&(i / W))).collect();
    let tone_hole: Vec<bool> = (0..W * H)
        .map(|i| ((160..180).contains(&(i % W)) && (30..90).contains(&(i / W))) || ((20..40).contains(&(i % W)) && (105..115).contains(&(i / W))))
        .collect();
    let mut generated = orig.clone();
    for i in 0..W * H {
        let (x, y) = (i % W, i / W);
        let v = if (85..95).contains(&x) && (55..65).contains(&y) { 0.1 } else { 0.93 };
        if hole[i] || tone_hole[i] {
            generated[i * 3..i * 3 + 3].copy_from_slice(&[v; 3]);
        }
    }
    let both: Vec<bool> = hole.iter().zip(&tone_hole).map(|(a, b)| *a || *b).collect();
    settle_paper(W, H, &orig, &both, &mut generated);
    let g = |x: usize, y: usize| generated[(y * W + x) * 3];
    assert_eq!((g(60, 30), g(70, 40), g(119, 89)), (1.0, 1.0, 1.0), "the paper's white");
    assert_eq!(g(90, 60), 0.1, "what the model drew stays");
    assert_eq!(g(170, 60), 0.93, "on screentone the model's fill stays");
    assert_eq!(g(30, 110), 0.93, "on a light grey too: it isn't paper");
}

#[test]
fn nothing_painted_is_nothing_to_fill() {
    let hole = expand(W, H, &page(&[]), &vec![false; W * H], G);
    assert!(hole.mask.iter().all(|m| !m) && hole.weight.iter().all(|w| *w == 0.0));
}
