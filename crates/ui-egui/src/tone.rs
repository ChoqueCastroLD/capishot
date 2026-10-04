//! Photoshop-style Levels and Curves editors for the Properties panel.
//!
//! Both draw the histogram of the image *below* the adjustment (what the adjustment receives) and
//! commit every change as `layer.setAdjustment` with a per-gesture `coalesce` key, so a drag is one
//! history step and the canvas (GPU compositor) updates live at full resolution.

use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use photocraft_doc::{Document, LayerContent, LayerId};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::theme::Tokens;

const CHANNELS: [(&str, &str); 4] = [("rgb", "RGB"), ("red", "Red"), ("green", "Green"), ("blue", "Blue")];

/// Histograms (RGB composite, R, G, B) of 256 bins each.
pub type Histograms = [[u32; 256]; 4];

/// Histogram of the document with `hide` (the adjustment being edited) switched off.
fn compute_histograms(doc: &Document, hide: LayerId) -> Histograms {
    let mut d = doc.clone();
    if let Some(l) = d.layer_mut(hide) {
        l.visible = false;
    }
    let img = photocraft_compose::thumbnail(&d, 384);
    let mut h = [[0u32; 256]; 4];
    for p in img.pixels.chunks_exact(4) {
        if p[3] == 0 {
            continue;
        }
        for c in 0..3 {
            h[c + 1][p[c] as usize] += 1;
        }
        h[0][((p[0] as u32 + p[1] as u32 + p[2] as u32) / 3) as usize] += 1;
    }
    h
}

/// Cached histograms for the adjustment layer; recomputed only when something other than this
/// editor changed the document (see `PhotocraftApp::tone_hist`).
fn histograms(app: &mut PhotocraftApp, id: LayerId) -> Arc<Histograms> {
    let Some(st) = app.session.active() else { return Arc::new([[0; 256]; 4]) };
    let (doc_id, rev) = (st.doc.id, st.revision);
    if let Some((d, l, r, h)) = &app.tone_hist
        && *d == doc_id
        && *l == id
        && *r == rev
    {
        return h.clone();
    }
    let t0 = crate::gpu_canvas::now_ms();
    let h = Arc::new(compute_histograms(&st.doc, id));
    app.perf.span("histogram", crate::gpu_canvas::now_ms() - t0);
    app.tone_hist = Some((doc_id, id, rev, h.clone()));
    h
}

/// Commit adjustment params; the editor's own edits keep the histogram cache valid.
fn commit(app: &mut PhotocraftApp, id: LayerId, mut params: Value, gesture: &str) {
    params["layer"] = json!(id.0);
    params["coalesce"] = json!(format!("tone-{}-{gesture}", id.0));
    let cached = app.tone_hist.as_ref().is_some_and(|(_, l, r, _)| *l == id && app.session.active().is_some_and(|s| s.revision == *r));
    if app.run("layer.setAdjustment", params).is_ok()
        && cached
        && let (Some((_, _, r, _)), Some(st)) = (app.tone_hist.as_mut(), app.session.active())
    {
        *r = st.revision;
    }
}

