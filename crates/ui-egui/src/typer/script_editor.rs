//! The script editor (TypeR `textBlock.jsx`): the translated chapter as editable text with a
//! gutter of line numbers. Page markers, ignored lines, style tags (chips in the tag colour),
//! ignore tags and Markdown markers are highlighted, and so is the current line. A click in the
//! gutter makes that line current; ⌘B / ⌘I or the toolbar toggle Markdown bold / italic around
//! the selection; a `.txt` (or `.md`) file can be imported. Every edit is `typer.script.set`.

use egui::text::{CCursor, CCursorRange, LayoutJob, TextFormat};
use egui::{Color32, FontId, Key, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use photocraft_typer::markdown::{self, Format};
use serde_json::json;

use crate::PhotocraftApp;
use crate::theme::Tokens;

/// Width of the line-number gutter (points).
const GUTTER: f32 = 34.0;
const FONT_SIZE: f32 = 12.5;

/// The editor's widget id (its `TextEdit`).
pub fn editor_id() -> egui::Id {
    egui::Id::unique("typer-script-editor")
}

fn buffer_id() -> egui::Id {
    egui::Id::unique("typer-script-buffer")
}

fn pending_id() -> egui::Id {
    egui::Id::unique("typer-script-format")
}

fn scroll_id() -> egui::Id {
    egui::Id::unique("typer-script-scroll-to")
}

fn last_line_id() -> egui::Id {
    egui::Id::unique("typer-script-last-line")
}

fn layout_id() -> egui::Id {
    egui::Id::unique("typer-script-layout")
}

/// The gutter and each raw line's rect as drawn last frame (screen points), for tests and
/// automation.
pub fn last_layout(ctx: &egui::Context) -> Option<(Rect, Vec<Rect>)> {
    ctx.data(|d| d.get_temp::<(Rect, Vec<Rect>)>(layout_id()))
}

/// Scroll the editor to raw line `raw` on its next frame.
pub fn scroll_to(ctx: &egui::Context, raw: usize) {
    ctx.data_mut(|d| d.insert_temp(scroll_id(), raw));
}

/// With the editor focused, ⌘B and ⌘I toggle Markdown bold / italic instead of reaching the menus
/// (Image › Adjustments › Color Balance / Invert). Called before the shortcuts are dispatched.
pub fn claim_format_keys(ctx: &egui::Context) {
    if ctx.memory(|m| m.focused()) != Some(editor_id()) {
        return;
    }
    let mut pending = None;
    ctx.input_mut(|i| {
        i.events.retain(|e| match e {
            egui::Event::Key { key: k @ (Key::B | Key::I), pressed: true, modifiers: m, .. } if m.command && !m.alt && !m.shift => {
                pending = Some(if *k == Key::B { Format::Bold } else { Format::Italic });
                false
            }
            _ => true,
        })
    });
    if let Some(f) = pending {
        ctx.data_mut(|d| d.insert_temp(pending_id(), f));
    }
}

/// What the layouter and gutter need to know about one raw line.
#[derive(Clone)]
struct LineView {
    ignore: bool,
    /// Byte length of the style tag (`SFX:`, `//`) the line starts with.
    style_prefix: usize,
    /// The tag chip colour, `None` for `//` continuations.
    chip: Option<Color32>,
    page: Option<u32>,
    number: u32,
}

fn views(app: &PhotocraftApp) -> Vec<LineView> {
    let state = &app.session.typer.state;
    state
        .lines()
        .iter()
        .map(|l| {
            let chip = if l.style_prefix.starts_with("//") {
                None
            } else {
                l.style
                    .as_ref()
                    .and_then(|id| state.library.style(id))
                    .and_then(|s| super::hex_color(s.prefix_color()))
                    .or(Some(Color32::from_rgb(0xFF, 0xF3, 0xB0)))
            };
            LineView {
                ignore: l.ignore,
                style_prefix: if l.raw_text.starts_with(&l.style_prefix) { l.style_prefix.len() } else { 0 },
                chip,
                page: l.page,
                number: l.index,
            }
        })
        .collect()
}

/// The highlighted layout of the script (the galley must hold exactly `text`).
fn layout_job(text: &str, views: &[LineView], t: &Tokens, wrap: f32, markdown: bool, tags: &[String]) -> LayoutJob {
    let font = FontId::proportional(FONT_SIZE);
    let fmt = |color: Color32, background: Color32| TextFormat { font_id: font.clone(), color, background, ..Default::default() };
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    for (i, raw) in text.split('\n').enumerate() {
        if i > 0 {
            job.append("\n", 0.0, fmt(t.text, Color32::TRANSPARENT));
        }
        let Some(v) = views.get(i) else {
            job.append(raw, 0.0, fmt(t.text, Color32::TRANSPARENT));
            continue;
        };
        if v.page.is_some() {
            job.append(raw, 0.0, TextFormat { font_id: crate::theme::semibold(FONT_SIZE), color: t.accent_text, ..Default::default() });
            continue;
        }
        if v.ignore {
            job.append(raw, 0.0, fmt(t.text_faint, Color32::TRANSPARENT));
            continue;
        }
        let cut = if raw.is_char_boundary(v.style_prefix) { v.style_prefix } else { 0 };
        let (tag, rest) = raw.split_at(cut);
        if !tag.is_empty() {
            match v.chip {
                Some(c) => job.append(tag, 0.0, fmt(Color32::from_gray(28), c)),
                None => job.append(tag, 0.0, fmt(t.text_faint, Color32::TRANSPARENT)),
            }
        }
        append_body(&mut job, rest, &fmt(t.text, Color32::TRANSPARENT), &fmt(t.text_faint, Color32::TRANSPARENT), markdown, tags);
    }
    job
}

/// A line's text with ignore tags and Markdown markers dimmed.
fn append_body(job: &mut LayoutJob, s: &str, normal: &TextFormat, dim: &TextFormat, markdown: bool, tags: &[String]) {
    let mut plain_from = 0;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        let tag = tags.iter().filter(|t| !t.is_empty()).find(|t| rest.starts_with(t.as_str())).map(String::len);
        let marker = (markdown && (rest.starts_with('*') || rest.starts_with('_'))).then_some(1);
        if let Some(n) = tag.or(marker) {
            job.append(&s[plain_from..i], 0.0, normal.clone());
            job.append(&s[i..i + n], 0.0, dim.clone());
            i += n;
            plain_from = i;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    job.append(&s[plain_from..], 0.0, normal.clone());
}

/// Screen rect of each raw line (all its wrapped rows), in order.
fn line_rects(galley: &egui::Galley, origin: egui::Pos2) -> Vec<Rect> {
    let mut out: Vec<Rect> = Vec::new();
    let mut current: Option<Rect> = None;
    for row in &galley.rows {
        let r = row.rect().translate(origin.to_vec2());
        current = Some(current.map_or(r, |c| c.union(r)));
        if row.ends_with_newline
            && let Some(c) = current.take()
        {
            out.push(c);
        }
    }
    if let Some(c) = current {
        out.push(c);
    }
    out
}

/// Toggle bold / italic around the editor's selection.
fn apply_format(app: &mut PhotocraftApp, ctx: &egui::Context, buf: &mut String, f: Format) {
    let Some(mut state) = egui::TextEdit::load_state(ctx, editor_id()) else { return };
    let Some(range) = state.cursor.char_range() else { return };
    let r = range.as_sorted_char_range();
    let out = markdown::format_selection(buf, r.start.0, r.end.0, f);
    if out.text != *buf && super::run(app, "typer.script.set", json!({"text": out.text})).is_ok() {
        buf.clone_from(&app.session.typer.state.text);
        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(out.selection_start), CCursor::new(out.selection_end))));
        state.store(ctx, editor_id());
    }
    ctx.memory_mut(|m| m.request_focus(editor_id()));
}

