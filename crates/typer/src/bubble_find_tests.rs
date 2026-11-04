use super::*;

const W: usize = 420;
const H: usize = 320;

fn area() -> photocraft_geom::Rect {
    photocraft_geom::Rect::new(0, 0, W as i32, H as i32)
}

/// A white page with the outline (`t` px, black) of the shape `inside` draws.
fn page(inside: &dyn Fn(f64, f64) -> bool, t: i32) -> Vec<[u8; 4]> {
    let fill: Vec<bool> = (0..W * H).map(|i| inside((i % W) as f64 + 0.5, (i / W) as f64 + 0.5)).collect();
    let get = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < W && (y as usize) < H && fill[y as usize * W + x as usize];
    (0..W * H)
        .map(|i| {
            let (x, y) = ((i % W) as i32, (i / W) as i32);
            let edge = fill[i] && (-t..=t).any(|dy| (-t..=t).any(|dx| dx * dx + dy * dy <= t * t && !get(x + dx, y + dy)));
            if edge { [0, 0, 0, 255] } else { [255, 255, 255, 255] }
        })
        .collect()
}

fn oval(cx: f64, cy: f64, a: f64, b: f64) -> impl Fn(f64, f64) -> bool {
    move |x, y| ((x - cx) / a).powi(2) + ((y - cy) / b).powi(2) <= 1.0
}

/// Inside the triangle `p`.
fn tri(p: [(f64, f64); 3]) -> impl Fn(f64, f64) -> bool {
    move |x, y| {
        let s = |a: (f64, f64), b: (f64, f64)| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        let (d1, d2, d3) = (s(p[0], p[1]), s(p[1], p[2]), s(p[2], p[0]));
        (d1 >= 0.0 && d2 >= 0.0 && d3 >= 0.0) || (d1 <= 0.0 && d2 <= 0.0 && d3 <= 0.0)
    }
}

fn found(img: &[[u8; 4]], at: (i32, i32)) -> Found {
    find(img, area(), at).unwrap_or_else(|m| panic!("no bubble at {at:?}: {m:?}"))
}

#[test]
fn an_oval_is_found_up_to_its_outline() {
    let img = page(&oval(210.0, 160.0, 120.0, 80.0), 3);
    let f = found(&img, (210, 160));
    let b = f.interior.bounds().unwrap();
    // The outline is 3 px thick inside the 240 × 160 oval.
    assert!((b.x0 - 93).abs() <= 2 && (b.x1 - 327).abs() <= 2 && (b.y0 - 83).abs() <= 2 && (b.y1 - 237).abs() <= 2, "{b:?}");
    let body = f.body.bounds().unwrap();
    assert!((body.width() - b.width()).abs() <= 3 && (body.height() - b.height()).abs() <= 3, "no tail to drop: {body:?}");
    // A click on lettering inside the oval finds the same bubble.
    let mut lettered = img.clone();
    for y in 150..170 {
        for x in 180..240 {
            if (x + y) % 3 == 0 {
                lettered[y * W + x] = [0, 0, 0, 255];
            }
        }
    }
    assert_eq!(found(&lettered, (200, 160)).interior.bounds(), Some(b), "letters are holes, filled");
}

#[test]
fn a_tail_belongs_to_the_bubble_but_not_to_its_body() {
    let body = oval(190.0, 130.0, 110.0, 70.0);
    let tail = tri([(220.0, 180.0), (260.0, 175.0), (300.0, 290.0)]);
    let img = page(&|x, y| body(x, y) || tail(x, y), 3);
    let f = found(&img, (190, 130));
    let all = f.interior.bounds().unwrap();
    assert!(all.y1 > 260, "the tail is in the interior: {all:?}");
    let b = f.body.bounds().unwrap();
    assert!(b.y1 < 210, "the body stops where the tail starts: {b:?}");
    assert!((b.x0 - 83).abs() <= 3 && (b.x1 - 297).abs() <= 3, "{b:?}");
}

