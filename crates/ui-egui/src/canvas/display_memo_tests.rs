//! `display_doc` builds a Duotone or live-adjust document once per snapshot (#4527), not on each
//! of its calls in a frame.

use super::*;
use serde_json::json;

fn app() -> PhotocraftApp {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 64, "height": 48, "mode": "gray"})).unwrap();
    app
}

#[test]
fn duotone_display_document_is_built_once_per_snapshot() {
    let mut app = app();
    app.run("image.mode.duotone", json!({"type": "duotone", "inks": [{"name": "Black"}, {"name": "Orange", "color": "#ff8000"}]})).unwrap();
    let (a, key) = display_doc(&mut app, 0);
    assert_eq!(key, 1 << 41);
    let (b, _) = display_doc(&mut app, 0);
    assert!(std::sync::Arc::ptr_eq(&a, &b), "the second call reuses the first's document");
    assert_eq!(a.layers.len(), app.session.documents()[0].doc.layers.len() + 1, "with the ink display layer");
    app.run("edit.fill", json!({"color": "#808080"})).unwrap();
    let (c, _) = display_doc(&mut app, 0);
    assert!(!std::sync::Arc::ptr_eq(&a, &c), "an edit builds it again");
    assert_eq!(c.layers.last().map(|l| l.name.as_str()), a.layers.last().map(|l| l.name.as_str()));
}

#[test]
fn live_adjust_document_is_built_once_per_setting() {
    let mut app = app();
    app.run("layer.newAdjustmentLayer.levels", json!({})).unwrap();
    let id = app.session.documents()[0].active_layer.unwrap();
    app.live_adjust = Some((id, json!({"inBlack": 20})));
    let (a, k1) = display_doc(&mut app, 0);
    let (b, k2) = display_doc(&mut app, 0);
    assert_eq!(k1, k2);
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    app.live_adjust = Some((id, json!({"inBlack": 40})));
    let (c, k3) = display_doc(&mut app, 0);
    assert_ne!(k1, k3);
    assert!(!std::sync::Arc::ptr_eq(&a, &c), "a new setting builds it again");
    app.live_adjust = None;
    let (d, k4) = display_doc(&mut app, 0);
    assert_eq!(k4, 0);
    assert!(std::sync::Arc::ptr_eq(&d, &app.session.documents()[0].doc), "no preview: the document itself");
}

/// A small image (a panel's preview, a clone overlay; here 200 x 150, two of the pool's pieces)
/// is made into a monitor image
/// by the thread that asks, whatever the worker pool is doing: with every worker compositing an
/// edit (a weak machine) the window's thread waited for one to be free. On the sRGB monitor and
/// on another (a colour transform), and the same pixels as an image large enough for the pool.
#[test]
fn a_small_display_image_never_waits_for_a_busy_worker_pool() {
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
    app.run("file.new", json!({"width": 64, "height": 48})).unwrap();
    let small = photocraft_compose::Buffer {
        rect: photocraft_geom::Rect::new(0, 0, 200, 150),
        px: (0..200 * 150).map(|i| [(i % 200) as f32 / 199.0, (i / 200) as f32 / 149.0, 0.3, 0.8]).collect(),
    };
    for monitor in ["srgb", "display-p3"] {
        app.session.color.settings.monitor_profile = monitor.into();
        let display = app.session.color.canvas_display(&app.session.active().unwrap().doc).ok();
        assert_eq!(display.as_ref().is_some_and(|d| !d.is_identity()), monitor != "srgb", "{monitor}");
        let (d, b) = (display.clone(), small.clone());
        let made = photocraft_testkit::busy_pool::while_busy(std::time::Duration::from_secs(5), move || display_image(d.as_deref(), &b));
        let img = made.unwrap_or_else(|| panic!("{monitor}: the image was made while every worker was busy"));
        // A 1024 x 150 image with the same pixels on its left goes through the pool.
        let wide = photocraft_compose::Buffer {
            rect: photocraft_geom::Rect::new(0, 0, 1024, 150),
            px: (0..1024 * 150).map(|i| if i % 1024 < 200 { small.px[(i / 1024) * 200 + i % 1024] } else { [0.0; 4] }).collect(),
        };
        let big = display_image(display.as_deref(), &wide);
        for y in 0..150 {
            for x in 0..200 {
                assert_eq!(img.pixels[y * 200 + x], big.pixels[y * 1024 + x], "{monitor} ({x}, {y})");
            }
        }
    }
}