fn draw_histogram(p: &egui::Painter, r: Rect, h: &[u32; 256], color: Color32) {
    // Scale to a high percentile so a single spike (e.g. pure white) doesn't flatten the rest.
    let mut sorted: Vec<u32> = h.to_vec();
    sorted.sort_unstable();
    let top = sorted[250].max(1) as f32 * 1.1;
    let mut pts = vec![pos2(r.left(), r.bottom())];
    for (i, v) in h.iter().enumerate() {
        let x = r.left() + (i as f32 + 0.5) / 256.0 * r.width();
        let y = r.bottom() - (*v as f32 / top).min(1.0) * r.height();
        pts.push(pos2(x, y));
    }
    pts.push(pos2(r.right(), r.bottom()));
    // One mesh of vertical strips: no anti-aliasing feather, so no seams between bins.
    let mut mesh = egui::Mesh::default();
    for w in pts.windows(2).skip(1).take(256) {
        let (a, b) = (w[0], w[1]);
        let i = mesh.vertices.len() as u32;
        for q in [pos2(a.x, r.bottom()), a, b, pos2(b.x, r.bottom())] {
            mesh.colored_vertex(q, color);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
    p.add(mesh);
}

fn channel_color(ch: usize, t: &Tokens) -> Color32 {
    match ch {
        1 => Color32::from_rgb(235, 80, 80),
        2 => Color32::from_rgb(80, 205, 95),
        3 => Color32::from_rgb(90, 140, 255),
        _ => t.text,
    }
}

fn channel_picker(ui: &mut egui::Ui, id: egui::Id, label: &str) -> usize {
    let mut ch: usize = ui.data(|d| d.get_temp(id)).unwrap_or(0);
    ui.horizontal(|ui| {
        let t = Tokens::get(ui.ctx());
        ui.label(egui::RichText::new(label).color(t.text_dim).size(12.0));
        let opts: Vec<(usize, &str)> = CHANNELS.iter().enumerate().map(|(i, (_, l))| (i, *l)).collect();
        crate::widgets::dropdown(ui, &format!("{id:?}-ch"), &mut ch, &opts, 110.0);
    });
    ui.data_mut(|d| d.insert_temp(id, ch));
    ch
}

// ---------------------------------------------------------------------------------------------
// Histogram panel

/// Histogram panel (Window › Histogram): whole-image histogram with Photoshop's statistics.
/// Recomputed at most every 250 ms while the document keeps changing (e.g. during painting).
pub fn histogram_panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let Some(st) = app.session.active() else {
        ui.label(egui::RichText::new("No document").color(t.text_faint));
        return;
    };
    let (doc_id, rev) = (st.doc.id, st.revision);
    let now = crate::gpu_canvas::now_ms();
    let stale = !matches!(&app.doc_hist, Some((d, r, _, _)) if *d == doc_id && *r == rev);
    let due = app.doc_hist.as_ref().is_none_or(|(d, _, at, _)| *d != doc_id || now - at > 250.0);
    if stale && due {
        let t0 = now;
        let h = Arc::new(compute_histograms(&st.doc, LayerId(u64::MAX)));
        app.perf.span("histogram", crate::gpu_canvas::now_ms() - t0);
        app.doc_hist = Some((doc_id, rev, now, h));
    } else if stale {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(260));
    }
    let Some((_, _, _, h)) = app.doc_hist.clone() else { return };
    let size = app.session.active().map_or(photocraft_doc::Size::new(0, 0), |s| s.doc.size);
    // Statistics come from a downsampled cache (like Photoshop's cache levels).
    let level = (size.width.max(size.height) as f32 / 384.0).max(1.0).log2().ceil() as u32 + 1;
    let ch = channel_picker(ui, egui::Id::new("histogram-panel-ch"), "Channel:");
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(vec2(w, 100.0), Sense::hover());
    let p = ui.painter_at(r);
    p.rect_filled(r, 0.0, t.field);
    draw_histogram(&p, r, &h[ch], if ch == 0 { Color32::from_gray(if t.pro { 190 } else { 70 }) } else { channel_color(ch, &t) });
    if stale {
        // Photoshop's "cached data" warning triangle while the histogram lags the image.
        crate::icons::paint(ui, Rect::from_min_size(r.right_top() + vec2(-18.0, 2.0), vec2(16.0, 16.0)), "triangle-alert", 12.0, t.warning);
    }
    let hist = &h[ch];
    let n: u64 = hist.iter().map(|v| *v as u64).sum();
    if n == 0 {
        return;
    }
    let mean = hist.iter().enumerate().map(|(i, v)| i as f64 * *v as f64).sum::<f64>() / n as f64;
    let var = hist.iter().enumerate().map(|(i, v)| (i as f64 - mean).powi(2) * *v as f64).sum::<f64>() / n as f64;
    let mut acc = 0u64;
    let median = hist.iter().position(|v| {
        acc += *v as u64;
        acc * 2 >= n
    });
    ui.add_space(4.0);
    egui::Grid::new("hist-stats").num_columns(2).spacing(vec2(12.0, 2.0)).show(ui, |ui| {
        for (k, v) in [
            ("Mean:", format!("{mean:.2}")),
            ("Std Dev:", format!("{:.2}", var.sqrt())),
            ("Median:", median.unwrap_or(0).to_string()),
            ("Pixels:", (size.width as u64 * size.height as u64).to_string()),
            ("Cache Level:", level.to_string()),
        ] {
            ui.label(egui::RichText::new(k).color(t.text_dim).size(11.5));
            ui.label(egui::RichText::new(v).font(crate::theme::mono(11.5)).color(t.text));
            ui.end_row();
        }
    });
}

