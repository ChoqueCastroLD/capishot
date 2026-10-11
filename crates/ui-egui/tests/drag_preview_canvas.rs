//! #253: the selection tools' drag preview must be on screen while the button is held, on any
//! pixels, at 1× and 2× (HiDPI).
//!
//! v0.2.0 drew the Rectangular and Elliptical Marquee previews as a plain 1 px white outline. On a
//! new document (white background by default) that is white on white, so nothing showed until
//! release turned it into a black-and-white marching-ants selection. #188 replaced it with
//! contrasting animated ants; these tests keep it that way.
//!
//! They drive the real app offscreen on wgpu with real egui pointer events (press, then a series
//! of moves past the click distance), render the frame mid-drag and look at the pixels along the
//! preview's edges on white and on black documents. They also check the preview follows every
//! move (each move's frame shows the new outline) and that the animated ants keep frames coming
//! while the pointer is held still. Skips when no GPU adapter exists (like `live_stroke_canvas.rs`).

use egui::{Modifiers, PointerButton, Pos2};
use photocraft_ui_egui::PhotocraftApp;
use photocraft_ui_egui::canvas::ViewXform;
use photocraft_ui_egui::control::{ControlRequest, handle};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

/// A rendered frame: RGBA8, physical pixels.
struct Image {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Image {
    /// Luma of pixel `(x, y)`, or `None` off the frame.
    fn luma(&self, x: i64, y: i64) -> Option<u8> {
        if x < 0 || y < 0 || x >= i64::from(self.w) || y >= i64::from(self.h) {
            return None;
        }
        let i = (y as usize * self.w as usize + x as usize) * 4;
        let p = self.px.get(i..i + 3)?;
        Some(((u16::from(p[0]) * 3 + u16::from(p[1]) * 6 + u16::from(p[2])) / 10) as u8)
    }
}

fn harness(ppp: f32) -> Option<Harness> {
    let built = std::panic::catch_unwind(|| {
        egui_kittest::Harness::builder().with_size(egui::vec2(900.0, 640.0)).with_pixels_per_point(ppp).with_max_steps(64).wgpu().build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            if let Some(rs) = cc.wgpu_render_state.as_ref() {
                app.set_wgpu(rs.clone());
            }
            app
        })
    });
    match built {
        Ok(h) => Some(h),
        Err(_) => {
            eprintln!("skipping: no GPU adapter");
            None
        }
    }
}

fn control(h: &mut Harness, method: &str, params: Value) {
    let ctx = h.ctx.clone();
    let (req, _rx) = ControlRequest::new(method, params);
    handle(h.state_mut(), &ctx, &req);
    h.run_steps(3);
}

/// A fresh 400 × 300 document in `background` at 100 %, with `tool` active.
fn setup(h: &mut Harness, background: &str, tool: &str) {
    {
        let app = h.state_mut();
        while app.session.active().is_some() {
            app.run("file.close", json!({"discard": true})).expect("close");
        }
        app.run("file.new", json!({"width": 400, "height": 300, "background": background})).expect("new");
        app.sync_views();
        app.ui.extras.rulers = false;
    }
    control(h, "ui.set", json!({"tool": tool, "zoom": 1.0, "center": [200, 150]}));
    h.run_steps(3);
}

fn xf(h: &Harness) -> ViewXform {
    // The same mapping the canvas draws with: `View::zoom` is device pixels per document pixel,
    // and the transform works in the viewport's egui points.
    ViewXform::active(h.state()).expect("an active document view")
}

/// Screen point (egui points) of document point `(x, y)`.
fn screen(h: &Harness, x: f32, y: f32) -> Pos2 {
    xf(h).to_screen(x, y)
}

fn pointer_move(h: &mut Harness, x: f32, y: f32) {
    let p = screen(h, x, y);
    h.event(egui::Event::PointerMoved(p));
    h.step();
}

