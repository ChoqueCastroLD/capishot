//! The tool toast: when a tool letter picks or cycles a tool (or, on a phone, a bar picks one), a
//! pill at the bottom centre shows the group's previous tool, the current one (its icon and name,
//! highlighted) and the next: "◂ Remove with AI · **Bubble Text Remover** · Spot Healing ▸".
//!
//! It fades and slides up as it appears, slides sideways with a crossfade when the tool changes
//! again while it shows (the way the change went: next or previous), and fades out after
//! [`IDLE_SECS`] without a change. With Enable Animations off it appears and goes at once. It never
//! takes input (a painter layer, not an area), so it never blocks the canvas. On a phone it is
//! shorter (the neighbours as icons only) and sits above the bars, clear of the other floating
//! controls.

use egui::{Color32, CornerRadius, FontId, Id, Order, Rect, Stroke, Vec2, pos2, vec2};

use crate::PhotocraftApp;
use crate::state::Tool;
use crate::theme::Tokens;

/// Seconds without a change before the toast fades out.
pub const IDLE_SECS: f64 = 1.4;
/// How long the appearance, the sideways change and the fade out take (seconds).
const APPEAR: f64 = 0.22;
const CHANGE: f64 = 0.2;
const FADE: f64 = 0.28;
/// How far (points) it slides up as it appears, and sideways on a change.
const RISE: f32 = 14.0;
const SLIDE: f32 = 28.0;

/// The group's previous, current and next tool (the neighbours `None` in a group of one).
pub type Trio = [Option<Tool>; 3];

#[derive(Clone, Copy, Debug, PartialEq)]
struct Toast {
    now: Trio,
    /// What showed before the last change, crossfading out.
    before: Option<Trio>,
    /// +1 when the change went to the next tool, -1 to the previous.
    dir: f32,
    /// When it appeared and when it last changed (input time).
    shown: f64,
    changed: f64,
}

fn id() -> Id {
    Id::unique("tool-toast")
}

/// The previous, current and next tool of `group` around `current`, wrapping.
pub fn trio(group: &[Tool], current: Tool) -> Trio {
    let Some(i) = group.iter().position(|t| *t == current) else { return [None, Some(current), None] };
    let n = group.len();
    if n < 2 {
        return [None, Some(current), None];
    }
    [Some(group[(i + n - 1) % n]), Some(current), Some(group[(i + 1) % n])]
}

/// Show the toast for `current` in `group`, the change going `dir` (+1 next, -1 previous).
pub fn show(ctx: &egui::Context, group: &[Tool], current: Tool, dir: i32) {
    let now = ctx.input(|i| i.time);
    let t = trio(group, current);
    ctx.data_mut(|d| {
        let old: Option<Toast> = d.get_temp(id());
        let visible = old.is_some_and(|o| opacity_at(&o, now, false) > 0.0);
        let toast = match old {
            Some(o) if visible => Toast { now: t, before: (o.now != t).then_some(o.now), dir: dir.signum() as f32, shown: o.shown, changed: now },
            _ => Toast { now: t, before: None, dir: dir.signum() as f32, shown: now, changed: now },
        };
        d.insert_temp(id(), toast);
    });
    ctx.request_repaint();
}

/// Show the toast for a tool picked some other way (a phone's bars): its letter's group.
pub fn show_picked(app: &PhotocraftApp, ctx: &egui::Context, tool: Tool) {
    let group = if tool.key() == '\0' { vec![tool] } else { crate::shortcuts::group_of(app, tool.key()) };
    let group = if group.contains(&tool) { group } else { vec![tool] };
    show(ctx, &group, tool, 1);
}

fn ease_out(x: f64) -> f32 {
    let x = x.clamp(0.0, 1.0);
    (1.0 - (1.0 - x).powi(3)) as f32
}

fn opacity_at(t: &Toast, now: f64, instant: bool) -> f32 {
    let idle = now - t.changed;
    let fade = if idle <= IDLE_SECS {
        1.0
    } else if instant {
        0.0
    } else {
        1.0 - ease_out((idle - IDLE_SECS) / FADE)
    };
    let appear = if instant { 1.0 } else { ease_out((now - t.shown) / APPEAR) };
    (appear * fade).clamp(0.0, 1.0)
}

/// What the toast looks like now, for tests and `ui.inspect`: the tools, its opacity, how far it
/// still sits below its place, the sideways change's progress (0..1) and direction.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct View {
    pub prev: Option<String>,
    pub current: Option<String>,
    pub next: Option<String>,
    pub opacity: f32,
    pub rise: f32,
    pub change: f32,
    pub dir: f32,
    pub changing_from: Option<String>,
}

