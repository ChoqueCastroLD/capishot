//! What one keystroke costs while typing with the Type tool, on a page of many styled type layers
//! (telemetry since 2026-11-12: `type.edit` frames froze in `dock.layers`, `gpu.composite`,
//! `cmd.run` and `canvas.cpu_full`). The real app runs offscreen, the Layers panel showing; the
//! text is typed one key a frame, the pointer resting on the canvas. Per keystroke: type layouts,
//! canvas pixels composited, Layers panel rows laid out and thumbnails made, and the frame's time
//! with its spans.

use egui::{Event, Modifiers, PointerButton};
use photocraft_doc::LayerId;
use serde_json::json;

use crate::PhotocraftApp;
use crate::move_repeat_tests::{Device, PSD, gpu_drawing, harness, pct, screen, stripes, styled_text};

type Harness = egui_kittest::Harness<'static, PhotocraftApp>;

/// A 2000x3000 page: art, ten cleaning patches and 50 type layers with a Stroke and an Outer
/// Glow, every fifth with a Gradient Overlay too (laid over the layer's frame: a keystroke that
/// grows the frame repaints all of it). The type layer to edit (in view at 100%, no gradient).
fn styled_page(app: &mut PhotocraftApp) -> LayerId {
    let (w, h) = (2000, 3000);
    app.run("file.new", json!({"width": w, "height": h, "background": "white", "name": "pagina"})).expect("doc");
    stripes(app, "art", w, h, 0);
    for i in 0..10 {
        let (x, y) = (100 + (i % 3) * 600, 150 + (i / 3) * 700);
        app.run("layer.new.layer", json!({"name": format!("clean {i}")})).expect("layer");
        app.run("select.rect", json!({"x": x, "y": y, "width": 420, "height": 260})).expect("select");
        app.run("edit.fill", json!({"color": "#ffffff"})).expect("fill");
        app.run("select.deselect", json!({})).expect("deselect");
    }
    let mut first = None;
    for i in 0..50u32 {
        let (x, y, text) = (80 + (i % 4) * 470, 90 + (i / 4) * 230, format!("GLOBO {i}: ¿QUÉ?"));
        if i % 5 == 4 {
            styled_text(app, x, y, &text, 40);
        } else {
            let l = crate::move_repeat_tests::layer_id(app.run("type.create", json!({"x": x, "y": y, "text": text, "size": 40, "color": "#101010"})).expect("type"));
            app.run("layer.layerStyle.stroke", json!({"layer": l.0, "size": 5, "position": "outside", "color": "#ffffff"})).expect("stroke");
            app.run("layer.layerStyle.outerGlow", json!({"layer": l.0})).expect("outer glow");
            first.get_or_insert(l);
        }
    }
    first.expect("type")
}

/// His file (read only), and its "capitulo" type layer.
fn his_file(app: &mut PhotocraftApp) -> Option<LayerId> {
    if !std::path::Path::new(PSD).exists() {
        eprintln!("skipping: {PSD} isn't here");
        return None;
    }
    app.background_jobs = false;
    app.open_path(PSD).expect("open his file");
    let doc = app.session.active().expect("doc").doc.clone();
    doc.walk().iter().map(|(_, _, l)| l).find(|l| l.name.to_lowercase().contains("capitulo") && matches!(l.content, photocraft_doc::LayerContent::Text(_))).map(|l| l.id)
}

