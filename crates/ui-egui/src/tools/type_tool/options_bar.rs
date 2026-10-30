use super::defaults::{apply, drag_key, formatting_params, open_color_picker, shown_scale, target, target_doc, target_text, type_unit};
use super::editing::{cancel, commit, commit_with};
use super::fonts::{font_picker, fonts_ready, selected_style, style_label, styles};
use super::keys::is_vertical;
use super::stroke::stroke_control;
use super::{Color32, LayerId, PhotocraftApp, Stroke, byte_of, json, text_layer};

/// Photoshop's Type options bar.
/// The options bar's Character/Paragraph button: hides the Character | Paragraph group when it
/// shows, else opens it on the Character panel (`window.panel.character`).
pub(crate) fn toggle_type_panels(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.panels.character {
        app.ui.panels.character = false;
        return;
    }
    if let Err(e) = crate::menus::invoke(app, ctx, "window.panel.character", json!({})) {
        app.ui.status = e;
        app.ui.status_error = true;
    }
}

/// The type target when it is type in a path: its layer, vertical alignment, inset (`None`:
/// automatic) and the inset in effect (px).
pub(crate) fn in_path_target(app: &PhotocraftApp) -> Option<(u64, photocraft_doc::text::InPathAlign, Option<f32>, f32)> {
    let (layer, _) = target(app)?;
    let t = target_text(app)?;
    let photocraft_doc::text::TextShape::InPath { inset, valign, .. } = &t.shape else { return None };
    let shown = photocraft_text::in_path::Area::of(&t.shape).map_or(0.0, |a| a.inset);
    Some((layer, *valign, *inset, shown))
}

/// Type in a path: where its block of lines sits and how far its glyphs stay inside the path
/// (Photoshop has neither; scanlation balloons want both).
fn in_path_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    use photocraft_doc::text::InPathAlign;
    let Some((layer, valign, inset, shown)) = in_path_target(app) else { return };
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 2.0;
    for (v, icon, tip) in [
        (InPathAlign::Top, "align-vertical-justify-start", tl!("Lines from the top of the path")),
        (InPathAlign::Center, "align-vertical-justify-center", tl!("Lines centered in the path")),
    ] {
        if crate::icons::button(ui, icon, 24.0, valign == v, tip).clicked() && valign != v {
            let _ = app.run("type.edit", json!({"layer": layer, "vertical": v.name()}));
        }
    }
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.label(tl!("Inset"));
    let mut px = (shown * 10.0).round() / 10.0;
    let resp = ui
        .add(egui::DragValue::new(&mut px).range(0.0..=1000.0).speed(0.5).max_decimals(1).suffix(" px"))
        .accessible_name(tl!("Inset"))
        .on_hover_text(tl!("How far the text stays inside the path"));
    if resp.changed() {
        let mut p = json!({"layer": layer, "inset": px});
        if let Some(key) = drag_key(ui.ctx()) {
            p["coalesce"] = json!(key);
        }
        let _ = app.run("type.edit", p);
    }
    let auto = inset.is_none();
    if ui.selectable_label(auto, tl!("Auto")).on_hover_text(tl!("Inset 5% of the path's smaller side")).clicked() {
        let v = if auto { json!(px) } else { json!("auto") };
        let _ = app.run("type.edit", json!({"layer": layer, "inset": v}));
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

/// Bubble Type: the style chip and the Styles popover; for selected bubble text, where its lines
/// sit and the margin inside it, applied live.
/// The line spacing (leading) shown: of the type target (its layer's points: `None` is Auto),
/// the size it's relative to and the paragraph's auto factor; without a target, the tool's for
/// new type.
fn shown_leading(app: &PhotocraftApp) -> (Option<f32>, f32, f32) {
    match styles_at(app) {
        Some((c, p)) => (c.leading_pt, c.size_pt, p.auto_leading.max(0.01)),
        None => (app.ui.tool_options.type_leading, app.ui.tool_options.type_size, 1.2),
    }
}

/// Set the line spacing in the target layer's points (`None`: Auto, 120% of the size), live on
/// the type target and for new type.
pub(super) fn set_leading(app: &mut PhotocraftApp, ctx: &egui::Context, pt: Option<f32>) {
    app.ui.tool_options.type_leading = pt.map(|v| v.clamp(0.1, 5000.0));
    if target(app).is_some() {
        apply(app, ctx, json!({ "leading": pt.map_or(json!("auto"), |v| json!(v.clamp(0.1, 5000.0))) }));
    }
}

/// The options bar's line spacing (Type and Bubble Type): the leading in the type unit, and Auto.
fn leading_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let (lead, size, auto_k) = shown_leading(app);
    let k = shown_scale(app);
    let (u, unit) = type_unit(app);
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 4.0;
    let (r, resp) = ui.allocate_exact_size(egui::vec2(18.0, 22.0), egui::Sense::hover());
    crate::icons::paint(ui, r, "line-height", 16.0, crate::theme::Tokens::get(ui.ctx()).text_dim);
    resp.on_hover_text(tl!("Line spacing (leading)"));
    let mut shown = lead.unwrap_or(size * auto_k) * k * u;
    if crate::widgets::value_field(ui, &mut shown, 0.1 * u..=5000.0 * u, unit, 56.0)
        .accessible_name(tl!("Line spacing (leading)"))
        .on_hover_text(tl!("Line spacing (leading)"))
        .changed()
    {
        set_leading(app, ui.ctx(), Some(shown / u / k));
    }
    let auto = lead.is_none();
    if ui.selectable_label(auto, tl!("Auto leading")).on_hover_text(tl!("Line spacing of 120% of the size")).clicked() {
        set_leading(app, ui.ctx(), if auto { Some(size * auto_k) } else { None });
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

/// The phone type bar's line spacing button and its popup: Auto, a step of a point either way,
/// and spacings relative to the size; applied to the edited layer as they're chosen.
fn touch_leading(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let open_id = egui::Id::unique("type-leading-open");
    let open = ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false));
    let button = crate::icons::button(ui, "line-height", 44.0, open, tl!("Line spacing (leading)"));
    crate::touch_ui::note_rect(ui.ctx(), "typeLeading", button.rect);
    if button.clicked() {
        ui.data_mut(|d| d.insert_temp(open_id, !open));
    }
    if !ui.data(|d| d.get_temp::<bool>(open_id).unwrap_or(false)) {
        return;
    }
    let (lead, size, auto_k) = shown_leading(app);
    let k = shown_scale(app);
    let (u, unit) = type_unit(app);
    let t = crate::theme::Tokens::get(ui.ctx());
    let screen = crate::safe_area::rect(ui.ctx());
    let x = button.rect.center().x.clamp(screen.left() + 150.0, (screen.right() - 150.0).max(screen.left() + 150.0));
    let area = egui::Area::new(egui::Id::unique("type-leading"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::CENTER_BOTTOM)
        .fixed_pos(egui::pos2(x, button.rect.top() - 8.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).fill(t.chrome).show(ui, |ui| {
                ui.set_width(280.0);
                ui.label(egui::RichText::new(tl!("Line spacing (leading)")).font(crate::theme::semibold(14.0)));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let auto = lead.is_none();
                    let r = ui.add_sized([72.0, 40.0], egui::Button::new(tl!("Auto")).selected(auto));
                    crate::touch_ui::note_rect(ui.ctx(), "typeLeadingAuto", r.rect);
                    if r.clicked() && !auto {
                        set_leading(app, ui.ctx(), None);
                    }
                    let current = lead.unwrap_or(size * auto_k);
                    let r = ui.add_sized([40.0, 40.0], egui::Button::new("−"));
                    crate::touch_ui::note_rect(ui.ctx(), "typeLeadingMinus", r.rect);
                    if r.clicked() {
                        set_leading(app, ui.ctx(), Some((current - 1.0 / k).max(0.1)));
                    }
                    let shown = current * k * u;
                    ui.add_sized([80.0, 40.0], egui::Label::new(egui::RichText::new(format!("{shown:.1} {unit}")).size(15.0)));
                    let r = ui.add_sized([40.0, 40.0], egui::Button::new("+"));
                    crate::touch_ui::note_rect(ui.ctx(), "typeLeadingPlus", r.rect);
                    if r.clicked() {
                        set_leading(app, ui.ctx(), Some(current + 1.0 / k));
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for pct in [100, 120, 150, 200] {
                        let v = size * pct as f32 / 100.0;
                        let on = lead.is_some_and(|l| (l - v).abs() < 0.05);
                        let r = ui.add_sized([60.0, 36.0], egui::Button::new(format!("{pct}%")).selected(on));
                        crate::touch_ui::note_rect(ui.ctx(), &format!("typeLeading{pct}"), r.rect);
                        if r.clicked() {
                            set_leading(app, ui.ctx(), Some(v));
                        }
                    }
                });
            });
        });
    let outside = ui.input(|i| i.pointer.any_pressed())
        && ui.input(|i| i.pointer.interact_pos()).is_some_and(|p| !area.response.rect.contains(p) && !button.rect.contains(p));
    if outside || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        ui.data_mut(|d| d.insert_temp(open_id, false));
    }
}

