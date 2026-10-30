//! Frame costs of the interactive tools and panels on many-layer documents: the main thread's
//! share of each frame (the app's update and egui's layout, then tessellation) while erasing,
//! painting, moving, transforming, typing, undoing, scrolling the Layers panel, toggling
//! visibility, scrubbing opacity, and while nothing happens (idle frames and the repaints the
//! app asks for).
//!
//! ```sh
//! cargo run --release -p photocraft-ui-egui --example tool_frames -- [--size 2000x3000] [--layers 100] [--only eraser,move] [--moves 60] [--cpu] [--json out.json] [--spin scenario] [--write-psd page.psd]
//! ```
//!
//! The real `PhotocraftApp` runs in an offscreen egui_kittest harness (on wgpu unless `--cpu`),
//! driven by real input events (pointer, wheel, keys, text) with several pointer moves per frame
//! like a fast mouse. The document is a comic page: raster layers with panels, inks and full-page
//! screentones, type layers, layer masks, effects, nested groups and a linked pair. Each row is
//! the scenario's frames: p50 / p95 / max of the frame (update + layout), tessellation, frames
//! over 16 ms, and app counters (thumbnails rebuilt, panel recomputes) when they moved. `--spin`
//! repeats one scenario for 20 s for a sampling profiler.

use std::time::Instant;

use egui::{Event, Modifiers, PointerButton, Pos2, ViewportId};
use egui_kittest::kittest::Queryable;
use photocraft_color::{BlendMode, Color, ColorMode, SampleType};
use photocraft_doc::{Document, Effect, FxCommon, FxPaint, Layer, LayerId, LayerMask, StrokeFx, StrokePosition};
use photocraft_geom::{Rect, Size};
use photocraft_ui_egui::canvas::ViewXform;
use photocraft_ui_egui::state::Tool;
use photocraft_ui_egui::{PhotocraftApp, Services};
use serde_json::{Value, json};

type H = egui_kittest::Harness<'static, PhotocraftApp>;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn rnd(k: u64) -> f32 {
    let mut x = k.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// Handles into the generated page.
struct Page {
    doc: Document,
    /// A mid-stack raster layer with a panel drawn on it.
    pixel: LayerId,
    group: LayerId,
    linked: LayerId,
    text: LayerId,
    /// Every layer id, bottom first.
    all: Vec<LayerId>,
}