/// The toast as it shows now (`None` once gone).
pub fn view(ctx: &egui::Context) -> Option<View> {
    let t: Toast = ctx.data(|d| d.get_temp(id()))?;
    let now = ctx.input(|i| i.time);
    let instant = ctx.global_style().animation_time <= 0.0;
    let opacity = opacity_at(&t, now, instant);
    if opacity <= 0.0 && now - t.changed > IDLE_SECS {
        return None;
    }
    let appear = if instant { 1.0 } else { ease_out((now - t.shown) / APPEAR) };
    let change = if instant || t.before.is_none() { 1.0 } else { ease_out((now - t.changed) / CHANGE) };
    let name = |x: Option<Tool>| x.map(|t| t.label().to_string());
    Some(View {
        prev: name(t.now[0]),
        current: name(t.now[1]),
        next: name(t.now[2]),
        opacity,
        rise: (1.0 - appear) * RISE,
        change,
        dir: t.dir,
        changing_from: t.before.and_then(|b| b[1]).filter(|_| change < 1.0).map(|t| t.label().to_string()),
    })
}

/// The tool's name as shown (translated, without " Tool").
fn shown_name(t: Tool) -> String {
    let s = crate::i18n::t(t.label()).to_string();
    // "Herramienta Quitar…" / "Ferramenta Remover…" / "Remove … Tool": the tool's own words.
    for p in ["Herramienta ", "Ferramenta "] {
        if let Some(r) = s.strip_prefix(p) {
            return r.to_string();
        }
    }
    s.strip_suffix(" Tool").unwrap_or(&s).to_string()
}