// ---------------------------------------------------------------------------------------------
// Curves

/// Curve points (0–255) per channel from the adjustment: [rgb, red, green, blue].
fn curves_points(adj: &photocraft_doc::Adjustment) -> [Vec<[f32; 2]>; 4] {
    let conv = |v: &[photocraft_doc::adjust::CurvePoint]| v.iter().map(|p| [p.input * 255.0, p.output * 255.0]).collect::<Vec<_>>();
    match adj {
        photocraft_doc::Adjustment::Curves { master, per_channel } => [conv(master), conv(&per_channel[0]), conv(&per_channel[1]), conv(&per_channel[2])],
        _ => Default::default(),
    }
}

fn curves_params(pts: &[Vec<[f32; 2]>; 4]) -> Value {
    let arr = |v: &Vec<[f32; 2]>| json!(v.iter().map(|p| [p[0].round(), p[1].round()]).collect::<Vec<_>>());
    json!({"points": arr(&pts[0]), "red": arr(&pts[1]), "green": arr(&pts[2]), "blue": arr(&pts[3])})
}

fn lut_of(points: &[[f32; 2]]) -> Vec<f32> {
    let cp: Vec<photocraft_doc::adjust::CurvePoint> =
        points.iter().map(|p| photocraft_doc::adjust::CurvePoint { input: p[0] / 255.0, output: p[1] / 255.0 }).collect();
    photocraft_compose::adjust::curve_lut(&cp)
}