/// A comic page `w`×`h` with `n` layers (groups count as layers): every 10 layers form a group
/// (every 5th group nested in the previous one), every 5th layer a full-page screentone, every
/// 7th a type layer, every 6th has a layer mask, every 8th an effect, and two layers are linked.
fn page(w: u32, h: u32, n: usize) -> Page {
    let mut doc = Document::with_background("Page", Size::new(w, h), ColorMode::Rgb, SampleType::U8, Color::rgb(1.0, 1.0, 1.0));
    let fmt = doc.pixel_format();
    let mut s = photocraft_engine::Session::new();
    let mut leaves: Vec<Layer> = Vec::new();
    let (wi, hi) = (w as i32, h as i32);
    let mut i = 0usize;
    let mut groups = 0usize;
    let mut out: Vec<Layer> = Vec::new();
    let mut text_layers: Vec<Layer> = Vec::new();
    // Type layers come from the engine (shaped text); make them on a scratch document.
    let ntext = n / 7;
    if ntext > 0 {
        s.add_document(Document::new("scratch", Size::new(w, h), ColorMode::Rgb, SampleType::U8), None);
        for k in 0..ntext {
            let p = json!({"x": (rnd(k as u64 * 3) * w as f32 * 0.7) as i32, "y": (100.0 + rnd(k as u64 * 3 + 1) * h as f32 * 0.9) as i32,
                "text": format!("Balloon {k}: what was that?!"), "font": "Inter", "size": (w as f32 / 60.0).max(8.0), "color": "#111111"});
            if s.execute("type.create", p).is_ok()
                && let Some(st) = s.active()
                && let Some(id) = st.active_layer
                && let Some(l) = st.doc.layer(id)
            {
                text_layers.push(l.clone());
            }
        }
    }
    let mut text_iter = text_layers.into_iter();
    let mut pixel = None;
    let mut linked = None;
    while i < n {
        let k = i as u64;
        let l = if i % 7 == 6
            && let Some(t) = text_iter.next()
        {
            t
        } else {
            let mut l = Layer::raster(format!("L{i}"), fmt);
            let surf = l.surface_mut().expect("raster");
            if i.is_multiple_of(5) {
                // A screentone covering the page.
                let c = 0.6 + rnd(k) * 0.3;
                surf.fill_rect(Rect::new(0, 0, wi, hi), &[c, c, c, 1.0]);
                l.blend = BlendMode::Multiply;
                l.opacity = 0.4;
            } else {
                // A panel with some ink blocks.
                let (pw, ph) = ((0.15 + rnd(k + 1) * 0.35) * w as f32, (0.1 + rnd(k + 2) * 0.3) * h as f32);
                let (x0, y0) = ((rnd(k + 3) * (w as f32 - pw)) as i32, (rnd(k + 4) * (h as f32 - ph)) as i32);
                let r = Rect::new(x0, y0, x0 + pw as i32, y0 + ph as i32);
                surf.fill_rect(r, &[rnd(k + 5), rnd(k + 6), rnd(k + 7), 1.0]);
                for j in 0..6u64 {
                    let (bx, by) = (x0 + (rnd(k * 31 + j) * pw) as i32, y0 + (rnd(k * 37 + j) * ph) as i32);
                    surf.fill_rect(Rect::new(bx, by, bx + 40, by + 160), &[0.05, 0.05, 0.05, 1.0]);
                }
            }
            if i % 6 == 3 {
                let mut m = LayerMask::reveal_all();
                m.surface.fill_rect(Rect::new(0, 0, wi / 3, hi / 4), &[0.0]);
                l.mask = Some(m);
            }
            if i % 8 == 5 {
                l.effects.items = vec![if i % 16 == 5 {
                    Effect::Stroke(StrokeFx {
                        common: FxCommon::new(BlendMode::Normal, 1.0),
                        size: 6.0,
                        position: StrokePosition::Outside,
                        paint: FxPaint::Color(Color::rgb(0.0, 0.0, 0.0)),
                        overprint: false,
                    })
                } else {
                    Effect::default_drop_shadow()
                }];
            }
            if pixel.is_none() && i >= n / 2 && !i.is_multiple_of(5) && l.mask.is_none() {
                pixel = Some(l.id);
            }
            if linked.is_none() && i >= n / 3 && !i.is_multiple_of(5) {
                l.link_group = Some(77);
                linked = Some(l.id);
            } else if linked.is_some_and(|_| !i.is_multiple_of(5))
                && !leaves.iter().chain(out.iter()).any(|x| x.link_group == Some(77) && x.id != linked.unwrap_or(LayerId(0)))
            {
                l.link_group = Some(77);
            }
            l
        };
        leaves.push(l);
        i += 1;
        // Close a group every 9 leaves (the group itself is the 10th layer).
        if leaves.len() == 9 && i < n {
            groups += 1;
            let mut g = Layer::group(format!("Panel group {groups}"), std::mem::take(&mut leaves));
            if groups.is_multiple_of(5)
                && let Some(prev) = out.pop()
            {
                // Nest the previous group inside this one.
                if let photocraft_doc::LayerContent::Group(gg) = &mut g.content {
                    gg.children.insert(0, prev);
                }
            }
            out.push(g);
            i += 1;
        }
    }
    out.append(&mut leaves);
    for l in out {
        doc.insert_above(None, l);
    }
    let all: Vec<LayerId> = doc.walk().iter().map(|(_, _, l)| l.id).collect();
    let group = doc.walk().iter().find(|(_, _, l)| l.is_group()).map_or(all[0], |(_, _, l)| l.id);
    let text = doc.walk().iter().find(|(_, _, l)| matches!(l.content, photocraft_doc::LayerContent::Text(_))).map_or(all[0], |(_, _, l)| l.id);
    let pixel = pixel.unwrap_or(all[all.len() / 2]);
    let linked = linked.unwrap_or(pixel);
    Page { doc, pixel, group, linked, text, all }
}

