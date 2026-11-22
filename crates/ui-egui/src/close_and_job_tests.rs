//! What the CPU canvas redoes when a document closes and the tab behind it shows again, and when
//! a background job's result lands (a 2-core AMD A6 froze then): the tab shown again draws from
//! the tiles and thumbnails it kept, and a job's result redraws the area it changed, not the
//! whole view, nor every thumbnail.

use serde_json::json;

use crate::PhotocraftApp;
use crate::move_repeat_tests::{Device, harness, stripes, styled_text};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

fn counter(app: &PhotocraftApp, name: &str) -> u64 {
    app.perf_counters.snapshot().iter().find(|c| c.0 == name).map_or(0, |c| c.1)
}

/// A 1600x2400 page of 30 layers: art, patches and styled type.
fn page(app: &mut PhotocraftApp, name: &str) {
    app.run("file.new", json!({"width": 1600, "height": 2400, "background": "white", "name": name})).expect("doc");
    stripes(app, "art", 1600, 2400, 0);
    for i in 0..14u32 {
        app.run("layer.new.layer", json!({"name": format!("patch {i}")})).expect("layer");
        app.run("select.rect", json!({"x": 80 + (i % 3) * 500, "y": 100 + (i / 3) * 450, "width": 380, "height": 220})).expect("select");
        app.run("edit.fill", json!({"color": "#ffffff"})).expect("fill");
        app.run("select.deselect", json!({})).expect("deselect");
        styled_text(app, 100 + (i % 3) * 500, 160 + (i / 3) * 450, &format!("GLOBO {i}"), 36);
    }
}

/// What the frames after `act` redid, until the canvas settles.
#[derive(Debug, Default)]
struct Redone {
    px: u64,
    thumbs: u64,
    navigator_full: u64,
    channels_full: u64,
    worst_ms: f64,
}

fn redone(h: &mut Harness, act: impl FnOnce(&mut PhotocraftApp)) -> Redone {
    let app = h.state();
    let (px, thumbs, nav, chan) = (app.perf.refreshed_px, counter(app, "thumbs"), counter(app, "navigator_full"), counter(app, "channel_thumbs_full"));
    act(h.state_mut());
    let mut worst = 0.0f64;
    for _ in 0..40 {
        let t = std::time::Instant::now();
        h.step();
        worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        // The worker pool's tiles land a frame or two later.
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let app = h.state();
    Redone {
        px: app.perf.refreshed_px - px,
        thumbs: counter(app, "thumbs") - thumbs,
        navigator_full: counter(app, "navigator_full") - nav,
        channels_full: counter(app, "channel_thumbs_full") - chan,
        worst_ms: worst,
    }
}

/// Three pages open, each shown once; closing the front one shows the one behind it from what it
/// kept: no canvas pixel composited again, no thumbnail, no whole Navigator image.
#[test]
fn closing_a_document_shows_the_one_behind_from_what_it_kept() {
    let mut h = harness(Device::Cpu).expect("the CPU canvas");
    for name in ["uno", "dos", "tres"] {
        page(h.state_mut(), name);
        h.state_mut().sync_views();
        for _ in 0..60 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    // Back to the second page and to the third again: each tab has been shown with its panels.
    for doc in [1, 2] {
        h.state_mut().run("document.activate", json!({"document": doc})).expect("select tab");
        for _ in 0..60 {
            h.step();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    let r = redone(&mut h, |app| {
        app.run("file.close", json!({"discard": true})).expect("close");
    });
    eprintln!("CLOSE: {r:?}, worst frame {:.1} ms", r.worst_ms);
    assert_eq!(h.state().session.documents().len(), 2);
    assert_eq!(r.px, 0, "the page behind was composited again");
    assert_eq!(r.channels_full, 0, "its Channels thumbnails were composited again");
    assert_eq!(r.thumbs, 0, "its layer thumbnails were made again");
    assert_eq!(r.navigator_full, 0, "its Navigator image was made again whole");
}

/// A background job's result (a filter run as a job on a patch) redraws the area it changed.
#[test]
fn a_job_result_redraws_the_area_it_changed() {
    let mut h = harness(Device::Cpu).expect("the CPU canvas");
    page(h.state_mut(), "uno");
    h.state_mut().sync_views();
    for _ in 0..60 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let patch = h.state().session.active().expect("doc").doc.walk().iter().find(|(_, _, l)| l.name == "patch 0").map(|(_, _, l)| l.id).expect("patch");
    h.state_mut().run("layer.select", json!({"layer": patch.0})).expect("select");
    h.state_mut().background_jobs = true;
    let tiles = |h: &Harness| -> Vec<(photocraft_doc::LayerId, u64)> {
        let doc = &h.state().session.active().expect("doc").doc;
        doc.walk().iter().filter_map(|(_, _, l)| Some((l.id, crate::app::surface_fingerprint(l.surface()?)))).collect()
    };
    let before = tiles(&h);
    let r = redone(&mut h, |app| {
        let _ = app.run("filter.blur.gaussianBlur", json!({"radius": 4}));
    });
    eprintln!("JOB: {r:?}, worst frame {:.1} ms", r.worst_ms);
    let view = h.state().last_canvas_rect;
    let view_px = (view.width() * view.height()) as u64;
    assert!(r.px * 4 < view_px, "the job's result composited {} px, the view holds {view_px}", r.px);
    // Only the blurred layer's pixels are new: every other layer keeps its tiles, so its
    // thumbnail and the canvas's caches of it stay (thumbnails made are rows the job's progress
    // brought into view).
    let changed: Vec<_> = before.iter().zip(tiles(&h)).filter(|(a, b)| a.1 != b.1).map(|(a, _)| a.0).collect();
    assert_eq!(changed, vec![patch], "layers whose pixels the job's result replaced");
}