pub(super) fn show(app: &mut PhotocraftApp, ui: &mut egui::Ui, height: f32) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    // Toolbar: fold · B / I · import · line count.
    let folded = super::is_folded(app, "script");
    ui.horizontal(|ui| {
        super::fold_button(app, ui, "script");
        if folded {
            ui.label(RichText::new(tl!("Script")).color(t.text_dim).size(11.5));
        }
        if crate::icons::button_with_icon_size(ui, "bold", 22.0, 15.0, false, &format!("{} ({})", tl!("Bold"), crate::shortcuts::pretty("Cmd+B"))).clicked() {
            ctx.data_mut(|d| d.insert_temp(pending_id(), Format::Bold));
        }
        if crate::icons::button_with_icon_size(ui, "italic", 22.0, 15.0, false, &format!("{} ({})", tl!("Italic"), crate::shortcuts::pretty("Cmd+I"))).clicked()
        {
            ctx.data_mut(|d| d.insert_temp(pending_id(), Format::Italic));
        }
        crate::widgets::vline(ui, 16.0);
        if crate::icons::button_with_icon_size(ui, "file-text", 22.0, 15.0, false, tl!("Import a script (.txt)…")).clicked() {
            let r = app.pick_file_bytes_filtered(&["txt", "md"], |app, _name, bytes| {
                let text = String::from_utf8_lossy(&bytes).into_owned();
                super::run(app, "typer.script.set", json!({"text": text}))
            });
            if let Err(e) = r {
                app.ui.typer.message = e;
            }
        }
        let usable = app.session.typer.state.lines().iter().filter(|l| !l.ignore).count();
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let n = crate::i18n::trn(crate::i18n::current(), usable as u64, "{n} line", "{n} lines");
            ui.label(RichText::new(n).color(t.text_faint).size(11.0));
        });
    });

    if folded {
        return;
    }
    // The text, taken out of the frame's memory while it is edited (no per-frame copy).
    let mut buf: String = ctx.data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<String>(buffer_id())));
    if buf != app.session.typer.state.text {
        buf.clone_from(&app.session.typer.state.text);
    }
    let pending = ctx.data_mut(|d| {
        let f = d.get_temp::<Format>(pending_id());
        d.remove::<Format>(pending_id());
        f
    });
    if let Some(f) = pending {
        apply_format(app, &ctx, &mut buf, f);
    }
    let views = views(app);
    let settings = &app.session.typer.state.settings;
    let (markdown, tags) = (settings.interpret_markdown, settings.ignore_tags.clone());
    let current = app.session.typer.state.current_line().map(|l| l.raw_index);
    let mut clicked_line = None;
    let mut changed = false;
    let frame = egui::Frame::NONE.fill(t.field).stroke(Stroke::new(1.0, t.field_border)).corner_radius(t.radius_sm);
    frame.show(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("typer-script-scroll").min_scrolled_height(height).max_height(height).auto_shrink([false, false]).show(ui, |ui| {
            let highlight = ui.painter().add(egui::Shape::Noop);
            let mut layouter = |ui: &egui::Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                let job = layout_job(text.as_str(), &views, &t, wrap, markdown, &tags);
                ui.ctx().fonts_mut(|f| f.layout_job(job))
            };
            let out = egui::TextEdit::multiline(&mut buf)
                .id(editor_id())
                // The margin leaves room for the gutter (a custom frame replaces `margin`).
                .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: (GUTTER + 4.0) as i8, right: 4, top: 4, bottom: 4 }))
                .desired_width(f32::INFINITY)
                .desired_rows(((height - 8.0) / 16.0).max(3.0) as usize)
                .lock_focus(true)
                .hint_text(tl!(
                    "Paste the translated script here: one line per bubble, \"Page 1\" lines mark pages, \"##\" lines are notes, \"TAG:\" picks a style"
                ))
                .layouter(&mut layouter)
                .show(ui);
            changed = out.response.changed();
            let area = out.response.rect;
            let rects = line_rects(&out.galley, out.galley_pos);
            // The current line, full width, under the text.
            if let Some(r) = current.and_then(|c| rects.get(c)) {
                let row = Rect::from_min_max(pos2(area.left() + 1.0, r.top()), pos2(area.right() - 1.0, r.bottom()));
                ui.painter().set(highlight, egui::Shape::rect_filled(row, 2.0, t.accent_soft));
            }
            // The gutter: numbers, page markers, the current line's marker.
            let gutter = Rect::from_min_max(area.left_top(), pos2(area.left() + GUTTER, area.bottom()));
            ui.painter().line_segment([gutter.right_top(), gutter.right_bottom()], Stroke::new(1.0, t.separator));
            let small = FontId::monospace(10.5);
            for (i, r) in rects.iter().enumerate() {
                let Some(v) = views.get(i) else { break };
                let y = r.top() + (FONT_SIZE + 3.0) / 2.0;
                let at = pos2(gutter.right() - 5.0, y);
                let is_current = current == Some(i);
                if let Some(p) = v.page {
                    ui.painter().text(at, egui::Align2::RIGHT_CENTER, format!("P{p}"), small.clone(), t.accent_text);
                } else if v.number > 0 {
                    let color = if is_current { t.accent_text } else { t.text_faint };
                    ui.painter().text(at, egui::Align2::RIGHT_CENTER, v.number.to_string(), small.clone(), color);
                }
                if is_current {
                    let m = Rect::from_center_size(pos2(gutter.left() + 5.0, y), vec2(3.0, (r.height() - 4.0).max(6.0)));
                    ui.painter().rect_filled(m, 1.5, t.accent);
                }
            }
            ctx.data_mut(|d| d.insert_temp(layout_id(), (gutter, rects.clone())));
            let g = ui.interact(gutter, editor_id().with("gutter"), Sense::click()).on_hover_text(tl!("Make this line current"));
            if g.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                if let Some(p) = g.hover_pos()
                    && let Some(r) = rects.iter().find(|r| p.y >= r.top() && p.y < r.bottom())
                {
                    ui.painter().rect_stroke(
                        Rect::from_min_max(pos2(gutter.left() + 1.0, r.top()), pos2(gutter.right() - 1.0, r.bottom())),
                        2.0,
                        Stroke::new(1.0, t.accent_border),
                        StrokeKind::Inside,
                    );
                }
            }
            if g.clicked()
                && let Some(p) = g.interact_pointer_pos()
            {
                clicked_line = rects.iter().position(|r| p.y >= r.top() && p.y < r.bottom());
            }
            // Keep the current line in view when it moves, and scroll where asked.
            let last = ctx.data(|d| d.get_temp::<Option<usize>>(last_line_id())).flatten();
            let asked = ctx.data_mut(|d| d.remove_temp::<usize>(scroll_id()));
            let target = asked.or(if last != current { current } else { None });
            if let Some(r) = target.and_then(|i| rects.get(i)) {
                ui.scroll_to_rect(r.expand2(vec2(0.0, 20.0)), Some(egui::Align::Center));
            }
            ctx.data_mut(|d| d.insert_temp(last_line_id(), current));
        });
    });
    if changed {
        let _ = super::run(app, "typer.script.set", json!({"text": buf}));
    }
    ctx.data_mut(|d| d.insert_temp(buffer_id(), buf));
    // A page marker or a note in the gutter: the next usable line (the page's first).
    if let Some(i) = clicked_line {
        let lines = app.session.typer.state.lines();
        let i = if lines.get(i).is_some_and(|l| l.ignore) { photocraft_typer::script::next_usable(lines, i).unwrap_or(i) } else { i };
        super::act(app, "typer.line.set", json!({"index": i}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_highlight_keeps_every_byte() {
        let mut job = LayoutJob::default();
        let (f, dim) = (TextFormat::default(), TextFormat { color: Color32::RED, ..Default::default() });
        append_body(&mut job, "a **b** _c_ ¡ñ! [x]", &f, &dim, true, &["[x]".to_string()]);
        assert_eq!(job.text, "a **b** _c_ ¡ñ! [x]");
        assert!(job.sections.len() > 4, "markers and tags are their own sections");
    }
}