/// One frame's main-thread work, split.
#[derive(Clone, Copy, Default)]
struct F {
    /// The app's update plus egui's layout (`Context::run`).
    step: f64,
    /// Tessellating the frame's shapes.
    tess: f64,
    /// The app asked for another frame right away.
    repaint: bool,
}

fn frame(h: &mut H) -> F {
    let t = Instant::now();
    h.step();
    let step = ms(t);
    let shapes = h.output().shapes.clone();
    let ppp = h.ctx.pixels_per_point();
    let t = Instant::now();
    let prims = h.ctx.tessellate(shapes, ppp);
    let tess = ms(t);
    drop(prims);
    let repaint = h.output().viewport_output.get(&ViewportId::ROOT).is_some_and(|v| v.repaint_delay < std::time::Duration::from_millis(500));
    F { step, tess, repaint }
}

fn settle(h: &mut H, n: usize) {
    for _ in 0..n {
        frame(h);
    }
}

/// Screen position of document point (x, y) in the main canvas.
fn screen(h: &H, x: f64, y: f64) -> Pos2 {
    let app = h.state();
    let idx = app.session.active_index().unwrap_or(0);
    let v = &app.ui.views[idx];
    let rect = photocraft_ui_egui::rulers::content_rect(app, app.last_canvas_rect);
    ViewXform { rect, zoom: v.zoom, center: v.center, flip: app.ui.view.flip_horizontal, rotation: v.rotation, aspect: 1.0 }.to_screen(x as f32, y as f32)
}

fn run(h: &mut H, id: &str, p: Value) {
    if let Err(e) = h.state_mut().run(id, p) {
        eprintln!("{id}: {e}");
    }
}

fn set_tool(h: &mut H, tool: Tool) {
    h.state_mut().ui.tool = tool;
    settle(h, 2);
}

/// A drag along `pts` (document points), `per_frame` pointer moves per frame. Returns the frames
/// from the press to the frame after the release.
fn drag(h: &mut H, pts: &[[f64; 2]], per_frame: usize, mods: Modifiers) -> Vec<F> {
    let mut out = Vec::new();
    let Some(&first) = pts.first() else { return out };
    let p0 = screen(h, first[0], first[1]);
    h.event(Event::PointerMoved(p0));
    frame(h);
    h.input_mut().events.push(Event::PointerButton { pos: p0, button: PointerButton::Primary, pressed: true, modifiers: mods });
    out.push(frame(h));
    for chunk in pts[1..].chunks(per_frame.max(1)) {
        for p in chunk {
            let s = screen(h, p[0], p[1]);
            h.input_mut().events.push(Event::PointerMoved(s));
        }
        out.push(frame(h));
    }
    let last = pts[pts.len() - 1];
    let p1 = screen(h, last[0], last[1]);
    h.input_mut().events.push(Event::PointerButton { pos: p1, button: PointerButton::Primary, pressed: false, modifiers: mods });
    out.push(frame(h));
    out.push(frame(h));
    out
}

/// A zigzag across the middle of the page.
fn zigzag(w: f64, hh: f64, n: usize) -> Vec<[f64; 2]> {
    (0..n)
        .map(|i| {
            let t = i as f64 / (n - 1).max(1) as f64;
            [w * (0.2 + 0.6 * t), hh * (0.35 + 0.3 * t) + (t * std::f64::consts::PI * 6.0).sin() * hh * 0.05]
        })
        .collect()
}

struct Row {
    name: String,
    frames: Vec<F>,
    counters: Vec<(&'static str, u64)>,
}

fn q(v: &[f64], p: f64) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v[((v.len() as f64 * p).ceil() as usize).clamp(1, v.len()) - 1]
}