fn bubble_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    crate::bubble_type_ui::options(app, ui);
    bubble_target_options(app, ui);
}

/// For selected bubble text: where its lines sit and the margin inside it, applied live.
fn bubble_target_options(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    use photocraft_doc::text::InPathAlign;
    let Some((layer, valign, inset, shown)) = in_path_target(app) else { return };
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 2.0;
    for (v, icon, tip) in [
        (InPathAlign::Top, "align-vertical-justify-start", tl!("Lines from the top of the bubble")),
        (InPathAlign::Center, "align-vertical-justify-center", tl!("Lines centered in the bubble")),
    ] {
        if crate::icons::button(ui, icon, 24.0, valign == v, tip).clicked() && valign != v {
            let _ = app.run("type.edit", json!({"layer": layer, "vertical": v.name()}));
        }
    }
    ui.spacing_mut().item_spacing.x = 4.0;
    ui.label(tl!("Margin"));
    let mut px = (shown * 10.0).round() / 10.0;
    let resp = ui
        .add(egui::DragValue::new(&mut px).range(0.0..=1000.0).speed(0.5).max_decimals(1).suffix(" px"))
        .accessible_name(tl!("Margin"))
        .on_hover_text(tl!("How far the text stays inside the bubble"));
    if resp.changed() {
        let mut p = json!({"layer": layer, "inset": px});
        if let Some(key) = drag_key(ui.ctx()) {
            p["coalesce"] = json!(key);
        }
        let _ = app.run("type.edit", p);
    }
    let auto = inset.is_none();
    if ui.selectable_label(auto, tl!("Auto")).on_hover_text(tl!("A margin of 5% of the bubble's smaller side")).clicked() {
        let v = if auto { json!(px) } else { json!("auto") };
        let _ = app.run("type.edit", json!({"layer": layer, "inset": v}));
    }
    ui.spacing_mut().item_spacing.x = 8.0;
}