pub fn curves_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId) {
    let Some(adj) = app.session.active().and_then(|s| s.doc.layer(id)).and_then(|l| match &l.content {
        LayerContent::Adjustment(a) => Some(a.clone()),
        _ => None,
    }) else {
        return;
    };
    let t = Tokens::get(ui.ctx());
    let mem = egui::Id::new(("curves", id.0));
    let ch = channel_picker(ui, mem.with("ch"), "Channel:");
    let mut pts = curves_points(&adj);
    let hist = histograms(app, id);
    let side = ui.available_width().min(300.0);
    ui.add_space(4.0);
    let (full, resp) = ui.allocate_exact_size(vec2(side, side + 14.0), Sense::click_and_drag());
    let graph = Rect::from_min_size(full.min + vec2(14.0, 0.0), vec2(side - 14.0, side - 14.0));
    let p = ui.painter_at(full);
    p.rect_filled(graph, 0.0, t.field);
    draw_histogram(&p, graph, &hist[ch], Color32::from_gray(if t.pro { 88 } else { 170 }).gamma_multiply(0.75));
    // Grid: quarters (Photoshop default), baseline diagonal.
    for i in 1..4 {
        let f = i as f32 / 4.0;
        let g = Stroke::new(1.0, t.separator);
        p.line_segment([pos2(graph.left() + f * graph.width(), graph.top()), pos2(graph.left() + f * graph.width(), graph.bottom())], g);
        p.line_segment([pos2(graph.left(), graph.top() + f * graph.height()), pos2(graph.right(), graph.top() + f * graph.height())], g);
    }
    p.line_segment([graph.left_bottom(), graph.right_top()], Stroke::new(1.0, t.separator.gamma_multiply(1.6)));
    p.rect_stroke(graph, 0.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
    // Gradient bars: input (bottom) and output (left).
    let hbar = Rect::from_min_max(pos2(graph.left(), graph.bottom() + 4.0), pos2(graph.right(), graph.bottom() + 12.0));
    let vbar = Rect::from_min_max(pos2(full.left(), graph.top()), pos2(full.left() + 8.0, graph.bottom()));
    let mut mesh = egui::Mesh::default();
    let (c0, c1) = (Color32::BLACK, if ch == 0 { Color32::WHITE } else { channel_color(ch, &t) });
    mesh.colored_vertex(hbar.left_top(), c0);
    mesh.colored_vertex(hbar.right_top(), c1);
    mesh.colored_vertex(hbar.right_bottom(), c1);
    mesh.colored_vertex(hbar.left_bottom(), c0);
    mesh.colored_vertex(vbar.left_bottom(), c0);
    mesh.colored_vertex(vbar.right_bottom(), c0);
    mesh.colored_vertex(vbar.right_top(), c1);
    mesh.colored_vertex(vbar.left_top(), c1);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh.add_triangle(4, 5, 6);
    mesh.add_triangle(4, 6, 7);
    p.add(mesh);
    let to_scr = |q: [f32; 2]| pos2(graph.left() + q[0] / 255.0 * graph.width(), graph.bottom() - q[1] / 255.0 * graph.height());
    let to_val =
        |s: Pos2| [((s.x - graph.left()) / graph.width() * 255.0).clamp(0.0, 255.0), ((graph.bottom() - s.y) / graph.height() * 255.0).clamp(0.0, 255.0)];
    // Other channels' curves, faintly, when editing the composite (Photoshop shows them too).
    let draw_curve = |pts: &[[f32; 2]], color: Color32, width: f32| {
        let lut = lut_of(pts);
        let n = lut.len();
        let line: Vec<Pos2> = (0..n).step_by((n / 256).max(1)).map(|i| to_scr([i as f32 / (n - 1) as f32 * 255.0, lut[i].clamp(0.0, 1.0) * 255.0])).collect();
        p.add(egui::Shape::line(line, Stroke::new(width, color)));
    };
    if ch == 0 {
        for (c, cp) in pts.iter().enumerate().skip(1) {
            if cp.len() > 2 || cp.first().is_some_and(|q| q[1] != 0.0) || cp.last().is_some_and(|q| q[1] != 255.0) {
                draw_curve(cp, channel_color(c, &t).gamma_multiply(0.8), 1.0);
            }
        }
    }
    draw_curve(&pts[ch], channel_color(ch, &t), 1.5);
    // Interaction.
    let sel_id = mem.with("sel");
    let mut sel: Option<usize> = ui.data(|d| d.get_temp(sel_id)).flatten();
    let gesture_id = mem.with("gesture");
    let mut changed = false;
    let cur = &mut pts[ch];
    if (resp.drag_started() || resp.clicked())
        && let Some(pos) = resp.interact_pointer_pos()
    {
        let hit =
            cur.iter().enumerate().map(|(i, q)| (i, to_scr(*q).distance(pos))).filter(|(_, d)| *d < 9.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i);
        sel = match hit {
            Some(i) => Some(i),
            None if graph.contains(pos) => {
                let v = to_val(pos);
                let i = cur.iter().position(|q| q[0] > v[0]).unwrap_or(cur.len());
                cur.insert(i, v);
                changed = true;
                Some(i)
            }
            None => sel,
        };
        ui.data_mut(|d| d.insert_temp(gesture_id, ui.input(|i| i.time).to_bits()));
    }
    if resp.dragged()
        && let (Some(i), Some(pos)) = (sel, resp.interact_pointer_pos())
        && i < cur.len()
    {
        let far = !graph.expand(24.0).contains(pos);
        let endpoint = i == 0 || i + 1 == cur.len();
        if far && !endpoint && cur.len() > 2 {
            // Dragging a point out of the graph deletes it (Photoshop).
            cur.remove(i);
            sel = None;
        } else {
            let mut v = to_val(pos);
            let lo = if i > 0 { cur[i - 1][0] + 1.0 } else { 0.0 };
            let hi = if i + 1 < cur.len() { cur[i + 1][0] - 1.0 } else { 255.0 };
            v[0] = v[0].clamp(lo, hi.max(lo));
            cur[i] = v;
        }
        changed = true;
    }
    // Points.
    for (i, q) in cur.iter().enumerate() {
        let r = Rect::from_center_size(to_scr(*q), vec2(7.0, 7.0));
        if Some(i) == sel {
            p.rect_filled(r, 0.0, t.text);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
        } else {
            p.rect_filled(r, 0.0, t.field);
            p.rect_stroke(r, 0.0, Stroke::new(1.0, t.text), egui::StrokeKind::Inside);
        }
    }
    // Input / Output of the selected point.
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let i = sel.filter(|i| *i < cur.len());
        let (mut vi, mut vo) = i.map_or((0.0, 0.0), |i| (cur[i][0], cur[i][1]));
        ui.label(egui::RichText::new("Input:").color(t.text_dim).size(12.0));
        let ri = ui.add_enabled_ui(i.is_some(), |ui| crate::widgets::value_field(ui, &mut vi, 0.0..=255.0, "", 52.0)).inner;
        ui.label(egui::RichText::new("Output:").color(t.text_dim).size(12.0));
        let ro = ui.add_enabled_ui(i.is_some(), |ui| crate::widgets::value_field(ui, &mut vo, 0.0..=255.0, "", 52.0)).inner;
        if let Some(i) = i
            && (ri.changed() || ro.changed())
        {
            let lo = if i > 0 { cur[i - 1][0] + 1.0 } else { 0.0 };
            let hi = if i + 1 < cur.len() { cur[i + 1][0] - 1.0 } else { 255.0 };
            cur[i] = [vi.round().clamp(lo, hi.max(lo)), vo.round().clamp(0.0, 255.0)];
            changed = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(sel_id, sel));
    if changed {
        let g: u64 = ui.data(|d| d.get_temp(gesture_id)).unwrap_or(0);
        commit(app, id, curves_params(&pts), &g.to_string());
    }
}

// ---------------------------------------------------------------------------------------------
// Levels

type Lv = [f32; 5]; // in_black, gamma, in_white, out_black, out_white (0–255, gamma as is)

fn levels_values(adj: &photocraft_doc::Adjustment) -> [Lv; 4] {
    let conv = |c: &photocraft_doc::adjust::LevelsChannel| [c.in_black * 255.0, c.gamma, c.in_white * 255.0, c.out_black * 255.0, c.out_white * 255.0];
    match adj {
        photocraft_doc::Adjustment::Levels { master, per_channel } => [conv(master), conv(&per_channel[0]), conv(&per_channel[1]), conv(&per_channel[2])],
        _ => [[0.0, 1.0, 255.0, 0.0, 255.0]; 4],
    }
}

fn levels_params(v: &[Lv; 4]) -> Value {
    let obj = |l: &Lv| json!({"inBlack": l[0].round(), "gamma": (l[1] * 100.0).round() / 100.0, "inWhite": l[2].round(), "outBlack": l[3].round(), "outWhite": l[4].round()});
    let mut p = obj(&v[0]);
    p["red"] = obj(&v[1]);
    p["green"] = obj(&v[2]);
    p["blue"] = obj(&v[3]);
    p
}

/// A triangular slider handle below a bar.
fn handle(p: &egui::Painter, x: f32, y: f32, fill: Color32, stroke: Color32) {
    p.add(egui::Shape::convex_polygon(vec![pos2(x, y), pos2(x + 5.5, y + 9.0), pos2(x - 5.5, y + 9.0)], fill, Stroke::new(1.0, stroke)));
}

/// Input level (0–255) where the gamma slider sits: the input that maps to 50% grey.
fn gamma_pos(black: f32, white: f32, gamma: f32) -> f32 {
    black + (white - black) * 0.5f32.powf(gamma)
}

fn gamma_from_pos(black: f32, white: f32, x: f32) -> f32 {
    let t = ((x - black) / (white - black).max(1.0)).clamp(0.01, 0.99);
    (t.ln() / 0.5f32.ln()).clamp(0.01, 9.99)
}

pub fn levels_editor(app: &mut PhotocraftApp, ui: &mut egui::Ui, id: LayerId) {
    let Some(adj) = app.session.active().and_then(|s| s.doc.layer(id)).and_then(|l| match &l.content {
        LayerContent::Adjustment(a) => Some(a.clone()),
        _ => None,
    }) else {
        return;
    };
    let t = Tokens::get(ui.ctx());
    let mem = egui::Id::new(("levels", id.0));
    let hist = histograms(app, id);
    let mut ch = 0;
    let mut auto = false;
    ui.horizontal(|ui| {
        ch = channel_picker(ui, mem.with("ch"), "");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            auto = crate::widgets::secondary_button(ui, "Auto", 0.0).clicked();
        });
    });
    let mut v = levels_values(&adj);
    let mut changed = false;
    let gesture_id = mem.with("gesture");
    if auto {
        // Clip 0.1% at each end of the composite histogram (Photoshop's default auto clipping).
        let h = &hist[if ch == 0 { 0 } else { ch }];
        let total: u32 = h.iter().sum();
        let clip = (total as f32 * 0.001) as u32;
        let (mut acc, mut lo) = (0u32, 0usize);
        while lo < 254 && acc + h[lo] <= clip {
            acc += h[lo];
            lo += 1;
        }
        let (mut acc, mut hi) = (0u32, 255usize);
        while hi > lo + 1 && acc + h[hi] <= clip {
            acc += h[hi];
            hi -= 1;
        }
        v[ch][0] = lo as f32;
        v[ch][2] = hi as f32;
        v[ch][1] = 1.0;
        changed = true;
        ui.data_mut(|d| d.insert_temp(gesture_id, ui.input(|i| i.time).to_bits()));
    }
    let w = ui.available_width().min(300.0);
    // Histogram.
    let (hr, _) = ui.allocate_exact_size(vec2(w, 110.0), Sense::hover());
    let hr = hr.shrink2(vec2(6.0, 0.0));
    let p = ui.painter_at(hr.expand(1.0));
    p.rect_filled(hr, 0.0, t.field);
    draw_histogram(&p, hr, &hist[ch], if ch == 0 { Color32::from_gray(if t.pro { 180 } else { 60 }) } else { channel_color(ch, &t).gamma_multiply(0.85) });
    p.rect_stroke(hr, 0.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    // Input sliders: black, gamma, white.
    let (sr, sresp) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click_and_drag());
    let track = sr.shrink2(vec2(6.0, 0.0));
    let xpos = |val: f32| track.left() + val / 255.0 * track.width();
    let xval = |x: f32| ((x - track.left()) / track.width() * 255.0).clamp(0.0, 255.0);
    let [b, g, wv, ob, ow] = v[ch];
    let gx = gamma_pos(b, wv, g);
    let drag_id = mem.with("drag");
    let mut which: Option<u8> = ui.data(|d| d.get_temp(drag_id)).flatten();
    if sresp.drag_started()
        && let Some(pos) = sresp.interact_pointer_pos()
    {
        let d = [(0u8, (xpos(b) - pos.x).abs()), (1, (xpos(gx) - pos.x).abs()), (2, (xpos(wv) - pos.x).abs())];
        which = d.iter().min_by(|a, c| a.1.total_cmp(&c.1)).map(|x| x.0);
        ui.data_mut(|d| d.insert_temp(gesture_id, ui.input(|i| i.time).to_bits()));
    }
    if sresp.dragged()
        && let (Some(k), Some(pos)) = (which, sresp.interact_pointer_pos())
    {
        let x = xval(pos.x);
        match k {
            0 => {
                v[ch][0] = x.min(wv - 2.0).round();
            }
            2 => {
                v[ch][2] = x.max(b + 2.0).round();
            }
            _ => v[ch][1] = gamma_from_pos(b, wv, x),
        }
        changed = true;
    }
    if sresp.drag_stopped() {
        which = None;
    }
    ui.data_mut(|d| d.insert_temp(drag_id, which));
    let sp = ui.painter_at(sr.expand(6.0));
    let [b, g, wv, _, _] = v[ch];
    handle(&sp, xpos(b), sr.top() + 2.0, Color32::BLACK, t.text_dim);
    handle(&sp, xpos(gamma_pos(b, wv, g)), sr.top() + 2.0, Color32::from_gray(128), t.text_dim);
    handle(&sp, xpos(wv), sr.top() + 2.0, Color32::WHITE, t.text_dim);
    ui.horizontal(|ui| {
        let fw = ((w - 16.0) / 3.0).min(70.0);
        let mut nb = v[ch][0];
        let mut ng = v[ch][1];
        let mut nw = v[ch][2];
        if crate::widgets::value_field(ui, &mut nb, 0.0..=253.0, "", fw).changed() {
            v[ch][0] = nb.round().min(v[ch][2] - 2.0);
            changed = true;
        }
        ui.add_space(((w - 3.0 * fw) / 2.0 - 12.0).max(0.0));
        if crate::widgets::value_field(ui, &mut ng, 0.01..=9.99, "", fw).changed() {
            v[ch][1] = ng.clamp(0.01, 9.99);
            changed = true;
        }
        ui.add_space(((w - 3.0 * fw) / 2.0 - 12.0).max(0.0));
        if crate::widgets::value_field(ui, &mut nw, 2.0..=255.0, "", fw).changed() {
            v[ch][2] = nw.round().max(v[ch][0] + 2.0);
            changed = true;
        }
    });
    // Output levels.
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Output Levels").color(t.text_dim).size(12.0));
    let (ob_r, _) = ui.allocate_exact_size(vec2(w, 10.0), Sense::hover());
    let ob_r = ob_r.shrink2(vec2(6.0, 0.0));
    let mut mesh = egui::Mesh::default();
    let c1 = if ch == 0 { Color32::WHITE } else { channel_color(ch, &t) };
    mesh.colored_vertex(ob_r.left_top(), Color32::BLACK);
    mesh.colored_vertex(ob_r.right_top(), c1);
    mesh.colored_vertex(ob_r.right_bottom(), c1);
    mesh.colored_vertex(ob_r.left_bottom(), Color32::BLACK);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    ui.painter().add(mesh);
    let (or, oresp) = ui.allocate_exact_size(vec2(w, 14.0), Sense::click_and_drag());
    let otrack = or.shrink2(vec2(6.0, 0.0));
    let oxpos = |val: f32| otrack.left() + val / 255.0 * otrack.width();
    let oval = |x: f32| ((x - otrack.left()) / otrack.width() * 255.0).clamp(0.0, 255.0);
    let odrag_id = mem.with("odrag");
    let mut owhich: Option<u8> = ui.data(|d| d.get_temp(odrag_id)).flatten();
    if oresp.drag_started()
        && let Some(pos) = oresp.interact_pointer_pos()
    {
        owhich = Some(if (oxpos(ob) - pos.x).abs() <= (oxpos(ow) - pos.x).abs() { 0 } else { 1 });
        ui.data_mut(|d| d.insert_temp(gesture_id, ui.input(|i| i.time).to_bits()));
    }
    if oresp.dragged()
        && let (Some(k), Some(pos)) = (owhich, oresp.interact_pointer_pos())
    {
        v[ch][if k == 0 { 3 } else { 4 }] = oval(pos.x).round();
        changed = true;
    }
    if oresp.drag_stopped() {
        owhich = None;
    }
    ui.data_mut(|d| d.insert_temp(odrag_id, owhich));
    let op = ui.painter_at(or.expand(6.0));
    handle(&op, oxpos(v[ch][3]), or.top() + 2.0, Color32::BLACK, t.text_dim);
    handle(&op, oxpos(v[ch][4]), or.top() + 2.0, Color32::WHITE, t.text_dim);
    ui.horizontal(|ui| {
        let fw = ((w - 16.0) / 3.0).min(70.0);
        let (mut a, mut c) = (v[ch][3], v[ch][4]);
        if crate::widgets::value_field(ui, &mut a, 0.0..=255.0, "", fw).changed() {
            v[ch][3] = a.round();
            changed = true;
        }
        ui.add_space((w - 2.0 * fw - 16.0).max(0.0));
        if crate::widgets::value_field(ui, &mut c, 0.0..=255.0, "", fw).changed() {
            v[ch][4] = c.round();
            changed = true;
        }
    });
    let _ = (g, ob, ow);
    if changed {
        let gk: u64 = ui.data(|d| d.get_temp(gesture_id)).unwrap_or(0);
        commit(app, id, levels_params(&v), &gk.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_slider_position_round_trips() {
        for g in [0.3f32, 1.0, 1.8, 4.0] {
            let x = gamma_pos(20.0, 230.0, g);
            assert!((gamma_from_pos(20.0, 230.0, x) - g).abs() < 1e-3, "{g}");
        }
        // Moving the grey slider left brightens (gamma > 1), as in Photoshop.
        assert!(gamma_from_pos(0.0, 255.0, 80.0) > 1.0);
        assert_eq!(gamma_pos(0.0, 255.0, 1.0), 127.5);
    }

    #[test]
    fn params_round_trip_through_engine() {
        let v: [Lv; 4] = [[10.0, 1.2, 240.0, 5.0, 250.0], [0.0, 1.0, 255.0, 0.0, 255.0], [3.0, 0.8, 200.0, 0.0, 255.0], [0.0, 1.0, 255.0, 0.0, 255.0]];
        let adj = photocraft_engine::commands::adjustment_from_params("levels", &levels_params(&v));
        let back = levels_values(&adj);
        for (a, b) in v.iter().flatten().zip(back.iter().flatten()) {
            assert!((a - b).abs() < 0.01, "{a} vs {b}");
        }
        let pts: [Vec<[f32; 2]>; 4] = [
            vec![[0.0, 0.0], [128.0, 150.0], [255.0, 255.0]],
            vec![[0.0, 10.0], [255.0, 245.0]],
            vec![[0.0, 0.0], [255.0, 255.0]],
            vec![[0.0, 0.0], [255.0, 255.0]],
        ];
        let adj = photocraft_engine::commands::adjustment_from_params("curves", &curves_params(&pts));
        assert_eq!(curves_points(&adj), pts);
    }

    #[test]
    fn histogram_ignores_the_edited_adjustment() {
        let mut s = photocraft_engine::Session::new();
        s.execute("file.new", json!({"width": 64, "height": 64})).unwrap(); // white
        s.execute("layer.newAdjustmentLayer.invert", json!({})).unwrap();
        let st = s.active().unwrap();
        let h = compute_histograms(&st.doc, st.active_layer.unwrap());
        assert!(h[0][255] > 0 && h[0][0] == 0, "sees the white image, not the inverted result");
    }
}