impl Row {
    fn print(&self) {
        let mut s: Vec<f64> = self.frames.iter().map(|f| f.step).collect();
        let mut t: Vec<f64> = self.frames.iter().map(|f| f.tess).collect();
        s.sort_by(f64::total_cmp);
        t.sort_by(f64::total_cmp);
        let long = self.frames.iter().filter(|f| f.step + f.tess > 16.0).count();
        // The press frame and the release frame (the second to last) of a drag.
        let press = self.frames.first().map_or(0.0, |f| f.step);
        let release = self.frames.len().checked_sub(2).and_then(|i| self.frames.get(i)).map_or(0.0, |f| f.step);
        let counters: Vec<String> = self.counters.iter().filter(|c| c.1 > 0).map(|(k, v)| format!("{k}={v}")).collect();
        println!(
            "{:<28} n {:>4}  frame p50 {:>7.2} p95 {:>7.2} max {:>7.2}  press {:>7.2} release {:>7.2}  tess p50 {:>5.2} p95 {:>5.2}  long {:>3}  {}",
            self.name,
            s.len(),
            q(&s, 0.5),
            q(&s, 0.95),
            s.last().copied().unwrap_or(0.0),
            press,
            release,
            q(&t, 0.5),
            q(&t, 0.95),
            long,
            counters.join(" ")
        );
    }
    fn json(&self) -> Value {
        let mut s: Vec<f64> = self.frames.iter().map(|f| f.step).collect();
        s.sort_by(f64::total_cmp);
        let mut t: Vec<f64> = self.frames.iter().map(|f| f.tess).collect();
        t.sort_by(f64::total_cmp);
        json!({"name": self.name, "frames": s.len(), "p50": q(&s, 0.5), "p95": q(&s, 0.95), "max": s.last().copied().unwrap_or(0.0),
            "press": self.frames.first().map_or(0.0, |f| f.step), "release": self.frames.len().checked_sub(2).and_then(|i| self.frames.get(i)).map_or(0.0, |f| f.step), "tess_p50": q(&t, 0.5), "tess_p95": q(&t, 0.95), "repaints": self.frames.iter().filter(|f| f.repaint).count(),
            "counters": self.counters.iter().map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>()})
    }
}

fn counters(h: &H) -> Vec<(&'static str, u64)> {
    h.state().perf_counters.snapshot()
}

fn diff(a: &[(&'static str, u64)], b: &[(&'static str, u64)]) -> Vec<(&'static str, u64)> {
    b.iter().map(|(k, v)| (*k, v.saturating_sub(a.iter().find(|x| x.0 == *k).map_or(0, |x| x.1)))).collect()
}