fn button(h: &mut Harness, x: f32, y: f32, pressed: bool) {
    let p = screen(h, x, y);
    h.event(egui::Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    h.step();
}

/// Press at `from` and move to `to` in a few real pointer moves, leaving the button held.
fn drag_to(h: &mut Harness, from: [f32; 2], to: [f32; 2]) {
    pointer_move(h, from[0], from[1]);
    button(h, from[0], from[1], true);
    for i in 1..=6 {
        let t = i as f32 / 6.0;
        pointer_move(h, from[0] + (to[0] - from[0]) * t, from[1] + (to[1] - from[1]) * t);
    }
}

fn render(h: &mut Harness) -> Image {
    let img = h.render().expect("render");
    Image { w: img.width(), h: img.height(), px: img.into_raw() }
}

/// Fraction of positions along the screen segment `a`–`b` (points) where a pixel within ±2
/// physical pixels across the line passes `test`.
fn coverage(img: &Image, ppp: f32, a: Pos2, b: Pos2, test: impl Fn(u8) -> bool) -> f32 {
    let (a, b) = (a * ppp, b * ppp);
    let len = a.distance(b).max(1.0);
    let dir = (b - a) / len;
    let normal = egui::vec2(-dir.y, dir.x);
    // Skip rectangle corners, where other edges and the readout may be.
    let n = (len as usize).saturating_sub(16);
    let mut hits = 0;
    for i in 0..n {
        let p = a + dir * (8.0 + i as f32);
        let hit = (-2..=2).any(|o| {
            let q = p + normal * o as f32;
            img.luma(q.x.round() as i64, q.y.round() as i64).is_some_and(&test)
        });
        hits += usize::from(hit);
    }
    hits as f32 / n.max(1) as f32
}

/// Sample eight dash periods along the actual ellipse, including its curvature.
/// A short tangent segment can land on antialiasing ramps instead of the stroke's core.
fn ellipse_coverage(images: &[&Image], ppp: f32, center: Pos2, radius: egui::Vec2, midpoint: Pos2, test: impl Fn(u8) -> bool) -> f32 {
    let (center, radius, midpoint) = (center * ppp, radius * ppp, midpoint * ppp);
    let horizontal = (midpoint.y - center.y).abs() > (midpoint.x - center.x).abs();
    let mut hits = 0;
    for offset in -32..32 {
        let p = if horizontal {
            let x = midpoint.x + offset as f32;
            let y = center.y + (midpoint.y - center.y).signum() * radius.y * (1.0 - ((x - center.x) / radius.x).powi(2)).max(0.0).sqrt();
            egui::pos2(x, y)
        } else {
            let y = midpoint.y + offset as f32;
            let x = center.x + (midpoint.x - center.x).signum() * radius.x * (1.0 - ((y - center.y) / radius.y).powi(2)).max(0.0).sqrt();
            egui::pos2(x, y)
        };
        let normal = if horizontal { egui::vec2(0.0, 1.0) } else { egui::vec2(1.0, 0.0) };
        hits += usize::from((-2..=2).any(|o| {
            let q = p + normal * o as f32;
            images.iter().any(|img| img.luma(q.x.round() as i64, q.y.round() as i64).is_some_and(&test))
        }));
    }
    hits as f32 / 64.0
}

/// Ink that contrasts with `background`: dark on white, light on black.
fn ink(background: &str) -> impl Fn(u8) -> bool {
    let white = background == "white";
    move |l| if white { l < 64 } else { l > 192 }
}

/// The four edges of the document rectangle `[x0, y0]`–`[x1, y1]` on screen.
fn edges(h: &Harness, a: [f32; 2], b: [f32; 2]) -> [(Pos2, Pos2); 4] {
    let (tl, tr) = (screen(h, a[0], a[1]), screen(h, b[0], a[1]));
    let (bl, br) = (screen(h, a[0], b[1]), screen(h, b[0], b[1]));
    [(tl, tr), (tr, br), (bl, br), (tl, bl)]
}

#[test]
fn marquee_previews_show_while_dragging_on_white_and_black_at_1x_and_2x() {
    for ppp in [1.0, 2.0] {
        let Some(mut h) = harness(ppp) else { return };
        h.run_steps(4);
        for tool in ["rectMarquee", "ellipseMarquee"] {
            for bg in ["white", "black"] {
                setup(&mut h, bg, tool);
                let (a, b) = ([100.0, 80.0], [300.0, 220.0]);
                let before = render(&mut h);
                let before_time = h.ctx.input(|input| input.time) + 0.4;
                h.input_mut().time = Some(before_time);
                h.step();
                let before_next = render(&mut h);
                drag_to(&mut h, a, b);
                let img = render(&mut h);
                if let Some(directory) = std::env::var_os("PHOTOCRAFT_DRAG_PREVIEW_FIXTURES") {
                    let directory = std::path::PathBuf::from(directory);
                    std::fs::create_dir_all(&directory).unwrap();
                    h.render().expect("capture render").save(directory.join(format!("{tool}-{bg}-{ppp}x.png"))).unwrap();
                }
                if tool == "rectMarquee" {
                    for (i, (p, q)) in edges(&h, a, b).into_iter().enumerate() {
                        let was = coverage(&before, ppp, p, q, ink(bg));
                        let now = coverage(&img, ppp, p, q, ink(bg));
                        // Ants: half the dashes contrast with the pixels under them.
                        assert!(was < 0.05 && now > 0.3, "{tool} on {bg} @{ppp}x, edge {i}: {was:.2} → {now:.2} contrasting");
                    }
                } else {
                    // Sample both halves of the animated dash cycle while the pointer is held.
                    // A single phase can put antialiasing ramps at a short arc's sample positions.
                    let next_time = h.ctx.input(|input| input.time) + 0.4;
                    h.input_mut().time = Some(next_time);
                    h.step();
                    let next = render(&mut h);
                    if let Some(directory) = std::env::var_os("PHOTOCRAFT_DRAG_PREVIEW_FIXTURES") {
                        h.render().expect("capture next phase").save(std::path::PathBuf::from(directory).join(format!("{tool}-{bg}-{ppp}x-next.png"))).unwrap();
                    }
                    // Check actual arcs around all four ellipse extrema at every display scale.
                    let center = screen(&h, 200.0, 150.0);
                    let radius = egui::vec2((screen(&h, 300.0, 150.0).x - center.x).abs(), (screen(&h, 200.0, 220.0).y - center.y).abs());
                    for (mx, my) in [(200.0, 80.0), (300.0, 150.0), (200.0, 220.0), (100.0, 150.0)] {
                        let m = screen(&h, mx, my);
                        let was = ellipse_coverage(&[&before, &before_next], ppp, center, radius, m, ink(bg));
                        let now = ellipse_coverage(&[&img, &next], ppp, center, radius, m, ink(bg));
                        assert!(was < 0.05 && now > 0.3, "{tool} on {bg} @{ppp}x at {m:?}: {was:.2} → {now:.2} contrasting");
                    }
                }
                // Each move's frame shows the outline where the pointer is now (no stale frame).
                let previous = img;
                pointer_move(&mut h, 340.0, 250.0);
                let img = render(&mut h);
                if tool == "rectMarquee" {
                    let [top, right, ..] = edges(&h, a, [340.0, 250.0]);
                    assert!(coverage(&img, ppp, top.0, top.1, ink(bg)) > 0.3, "{tool} {bg} @{ppp}x: top edge follows the pointer");
                    assert!(coverage(&img, ppp, right.0, right.1, ink(bg)) > 0.3, "{tool} {bg} @{ppp}x: right edge follows the pointer");
                } else {
                    let next_time = h.ctx.input(|input| input.time) + 0.4;
                    h.input_mut().time = Some(next_time);
                    h.step();
                    let next = render(&mut h);
                    assert_ne!(img.px, next.px, "held ellipse must animate between the sampled phases");
                    let center = screen(&h, 220.0, 165.0);
                    let radius = egui::vec2((screen(&h, 340.0, 165.0).x - center.x).abs(), (screen(&h, 220.0, 250.0).y - center.y).abs());
                    for midpoint in [screen(&h, 340.0, 165.0), screen(&h, 220.0, 250.0)] {
                        let old = ellipse_coverage(&[&previous], ppp, center, radius, midpoint, ink(bg));
                        let now = ellipse_coverage(&[&img, &next], ppp, center, radius, midpoint, ink(bg));
                        assert!(old < 0.05 && now > 0.3, "ellipse {bg} @{ppp}x follows current bounds: {old:.2} → {now:.2}");
                    }
                }
                button(&mut h, 340.0, 250.0, false);
                h.run_steps(2);
            }
        }
    }
}

#[test]
fn lasso_and_crop_previews_show_while_dragging() {
    for ppp in [1.0, 2.0] {
        let Some(mut h) = harness(ppp) else { return };
        h.run_steps(4);
        for bg in ["white", "black"] {
            // Lasso: a straight run, then down; the path shows as contrasting ants.
            setup(&mut h, bg, "lasso");
            drag_to(&mut h, [80.0, 100.0], [320.0, 100.0]);
            pointer_move(&mut h, 320.0, 200.0);
            let img = render(&mut h);
            let (p, q) = (screen(&h, 80.0, 100.0), screen(&h, 320.0, 100.0));
            let now = coverage(&img, ppp, p, q, ink(bg));
            assert!(now > 0.3, "lasso on {bg} @{ppp}x: {now:.2} contrasting");
            button(&mut h, 320.0, 200.0, false);
            h.run_steps(2);
        }
        // Crop on white: the area outside the frame dims while dragging, inside stays white.
        setup(&mut h, "white", "crop");
        drag_to(&mut h, [100.0, 80.0], [300.0, 220.0]);
        let img = render(&mut h);
        let px = |x: f32, y: f32| {
            let p = screen(&h, x, y) * ppp;
            img.luma(p.x as i64, p.y as i64).unwrap_or(0)
        };
        assert!(px(200.0, 150.0) > 240, "inside the crop frame stays as is");
        assert!(px(50.0, 40.0) < 180 && px(350.0, 260.0) < 180, "outside the crop frame is dimmed mid-drag");
        button(&mut h, 300.0, 220.0, false);
        h.run_steps(2);
    }
}

#[test]
fn a_held_marquee_drag_keeps_requesting_frames() {
    let Some(mut h) = harness(1.0) else { return };
    h.run_steps(4);
    setup(&mut h, "white", "rectMarquee");
    let shortest: Arc<Mutex<Option<Duration>>> = Arc::default();
    let s = shortest.clone();
    h.ctx.set_request_repaint_callback(move |info| {
        let mut s = s.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *s = Some(s.map_or(info.delay, |d| d.min(info.delay)));
    });
    let still_frame = |h: &mut Harness| {
        *shortest.lock().unwrap() = None;
        h.step();
        *shortest.lock().unwrap()
    };
    drag_to(&mut h, [100.0, 80.0], [300.0, 220.0]);
    // Pointer held still: the animated ants still ask for the next frame within 100 ms.
    let d = still_frame(&mut h);
    assert!(d.is_some_and(|d| d <= Duration::from_millis(100)), "mid-drag repaint request: {d:?}");
    button(&mut h, 300.0, 220.0, false);
}