/// The Type tools' options on a phone (the simple interface's options popover): a labelled form,
/// each control on its row, alignment as one segmented row, the colour inline.
pub fn phone_form(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let shown = target(app).and_then(|(id, _)| {
        let st = app.session.active()?;
        let tl = text_layer(&st.doc, LayerId(id))?;
        let run = tl.char_runs().into_iter().next().map(|r| r.style);
        let size = styles_at(app).map_or(tl.size_pt, |(c, _)| c.size_pt) * shown_scale(app);
        Some((tl.font_family.clone(), run.as_ref().map(selected_style).unwrap_or_default(), size, styles_at(app).map_or(tl.color, |(c, _)| c.color)))
    });
    let o = app.ui.tool_options.clone();
    let (mut fam, mut style, size) = match &shown {
        Some((f, s, z, _)) => (f.clone(), if s.is_empty() { "Regular".into() } else { s.clone() }, *z),
        None => (o.type_font.clone(), o.type_style.clone(), o.type_size),
    };
    let ctx = ui.ctx().clone();
    // The controls take what is left of the row after the labels.
    let field = (ui.available_width() - 110.0).max(140.0);
    egui::Grid::new("type-phone-form").num_columns(2).spacing([12.0, 10.0]).min_col_width(90.0).show(ui, |ui| {
        if app.ui.tool == crate::state::Tool::BubbleType {
            ui.label(tl!("Styles"));
            ui.horizontal(|ui| crate::bubble_type_ui::options(app, ui));
            ui.end_row();
        }
        ui.label(tl!("Font"));
        if fonts_ready(ui, field) && font_picker(app, ui, &mut fam, field).is_some() {
            app.ui.tool_options.type_font = fam.clone();
            let st = styles(&fam);
            style = if st.contains(&style) { style.clone() } else { st.first().cloned().unwrap_or_else(|| "Regular".into()) };
            app.ui.tool_options.type_style = style.clone();
            apply(app, &ctx, json!({"font": fam}));
        }
        ui.end_row();
        ui.label(tl!("Style"));
        let opts: Vec<(String, String)> = styles(&fam).into_iter().map(|s| (s.clone(), style_label(&s))).collect();
        let opts_ref: Vec<(String, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
        if crate::widgets::dropdown(ui, "type-style-phone", tl!("Style"), &mut style, &opts_ref, field) {
            app.ui.tool_options.type_style = style.clone();
            apply(app, &ctx, json!({"fontStyle": style}));
        }
        ui.end_row();
        ui.label(tl!("Size"));
        let (unit, label) = type_unit(app);
        let mut display = size * unit;
        if crate::widgets::value_field(ui, &mut display, unit..=1296.0 * unit, label, 90.0).accessible_name(tl!("Size")).changed() {
            let size = display / unit;
            app.ui.tool_options.type_size = size;
            apply(app, &ctx, json!({"size": size, "metricsAsShown": true}));
        }
        ui.end_row();
        ui.label(tl!("Anti-alias"));
        let mut aa = target(app)
            .and_then(|(id, _)| app.session.active().and_then(|st| text_layer(&st.doc, LayerId(id))))
            .map(|t| photocraft_engine::type_extra_cmds::aa_name(t.antialias).to_string())
            .unwrap_or_else(|| o.type_aa.clone());
        let aa_opts = [
            ("none".to_string(), tl!("None")),
            ("sharp".to_string(), tl!("Sharp")),
            ("crisp".to_string(), tl!("Crisp")),
            ("strong".to_string(), tl!("Strong")),
            ("smooth".to_string(), tl!("Smooth")),
        ];
        if crate::widgets::dropdown(ui, "type-aa-phone", "", &mut aa, &aa_opts, field) {
            app.ui.tool_options.type_aa = aa.clone();
            if let Some(params) = formatting_params(app) {
                let _ = app.run(&format!("type.antiAlias.{aa}"), params);
            }
        }
        ui.end_row();
        ui.label(tl!("Alignment"));
        let shown_align = match target_text(app).and_then(|t| t.paragraph_runs().into_iter().next()).map(|r| r.style.align) {
            Some(photocraft_doc::text::TextAlign::Left) => "left",
            Some(photocraft_doc::text::TextAlign::Center) => "center",
            Some(photocraft_doc::text::TextAlign::Right) => "right",
            Some(_) => "",
            None if app.ui.tool == crate::state::Tool::BubbleType => "center",
            None => o.type_align.as_str(),
        }
        .to_string();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (align, icon, tip) in [
                ("left", "align-left", tl!("Left align text")),
                ("center", "align-center", tl!("Center text")),
                ("right", "align-right", tl!("Right align text")),
            ] {
                if crate::icons::button(ui, icon, 36.0, shown_align == align, tip).clicked() {
                    app.ui.tool_options.type_align = align.into();
                    apply(app, &ctx, json!({"align": align}));
                }
            }
        });
        ui.end_row();
        ui.label(tl!("Color"));
        let c = shown
            .as_ref()
            .map(|s| s.3)
            .map(|c| {
                let v = c.to_rgb();
                [v[0], v[1], v[2], 1.0]
            })
            .unwrap_or(app.session.tools.foreground);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(56.0, 30.0), egui::Sense::click());
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        ui.painter().rect_filled(rect, 6.0, Color32::from_rgb(q(c[0]), q(c[1]), q(c[2])));
        ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Set the text color")));
        if resp.clicked() {
            open_color_picker(app, [c[0], c[1], c[2]]);
        }
        ui.end_row();
        ui.label(tl!("Orientation"));
        if ui.add(egui::Button::new(tl!("Toggle text orientation")).min_size(egui::vec2(0.0, 32.0))).clicked()
            && let Some((layer, _)) = target(app)
            && let Some(params) = formatting_params(app)
        {
            let to = if is_vertical(app, LayerId(layer)) { "horizontal" } else { "vertical" };
            if let Err(e) = app.run(&format!("type.orientation.{to}"), params) {
                app.ui.status = e;
                app.ui.status_error = true;
            }
        }
        ui.end_row();
    });
    // Type in a path (bubble text): where its lines sit and its margin.
    ui.horizontal_wrapped(|ui| {
        if app.ui.tool == crate::state::Tool::BubbleType {
            bubble_target_options(app, ui);
        } else {
            in_path_options(app, ui);
        }
    });
}