/// The frames of one scenario.
fn scenario(h: &mut H, pg: &Page, name: &str, moves: usize, per_frame: usize) -> Vec<F> {
    let (w, hh) = {
        let d = &h.state().session.active().expect("doc").doc;
        (f64::from(d.size.width), f64::from(d.size.height))
    };
    let undo = |h: &mut H| {
        run(h, "edit.undo", json!({}));
        settle(h, 2);
    };
    let select = |h: &mut H, id: LayerId| {
        run(h, "layer.select", json!({"layer": id.0}));
        settle(h, 2);
    };
    let path = zigzag(w, hh, moves);
    match name {
        "idle" => (0..60).map(|_| frame(h)).collect(),
        "hover" => {
            set_tool(h, Tool::Brush);
            (0..60)
                .map(|i| {
                    let p = screen(h, w * (0.3 + 0.004 * i as f64), hh * 0.5);
                    h.event(Event::PointerMoved(p));
                    frame(h)
                })
                .collect()
        }
        n if n.starts_with("eraser") || n.starts_with("brush") || n.starts_with("pencil") => {
            // eraser-<size>-<hard|soft>[-block|-pencil], brush-<size>-<hard|soft>
            let parts: Vec<&str> = n.split('-').collect();
            let tool = match parts[0] {
                "eraser" => Tool::Eraser,
                "pencil" => Tool::Pencil,
                _ => Tool::Brush,
            };
            let size: f64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(100.0);
            let hard = parts.get(2).is_none_or(|s| *s == "hard");
            select(h, pg.pixel);
            set_tool(h, tool);
            h.state_mut().ui.tool_options.eraser_mode = parts.get(3).map_or("brush", |m| if *m == "block" { "block" } else { "pencil" }).to_string();
            run(h, "tools.setBrush", json!({"brush": {"size": size, "hardness": if hard { 1.0 } else { 0.0 }}}));
            settle(h, 2);
            let f = drag(h, &path, per_frame, Modifiers::NONE);
            undo(h);
            f
        }
        "bgeraser" => {
            select(h, pg.pixel);
            set_tool(h, Tool::BackgroundEraser);
            run(h, "tools.setBrush", json!({"brush": {"size": 100, "hardness": 1.0}}));
            let f = drag(h, &path, per_frame, Modifiers::NONE);
            undo(h);
            f
        }
        "magiceraser" => {
            select(h, pg.pixel);
            set_tool(h, Tool::MagicEraser);
            let p = screen(h, w * 0.5, hh * 0.5);
            h.event(Event::PointerMoved(p));
            frame(h);
            h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
            let a = frame(h);
            h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
            let f = vec![a, frame(h), frame(h)];
            undo(h);
            f
        }
        n if n.starts_with("move-") => {
            let target = match n {
                "move-group" => pg.group,
                "move-linked" => pg.linked,
                _ => pg.pixel,
            };
            select(h, target);
            set_tool(h, Tool::Move);
            h.state_mut().ui.tool_options.move_auto_select = false;
            if n == "move-selection" {
                run(h, "select.rect", json!({"x": w * 0.3, "y": hh * 0.3, "width": w * 0.3, "height": hh * 0.3, "mode": "replace"}));
                settle(h, 2);
            }
            let pts: Vec<[f64; 2]> = (0..moves).map(|i| [w * 0.45 + (i as f64 / 20.0).sin() * w * 0.1, hh * 0.45 + i as f64 * hh * 0.001]).collect();
            let f = drag(h, &pts, per_frame, Modifiers::NONE);
            undo(h);
            if n == "move-selection" {
                undo(h);
                run(h, "select.deselect", json!({}));
                settle(h, 2);
            }
            f
        }
        "autoselect-hover" => {
            set_tool(h, Tool::Move);
            h.state_mut().ui.tool_options.move_auto_select = true;
            let f = (0..moves.min(120))
                .map(|i| {
                    let p = path[i % path.len()];
                    let s = screen(h, p[0], p[1]);
                    h.event(Event::PointerMoved(s));
                    frame(h)
                })
                .collect();
            h.state_mut().ui.tool_options.move_auto_select = false;
            f
        }
        "transform" => {
            select(h, pg.pixel);
            set_tool(h, Tool::Move);
            let ctx = h.ctx.clone();
            if let Err(e) = photocraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "edit.freeTransform", json!({})) {
                eprintln!("free transform: {e}");
            }
            settle(h, 3);
            let b = h
                .state()
                .session
                .active()
                .and_then(|s| s.doc.layer(pg.pixel))
                .and_then(photocraft_engine::layer_multi_cmds::layer_bounds)
                .unwrap_or(Rect::new(0, 0, 10, 10));
            let c = [f64::from(b.x0 + b.x1) / 2.0, f64::from(b.y0 + b.y1) / 2.0];
            let pts: Vec<[f64; 2]> = (0..moves).map(|i| [c[0] + i as f64 * 2.0, c[1] + (i as f64 / 10.0).sin() * 40.0]).collect();
            let f = drag(h, &pts, per_frame, Modifiers::NONE);
            h.key_press(egui::Key::Escape);
            settle(h, 3);
            f
        }
        "crop" => {
            set_tool(h, Tool::Crop);
            let pts: Vec<[f64; 2]> = (0..moves).map(|i| [w * (0.05 + 0.3 * i as f64 / moves as f64), hh * (0.05 + 0.3 * i as f64 / moves as f64)]).collect();
            let f = drag(h, &pts, per_frame, Modifiers::NONE);
            h.key_press(egui::Key::Escape);
            settle(h, 3);
            set_tool(h, Tool::Move);
            f
        }
        "marquee" | "lasso" => {
            set_tool(h, if name == "lasso" { Tool::Lasso } else { Tool::RectMarquee });
            let f = drag(h, &path, per_frame, Modifiers::NONE);
            run(h, "select.deselect", json!({}));
            settle(h, 2);
            f
        }
        "type" => {
            select(h, pg.text);
            set_tool(h, Tool::Type);
            let b = h
                .state()
                .session
                .active()
                .and_then(|s| s.doc.layer(pg.text))
                .and_then(photocraft_engine::layer_multi_cmds::layer_bounds)
                .unwrap_or(Rect::new(0, 0, 10, 10));
            let p = screen(h, f64::from(b.x0 + b.x1) / 2.0, f64::from(b.y0 + b.y1) / 2.0);
            h.event(Event::PointerMoved(p));
            frame(h);
            h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
            frame(h);
            h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
            settle(h, 3);
            let f = "Typing on a busy page!!"
                .chars()
                .map(|c| {
                    h.event(Event::Text(c.to_string()));
                    frame(h)
                })
                .collect();
            h.key_press(egui::Key::Escape);
            settle(h, 3);
            set_tool(h, Tool::Move);
            f
        }
        "undo-redo" => {
            // Ten painted strokes, undone and redone one per frame.
            select(h, pg.pixel);
            for k in 0..10 {
                run(
                    h,
                    "paint.stroke",
                    json!({"points": [[w * 0.2, hh * (0.2 + k as f64 * 0.05), 1], [w * 0.8, hh * (0.25 + k as f64 * 0.05), 1]], "size": 30, "color": "#202020"}),
                );
            }
            settle(h, 3);
            let mut f = Vec::new();
            for _ in 0..10 {
                let t = Instant::now();
                run(h, "edit.undo", json!({}));
                let mut x = frame(h);
                x.step += ms(t) - x.step.min(ms(t));
                f.push(x);
            }
            for _ in 0..10 {
                let t = Instant::now();
                run(h, "edit.redo", json!({}));
                let c = ms(t);
                let mut x = frame(h);
                x.step += c;
                f.push(x);
            }
            for _ in 0..10 {
                run(h, "edit.undo", json!({}));
            }
            settle(h, 2);
            f
        }
        "layers-scroll" => {
            let row = h.query_all_by_label("L1").map(|n| n.rect()).max_by(|a, b| a.width().total_cmp(&b.width()));
            let at = row.map(|r| r.center()).unwrap_or(egui::pos2(1250.0, 600.0));
            h.event(Event::PointerMoved(at));
            frame(h);
            (0..60)
                .map(|i| {
                    let dy = if i < 30 { -40.0 } else { 40.0 };
                    h.input_mut().events.push(Event::MouseWheel {
                        unit: egui::MouseWheelUnit::Point,
                        delta: egui::vec2(0.0, dy),
                        modifiers: Modifiers::NONE,
                        phase: egui::TouchPhase::Move,
                    });
                    frame(h)
                })
                .collect()
        }
        "visibility" => {
            let mut f = Vec::new();
            for k in 0..20 {
                let id = pg.all[(k * 7 + 3) % pg.all.len()];
                let t = Instant::now();
                run(h, "layer.setProps", json!({"layer": id.0, "visible": k % 2 == 1}));
                let c = ms(t);
                let mut x = frame(h);
                x.step += c;
                f.push(x);
            }
            for &id in &pg.all {
                if h.state().session.active().and_then(|s| s.doc.layer(id)).is_some_and(|l| !l.visible) {
                    run(h, "layer.setProps", json!({"layer": id.0, "visible": true}));
                }
            }
            settle(h, 2);
            f
        }
        "opacity" => {
            select(h, pg.pixel);
            let mut f = Vec::new();
            for k in 0..40 {
                let t = Instant::now();
                run(h, "layer.setProps", json!({"layer": pg.pixel.0, "opacity": 1.0 - k as f64 * 0.02, "coalesce": "layer-opacity-slider:1"}));
                let c = ms(t);
                let mut x = frame(h);
                x.step += c;
                f.push(x);
            }
            undo(h);
            f
        }
        "select-layer" => {
            let mut f = Vec::new();
            for k in 0..20 {
                let id = pg.all[(k * 11 + 5) % pg.all.len()];
                let t = Instant::now();
                run(h, "layer.select", json!({"layer": id.0}));
                let c = ms(t);
                let mut x = frame(h);
                x.step += c;
                f.push(x);
            }
            f
        }
        _ => {
            eprintln!("unknown scenario {name}");
            Vec::new()
        }
    }
}