/// The work of one keystroke's frame.
#[derive(Debug, Default, Clone)]
struct Key {
    ms: f64,
    layouts: u64,
    /// Canvas pixels composited.
    px: u64,
    /// Layers panel rows laid out in full.
    rows: u64,
    /// Layers panel rows visited at all (laid out, or only given their place).
    visited: u64,
    thumbs: u64,
    /// Channels panel composites made whole.
    channels_full: u64,
    spans: Vec<(&'static str, f64)>,
}

struct Typed {
    keys: Vec<Key>,
    /// Type layers in the document.
    layers: usize,
    /// Layers panel rows in view.
    rows_in_view: u64,
    /// The edited layer's extent at the end (effects included): what a keystroke redrew whole.
    extent: u64,
}

/// How the keys are typed.
#[derive(Clone, Copy)]
struct Typing {
    /// Keys a frame (a slow device's frame takes several of a fast typist's keys).
    per_frame: usize,
    /// The Layers card's tab shown (0 Layers, 1 Channels).
    tab: usize,
}

const ONE_A_FRAME: Typing = Typing { per_frame: 1, tab: 0 };

fn counter(app: &PhotocraftApp, name: &str) -> u64 {
    app.perf_counters.snapshot().iter().find(|c| c.0 == name).map_or(0, |c| c.1)
}

/// Opens `page` on `device`, starts editing its type layer at the end of its text and types
/// `text` one key a frame. `None` when the device or file isn't here.
fn type_on(device: Device, page: fn(&mut PhotocraftApp) -> Option<LayerId>, text: &str, how: Typing) -> Option<Typed> {
    let _gpu = photocraft_testkit::gpu_lock();
    let Some(mut h) = harness(device) else {
        eprintln!("skipping: no GPU adapter");
        return None;
    };
    let id = page(h.state_mut())?;
    {
        let app = h.state_mut();
        app.sync_views();
        app.ui.extras.rulers = false;
        app.run("layer.select", json!({"layer": id.0})).expect("select");
    }
    let ctx = h.ctx.clone();
    let b = h.state().session.active().and_then(|st| st.doc.layer(id)).and_then(photocraft_engine::layer_multi_cmds::layer_bounds).expect("bounds");
    let center = [(b.x0 + b.x1) / 2, (b.y0 + b.y1) / 2 + 300];
    let (req, _rx) = crate::control::ControlRequest::new("ui.set", json!({"zoom": 1.0, "center": center, "dockTabs": {"layers": how.tab}}));
    crate::control::handle(h.state_mut(), &ctx, &req);
    h.run_steps(10);
    if device != Device::Cpu && !gpu_drawing(&mut h, device) {
        return None;
    }
    h.run_steps(30);
    crate::type_tool::edit_active(h.state_mut()).expect("edit");
    if let Some(ed) = h.state_mut().ui.text_edit.as_mut() {
        ed.anchor = ed.caret;
    }
    // The pointer rests on the canvas, off the text, as a hand on the mouse does while typing.
    let rest = screen(&h, b.x0 as f32 + 10.0, b.y1 as f32 + 120.0);
    h.event(Event::PointerMoved(rest));
    h.run_steps(30);
    let layers = h.state().session.active().expect("doc").doc.walk().iter().filter(|(_, _, l)| matches!(l.content, photocraft_doc::LayerContent::Text(_))).count();
    let rows0 = counter(h.state(), "rows_drawn");
    h.step();
    let rows_in_view = counter(h.state(), "rows_drawn") - rows0;
    let mut keys = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    for c in chars.chunks(how.per_frame) {
        let app = h.state();
        let visited = |app: &PhotocraftApp| counter(app, "rows_drawn") + counter(app, "rows_skipped");
        let (l0, px0, r0, v0, t0, c0) =
            (photocraft_text::layouts_here(), app.perf.refreshed_px, counter(app, "rows_drawn"), visited(app), counter(app, "thumbs"), counter(app, "channel_thumbs_full"));
        for c in c {
            h.event(Event::Text(c.to_string()));
        }
        crate::perf_span::begin_frame();
        let t = std::time::Instant::now();
        h.step();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let spans = crate::perf_span::frame().into_iter().take(8).collect();
        let app = h.state();
        keys.push(Key {
            ms,
            layouts: photocraft_text::layouts_here() - l0,
            px: app.perf.refreshed_px - px0,
            rows: counter(app, "rows_drawn") - r0,
            visited: visited(app) - v0,
            thumbs: counter(app, "thumbs") - t0,
            channels_full: counter(app, "channel_thumbs_full") - c0,
            spans,
        });
    }
    let extent = h
        .state()
        .session
        .active()
        .and_then(|st| st.doc.layer(id).and_then(|l| photocraft_compose::change_bounds(l, st.doc.bounds())))
        .map_or(0, |r| u64::from(r.width()) * u64::from(r.height()));
    let typed = h.state().session.active().and_then(|st| st.doc.layer(id)).and_then(|l| match &l.content {
        photocraft_doc::LayerContent::Text(t) => Some(t.text.clone()),
        _ => None,
    });
    assert!(typed.is_some_and(|t| t.ends_with(text)), "the keys went into the text");
    h.event(Event::PointerButton { pos: rest, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    Some(Typed { keys, layers, rows_in_view, extent })
}

fn report(device: Device, name: &str, t: &Typed) {
    let ms: Vec<f64> = t.keys.iter().map(|k| k.ms).collect();
    eprintln!(
        "TYPE {device:?} {name}: {} type layers, {} rows in view; per key median {:.1} ms, p99 {:.1} ms, max {:.1} ms",
        t.layers,
        t.rows_in_view,
        pct(&ms, 0.5),
        pct(&ms, 0.99),
        pct(&ms, 1.0)
    );
    for (i, k) in t.keys.iter().enumerate() {
        let spans: Vec<String> = k.spans.iter().map(|(n, ms)| format!("{n} {ms:.1}")).collect();
        eprintln!(
            "  key {i}: {:.1} ms, {} layouts, {} px, {} rows ({} visited), {} thumbs, {} channel composites [{}]",
            k.ms,
            k.layouts,
            k.px,
            k.rows,
            k.visited,
            k.thumbs,
            k.channels_full,
            spans.join(", ")
        );
    }
}

const TEXT: &str = "Y ENTONCES DIJO ALGO";

/// One layout per keystroke (the edit's; `type.info` and the caret overlay reuse it), the canvas
/// composites what the new glyph changed (not the line's whole extent), and the Layers panel
/// visits only the rows in view. On a92ad8384 (before): 3 layouts a key, the layer's extent, and
/// every row of the panel each frame.
#[test]
fn a_keystroke_lays_out_its_layer_once_and_redraws_what_it_changed() {
    let t = type_on(Device::Cpu, |app| Some(styled_page(app)), TEXT, ONE_A_FRAME).expect("the CPU canvas");
    report(Device::Cpu, "50 styled type layers", &t);
    for (i, k) in t.keys.iter().enumerate() {
        assert_eq!(k.layouts, 1, "key {i}: one layout");
        assert!(k.px * 3 <= t.extent, "key {i}: {} px composited, the layer's extent is {} px", k.px, t.extent);
        assert!(k.visited <= t.rows_in_view + 2, "key {i}: {} Layers rows visited, {} in view", k.visited, t.rows_in_view);
    }
}

/// Two keys typed in one frame (a slow device's frame takes several): one layout each, and the
/// canvas composites what both changed. On a92ad8384: 6 layouts.
#[test]
fn keys_typed_in_one_frame_redraw_only_what_they_changed() {
    let t = type_on(Device::Cpu, |app| Some(styled_page(app)), TEXT, Typing { per_frame: 2, tab: 0 }).expect("the CPU canvas");
    report(Device::Cpu, "50 styled type layers, 2 keys a frame", &t);
    for (i, k) in t.keys.iter().enumerate() {
        assert_eq!(k.layouts, 2, "frame {i}: a layout per key");
        assert!(k.px * 3 <= t.extent, "frame {i}: {} px composited, the layer's extent is {} px", k.px, t.extent);
    }
}

/// With the Channels tab in front, a keystroke updates its thumbnails where the glyph landed:
/// the document isn't composited whole for them.
#[test]
fn typing_with_the_channels_tab_shown_composites_no_whole_thumbnail() {
    let t = type_on(Device::Cpu, |app| Some(styled_page(app)), "ABC", Typing { per_frame: 1, tab: 1 }).expect("the CPU canvas");
    report(Device::Cpu, "50 styled type layers, Channels tab", &t);
    for (i, k) in t.keys.iter().enumerate() {
        assert_eq!(k.channels_full, 0, "key {i}: the Channels thumbnails composited the whole page");
    }
}

#[test]
#[ignore = "timing report"]
fn typing_timing_report() {
    for device in [Device::Cpu, Device::Gpu] {
        for (name, how) in [("", ONE_A_FRAME), (", Channels tab", Typing { per_frame: 1, tab: 1 }), (", 2 keys a frame", Typing { per_frame: 2, tab: 0 })] {
            if let Some(t) = type_on(device, |app| Some(styled_page(app)), TEXT, how) {
                report(device, &format!("50 styled type layers{name}"), &t);
            }
        }
        if let Some(t) = type_on(device, his_file, TEXT, ONE_A_FRAME) {
            report(device, "his file", &t);
        }
    }
}

/// Changing the style (Character panel, options bar) lays out each layer it changes once and
/// leaves the other type layers' renderings as they were: the edited layer while typing, then
/// three selected layers. On a92ad8384: 3 layouts for each layer changed.
#[test]
fn set_style_lays_out_each_layer_it_changes_once() {
    let mut h = harness(Device::Cpu).expect("the CPU canvas");
    let first = styled_page(h.state_mut());
    h.state_mut().sync_views();
    h.state_mut().run("layer.select", json!({"layer": first.0})).expect("select");
    h.run_steps(10);
    crate::type_tool::edit_active(h.state_mut()).expect("edit");
    h.run_steps(5);
    // This thread's: other tests lay type out in the same process at the same time.
    let layouts = |_: &Harness| photocraft_text::layouts_here();
    let renderings = |h: &Harness| -> Vec<(LayerId, Option<photocraft_raster::Surface>)> {
        let doc = &h.state().session.active().expect("doc").doc;
        doc.walk()
            .iter()
            .filter_map(|(_, _, l)| match &l.content {
                photocraft_doc::LayerContent::Text(t) => Some((l.id, t.cache.clone())),
                _ => None,
            })
            .collect()
    };
    let changed = |before: &[(LayerId, Option<photocraft_raster::Surface>)], after: &[(LayerId, Option<photocraft_raster::Surface>)]| -> Vec<LayerId> {
        // Drawn again: its pixels aren't the same tiles any more.
        let same = |x: &photocraft_raster::Surface, y: &photocraft_raster::Surface| x.tile_count() == y.tile_count() && x.tiles().zip(y.tiles()).all(|(p, q)| p.0 == q.0 && std::sync::Arc::ptr_eq(p.1, q.1));
        before.iter().zip(after).filter(|(a, b)| !a.1.as_ref().zip(b.1.as_ref()).is_some_and(|(x, y)| same(x, y))).map(|(a, _)| a.0).collect()
    };
    for size in [44, 36, 52] {
        let (l0, r0) = (layouts(&h), renderings(&h));
        h.state_mut().run("type.setStyle", json!({"layer": first.0, "size": size})).expect("size");
        h.step();
        h.step();
        assert_eq!(layouts(&h) - l0, 1, "size {size}: one layout");
        assert_eq!(changed(&r0, &renderings(&h)), vec![first], "size {size}: only the edited layer drawn again");
    }
    crate::type_tool::commit(h.state_mut());
    h.run_steps(3);
    let ids: Vec<LayerId> = renderings(&h).iter().map(|r| r.0).skip(10).step_by(7).take(3).collect();
    for (i, id) in ids.iter().enumerate() {
        h.state_mut().run("layer.select", json!({"layer": id.0, "mode": if i == 0 { "replace" } else { "add" }})).expect("select");
    }
    h.run_steps(3);
    let (l0, r0) = (layouts(&h), renderings(&h));
    h.state_mut().run("type.setStyle", json!({"color": "#c02020"})).expect("colour");
    h.step();
    h.step();
    assert_eq!(layouts(&h) - l0, 3, "one layout for each of the three");
    let mut drawn = changed(&r0, &renderings(&h));
    drawn.sort_by_key(|l| l.0);
    let mut want = ids.clone();
    want.sort_by_key(|l| l.0);
    assert_eq!(drawn, want, "only the three drawn again");
}