pub fn options_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    // Show the target layer's (first-run) style, else the tool defaults.
    let shown = target(app).and_then(|(id, _)| {
        let st = app.session.active()?;
        let tl = text_layer(&st.doc, LayerId(id))?;
        let run = tl.char_runs().into_iter().next().map(|r| r.style);
        // The size at the selection (else the first run), as the layer shows it (#124).
        let size = styles_at(app).map_or(tl.size_pt, |(c, _)| c.size_pt) * shown_scale(app);
        Some((tl.font_family.clone(), run.as_ref().map(selected_style).unwrap_or_default(), size, styles_at(app).map_or(tl.color, |(c, _)| c.color)))
    });
    let o = app.ui.tool_options.clone();
    let (mut fam, mut style, size) = match &shown {
        Some((f, s, z, _)) => (f.clone(), if s.is_empty() { "Regular".into() } else { s.clone() }, *z),
        None => (o.type_font.clone(), o.type_style.clone(), o.type_size),
    };
    if crate::icons::button(ui, "text-cursor", 24.0, false, tl!("Toggle text orientation")).clicked()
        && let Some((layer, _)) = target(app)
        && let Some(params) = formatting_params(app)
    {
        let to = if is_vertical(app, LayerId(layer)) { "horizontal" } else { "vertical" };
        if let Err(e) = app.run(&format!("type.orientation.{to}"), params) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
    // Font family and style menus (one placeholder in their place while the fonts load).
    if fonts_ready(ui, 170.0 + ui.spacing().item_spacing.x + 110.0) {
        if let Some(picked) = font_picker(app, ui, &mut fam, 170.0) {
            app.ui.tool_options.type_font = fam.clone();
            let st = styles(&fam);
            let picked = picked.filter(|s| st.contains(s));
            style = match &picked {
                Some(s) => s.clone(),
                None if st.contains(&style) => style,
                None => st.first().cloned().unwrap_or_else(|| "Regular".into()),
            };
            app.ui.tool_options.type_style = style.clone();
            // Only a style picked under the family's ">" is set: otherwise each selected layer
            // keeps its own face in the new family (#3556).
            match picked {
                Some(s) => apply(app, ui.ctx(), json!({"font": fam, "fontStyle": s})),
                None => apply(app, ui.ctx(), json!({"font": fam})),
            }
        }
        let opts: Vec<(String, String)> = styles(&fam).into_iter().map(|s| (s.clone(), style_label(&s))).collect();
        let opts_ref: Vec<(String, &str)> = opts.iter().map(|(a, b)| (a.clone(), b.as_str())).collect();
        if crate::widgets::dropdown(ui, "type-style", tl!("Style"), &mut style, &opts_ref, 110.0) {
            app.ui.tool_options.type_style = style.clone();
            apply(app, ui.ctx(), json!({"fontStyle": style}));
        }
    }
    let (r, icon) = ui.allocate_exact_size(egui::vec2(18.0, 22.0), egui::Sense::hover());
    crate::icons::paint(ui, r, "type", 13.0, t.icon);
    icon.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, tl!("Font size")));
    // Photoshop's "T" scrubs the size field beside it, like the Character panel's icons (#6437).
    crate::widgets::scrub_option_icon(ui, &icon, tl!("Font size"));
    let (unit, label) = type_unit(app);
    let mut display = size * unit;
    if crate::widgets::value_field(ui, &mut display, unit..=1296.0 * unit, label, 66.0).changed() {
        let size = display / unit;
        app.ui.tool_options.type_size = size;
        apply(app, ui.ctx(), json!({"size": size, "metricsAsShown": true}));
    }
    let mut aa = target(app)
        .and_then(|(id, _)| app.session.active().and_then(|st| text_layer(&st.doc, LayerId(id))))
        .map(|t| photocraft_engine::type_extra_cmds::aa_name(t.antialias).to_string())
        .unwrap_or_else(|| o.type_aa.clone());
    let aa_opts = [
        ("none".to_string(), tl!("None")),
        ("sharp".to_string(), tl!("Sharp")),
        ("crisp".to_string(), tl!("Crisp")),
        ("strong".to_string(), tl!("Strong")),
        ("smooth".to_string(), tl!("Smooth")),
    ];
    if crate::widgets::dropdown(ui, "type-aa", tl!("Anti-aliasing method"), &mut aa, &aa_opts, 80.0) {
        app.ui.tool_options.type_aa = aa.clone();
        if let Some(params) = formatting_params(app) {
            let _ = app.run(&format!("type.antiAlias.{aa}"), params);
        }
    }
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 2.0;
    // Alignment and direction show the target paragraph's, as Photoshop's bar follows the text
    // being edited or the selected type layer; the tool defaults otherwise. A justified paragraph
    // lights none of the three.
    let para = styles_at(app).map(|(_, p)| p);
    let shown_align = match &para {
        Some(p) => match p.align {
            photocraft_doc::text::TextAlign::Left => "left",
            photocraft_doc::text::TextAlign::Center => "center",
            photocraft_doc::text::TextAlign::Right => "right",
            _ => "",
        },
        // Bubble Type centres its text.
        None if app.ui.tool == crate::state::Tool::BubbleType => "center",
        None => o.type_align.as_str(),
    };
    for (align, icon, tip) in
        [("left", "align-left", tl!("Left align text")), ("center", "align-center", tl!("Center text")), ("right", "align-right", tl!("Right align text"))]
    {
        if crate::icons::button(ui, icon, 24.0, shown_align == align, tip).clicked() {
            app.ui.tool_options.type_align = align.into();
            apply(app, ui.ctx(), json!({"align": align}));
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
    leading_options(app, ui);
    if app.ui.tool == crate::state::Tool::BubbleType {
        bubble_options(app, ui);
    } else {
        in_path_options(app, ui);
    }
    crate::widgets::vline(ui, 22.0);
    ui.spacing_mut().item_spacing.x = 2.0;
    for (key, tip, d) in direction_items() {
        let on = para.as_ref().map_or(o.type_direction == key, |p| p.direction == d);
        if direction_button(ui, d, on, tip).clicked() {
            app.ui.tool_options.type_direction = key.into();
            apply(app, ui.ctx(), json!({"direction": key}));
        }
    }
    ui.spacing_mut().item_spacing.x = 8.0;
    crate::widgets::vline(ui, 22.0);
    // Text colour swatch with a picker popup.
    let c = shown
        .as_ref()
        .map(|s| s.3)
        .map(|c| {
            let v = c.to_rgb();
            [v[0], v[1], v[2], 1.0]
        })
        .unwrap_or(app.session.tools.foreground);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 18.0), egui::Sense::click());
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(q(c[0]), q(c[1]), q(c[2])));
    ui.painter().rect_stroke(rect, 2.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Outside);
    let resp = resp.on_hover_text(tl!("Set the text color"));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Set the text color")));
    if resp.clicked() {
        open_color_picker(app, [c[0], c[1], c[2]]);
    }
    // Create warped text, after the swatch as in Photoshop (#5911): the Type › Warp Text… dialog.
    let can_warp = crate::menus::is_enabled(app, "type.warpText");
    if ui.add_enabled_ui(can_warp, |ui| crate::icons::button(ui, "text-warp", 24.0, false, tl!("Create warped text"))).inner.clicked()
        && let Err(e) = crate::menus::invoke(app, &ui.ctx().clone(), "type.warpText", json!({}))
    {
        app.ui.status = e;
        app.ui.status_error = true;
    }
    // Contorno: the type target's stroke (its Stroke layer effect).
    stroke_control(app, ui, false);
    // Photoshop's button at the end of the bar: shows or hides the Character and Paragraph panels.
    let shown = app.ui.panels.character;
    if crate::icons::button(ui, "panels-top-left", 24.0, shown, tl!("Toggle the Character and Paragraph panels")).clicked() {
        toggle_type_panels(app, ui.ctx());
    }
    if app.ui.text_edit.is_some() {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(8.0);
            if crate::icons::button(
                ui,
                "check",
                24.0,
                false,
                &crate::i18n::fmt(tl!("Commit any current edits ({key})"), &[("key", &crate::shortcuts::pretty("Cmd+Enter"))]),
            )
            .clicked()
            {
                commit_with(app, ui.input(|i| i.modifiers));
            }
            if crate::icons::button(ui, "ban", 24.0, false, tl!("Cancel any current edits (Esc)")).clicked() {
                cancel(app);
            }
        });
    }
}

