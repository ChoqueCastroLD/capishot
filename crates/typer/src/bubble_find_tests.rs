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
    for y in 20..60 {
        for x in 20..60 {
            dark[y * W + x] = [30, 30, 30, 255];
        }
    }
    assert_eq!(find(&dark, area(), (40, 40)).map(|_| ()), Err(Miss::Dark));
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