#[test]
fn a_small_gap_in_the_outline_does_not_leak() {
    let mut img = page(&oval(210.0, 160.0, 120.0, 80.0), 2);
    // A 3 px break in the outline at the right side.
    for y in 158..161 {
        for x in 320..335 {
            img[y * W + x] = [255, 255, 255, 255];
        }
    }
    let f = found(&img, (210, 160));
    let b = f.interior.bounds().unwrap();
    assert!(b.x1 <= 334, "the flood stays inside: {b:?}");
    // A wide opening is no bubble: the page around it is reached.
    let mut open = page(&oval(210.0, 160.0, 120.0, 80.0), 2);
    for y in 140..181 {
        for x in 320..335 {
            open[y * W + x] = [255, 255, 255, 255];
        }
    }
    assert!(matches!(find(&open, area(), (210, 160)).map(|_| ()), Err(Miss::Edge | Miss::TooBig)));
}

#[test]
fn a_spiky_shout_bubble_is_found() {
    let (cx, cy, spikes) = (210.0, 160.0, 14);
    let star = move |x: f64, y: f64| {
        let (dx, dy) = (x - cx, y - cy);
        let a = dy.atan2(dx);
        let phase = (a / std::f64::consts::TAU * f64::from(spikes)).rem_euclid(1.0);
        // From 0.62 between the spikes to 1 at their tips.
        let reach = 0.62 + 0.38 * (1.0 - (phase - 0.5).abs() * 2.0);
        (dx / 150.0).powi(2) + (dy / 120.0).powi(2) <= reach * reach
    };
    let img = page(&star, 3);
    let f = found(&img, (210, 160));
    let all = f.interior.bounds().unwrap();
    let body = f.body.bounds().unwrap();
    assert!(all.width() > 250 && all.height() > 200, "the spikes are in: {all:?}");
    assert!(body.width() >= 160 && body.width() <= all.width(), "{body:?}");
}

#[test]
fn art_the_page_and_dark_pixels_are_no_bubble() {
    // The white page around a bubble reaches the edge.
    let img = page(&oval(210.0, 160.0, 120.0, 80.0), 3);
    assert_eq!(find(&img, area(), (10, 10)).map(|_| ()), Err(Miss::Edge));
    // In a dark area.
    let mut dark = img.clone();
    for y in 10..110 {
        for x in 10..110 {
            dark[y * W + x] = [30, 30, 30, 255];
        }
    }
    assert_eq!(find(&dark, area(), (60, 60)).map(|_| ()), Err(Miss::Dark));
    // A light patch in mid-light art (no dark outline): text over art.
    let art: Vec<[u8; 4]> = (0..W * H)
        .map(|i| {
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let inside = ((x - 210.0) / 90.0).powi(2) + ((y - 160.0) / 60.0).powi(2) <= 1.0;
            let v = if inside { 250 } else { 175 + ((x * 0.3).sin() * 10.0) as i32 } as u8;
            [v, v, v, 255]
        })
        .collect();
    assert_eq!(find(&art, area(), (210, 160)).map(|_| ()), Err(Miss::NotBubble));
    // A dot too small for a line of text.
    let dot = page(&oval(210.0, 160.0, 9.0, 7.0), 2);
    assert_eq!(find(&dot, area(), (210, 160)).map(|_| ()), Err(Miss::TooSmall));
}

/// A page of screentone dots (mid grey) with a white bubble, and a light grey bubble on a darker
/// grey page: the inside is lighter than the outline, not white.
#[test]
fn bubbles_on_toned_and_grey_pages_are_found() {
    let inside = oval(210.0, 160.0, 120.0, 80.0);
    let base = page(&inside, 3);
    let tone: Vec<[u8; 4]> = (0..W * H)
        .map(|i| {
            let (x, y) = ((i % W) as f64 + 0.5, (i / W) as f64 + 0.5);
            if inside(x, y) {
                base[i]
            } else if (i % W) % 4 < 2 && (i / W) % 4 < 2 {
                [40, 40, 40, 255]
            } else {
                [175, 175, 175, 255]
            }
        })
        .collect();
    assert!(find(&tone, area(), (210, 160)).is_ok(), "on screentone");
    let grey: Vec<[u8; 4]> = base
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let (x, y) = ((i % W) as f64 + 0.5, (i / W) as f64 + 0.5);
            if p[0] == 0 {
                [20, 20, 20, 255]
            } else if inside(x, y) {
                [170, 170, 170, 255]
            } else {
                [110, 110, 110, 255]
            }
        })
        .collect();
    let f = find(&grey, area(), (210, 160)).expect("a light grey bubble on a grey page");
    let b = f.interior.bounds().unwrap();
    assert!(b.width() > 220 && b.width() < 240, "{b:?}");
}