/// Width the phone's type bar needs to fit in one row (the font field at 120 points); narrower,
/// it takes two rows ([`touch_bar_rows`]).
pub const TOUCH_BAR_ONE_ROW: f32 = 120.0 + 64.0 + 5.0 * 44.0 + 70.0 + 7.0 * 6.0;
/// The height of one of the bar's rows.
pub const TOUCH_BAR_ROW: f32 = 44.0;

/// How many rows the phone's type bar takes in `width` points: one when everything fits,
/// else two (font, size and Done; then colour, outline, alignment and line spacing).
pub fn touch_bar_rows(width: f32) -> usize {
    if width >= TOUCH_BAR_ONE_ROW { 1 } else { 2 }
}

/// The type bar above a phone's keyboard while text is edited: font, size, color, outline,
/// alignment, line spacing and Done, each a finger-sized control (touch_ui.rs places it). Every
/// control shows whatever the phone's width: on a narrow one the bar takes two rows.
pub fn touch_bar(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let w = ui.available_width();
    ui.spacing_mut().item_spacing = egui::vec2(6.0, 6.0);
    if touch_bar_rows(w) == 1 {
        ui.horizontal(|ui| {
            let extra = if app.ui.tool == crate::state::Tool::BubbleType { 50.0 } else { 0.0 };
            let font_w = (w - (TOUCH_BAR_ONE_ROW - 120.0) - extra).clamp(120.0, 220.0);
            font_and_size(app, ui, font_w);
            color_button(app, ui);
            stroke_control(app, ui, true);
            align_button(app, ui);
            touch_leading(app, ui);
            warp_button(app, ui);
            if app.ui.tool == crate::state::Tool::BubbleType {
                bubble_style_button(app, ui);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| done_button(app, ui));
        });
        return;
    }
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            // The font field takes what the size and Done leave; long names end in an ellipsis.
            let font_w = (w - 64.0 - 70.0 - 2.0 * 6.0).max(60.0);
            font_and_size(app, ui, font_w);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| done_button(app, ui));
        });
        // Five buttons (six with Bubble Type's style), evenly spaced across the width.
        let n = if app.ui.tool == crate::state::Tool::BubbleType { 6 } else { 5 };
        let (row, _) = ui.allocate_exact_size(egui::vec2(w, TOUCH_BAR_ROW), egui::Sense::hover());
        let slot = w / n as f32;
        for i in 0..n {
            let c = egui::pos2(row.left() + slot * (i as f32 + 0.5), row.center().y);
            let r = egui::Rect::from_center_size(c, egui::vec2(TOUCH_BAR_ROW, TOUCH_BAR_ROW));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::left_to_right(egui::Align::Center)));
            match i {
                0 => color_button(app, &mut child),
                1 => stroke_control(app, &mut child, true),
                2 => align_button(app, &mut child),
                3 => touch_leading(app, &mut child),
                4 => warp_button(app, &mut child),
                _ => bubble_style_button(app, &mut child),
            }
        }
    });
}

