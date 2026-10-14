//! The TextShapeR strip under the preview line (TypeR 3.0's inline TextShapeR): the best line
//! breakings of the active type layer, shaped like its bubble (`typer.shapeText`), three at a
//! time. Click applies one (`typer.applyShape`, one undo step); Shift+click also moves the
//! script to the next line. Hidden when the active layer isn't type or while it is being edited.

use egui::{Rect, RichText, Sense, Stroke, StrokeKind, vec2};
use serde_json::{Value, json};

use super::act;
use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Variants shown at a time.
const PER_PAGE: usize = 3;
/// Height of a preview tile.
const TILE_H: f32 = 64.0;

/// What the variants were computed for: document, layer, a hash of the layer's text, style and
/// geometry, and whether a selection is set (it replaces the bubble). Edits elsewhere in the
/// document (painting, other layers) don't reshape.
type Key = (u64, u64, u64, bool);

#[derive(Clone, Default)]
struct Strip {
    key: Option<Key>,
    variants: Vec<Variant>,
    /// No bubble was found around the text.
    no_bubble: bool,
    page: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct Variant {
    lines: Vec<String>,
    fits: Option<bool>,
}

fn strip_id() -> egui::Id {
    egui::Id::new("typer-textshaper")
}

fn parse(v: &Value) -> (Vec<Variant>, bool) {
    let variants = v["variants"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| Variant {
                    lines: x["lines"].as_array().map(|l| l.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                    fits: x["fits"].as_bool(),
                })
                .filter(|x| !x.lines.is_empty())
                .collect()
        })
        .unwrap_or_default();
    (variants, v["bubble"]["found"] == json!(false))
}

/// The active type layer's key.
fn target(app: &PhotocraftApp) -> Option<Key> {
    use std::hash::{Hash, Hasher};
    let d = app.session.active()?;
    let id = d.active_layer?;
    let photocraft_doc::LayerContent::Text(t) = &d.doc.layer(id)?.content else { return None };
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.text.hash(&mut h);
    format!("{:?}{:?}{:?}", t.shape, t.transform, t.char_runs().first().map(|r| &r.style)).hash(&mut h);
    Some((d.doc.id.0, id.0, h.finish(), d.doc.selection.is_some()))
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    // Not while the Type tool edits text: each keystroke would reshape.
    if app.ui.text_edit.is_some() {
        return;
    }
    let Some(key) = target(app) else { return };
    let ctx = ui.ctx().clone();
    let mut strip: Strip = ctx.data_mut(|d| d.get_temp::<Strip>(strip_id())).unwrap_or_default();
    if strip.key != Some(key) {
        let r = app.session.execute("typer.shapeText", json!({"layer": key.1, "limit": 12}));
        let (variants, no_bubble) = r.as_ref().map(parse).unwrap_or_default();
        strip = Strip { key: Some(key), variants, no_bubble, page: 0 };
    }
    let t = Tokens::get(ui.ctx());
    let open_id = egui::Id::new("typer-textshaper-open");
    let mut open = ctx.data_mut(|d| *d.get_persisted_mut_or_insert_with(open_id, || true));
    let pages = strip.variants.len().div_ceil(PER_PAGE).max(1);
    strip.page %= pages;
    ui.horizontal(|ui| {
        let icon = if open { "chevron-down" } else { "chevron-right" };
        if crate::icons::button_with_icon_size(ui, icon, 18.0, 13.0, false, tl!("TextShapeR")).clicked() {
            open = !open;
        }
        ui.label(RichText::new(tl!("TextShapeR")).color(t.text_dim).size(11.5))
            .on_hover_text(tl!("Line breaks shaped like the bubble. Click: apply · Shift+click: apply and go to the next line"));
        if strip.no_bubble {
            ui.label(RichText::new("·").color(t.text_faint)).on_hover_text(tl!("No bubble around the text: shapes without an outline"));
        }
        if open && pages > 1 {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons::button_with_icon_size(ui, "chevron-right", 18.0, 13.0, false, tl!("More shapes")).clicked() {
                    strip.page = (strip.page + 1) % pages;
                }
                ui.label(RichText::new(format!("{}/{pages}", strip.page + 1)).color(t.text_faint).size(10.5));
                if crate::icons::button_with_icon_size(ui, "chevron-left", 18.0, 13.0, false, tl!("More shapes")).clicked() {
                    strip.page = (strip.page + pages - 1) % pages;
                }
            });
        }
    });
    ctx.data_mut(|d| d.insert_persisted(open_id, open));
    let mut chosen: Option<(Vec<String>, bool)> = None;
    if open && !strip.variants.is_empty() {
        let shown: Vec<Variant> = strip.variants.iter().skip(strip.page * PER_PAGE).take(PER_PAGE).cloned().collect();
        let gap = 4.0;
        let w = ((ui.available_width() - gap * (PER_PAGE as f32 - 1.0)) / PER_PAGE as f32).max(24.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for v in &shown {
                let (rect, resp) = ui.allocate_exact_size(vec2(w, TILE_H), Sense::click());
                #[cfg(test)]
                ctx.data_mut(|d| d.insert_temp(egui::Id::new("typer-textshaper-tiles"), rect.center().y));
                let hovered = resp.hovered();
                let border = if v.fits == Some(false) {
                    t.danger
                } else if hovered {
                    t.accent
                } else {
                    t.field_border
                };
                ui.painter().rect_filled(rect, t.radius_sm, t.field);
                ui.painter().rect_stroke(rect, t.radius_sm, Stroke::new(1.0, border), StrokeKind::Inside);
                paint_lines(ui, &v.lines, rect.shrink(4.0), t.text);
                let mut tip = v.lines.join("\n");
                if v.fits == Some(false) {
                    tip.push_str(&format!("\n\n{}", tl!("Doesn't fit the bubble")));
                }
                let resp = resp.on_hover_text(tip);
                if resp.clicked() {
                    chosen = Some((v.lines.clone(), ui.input(|i| i.modifiers.shift)));
                }
            }
        });
    }
    ctx.data_mut(|d| d.insert_temp(strip_id(), strip));
    if let Some((lines, advance)) = chosen {
        act(app, "typer.applyShape", json!({"layer": key.1, "lines": lines}));
        if advance && app.ui.typer.message.is_empty() {
            act(app, "typer.line.next", json!({"add": true}));
        }
    }
}