/// Draw the toast (each frame, after the other overlays).
pub fn draw(app: &PhotocraftApp, ctx: &egui::Context) {
    let Some(t): Option<Toast> = ctx.data(|d| d.get_temp(id())) else { return };
    let now = ctx.input(|i| i.time);
    let instant = ctx.global_style().animation_time <= 0.0;
    let opacity = opacity_at(&t, now, instant);
    if opacity <= 0.0 {
        if now - t.changed > IDLE_SECS {
            ctx.data_mut(|d| d.remove::<Toast>(id()));
        } else {
            ctx.request_repaint();
        }
        return;
    }
    ctx.request_repaint();
    let phone = crate::simple_ui::active(app, ctx);
    let tokens = Tokens::get(ctx);
    let appear = if instant { 1.0 } else { ease_out((now - t.shown) / APPEAR) };
    let change = if instant || t.before.is_none() { 1.0 } else { ease_out((now - t.changed) / CHANGE) };
    let font = ctx.global_style().text_styles.get(&egui::TextStyle::Body).cloned().unwrap_or_else(|| FontId::proportional(14.0));
    let small = FontId::new(font.size * 0.9, font.family.clone());
    let strong = FontId::new(font.size, egui::FontFamily::Name("medium".into()));
    let strong = if ctx.fonts(|f| f.families().contains(&egui::FontFamily::Name("medium".into()))) { strong } else { font.clone() };
    let icon = if phone { 22.0 } else { 18.0 };
    let pad = vec2(16.0, 9.0);
    let height = icon + pad.y * 2.0;
    // Lay out a trio: [(icon, name?, current)] and its width.
    let parts = |trio: &Trio| -> Vec<(Tool, Option<std::sync::Arc<egui::Galley>>, bool)> {
        let mut v = Vec::new();
        for (k, t) in trio.iter().enumerate() {
            let Some(t) = *t else { continue };
            let cur = k == 1;
            let label = if cur || !phone { Some(shown_name(t)) } else { None };
            let galley = label.map(|l| ctx.fonts_mut(|f| f.layout_no_wrap(l, if cur { strong.clone() } else { small.clone() }, Color32::WHITE)));
            v.push((t, galley, cur));
        }
        v
    };
    let width_of = |p: &Vec<(Tool, Option<std::sync::Arc<egui::Galley>>, bool)>| -> f32 {
        let gap = 10.0;
        let arrows = if p.len() > 1 { 2.0 * 14.0 } else { 0.0 };
        p.iter().map(|(_, g, _)| icon + g.as_ref().map_or(0.0, |g| 6.0 + g.size().x)).sum::<f32>() + gap * (p.len().saturating_sub(1)) as f32 + arrows
    };
    let now_parts = parts(&t.now);
    let before_parts = t.before.filter(|_| change < 1.0).map(|b| parts(&b));
    let w_now = width_of(&now_parts);
    let w = before_parts.as_ref().map_or(w_now, |b| w_now + (width_of(b) - w_now) * (1.0 - change)) + pad.x * 2.0;
    let screen = crate::safe_area::rect(ctx);
    let w = w.min(screen.width() - 16.0);
    // Its place: the bottom centre; on a phone above the bars and clear of the floating controls.
    let mut bottom = screen.bottom() - 28.0;
    let mut rect = Rect::from_min_size(pos2(screen.center().x - w / 2.0, bottom - height), vec2(w, height));
    if phone {
        let mut taken: Vec<Rect> = Vec::new();
        let s = crate::simple_ui::inspect(app, ctx);
        let tch = crate::touch_ui::inspect(app, ctx);
        for r in s["buttons"].as_array().into_iter().flatten().chain(tch["rects"].as_array().into_iter().flatten()) {
            let label = r["label"].as_str().unwrap_or_default();
            if label == "toolToast" {
                continue;
            }
            if let Ok(v) = serde_json::from_value::<[f32; 4]>(r["rect"].clone()) {
                taken.push(Rect::from_min_max(pos2(v[0], v[1]), pos2(v[2], v[3])));
            }
        }
        bottom = screen.bottom() - 8.0;
        rect = Rect::from_min_size(pos2(screen.center().x - w / 2.0, bottom - height), vec2(w, height));
        for _ in 0..12 {
            match taken.iter().filter(|r| r.expand(4.0).intersects(rect)).map(|r| r.top()).reduce(f32::min) {
                Some(top) => rect = rect.translate(vec2(0.0, top - 8.0 - rect.bottom())),
                None => break,
            }
        }
        crate::touch_ui::note_rect(ctx, "toolToast", rect);
    }
    let rect = rect.translate(vec2(0.0, (1.0 - appear) * RISE));
    // An area that takes no input: drawn above the canvas, never in the way of a press.
    crate::overlay_guard::non_modal(ctx, id());
    egui::Area::new(id()).order(Order::Foreground).interactable(false).fixed_pos(screen.min).constrain(false).show(ctx, |ui| {
        let painter = ui.painter().clone().with_clip_rect(screen);
        let a = |c: Color32, k: f32| c.gamma_multiply(k * opacity);
        let fill = Color32::from_rgb(28, 30, 34);
        let radius = CornerRadius::same((height / 2.0).round().min(255.0) as u8);
        for (spread, alpha) in [(6.0, 0.06), (3.0, 0.10)] {
            painter.rect_filled(
                rect.expand(spread).translate(vec2(0.0, 3.0)),
                CornerRadius::same(((height / 2.0) + spread).min(255.0) as u8),
                a(Color32::BLACK, alpha),
            );
        }
        painter.rect(rect, radius, a(fill, 0.96), Stroke::new(1.0, a(Color32::from_white_alpha(28), 1.0)), egui::StrokeKind::Inside);
        let inner = rect.shrink2(pad);
        let clip = painter.with_clip_rect(rect.shrink(2.0).intersect(screen));
        let draw_parts = |p: &Vec<(Tool, Option<std::sync::Arc<egui::Galley>>, bool)>, dx: f32, k: f32| {
            if k <= 0.0 {
                return;
            }
            let pw = width_of(p);
            let mut x = inner.center().x - pw / 2.0 + dx;
            let cy = inner.center().y;
            let dim = Color32::from_gray(160);
            let n = p.len();
            for (idx, (tool, galley, cur)) in p.iter().enumerate() {
                if n > 1 && idx == 0 {
                    arrow(&clip, pos2(x + 4.0, cy), -1.0, a(dim, k));
                    x += 14.0;
                }
                let tint = if *cur { tokens.accent } else { dim };
                if *cur {
                    let hl_w = icon + galley.as_ref().map_or(0.0, |g| 6.0 + g.size().x);
                    let hl = Rect::from_min_size(pos2(x - 6.0, cy - icon / 2.0 - 4.0), vec2(hl_w + 12.0, icon + 8.0));
                    clip.rect_filled(hl, CornerRadius::same(((icon + 8.0) / 2.0) as u8), a(tokens.accent, 0.18 * k));
                }
                let ir = Rect::from_center_size(pos2(x + icon / 2.0, cy), Vec2::splat(icon));
                crate::icons::paint(ui, ir, crate::icons::tool_icon(*tool), icon, a(if *cur { Color32::WHITE } else { tint }, k));
                x += icon;
                if let Some(g) = galley {
                    x += 6.0;
                    let color = if *cur { Color32::WHITE } else { dim };
                    clip.galley_with_override_text_color(pos2(x, cy - g.size().y / 2.0), g.clone(), a(color, k));
                    x += g.size().x;
                }
                if idx + 1 < n {
                    x += 10.0;
                } else if n > 1 {
                    arrow(&clip, pos2(x + 10.0, cy), 1.0, a(dim, k));
                }
            }
        };
        if let Some(b) = &before_parts {
            draw_parts(b, -t.dir * SLIDE * change, 1.0 - change);
        }
        draw_parts(&now_parts, t.dir * SLIDE * (1.0 - change), change);
    });
}

/// A small solid triangle pointing left (`dir` -1) or right (+1) centred on `c` (a glyph-free ◂ ▸).
fn arrow(p: &egui::Painter, c: egui::Pos2, dir: f32, color: Color32) {
    let (w, h) = (4.5, 5.0);
    p.add(egui::Shape::convex_polygon(vec![pos2(c.x + dir * w, c.y), pos2(c.x - dir * w, c.y - h), pos2(c.x - dir * w, c.y + h)], color, Stroke::NONE));
}