/// The type bar's Done.
fn done_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(70.0, 44.0), egui::Sense::click());
    ui.painter().rect_filled(rect.shrink2(egui::vec2(0.0, 4.0)), egui::CornerRadius::same(18), t.accent);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tl!("Done"), crate::theme::semibold(14.0), Color32::WHITE);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Done")));
    crate::touch_ui::note_rect(ui.ctx(), "typeDone", rect);
    if resp.clicked() {
        commit(app);
    }
}

/// The type target's font, size and colour as the bar shows them.
fn touch_shown(app: &PhotocraftApp) -> Option<(String, f32, photocraft_color::Color)> {
    let (id, _) = target(app)?;
    let st = app.session.active()?;
    let tl = text_layer(&st.doc, LayerId(id))?;
    let size = styles_at(app).map_or(tl.size_pt, |(c, _)| c.size_pt) * shown_scale(app);
    Some((tl.font_family.clone(), size, styles_at(app).map_or(tl.color, |(c, _)| c.color)))
}

/// The bar's font field (`font_w` wide) and size field.
fn font_and_size(app: &mut PhotocraftApp, ui: &mut egui::Ui, font_w: f32) {
    let shown = touch_shown(app);
    let o = &app.ui.tool_options;
    let (mut fam, size) = match &shown {
        Some((f, z, _)) => (f.clone(), *z),
        None => (o.type_font.clone(), o.type_size),
    };
    // Each in a finger-sized slot (44 points tall): the font menu's button fills it, a tap
    // anywhere on the size's slot edits the size.
    let slot = |ui: &mut egui::Ui, w: f32| {
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, TOUCH_BAR_ROW), egui::Sense::click());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        child.spacing_mut().interact_size.y = TOUCH_BAR_ROW;
        (rect, resp, child)
    };
    let (rect, _, mut child) = slot(ui, font_w);
    if fonts_ready(&mut child, font_w) && font_picker(app, &mut child, &mut fam, font_w).is_some() {
        app.ui.tool_options.type_font = fam.clone();
        apply(app, ui.ctx(), json!({"font": fam}));
    }
    crate::touch_ui::note_rect(ui.ctx(), "typeFont", rect);
    let (unit, label) = type_unit(app);
    let mut display = size * unit;
    let (rect, tap, mut child) = slot(ui, 64.0);
    let r = crate::widgets::value_field(&mut child, &mut display, unit..=1296.0 * unit, label, 64.0).accessible_name(tl!("Font size"));
    if tap.clicked() {
        ui.memory_mut(|m| m.request_focus(r.id));
    }
    crate::touch_ui::note_rect(ui.ctx(), "typeSize", rect);
    if r.changed() {
        let size = display / unit;
        app.ui.tool_options.type_size = size;
        apply(app, ui.ctx(), json!({"size": size, "metricsAsShown": true}));
    }
}

/// The bar's text colour swatch.
fn color_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let c = touch_shown(app)
        .map(|s| {
            let v = s.2.to_rgb();
            [v[0], v[1], v[2], 1.0]
        })
        .unwrap_or(app.session.tools.foreground);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(44.0, 44.0), egui::Sense::click());
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    ui.painter().circle(rect.center(), 15.0, Color32::from_rgb(q(c[0]), q(c[1]), q(c[2])), Stroke::new(1.5, t.card_border));
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, tl!("Set the text color")));
    crate::touch_ui::note_rect(ui.ctx(), "typeColor", rect);
    if resp.clicked() {
        open_color_picker(app, [c[0], c[1], c[2]]);
    }
}

/// The bar's Warp Text… (Arc, Arch, Flag…: the type's warp, with its preview).
/// The bar's Bubble Type style: opens the style picker as a sheet.
fn bubble_style_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let r = crate::icons::button(ui, "type-bubble", 44.0, crate::bubble_type_ui::picker_open(ui.ctx()), tl!("Bubble style"));
    crate::touch_ui::note_rect(ui.ctx(), "typeBubbleStyle", r.rect);
    if r.clicked() {
        crate::bubble_type_ui::open_picker(ui.ctx(), r.rect);
    }
    let _ = app;
}

pub(crate) fn warp_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let r = crate::icons::button(ui, "spline", 44.0, false, tl!("Warp"));
    crate::touch_ui::note_rect(ui.ctx(), "typeWarp", r.rect);
    if r.clicked() {
        let ctx = ui.ctx().clone();
        if let Err(e) = crate::menus::invoke(app, &ctx, "type.warpText", json!({})) {
            app.ui.status = e;
            app.ui.status_error = true;
        }
    }
}

/// The bar's alignment: one button that steps left → centre → right.
fn align_button(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let (next, icon) = match app.ui.tool_options.type_align.as_str() {
        "center" => ("right", "align-center"),
        "right" => ("left", "align-right"),
        _ => ("center", "align-left"),
    };
    let r = crate::icons::button(ui, icon, 44.0, false, tl!("Paragraph alignment"));
    crate::touch_ui::note_rect(ui.ctx(), "typeAlign", r.rect);
    if r.clicked() {
        app.ui.tool_options.type_align = next.into();
        apply(app, ui.ctx(), json!({"align": next}));
    }
}

