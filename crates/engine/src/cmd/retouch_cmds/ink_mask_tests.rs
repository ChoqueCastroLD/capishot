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

const G: Grow = Grow { dilate: 4, reach: 24 };

#[test]
fn the_page_sets_the_growth() {
    assert_eq!(Grow::for_page(1644, 42.0), Grow { dilate: 5, reach: 42 });
    assert_eq!(Grow::for_page(720, 10.0), Grow { dilate: 4, reach: 16 });
    assert_eq!(Grow::for_page(6000, 500.0), Grow { dilate: 10, reach: 128 });
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
    // A letter (40..52 × 40..70) the stroke covers only its left half of, a letter 14 px to the
    // right of it the stroke never touches, and a long outline 10 px below the stroke.
    let lum = page(&[(40, 40, 52, 70), (66, 40, 74, 70), (0, 83, W, 86)]);
    let hole = expand(W, H, &lum, &painted(20, 38, 46, 73), G);
    let at = |x: usize, y: usize| hole[y * W + x];
    assert!(at(51, 55) && at(51, 41) && at(52, 55), "the grazed letter and its soft edge go");
    assert!(!at(58, 55), "not the paper beside it");
    assert!(!at(66, 55) && !at(70, 55), "the untouched letter stays");
    assert!((0..W).all(|x| !at(x, 83) && !at(x, 84) && !at(x, 85)), "the outline stays whole");
    assert!(at(16, 55), "the stroke grows by the dilation");
    assert!(!at(100, 30) && !at(2, 55), "open paper stays");
}

/// An SFX: black letters with a white outline on a black background. The hole is the stroke and
/// its dilation only: it doesn't grow over the white around it (the outline, hair highlights),
/// which took art with it and left the model a smear to rebuild.
#[test]
fn the_hole_does_not_grow_over_the_light_around_it() {
    let mut lum = vec![0.0f32; W * H];
    for y in 30..90 {
        for x in 30..170 {
            lum[y * W + x] = 1.0;
        }
    }
    for (x0, x1) in [(40, 70), (85, 115), (130, 160)] {
        for y in 40..80 {
            for x in x0..x1 {
                lum[y * W + x] = 0.0;
            }
        }
    }
    let hole = expand(W, H, &lum, &painted(45, 54, 155, 66), G);
    let at = |x: usize, y: usize| hole[y * W + x];
    assert!(at(100, 50) && at(41, 60) && at(159, 60), "the stroke grown by the dilation");
    assert!(!at(31, 31) && !at(100, 31) && !at(168, 88), "the outline beyond the dilation stays");
    assert!(!at(10, 10) && !at(100, 110) && !at(190, 60), "the background stays");
}

/// Hatching beside the stroke: the hole's edge is smooth, not a comb along the strands.
#[test]
fn the_hole_is_smooth_along_hatching() {
    let lum: Vec<f32> = (0..W * H).map(|i| if ((i % W) / 3).is_multiple_of(2) { 1.0 } else { 0.0 }).collect();
    let hole = expand(W, H, &lum, &painted(60, 40, 140, 80), G);
    for y in 20..100 {
        let row: Vec<bool> = (55..145).map(|x| hole[y * W + x]).collect();
        let edges = row.windows(2).filter(|p| p[0] != p[1]).count();
        assert!(edges <= 2, "row {y} of the hole is one span: {edges} edges");
    }
}

#[test]
fn nothing_painted_is_nothing_to_fill() {
    let hole = expand(W, H, &page(&[]), &vec![false; W * H], G);
    assert!(hole.iter().all(|m| !m));
}