/// The lines centred in `rect`, one font size for all, as large as fits.
fn paint_lines(ui: &egui::Ui, lines: &[String], rect: Rect, color: egui::Color32) {
    let n = lines.len().max(1) as f32;
    let fonts = |size: f32| egui::FontId::proportional(size);
    let widest = |size: f32| lines.iter().map(|l| ui.painter().layout_no_wrap(l.clone(), fonts(size), color).size().x).fold(0.0f32, f32::max);
    let base = 10.0f32;
    let size = (base * (rect.width() / widest(base).max(1.0))).min(rect.height() / n / 1.15).clamp(3.0, 14.0);
    let line_h = size * 1.15;
    let top = rect.center().y - line_h * n / 2.0;
    let painter = ui.painter().with_clip_rect(rect);
    for (i, l) in lines.iter().enumerate() {
        let y = top + line_h * (i as f32 + 0.5);
        painter.text(egui::pos2(rect.center().x, y), egui::Align2::CENTER_CENTER, l, fonts(size), color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Modifiers, PointerButton, Pos2};
    use egui_kittest::Harness;

    const LIBRARY: &str = r##"{"styles": [{"id": "reg", "name": "Regular", "prefixes": [],
        "textProps": {"typeUnit": "pixelsUnit", "layerText": {"textStyleRange": [{"textStyle": {"fontName": "Inter", "size": 20}}],
        "paragraphStyleRange": [{"paragraphStyle": {"alignment": "center", "hyphenate": true}}]}}}], "defaultStyleId": "reg"}"##;

    /// A white page with one pasted line of dialogue, the active layer.
    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("typer.library.import", json!({"json": LIBRARY})).unwrap();
        app.run("typer.script.set", json!({"text": "Page 1\nNunca pensé que llegaríamos tan lejos, pero aquí estamos todavía juntos.\nSegunda."})).unwrap();
        app.run("file.new", json!({"width": 800, "height": 600, "background": "white"})).unwrap();
        app.sync_views();
        app.run("select.rect", json!({"x": 150, "y": 120, "width": 420, "height": 300})).unwrap();
        app.run("typer.paste", json!({})).unwrap();
        app.run("typer.line.previous", json!({})).unwrap();
        app
    }

    fn harness(app: PhotocraftApp, wgpu: bool) -> Harness<'static, PhotocraftApp> {
        let b = Harness::builder().with_size(vec2(320.0, 260.0)).with_step_dt(1.0 / 60.0);
        let b = if wgpu { b.wgpu() } else { b };
        let mut h = b.build_ui_state(
            |ui, app: &mut PhotocraftApp| {
                if !ui.ctx().fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) {
                    return;
                }
                egui::CentralPanel::default().show(ui, |ui| super::super::preview::show(app, ui));
            },
            app,
        );
        PhotocraftApp::setup_context(&h.ctx, crate::theme::ThemeKind::ALL[0]);
        h.run_steps(4);
        h
    }

    fn layer_text(app: &PhotocraftApp) -> String {
        let d = app.session.active().unwrap();
        match &d.doc.layer(d.active_layer.unwrap()).unwrap().content {
            photocraft_doc::LayerContent::Text(t) => t.text.clone(),
            _ => panic!("not text"),
        }
    }

    fn click(h: &mut Harness<'static, PhotocraftApp>, p: Pos2, modifiers: Modifiers) {
        h.event(Event::ModifiersChanged(modifiers));
        h.input_mut().events.push(Event::PointerMoved(p));
        h.step();
        h.input_mut().events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers });
        h.step();
        h.input_mut().events.push(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers });
        h.run_steps(3);
        h.event(Event::ModifiersChanged(Modifiers::NONE));
    }

    #[test]
    fn tiles_apply_the_shape_and_shift_also_advances() {
        let mut h = harness(app(), false);
        let strip: Strip = h.ctx.data_mut(|d| d.get_temp(strip_id())).expect("the strip ran");
        assert!(strip.variants.len() >= 3, "{:?}", strip.variants);
        assert!(!strip.no_bubble);
        // The tiles sit at the bottom of the preview block, three across.
        let first = strip.variants[0].lines.join("\n");
        let y = h.ctx.data_mut(|d| d.get_temp::<f32>(egui::Id::new("typer-textshaper-tiles"))).expect("tiles drawn");
        let before = h.state().session.active().unwrap().history.past_len();
        click(&mut h, Pos2::new(40.0, y), Modifiers::NONE);
        assert_eq!(layer_text(h.state()), first);
        assert_eq!(h.state().session.active().unwrap().history.past_len(), before + 1, "one undo step");
        assert_eq!(h.state().session.typer.state.current_line_index, 1, "a plain click stays on the line");
        let second = strip.variants[1].lines.join("\n");
        click(&mut h, Pos2::new(160.0, y), Modifiers::SHIFT);
        assert_eq!(layer_text(h.state()), second);
        assert_eq!(h.state().session.typer.state.current_line_index, 2, "Shift+click goes to the next line");
    }

    /// `CAPISHOT_SNAPSHOT_DIR=<dir>`: writes the strip as a PNG to look at.
    #[test]
    fn snapshot_png() {
        let Some(dir) = std::env::var_os("CAPISHOT_SNAPSHOT_DIR") else { return };
        let mut h = harness(app(), true);
        h.run_steps(3);
        h.render().unwrap().save(std::path::Path::new(&dir).join("typer-textshaper-strip.png")).unwrap();
    }

    #[test]
    fn parses_shape_results() {
        let v = json!({"variants": [{"lines": ["a", "b"], "fits": true}, {"lines": []}, {"lines": ["c"], "fits": false}], "bubble": {"found": false}});
        let (variants, no_bubble) = parse(&v);
        assert_eq!(variants.len(), 2);
        assert_eq!(variants[1], Variant { lines: vec!["c".into()], fits: Some(false) });
        assert!(no_bubble);
        assert_eq!(parse(&json!({})), (Vec::new(), false));
    }
}