/// Character and paragraph style at the target (selection start, else the layer's first run).
/// The paragraph at a collapsed caret is the caret's, as Photoshop's options bar and Paragraph
/// panel show it.
pub(crate) fn styles_at(app: &PhotocraftApp) -> Option<(photocraft_doc::text::CharStyle, photocraft_doc::text::ParagraphStyle)> {
    let (layer, range) = target(app)?;
    let t = text_layer(&target_doc(app)?.doc, LayerId(layer))?;
    let at = range.map_or(0, |r| byte_of(&t.text, r[0]));
    let para_at = match (range, &app.ui.text_edit) {
        (None, Some(ed)) => byte_of(&t.text, ed.caret),
        _ => at,
    };
    let pick_at = |at: usize, lens: Vec<usize>| -> usize {
        let mut acc = 0;
        for (i, len) in lens.iter().enumerate() {
            acc += len;
            if at < acc {
                return i;
            }
        }
        lens.len().saturating_sub(1)
    };
    let runs = t.char_runs();
    let paras = t.paragraph_runs();
    let c = runs.get(pick_at(at, runs.iter().map(|r| r.len).collect())).map(|r| r.style.clone())?;
    let p = paras.get(pick_at(para_at, paras.iter().map(|r| r.len).collect())).map(|r| r.style.clone()).unwrap_or_default();
    Some((c, p))
}

/// Kerning shown for the target: at a collapsed caret, the pair before it (Photoshop's
/// Character panel); else the style at the selection.
pub(super) fn kerning_at(app: &PhotocraftApp) -> Option<(photocraft_doc::text::Kerning, f32)> {
    let ed = app.ui.text_edit.as_ref().filter(|e| e.caret == e.anchor && e.caret > 0)?;
    let st = app.session.active()?;
    let t = text_layer(&st.doc, LayerId(ed.layer))?;
    let at = byte_of(&t.text, ed.caret - 1);
    let mut end = 0;
    t.char_runs()
        .into_iter()
        .find(|r| {
            end += r.len;
            at < end
        })
        .map(|r| (r.style.kerning, r.style.kern))
}

/// The Character panel's kerning text: "Metrics", "Optical" or the manual value.
pub(crate) fn kerning_label((mode, kern): (photocraft_doc::text::Kerning, f32)) -> String {
    use photocraft_doc::text::Kerning;
    match mode {
        _ if kern != 0.0 && kern.is_finite() => format!("{}", kern.round() as i64),
        Kerning::Metrics => "Metrics".into(),
        Kerning::Optical => "Optical".into(),
        Kerning::Off => "0".into(),
    }
}

/// Parses a typed kerning value: Metrics / Optical (any case) or a number in 1/1000 em
/// (Photoshop's range, -1000..10000). `None` for anything else.
pub(crate) fn parse_kerning(s: &str) -> Option<serde_json::Value> {
    let s = s.trim();
    match s.to_ascii_lowercase().as_str() {
        "metrics" => Some(json!("metrics")),
        "optical" => Some(json!("optical")),
        _ => crate::numeric_expression::parse(s).filter(|v| v.is_finite() && (-1000.0..=10000.0).contains(v)).map(|v| json!(v.round())),
    }
}

/// Photoshop's editable kerning combo: type a value or Metrics/Optical, or pick a preset.
pub(super) fn kerning_field(ui: &mut egui::Ui, shown: &str, width: f32) -> Option<serde_json::Value> {
    let id = ui.make_persistent_id("props-kern-text");
    let mut buf: String = ui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| shown.to_string());
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let resp = ui.add(egui::TextEdit::singleline(&mut buf).desired_width((width - 40.0).max(24.0)).id(id.with("edit"))).accessible_name(tl!("Kerning"));
        if resp.has_focus() {
            ui.data_mut(|d| d.insert_temp(id, buf.clone()));
        } else {
            ui.data_mut(|d| d.remove::<String>(id));
        }
        if resp.lost_focus() && buf.trim() != shown {
            out = parse_kerning(&buf);
        }
        let presets = ["Metrics", "Optical", "0", "-100", "-75", "-50", "-25", "-10", "-5", "5", "10", "25", "50", "75", "100", "200"];
        let (press, close) = crate::press_menu::PressCombo::before(ui, "props-kern-presets");
        let combo = craft_ui::ComboBox::from_id_salt("props-kern-presets")
            .selected_text("")
            .width(16.0)
            .height(420.0)
            .icon(crate::widgets::chevron_icon)
            .close_behavior(close)
            .show_ui(ui, |ui| {
                for p in presets {
                    let item = ui.selectable_label(p == shown, p);
                    if press.chosen(ui, &item) {
                        out = parse_kerning(p);
                    }
                }
            });
        press.after(&combo.response);
        let _ = combo.response.accessible_name(tl!("Kerning"));
    });
    out
}

/// Applies a kerning value: at a collapsed caret to the pair before it, else to the selection
/// (or the whole layer).
pub(super) fn apply_kerning(app: &mut PhotocraftApp, ctx: &egui::Context, v: serde_json::Value) {
    match app.ui.text_edit.clone().filter(|e| e.caret == e.anchor) {
        Some(ed) if ed.caret > 0 => {
            let _ = app.run("type.edit", json!({"layer": ed.layer, "range": [ed.caret - 1, ed.caret], "kerning": v}));
        }
        Some(_) => {}
        None => apply(app, ctx, json!({ "kerning": v })),
    }
}