/// A click on the outline, or on a letter near it, still finds the bubble (not the page).
#[test]
fn a_click_on_the_outline_finds_the_bubble() {
    let img = page(&oval(210.0, 160.0, 120.0, 80.0), 3);
    // The left edge of the outline, at x 90..93.
    let f = find(&img, area(), (91, 160)).expect("from the outline");
    assert!(f.interior.bounds().unwrap().width() > 220);
}

/// Boxes (narration) are rectangular, bubbles aren't.
#[test]
fn rectangularity_tells_boxes_from_bubbles() {
    let r = |img: &[[u8; 4]]| rectangularity(&find(img, area(), (210, 160)).unwrap().interior).unwrap().0;
    let ov = r(&page(&oval(210.0, 160.0, 120.0, 80.0), 3));
    assert!(ov < 0.85, "an oval: {ov}");
    let boxed = r(&page(&|x, y| (90.0..330.0).contains(&x) && (90.0..230.0).contains(&y), 3));
    assert!(boxed > 0.97, "a box: {boxed}");
    // A rounded rectangle, corner radius 12.
    let rr = move |x: f64, y: f64| {
        let (cx, cy) = (x.clamp(102.0, 318.0), y.clamp(102.0, 218.0));
        (90.0..330.0).contains(&x) && (90.0..230.0).contains(&y) && (x - cx).powi(2) + (y - cy).powi(2) <= 144.0
    };
    let rounded = r(&page(&rr, 3));
    assert!(rounded > BOX_RECTANGULARITY, "a rounded box: {rounded}");
    // A tilted box is still a box: its minimum rectangle turns with it.
    let (c, s) = (0.2f64.cos(), 0.2f64.sin());
    let tilted = r(&page(
        &move |x, y| {
            let (u, v) = ((x - 210.0) * c + (y - 160.0) * s, -(x - 210.0) * s + (y - 160.0) * c);
            u.abs() < 110.0 && v.abs() < 60.0
        },
        3,
    ));
    assert!(tilted > 0.95, "a tilted box: {tilted}");
    let (cx, cy) = (210.0, 160.0);
    let star = move |x: f64, y: f64| {
        let (dx, dy) = (x - cx, y - cy);
        let phase = (dy.atan2(dx) / std::f64::consts::TAU * 14.0).rem_euclid(1.0);
        let reach = 0.62 + 0.38 * (1.0 - (phase - 0.5).abs() * 2.0);
        (dx / 150.0).powi(2) + (dy / 120.0).powi(2) <= reach * reach
    };
    let spiky = r(&page(&star, 3));
    assert!(spiky < 0.75, "a spiky bubble: {spiky}");
}

/// The search starts in a window around the click; on a page larger than the window every
/// answer (found, refused, a bubble larger than the window, one cut by the page edge) is the
/// page's.
#[test]
fn the_window_answers_as_the_whole_page() {
    let (pw, ph) = (1800usize, 2600usize);
    let big = oval(900.0, 1500.0, 820.0, 700.0);
    let small = oval(400.0, 300.0, 150.0, 100.0);
    let cut = oval(1790.0, 300.0, 120.0, 90.0);
    let inside = |x: f64, y: f64| small(x, y) || big(x, y) || cut(x, y);
    let fill: Vec<bool> = (0..pw * ph).map(|i| inside((i % pw) as f64 + 0.5, (i / pw) as f64 + 0.5)).collect();
    let get = |x: i32, y: i32| x >= 0 && y >= 0 && (x as usize) < pw && (y as usize) < ph && fill[y as usize * pw + x as usize];
    let img: Vec<[u8; 4]> = (0..pw * ph)
        .map(|i| {
            let (x, y) = ((i % pw) as i32, (i / pw) as i32);
            let edge = fill[i] && (-3..=3).any(|dy| (-3..=3).any(|dx| dx * dx + dy * dy <= 9 && !get(x + dx, y + dy)));
            // Grey page outside the bubbles, so the page background isn't white paper.
            if edge { [0, 0, 0, 255] } else if fill[i] { [255, 255, 255, 255] } else { [150, 150, 150, 255] }
        })
        .collect();
    let page = photocraft_geom::Rect::new(0, 0, pw as i32, ph as i32);
    let whole = |at: (i32, i32), walls: bool| search(&img, page, page, page, at, walls, MAX_PAGE_SHARE, &Opts::default()).unwrap();
    let key = |r: &Result<Found, Miss>| r.as_ref().map(|f| (f.interior.bounds(), f.interior.count(), f.body.bounds())).map_err(|m| *m);
    for (at, walls) in [((400, 300), false), ((900, 1500), false), ((900, 1500), true), ((1750, 300), false), ((1750, 300), true), ((100, 1000), false), ((60, 2550), true)] {
        let got = find_with(&img, page, at, walls, MAX_PAGE_SHARE, &Opts::default());
        assert_eq!(key(&got), key(&whole(at, walls)), "at {at:?} walls {walls}");
    }
    let f = find(&img, page, (900, 1500)).unwrap();
    assert!(f.interior.bounds().unwrap().width() > 2 * WINDOWS[0], "the bubble larger than the first window is found whole");
    assert!(find(&img, page, (400, 300)).is_ok());
}