const ALL: &[&str] = &[
    "idle",
    "hover",
    "eraser-20-hard",
    "eraser-100-hard",
    "eraser-100-soft",
    "eraser-400-hard",
    "eraser-400-soft",
    "eraser-100-hard-block",
    "eraser-100-hard-pencil",
    "bgeraser",
    "magiceraser",
    "brush-100-soft",
    "brush-400-soft",
    "move-layer",
    "move-group",
    "move-linked",
    "move-selection",
    "autoselect-hover",
    "transform",
    "marquee",
    "lasso",
    "crop",
    "type",
    "undo-redo",
    "layers-scroll",
    "visibility",
    "opacity",
    "select-layer",
];

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (w, hh) =
        arg(&args, "--size").and_then(|s| s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))).unwrap_or((2000u32, 3000u32));
    let n: usize = arg(&args, "--layers").and_then(|v| v.parse().ok()).unwrap_or(100);
    let moves: usize = arg(&args, "--moves").and_then(|v| v.parse().ok()).unwrap_or(120);
    let per_frame: usize = arg(&args, "--per-frame").and_then(|v| v.parse().ok()).unwrap_or(2);
    let cpu = args.iter().any(|a| a == "--cpu");
    let reps: usize = arg(&args, "--reps").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let only: Vec<String> = arg(&args, "--only").map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_default();
    let t = Instant::now();
    let pg = page(w, hh, n);
    let total = pg.doc.walk().len();
    println!("page {w}x{hh}: {total} layers built in {:.0} ms", ms(t));
    if let Some(path) = arg(&args, "--write-psd") {
        match photocraft_io::export(&pg.doc, "page.psd", &Default::default()) {
            Ok(r) => match std::fs::write(&path, r.bytes) {
                Ok(()) => println!("wrote {path}"),
                Err(e) => eprintln!("{path}: {e}"),
            },
            Err(e) => eprintln!("psd export: {e}"),
        }
        return;
    }
    let doc = pg.doc.clone();
    let builder = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).with_pixels_per_point(1.0).with_max_steps(1000);
    let make = move |cc: &mut eframe::CreationContext<'_>| {
        PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            app.set_wgpu(rs.clone());
        }
        app.session.open_document(doc.clone(), Some("page.psd".into()));
        app.sync_views();
        app
    };
    let mut h: H = if cpu { builder.build_eframe(make) } else { builder.wgpu_setup(photocraft_ui_egui::gpu_canvas::wgpu_setup()).build_eframe(make) };
    settle(&mut h, 10);
    println!("open + first frames {:.0} ms; canvas gpu {}", ms(t), h.state().perf.gpu);
    if let Some(what) = arg(&args, "--spin") {
        let t = Instant::now();
        while t.elapsed().as_secs() < 20 {
            scenario(&mut h, &pg, &what, moves, per_frame);
        }
        return;
    }
    let mut rows = Vec::new();
    for name in ALL.iter().filter(|s| only.is_empty() || only.iter().any(|o| s.starts_with(o.as_str()))) {
        settle(&mut h, 3);
        // `--reps N`: run the scenario N times and keep the last (warm) run.
        for _ in 1..reps {
            scenario(&mut h, &pg, name, moves, per_frame);
            settle(&mut h, 3);
        }
        let before = counters(&h);
        let frames = scenario(&mut h, &pg, name, moves, per_frame);
        let row = Row { name: name.to_string(), frames, counters: diff(&before, &counters(&h)) };
        row.print();
        if std::env::var_os("TOOL_FRAMES_SPANS").is_some() {
            println!(
                "    spans {:?} last refresh {} {:.1} ms command {:.1} ms",
                h.state().perf.spans,
                h.state().perf.last_refresh,
                h.state().perf.composite_ms,
                h.state().perf.command_ms
            );
        }
        if *name == "idle" {
            let reps = row.frames.iter().filter(|f| f.repaint).count();
            println!("    idle frames asking for another: {reps}/{}; causes: {:?}", row.frames.len(), h.ctx.repaint_causes());
        }
        rows.push(row);
    }
    if let Some(out) = arg(&args, "--json") {
        let r: Vec<Value> = rows.iter().map(Row::json).collect();
        let report = json!({"width": w, "height": hh, "layers": total, "rows": r});
        if let Err(e) = std::fs::write(&out, serde_json::to_string_pretty(&report).unwrap_or_default()) {
            eprintln!("{out}: {e}");
        }
    }
}