pub(super) fn icon_label(ui: &mut egui::Ui, icon: &str, tip: &str) {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(egui::vec2(crate::props_layout::LABEL_W, 22.0), egui::Sense::hover());
    crate::icons::paint(ui, r, icon, 12.0, t.text_dim);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), tl!(tip)));
    // Photoshop's Character panel icons scrub the field beside them.
    crate::widgets::scrub_label(ui, &resp.on_hover_text(tl!(tip)));
}

/// Labelled numeric field (Photoshop's icon + value pairs). Returns the new value when edited.
pub(super) fn num_field(ui: &mut egui::Ui, label: &str, tip: &str, v: f32, range: std::ops::RangeInclusive<f32>, unit: &str, width: f32) -> Option<f32> {
    let t = crate::theme::Tokens::get(ui.ctx());
    let mut v = v;
    let mut out = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let (r, resp) = ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
        ui.painter().text(r.center(), egui::Align2::CENTER_CENTER, label, crate::theme::semibold(10.0), t.text_dim);
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, ui.is_enabled(), tl!(tip)));
        crate::widgets::scrub_label(ui, &resp.on_hover_text(tl!(tip)));
        let response = crate::widgets::value_field(ui, &mut v, range, unit, width);
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::drag_value(ui.is_enabled(), f64::from(v));
            info.label = Some(tl!(tip).to_owned());
            info
        });
        if response.changed() {
            out = Some(v);
        }
    });
    out
}

/// Photoshop's paragraph alignment glyphs: four text lines whose widths/offsets show the mode
/// (justify variants run full width, with the last line placed left/centre/right).
pub(super) fn align_glyph(p: &egui::Painter, r: egui::Rect, a: photocraft_doc::text::TextAlign, c: Color32) {
    use photocraft_doc::text::TextAlign as A;
    let widths = [1.0, 0.62, 1.0, 0.62];
    for (i, w) in widths.iter().enumerate() {
        let y = r.top() + i as f32 * r.height() / 3.0;
        let full = r.width();
        let (len, x0) = match a {
            A::Left => (full * w, r.left()),
            A::Center => (full * w, r.center().x - full * w / 2.0),
            A::Right => (full * w, r.right() - full * w),
            A::JustifyAll => (full, r.left()),
            _ if i < 3 => (full, r.left()),
            A::JustifyLeft => (full * 0.55, r.left()),
            A::JustifyCenter => (full * 0.55, r.center().x - full * 0.275),
            _ => (full * 0.55, r.right() - full * 0.55),
        };
        p.line_segment([egui::pos2(x0, y), egui::pos2(x0 + len, y)], Stroke::new(1.5, c));
    }
}

/// Paragraph direction choices for the Paragraph panel and the options bar (TYPE-218-2): the
/// `direction` value `type.setStyle` takes, the tooltip, and the direction it sets.
pub(super) fn direction_items() -> [(&'static str, &'static str, photocraft_doc::text::TextDirection); 3] {
    use photocraft_doc::text::TextDirection as D;
    [
        ("auto", tl!("Automatic paragraph direction"), D::Auto),
        ("ltr", tl!("Left-to-right paragraph direction"), D::Ltr),
        ("rtl", tl!("Right-to-left paragraph direction"), D::Rtl),
    ]
}

/// A pilcrow over an arrow pointing the way the paragraph runs; Auto points both ways.
pub(super) fn direction_glyph(p: &egui::Painter, r: egui::Rect, d: photocraft_doc::text::TextDirection, c: Color32) {
    use photocraft_doc::text::TextDirection as D;
    let g = p.layout_no_wrap("¶".to_string(), egui::FontId::proportional(11.0), c);
    p.galley(egui::pos2(r.center().x - g.size().x / 2.0, r.top() - 2.0), g, c);
    let (y, s) = (r.bottom() - 1.5, Stroke::new(1.3, c));
    p.line_segment([egui::pos2(r.left(), y), egui::pos2(r.right(), y)], s);
    let head = |x: f32, towards: f32| {
        for dy in [-3.0, 3.0] {
            p.line_segment([egui::pos2(x, y), egui::pos2(x - towards * 3.0, y + dy)], s);
        }
    };
    if d != D::Rtl {
        head(r.right(), 1.0);
    }
    if d != D::Ltr {
        head(r.left(), -1.0);
    }
}

/// The options bar's 24 px square button for one paragraph direction, styled like its icon buttons.
fn direction_button(ui: &mut egui::Ui, d: photocraft_doc::text::TextDirection, selected: bool, tip: &str) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
    let tint = crate::icons::button_chrome(ui, r, selected, resp.hovered());
    direction_glyph(ui.painter(), egui::Rect::from_center_size(r.center(), egui::vec2(14.0, 16.0)), d, tint);
    let tip = tip.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, selected, &tip));
    resp.on_hover_text(tip)
}

/// A toggle button (faux styles, paragraph alignment) of width `w`, painted by `paint`.
pub(super) fn toggle_cell(ui: &mut egui::Ui, w: f32, on: bool, tip: &str, paint: impl FnOnce(&egui::Ui, egui::Rect, Color32)) -> bool {
    let t = crate::theme::Tokens::get(ui.ctx());
    let (r, resp) = ui.allocate_exact_size(egui::vec2(w, 24.0), egui::Sense::click());
    let bg = if on {
        t.accent_soft
    } else if resp.hovered() {
        t.hover
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(r, t.radius_sm, bg);
    if on {
        ui.painter().rect_stroke(r, t.radius_sm, Stroke::new(1.0, t.accent_border), egui::StrokeKind::Inside);
    }
    let enabled = ui.is_enabled();
    paint(
        ui,
        r,
        if !enabled {
            t.text_faint
        } else if on {
            t.text
        } else {
            t.text_dim
        },
    );
    let tip = tip.to_string();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, on, &tip));
    resp.on_hover_text(tl!(&tip)).clicked()
}