/// The 1x test page with a bubble whose 3 px outline has a 4 px break at the right and two lines
/// of 3 px strokes (lettering) inside, upscaled `k` times bilinearly (as an upscaler or a
/// high-resolution scan leaves it: grey anti-aliased edges, the break `4k` px wide).
fn scaled_page(k: usize) -> (Vec<[u8; 4]>, photocraft_geom::Rect) {
    let mut img = page(&oval(210.0, 160.0, 120.0, 80.0), 3);
    for y in 158..162 {
        for x in 320..335 {
            img[y * W + x] = [255, 255, 255, 255];
        }
    }
    for (line, y0) in [(0, 132), (1, 168)] {
        for i in 0..8 {
            let x0 = 140 + i * 18 + line * 5;
            for y in y0..y0 + 22 {
                for x in x0..x0 + 3 {
                    img[y * W + x] = [0, 0, 0, 255];
                }
            }
            for y in y0 + 9..y0 + 12 {
                for x in x0..x0 + 11 {
                    img[y * W + x] = [0, 0, 0, 255];
                }
            }
        }
    }
    let (sw, sh) = (W * k, H * k);
    let get = |x: usize, y: usize| img[y.min(H - 1) * W + x.min(W - 1)];
    let up: Vec<[u8; 4]> = (0..sw * sh)
        .map(|i| {
            let (fx, fy) = (((i % sw) as f64 + 0.5) / k as f64 - 0.5, ((i / sw) as f64 + 0.5) / k as f64 - 0.5);
            let (x0, y0) = (fx.floor().max(0.0), fy.floor().max(0.0));
            let (tx, ty) = ((fx - x0).clamp(0.0, 1.0), (fy - y0).clamp(0.0, 1.0));
            let (x0, y0) = (x0 as usize, y0 as usize);
            let v = |p: [u8; 4]| f64::from(p[0]);
            let top = v(get(x0, y0)) * (1.0 - tx) + v(get(x0 + 1, y0)) * tx;
            let bot = v(get(x0, y0 + 1)) * (1.0 - tx) + v(get(x0 + 1, y0 + 1)) * tx;
            let l = (top * (1.0 - ty) + bot * ty).round() as u8;
            [l, l, l, 255]
        })
        .collect();
    (up, photocraft_geom::Rect::new(0, 0, sw as i32, sh as i32))
}

/// The same bubble at 1x, 2x and 3x is found on the first try (no walled or page-wide retry), the
/// same shape scaled: the gap radius, the reach and the windows follow the ink's stroke width.
/// With 1x sizes the upscaled break lets the flood out.
#[test]
fn a_bubble_is_found_first_try_at_any_resolution() {
    let mut first = None;
    for k in [1usize, 2, 3] {
        let (img, area) = scaled_page(k);
        let ki = k as i32;
        let scale = ink_scale(&img, area, (210 * ki, 160 * ki));
        assert!((scale - k as f64).abs() <= 0.5 * k as f64 - 0.01 || (k == 1 && scale == 1.0), "{k}x: scale {scale}");
        for click in [(210, 160), (141, 140), (300, 120)] {
            let at = (click.0 * ki, click.1 * ki);
            let bounded = Opts::auto(&img, area, at).bounded(area);
            let f = find_opts(&img, area, at, false, &bounded).unwrap_or_else(|m| panic!("{k}x at {at:?}: {m:?} ({bounded:?})"));
            let b = f.interior.bounds().unwrap();
            assert!(b.x1 <= 334 * ki, "{k}x: the flood stays inside: {b:?}");
            let one = *first.get_or_insert([b.x0, b.y0, b.x1, b.y1]);
            for (got, want) in [b.x0, b.y0, b.x1, b.y1].iter().zip(one) {
                assert!((got - want * ki).abs() <= 3 * ki, "{k}x: {b:?} against 1x {one:?}");
            }
            assert_eq!(find(&img, area, at).map(|f| f.interior.bounds()), Ok(Some(b)), "{k}x: find sizes itself the same");
        }
        if k > 1 {
            assert!(Opts::auto(&img, area, (210 * ki, 160 * ki)).gap_radius > GAP_RADIUS, "{k}x: a wider gap radius");
            let one_x = Opts { page: false, ..Opts::default() };
            assert!(find_opts(&img, area, (210 * ki, 160 * ki), false, &one_x).is_err(), "{k}x with 1x sizes leaks");
        }
    }
}

/// Explicit sizes: a gap radius wide enough closes the upscaled break without measuring.
#[test]
fn explicit_sizes_close_a_wide_break() {
    let (img, area) = scaled_page(2);
    let o = Opts { gap_radius: 4, reach: 48, ..Opts::default() };
    assert!(find_opts(&img, area, (420, 320), false, &o).is_ok());
    // Absurd sizes are clamped, not trusted.
    let o = Opts { gap_radius: i32::MAX, reach: -5, windows: [i32::MIN, 0], scale: f64::NAN, page: false };
    let _ = find_opts(&img, area, (420, 320), false, &o);
}

/// A 3000 × 4300 page (2x): panels with thick borders, gutters, a dark blob and a bubble. A
/// bounded search's miss stays in its window and returns fast (release), a page-wide one only
/// when asked.
#[test]
fn a_miss_on_a_large_page_is_bounded() {
    let (pw, ph) = (3000usize, 4300usize);
    let mut img = vec![[255u8, 255, 255, 255]; pw * ph];
    let mut rect = |x0: usize, y0: usize, x1: usize, y1: usize, v: u8| {
        for y in y0..y1 {
            img[y * pw + x0..y * pw + x1].fill([v, v, v, 255]);
        }
    };
    // Two rows of panels, 8 px borders, 80 px gutters; light art inside.
    for (x0, y0, x1, y1) in [(80, 80, 1460, 2100), (1540, 80, 2920, 2100), (80, 2180, 2920, 4220)] {
        rect(x0, y0, x1, y0 + 8, 0);
        rect(x0, y1 - 8, x1, y1, 0);
        rect(x0, y0, x0 + 8, y1, 0);
        rect(x1 - 8, y0, x1, y1, 0);
    }
    rect(600, 3000, 1400, 3800, 20);
    let e = oval(2200.0, 1000.0, 300.0, 200.0);
    for y in 700..1300 {
        for x in 1800..2600 {
            let d = ((x as f64 + 0.5 - 2200.0) / 300.0).powi(2) + ((y as f64 + 0.5 - 1000.0) / 200.0).powi(2);
            if !e(x as f64 + 0.5, y as f64 + 0.5) && d <= 1.05 {
                img[y * pw + x] = [0, 0, 0, 255];
            }
        }
    }
    let area = photocraft_geom::Rect::new(0, 0, pw as i32, ph as i32);
    let bounded = |at| Opts::auto(&img, area, at).bounded(area);
    let mut worst = 0f64;
    // The left gutter by the page edge, the gutter between rows, the dark blob, the blank panel
    // under the bubble (light, but past the window: art in a real page).
    for (at, walled) in [((40, 1000), false), ((40, 1000), true), ((1500, 2140), false), ((1500, 2140), true), ((1000, 3400), false), ((1000, 3400), true), ((2000, 3000), false), ((2000, 3000), true)] {
        let o = bounded(at);
        let t = std::time::Instant::now();
        let r = find_opts(&img, area, at, walled, &o);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        worst = worst.max(ms);
        eprintln!("miss at {at:?} walled {walled}: {:?} in {ms:.1} ms", r.as_ref().map(|_| ()));
        assert!(r.is_err(), "{at:?}");
    }
    assert!(find_opts(&img, area, (2200, 1000), false, &bounded((2200, 1000))).is_ok(), "the bubble still is found");
    if !cfg!(debug_assertions) {
        assert!(worst < 150.0, "{worst} ms");
    }
}
