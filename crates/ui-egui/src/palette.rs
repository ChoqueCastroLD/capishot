//! ⌘F command palette: fuzzy search over every menu command and tool.

use egui::{Align2, Color32, CornerRadius, RichText, Sense, Stroke, vec2};

use crate::theme::{self, Tokens};
use crate::{PhotocraftApp, icons};

/// Case-insensitive subsequence match score (higher is better); None = no match.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    // Case and accents don't matter: "tamano" finds "Tamaño", "configuracoes" "Configurações".
    let t: Vec<char> = crate::palette_search::fold(text).chars().collect();
    let mut score = 0;
    let mut ti = 0;
    let mut last = None;
    for qc in crate::palette_search::fold(query).chars() {
        let mut found = false;
        while ti < t.len() {
            if t[ti] == qc {
                score += if last == Some(ti.wrapping_sub(1)) { 8 } else { 1 };
                if ti == 0 || t[ti - 1] == ' ' {
                    score += 5;
                }
                last = Some(ti);
                ti += 1;
                found = true;
                break;
            }
            ti += 1;
        }
        if !found {
            return None;
        }
    }
    Some(score - (t.len() as i32 / 8))
}

/// Bonus that puts a command or tool whose name is exactly the query ahead of every fuzzy match
/// ("settings" finds Settings before Color Settings).
fn exact_bonus(query: &str, names: &[&str]) -> i32 {
    let q = query.trim();
    if !q.is_empty() && names.iter().any(|n| n.trim_end_matches('…').trim().to_lowercase() == q.to_lowercase()) { 1000 } else { 0 }
}

/// How many recently run commands the palette remembers.
const RECENT_CAP: usize = 8;

/// The id of the palette's recently-run list (egui temp data: session-scoped, like the query).
fn recent_id() -> egui::Id {
    egui::Id::new("palette-recent")
}

/// Remember `id` as the most recently run command: deduplicated, newest first, capped.
pub(crate) fn note_recent(ctx: &egui::Context, id: &str) {
    ctx.data_mut(|d| {
        let mut list: Vec<String> = d.get_temp(recent_id()).unwrap_or_default();
        push_recent(&mut list, id, RECENT_CAP);
        d.insert_temp(recent_id(), list);
    });
}

/// The palette's recently run commands, newest first (for tests).
#[cfg(test)]
pub(crate) fn recent_ids(ctx: &egui::Context) -> Vec<String> {
    recent_list(ctx)
}

/// The commands run lately, newest first (the palette's empty query, the simple interface's
/// recents bar).
pub(crate) fn recent_list(ctx: &egui::Context) -> Vec<String> {
    ctx.data(|d| d.get_temp(recent_id()).unwrap_or_default())
}

/// [`note_recent`]'s core: move `id` to the front, dropping the oldest past `cap`.
fn push_recent(list: &mut Vec<String>, id: &str, cap: usize) {
    list.retain(|x| x != id);
    list.insert(0, id.to_string());
    list.truncate(cap);
}

/// The empty-query hits: the recent commands as rows, newest first (UI-217-7). Ids that are
/// no longer menu commands are skipped.
fn recent_hits(recents: &[String], menus: &[crate::menus::MenuItem], lang: crate::i18n::Lang) -> Vec<(i32, String, String, Option<String>, bool)> {
    recents
        .iter()
        .enumerate()
        .filter_map(|(i, id)| {
            let m = menus.iter().find(|m| &m.id == id)?;
            let path = m.path.iter().map(|p| crate::i18n::tr(lang, p)).collect::<Vec<_>>().join(" › ");
            let label = crate::i18n::tr_id(lang, &m.id, &m.label);
            Some((
                1000 - i as i32,
                m.id.clone(),
                label.trim_end_matches('…').to_string(),
                Some(path + &m.shortcut.as_ref().map(|s| format!("   {}", crate::shortcuts::pretty(s))).unwrap_or_default()),
                m.enabled,
            ))
        })
        .collect()
}

/// The menu commands matching `q`, as palette rows. Separators are not commands: they would
/// match on their menu path and list as blank "---" rows (#2479).
fn menu_hits(menus: Vec<crate::menus::MenuItem>, q: &str, lang: crate::i18n::Lang) -> Vec<(i32, String, String, Option<String>, bool)> {
    menus
        .into_iter()
        .filter(|m| m.label != "---")
        .filter_map(|m| {
            let path = m.path.iter().map(|p| crate::i18n::tr(lang, p)).collect::<Vec<_>>().join(" › ");
            let label = crate::i18n::tr_id(lang, &m.id, &m.label);
            // Match what is shown, then the same command in English, Spanish and Portuguese
            // (people search in the language they think in, whatever the UI shows).
            let mut s = fuzzy_score(q, &format!("{label} {path}"));
            let mut names = vec![label];
            for other in crate::palette_search::SEARCH_LANGS.iter().filter_map(|c| crate::i18n::Lang::from_code(c)).filter(|l| l.code() != lang.code()) {
                let l = crate::i18n::tr_id(other, &m.id, &m.label);
                let p = m.path.iter().map(|p| crate::i18n::tr(other, p)).collect::<Vec<_>>().join(" ");
                if let Some(o) = fuzzy_score(q, &format!("{l} {p}")) {
                    s = Some(s.map_or(o - 1, |v| v.max(o - 1)));
                }
                names.push(l);
            }
            names.push(&m.label);
            let s = s? + exact_bonus(q, &names);
            Some((
                s,
                m.id,
                label.trim_end_matches('…').to_string(),
                Some(path + &m.shortcut.map(|s| format!("   {}", crate::shortcuts::pretty(&s))).unwrap_or_default()),
                m.enabled,
            ))
        })
        .collect()
}

/// A palette row: score, id (`tool:Name`, `quick:`, or a command), label, detail, enabled.
type Hit = (i32, String, String, Option<String>, bool);

/// The tools matching `q`, by their names in the UI language, English, Spanish and Portuguese.
fn tool_hits(q: &str, lang: crate::i18n::Lang) -> Vec<Hit> {
    let langs: Vec<crate::i18n::Lang> = crate::palette_search::SEARCH_LANGS.iter().filter_map(|c| crate::i18n::Lang::from_code(c)).collect();
    crate::state::Tool::ALL
        .iter()
        .filter_map(|tool| {
            let shown = crate::i18n::tr(lang, tool.label());
            let others: Vec<&str> = langs.iter().map(|l| crate::i18n::tr(*l, tool.label())).collect();
            let s = std::iter::once(shown).chain(others.iter().copied()).filter_map(|n| fuzzy_score(q, n)).max()?;
            let mut names = others.clone();
            names.push(shown);
            Some((s + 2 + exact_bonus(q, &names), format!("tool:{tool:?}"), shown.to_string(), Some(format!("{}   {}", crate::i18n::tr(lang, "Tool"), tool.key())), true))
        })
        .collect()
}

/// The commands and tools everyday words in `q` name ([`crate::palette_search::SYNONYMS`]).
fn synonym_hits(q: &str, menus: &[crate::menus::MenuItem], lang: crate::i18n::Lang) -> Vec<Hit> {
    crate::palette_search::synonym_targets(q)
        .into_iter()
        .filter_map(|(score, target)| {
            if let Some(name) = target.strip_prefix("tool:") {
                let tool = crate::state::Tool::from_name(name)?;
                return Some((score, target.to_string(), crate::i18n::tr(lang, tool.label()).to_string(), Some(crate::i18n::tr(lang, "Tool").to_string()), true));
            }
            let m = menus.iter().find(|m| m.id == target)?;
            let path = m.path.iter().map(|p| crate::i18n::tr(lang, p)).collect::<Vec<_>>().join(" › ");
            Some((score, m.id.clone(), crate::i18n::tr_id(lang, &m.id, &m.label).trim_end_matches('…').to_string(), Some(path), m.enabled))
        })
        .collect()
}

/// The tools and commands whose plain-language description ([`crate::help_text`]) has the
/// query's words, in the UI language, Spanish, Portuguese or English ("borrar texto" finds
/// Remove with AI). Several words matched rank above a single everyday word; one word ranks
/// below any name it matches.
fn description_hits(q: &str, menus: &[crate::menus::MenuItem], lang: crate::i18n::Lang) -> Vec<Hit> {
    let words = crate::palette_search::fold(q).split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).count();
    if words == 0 {
        return Vec::new();
    }
    crate::help_text::HELP
        .iter()
        .filter_map(|h| {
            let s = [h.text(lang), h.en, h.es, h.pt].iter().filter_map(|t| crate::help_text::words_score(q, t)).max()?;
            let s = if words >= 2 { 650 + 20 * s } else { 5 + s };
            if let Some(name) = h.id.strip_prefix("tool:") {
                let tool = crate::state::Tool::from_name(name)?;
                return Some((
                    s + 2,
                    h.id.to_string(),
                    crate::i18n::tr(lang, tool.label()).to_string(),
                    Some(format!("{}   {}", crate::i18n::tr(lang, "Tool"), tool.key())),
                    true,
                ));
            }
            let m = menus.iter().find(|m| m.id == h.id)?;
            let path = m.path.iter().map(|p| crate::i18n::tr(lang, p)).collect::<Vec<_>>().join(" › ");
            Some((s, m.id.clone(), crate::i18n::tr_id(lang, &m.id, &m.label).trim_end_matches('…').to_string(), Some(path), m.enabled))
        })
        .collect()
}

/// `hits` (best first) with each group's rows together: the groups in the order of their best
/// row, a quick action first.
fn grouped(hits: Vec<Hit>) -> Vec<Hit> {
    let mut order: Vec<Option<crate::help_text::Group>> = Vec::new();
    let key = |h: &Hit| (!h.1.starts_with("quick:")).then(|| crate::help_text::group_of(&h.1));
    for h in &hits {
        let k = key(h);
        if !order.contains(&k) {
            order.push(k);
        }
    }
    let mut out = Vec::with_capacity(hits.len());
    for k in order {
        out.extend(hits.iter().filter(|h| key(h) == k).cloned());
    }
    out
}

/// Every row for query `q`, best first.
pub(crate) fn hits_for(app: &PhotocraftApp, ctx: &egui::Context, q: &str) -> Vec<Hit> {
    let lang = crate::i18n::Lang::from_pref(&app.session.prefs().interface.language);
    let menus = crate::menus::menu_items(app);
    let empty = q.trim().is_empty();
    // An empty query lists the recently run commands (UI-217-7); with none (or a fresh session)
    // it falls back to the full fuzzy list, as before.
    let mut hits: Vec<Hit> = if empty { recent_hits(&recent_list(ctx), &menus, lang) } else { Vec::new() };
    if !empty {
        // A quick action the query spells ("zoom 200", "nuevo 1200x1700") comes first.
        if let Some(action) = crate::palette_search::quick(q) {
            hits.push((5000, "quick:".into(), action.label(), Some(tl!("Quick action").to_string()), true));
        }
        hits.extend(synonym_hits(q, &menus, lang));
        hits.extend(description_hits(q, &menus, lang));
    }
    if !empty || hits.is_empty() {
        hits.extend(menu_hits(menus, q, lang));
        hits.extend(tool_hits(q, lang));
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.2.cmp(&b.2)));
    // A command found both by an everyday word and by its name is listed once.
    let mut seen = std::collections::HashSet::new();
    hits.retain(|h| seen.insert(h.1.clone()));
    hits.truncate(12);
    if empty { hits } else { grouped(hits) }
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.ui.palette_open {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    // Scrim.
    egui::Area::new(egui::Id::new("palette-scrim")).order(egui::Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let r = ui.allocate_exact_size(screen.size(), Sense::click()).1;
        ui.painter().rect_filled(screen, 0.0, Color32::from_black_alpha(110));
        if r.clicked() {
            app.ui.palette_open = false;
        }
    });
    if !app.ui.palette_open {
        // Closed by a tap outside: the next search starts empty.
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("palette-query"), String::new()));
        return;
    }
    // On a phone the palette spans the window near its top, with finger-sized two-line rows.
    let narrow = screen.width() < 600.0;
    let width = if narrow { screen.width() - 16.0 - 20.0 } else { 560.0 };
    let row_h = if narrow { 52.0 } else { 32.0 };
    // The simple interface's rows have a star: tap it to put the command on the favorites bar.
    let stars = app.simple;
    let mut run: Option<String> = None;
    let mut star: Option<String> = None;
    egui::Area::new(egui::Id::new("palette"))
        .order(egui::Order::Tooltip)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(egui::pos2(screen.center().x, screen.top() + if narrow { 52.0 } else { 110.0 }))
        .show(ctx, |ui| {
            egui::Frame::NONE
                .fill(t.card)
                .stroke(Stroke::new(1.0, t.card_border))
                .corner_radius(CornerRadius::same(t.radius_lg as u8))
                .shadow(egui::Shadow { offset: [0, 18], blur: 48, spread: 0, color: t.shadow })
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(width);
                    let qid = egui::Id::new("palette-query");
                    let mut q: String = ui.data_mut(|d| d.get_temp(qid).unwrap_or_default());
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                        icons::paint(ui, r, "search", 16.0, t.text_dim);
                        let te = egui::TextEdit::singleline(&mut q)
                            .hint_text(tl!("Search commands, tools and panels…"))
                            .frame(egui::Frame::NONE)
                            .font(egui::FontId::proportional(if narrow { 16.0 } else { 15.0 }))
                            .desired_width(width - 40.0);
                        let resp = ui.add(te);
                        // Only when focus is missing: every request_focus interrupts the IME, and
                        // doing that each frame makes Wayland input methods drop key releases (#585).
                        if !resp.has_focus() {
                            resp.request_focus();
                        }
                    });
                    ui.add_space(6.0);
                    crate::widgets::hairline(ui);
                    ui.add_space(6.0);
                    let hits = hits_for(app, ui.ctx(), &q);
                    let sel_id = egui::Id::new("palette-sel");
                    let mut sel: usize = ui.data_mut(|d| d.get_temp(sel_id).unwrap_or(0));
                    let (down, up, enter, esc) = ui.input(|i| {
                        (
                            i.key_pressed(egui::Key::ArrowDown),
                            i.key_pressed(egui::Key::ArrowUp),
                            i.key_pressed(egui::Key::Enter),
                            i.key_pressed(egui::Key::Escape),
                        )
                    });
                    if down {
                        sel = (sel + 1).min(hits.len().saturating_sub(1));
                    }
                    if up {
                        sel = sel.saturating_sub(1);
                    }
                    sel = sel.min(hits.len().saturating_sub(1));
                    if hits.is_empty() {
                        ui.label(RichText::new(tl!("No matching commands")).color(t.text_faint));
                    } else if q.trim().is_empty() && !recent_list(ui.ctx()).is_empty() {
                        // An empty query lists the commands run lately.
                        ui.label(RichText::new(tl!("Recent commands")).font(theme::semibold(12.5)).color(t.text_dim));
                        ui.add_space(2.0);
                    }
                    let lang = crate::i18n::Lang::from_pref(&app.session.prefs().interface.language);
                    // Rows (and group headings) as long as they fit: a phone's window is short.
                    let mut room = if narrow { (screen.height() - 52.0 - 140.0).max(4.0 * row_h) } else { 12.0 * 44.0 };
                    let grouped = !q.trim().is_empty();
                    let mut last_group = None;
                    let mut shown: Vec<&str> = Vec::new();
                    for (i, (_, id, label, detail, enabled)) in hits.iter().enumerate() {
                        let quick = id.starts_with("quick:");
                        let group = (!quick).then(|| crate::help_text::group_of(id));
                        let heading = grouped && group.is_some() && group != last_group;
                        let about = if quick { None } else { crate::help_text::describe(id, lang) };
                        let h = if narrow {
                            row_h
                        } else if about.is_some() {
                            44.0
                        } else {
                            32.0
                        };
                        if h + if heading { 22.0 } else { 0.0 } > room {
                            break;
                        }
                        room -= h;
                        if heading && let Some(g) = group {
                            room -= 22.0;
                            ui.add_space(4.0);
                            ui.label(RichText::new(g.title()).font(theme::semibold(11.5)).color(t.text_dim));
                            last_group = group;
                        }
                        shown.push(id);
                        let (rect, resp) = ui.allocate_exact_size(vec2(width, h), Sense::click());
                        if i == sel || resp.hovered() {
                            ui.painter().rect_filled(rect, t.radius_sm, if i == sel { t.accent_soft } else { t.hover });
                        }
                        let color = if *enabled { t.text } else { t.text_faint };
                        let star_rect = (stars && !quick).then(|| egui::Rect::from_center_size(rect.right_center() - vec2(20.0, 0.0), vec2(36.0, 36.0)));
                        let text_right = star_rect.map_or(rect.right() - 12.0, |s| s.left() - 4.0);
                        if quick {
                            icons::paint(ui, egui::Rect::from_center_size(rect.left_center() + vec2(18.0, 0.0), vec2(18.0, 18.0)), "sparkles", 15.0, t.accent);
                        }
                        let left = rect.left() + if quick { 34.0 } else { 12.0 };
                        let line = |ui: &egui::Ui, text: String, font: egui::FontId, c: Color32, max: f32| {
                            let mut job = egui::text::LayoutJob::simple_singleline(text, font, c);
                            job.wrap = egui::text::TextWrapping::truncate_at_width(max.max(20.0));
                            ui.painter().layout_job(job)
                        };
                        if narrow {
                            // Two lines: the name, then what it does (or where it lives).
                            let g = line(ui, label.clone(), theme::medium(14.0), color, text_right - left);
                            ui.painter().galley(egui::pos2(left, rect.top() + 7.0), g, color);
                            if let Some(d) = about.map(str::to_string).or_else(|| detail.as_ref().map(|d| d.trim().to_string())) {
                                let g = line(ui, d, egui::FontId::proportional(11.5), t.text_dim, text_right - left);
                                ui.painter().galley(egui::pos2(left, rect.bottom() - 7.0 - g.size().y), g, t.text_dim);
                            }
                        } else {
                            let name_y = if about.is_some() { rect.top() + 13.0 } else { rect.center().y };
                            ui.painter().text(egui::pos2(left, name_y), Align2::LEFT_CENTER, label, theme::medium(13.0), color);
                            if let Some(d) = detail {
                                ui.painter().text(egui::pos2(text_right, name_y), Align2::RIGHT_CENTER, d, egui::FontId::proportional(11.5), t.text_faint);
                            }
                            if let Some(a) = about {
                                let g = line(ui, a.to_string(), egui::FontId::proportional(11.5), t.text_dim, text_right - left);
                                ui.painter().galley(egui::pos2(left, rect.bottom() - 7.0 - g.size().y), g, t.text_dim);
                            }
                        }
                        let mut starred = false;
                        if let Some(sr) = star_rect {
                            let on = crate::simple_ui::is_favorite(app, id);
                            icons::paint(ui, sr, if on { "star-filled" } else { "star" }, 17.0, if on { t.accent } else { t.text_faint });
                            if resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| sr.contains(p)) {
                                star = Some(id.clone());
                                starred = true;
                            }
                        }
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, *enabled, label));
                        if resp.clicked() && *enabled && !starred {
                            run = Some(id.clone());
                        }
                    }
                    // Similar tools among the results: how to choose.
                    if grouped && let Some(w) = crate::help_text::which_for(shown.iter().copied()) {
                        ui.add_space(6.0);
                        egui::Frame::NONE.fill(t.chrome).corner_radius(CornerRadius::same(t.radius_sm as u8)).inner_margin(egui::Margin::same(8)).show(
                            ui,
                            |ui| {
                                ui.set_width(width - 16.0);
                                ui.label(RichText::new(tl!("Which one should I use?")).font(theme::semibold(12.5)).color(t.text));
                                ui.label(RichText::new(w.text(lang)).size(12.0).color(t.text_dim));
                            },
                        );
                        ctx.data_mut(|d| d.insert_temp(egui::Id::new("palette-which"), true));
                    } else {
                        ctx.data_mut(|d| d.remove::<bool>(egui::Id::new("palette-which")));
                    }
                    if enter && let Some(h) = hits.get(sel).filter(|h| h.4) {
                        run = Some(h.1.clone());
                        // dialogs::show runs later in the same frame and would read this Enter as
                        // its OK button, instantly closing the dialog we are about to open (#536).
                        ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
                    }
                    if esc {
                        app.ui.palette_open = false;
                        // The next search starts empty.
                        q.clear();
                    }
                    if !narrow {
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new(crate::i18n::fmt(tl!("↑↓ navigate   {key} run   esc close"), &[("key", &crate::shortcuts::pretty("Enter"))]))
                                .small()
                                .color(t.text_faint),
                        );
                    }
                    if run.as_deref() == Some("quick:") {
                        // The quick action is the query's: read it before the query is cleared.
                        ui.data_mut(|d| d.insert_temp(egui::Id::new("palette-quick"), q.clone()));
                    }
                    ui.data_mut(|d| {
                        d.insert_temp(qid, q);
                        d.insert_temp(sel_id, sel);
                    });
                });
        });
    if let Some(id) = star {
        crate::simple_ui::toggle_favorite(app, &id);
    }
    if let Some(id) = run {
        app.ui.palette_open = false;
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("palette-query"), String::new()));
        if id == "quick:" {
            let q: String = ctx.data(|d| d.get_temp(egui::Id::new("palette-quick")).unwrap_or_default());
            if let Err(e) = run_quick(app, ctx, &q) {
                app.ui.status = e;
            }
        } else if let Some(tool) = id.strip_prefix("tool:") {
            if let Some(t) = crate::state::Tool::from_name(tool) {
                app.ui.tool = t;
            }
        } else if let Err(e) = crate::telemetry::with_source(crate::telemetry::Source::Palette, || crate::menus::invoke(app, ctx, &id, serde_json::json!({}))) {
            app.ui.status = e;
        }
    }
}

/// Carry out the quick action query `q` spells.
pub(crate) fn run_quick(app: &mut PhotocraftApp, ctx: &egui::Context, q: &str) -> Result<(), String> {
    let Some(action) = crate::palette_search::quick(q) else { return Err(tl!("No matching commands").to_string()) };
    if let crate::palette_search::Quick::Zoom(z) = action {
        app.sync_views();
        let i = app.session.active_index().ok_or_else(|| tl!("Open a document first").to_string())?;
        let v = app.ui.views.get_mut(i).ok_or("no document view")?;
        v.fit_pending = false;
        v.fill_pending = false;
        v.zoom = z;
        ctx.request_repaint();
        return Ok(());
    }
    // Engine commands with their parameters (the menus' File › New would open its dialog).
    let Some((id, params)) = action.command(app.session.prefs()) else { return Ok(()) };
    crate::telemetry::with_source(crate::telemetry::Source::Palette, || app.run(id, params)).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use egui::{Event, Key, Modifiers, vec2};
    use egui_kittest::Harness;
    use serde_json::json;

    use super::{exact_bonus, fuzzy_score, push_recent};
    use crate::PhotocraftApp;

    #[test]
    fn open_palette_interrupts_the_ime_only_when_it_takes_focus() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let ctx = egui::Context::default();
        crate::theme::install_fonts(&ctx);
        // Fonts bind on the next pass; this one opens nothing.
        ctx.run_ui(Default::default(), |_| {}).textures_delta.clear();
        app.ui.palette_open = true;
        // The field takes focus on the first pass and owns the IME from the second on.
        let interrupts: Vec<Option<bool>> = (0..4)
            .map(|_| {
                let mut out = ctx.run_ui(Default::default(), |ui| super::show(&mut app, ui.ctx()));
                out.textures_delta.clear();
                out.platform_output.ime.map(|ime| ime.should_interrupt_composition)
            })
            .collect();
        assert_eq!(interrupts, [None, Some(false), Some(false), Some(false)]);
    }

    #[test]
    fn push_recent_moves_to_the_front_and_caps() {
        let mut l = Vec::new();
        push_recent(&mut l, "b", 3);
        push_recent(&mut l, "a", 3);
        push_recent(&mut l, "c", 3);
        assert_eq!(l, ["c", "a", "b"]);
        push_recent(&mut l, "a", 3);
        assert_eq!(l, ["a", "c", "b"], "re-running a command moves it to the front");
        push_recent(&mut l, "d", 3);
        assert_eq!(l, ["d", "a", "c"], "the oldest falls off the cap");
    }

    #[test]
    fn the_palette_records_and_replays_recent_commands_on_an_empty_query() {
        let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(|cc| {
            PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
            PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
        });
        h.state_mut().run("file.new", json!({"width": 64, "height": 64})).unwrap();
        h.state_mut().sync_views();
        let ctx = h.ctx.clone();
        // Run "Keyboard Shortcuts" through the palette: it becomes the recent command.
        crate::menus::invoke(h.state_mut(), &ctx, "edit.search", json!({})).unwrap();
        h.run_steps(2);
        h.event(Event::Text("keyboard shortcuts".into()));
        h.run_steps(2);
        h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(3);
        assert_eq!(super::recent_ids(&ctx).len(), 1, "the command the palette ran is remembered");
        // Close its dialog, reopen the palette with an empty query and press Enter: the recent
        // command is the first hit, so the same dialog opens again.
        let dialog = h
            .state()
            .ui
            .dialogs
            .iter()
            .find(|d| d.fields.get("__prefsui").and_then(|v| v.as_str()) == Some("shortcuts"))
            .map(|d| d.id)
            .expect("the shortcuts dialog");
        h.state_mut().ui.close_dialog(dialog);
        h.run_steps(2);
        crate::menus::invoke(h.state_mut(), &ctx, "edit.search", json!({})).unwrap();
        h.run_steps(2);
        h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(3);
        assert!(
            h.state().ui.dialogs.iter().any(|d| d.fields.get("__prefsui").and_then(|v| v.as_str()) == Some("shortcuts")),
            "the empty-query palette runs the recent command"
        );
        assert_eq!(super::recent_ids(&ctx).len(), 1, "re-running it moves it to the front, no duplicate");
    }

    /// #2479: menu separators matched on their menu path ("f" → File, Filter) and filled the
    /// palette with blank "---" rows.
    #[test]
    fn separators_are_not_palette_hits() {
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        let menus = crate::menus::menu_items(&app);
        assert!(menus.iter().any(|m| m.label == "---"), "the menus have separators to filter");
        for q in ["f", "file", "filter", "", "-"] {
            let hits = super::menu_hits(menus.clone(), q, crate::i18n::Lang::EN);
            assert!(!hits.is_empty(), "{q:?} still matches commands");
            assert!(hits.iter().all(|h| h.1 != "---" && h.2 != "---"), "{q:?} lists a separator");
        }
    }

    #[test]
    fn fuzzy_ranks_prefix_and_contiguous_higher() {
        assert!(fuzzy_score("hue", "Hue/Saturation…").unwrap() > fuzzy_score("hue", "Channel Mixer Hue").unwrap_or(-99));
        assert!(fuzzy_score("gblur", "Gaussian Blur").is_some());
        assert!(fuzzy_score("xyz", "Gaussian Blur").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    /// Typing a command's exact name ranks it first, even when a longer name scores the same.
    #[test]
    fn exact_name_ranks_first() {
        let settings = fuzzy_score("settings", "Settings Edit › Preferences Settings Edit › Preferences").unwrap() + exact_bonus("settings", &["Settings"]);
        let color = fuzzy_score("settings", "Color Settings… Edit Color Settings… Edit").unwrap() + exact_bonus("settings", &["Color Settings…"]);
        assert!(settings > color);
        assert_eq!(exact_bonus("Color settings", &["Color Settings…"]), 1000);
        assert_eq!(exact_bonus(" ", &[""]), 0);
        assert_eq!(exact_bonus("set", &["Settings"]), 0);
    }

    /// #536: Edit › Search → "Keyboard Shortcuts" must open the dialog, with and without a document.
    #[test]
    fn palette_opens_the_keyboard_shortcuts_dialog() {
        for with_doc in [false, true] {
            let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).with_max_steps(64).build_eframe(|cc| {
                PhotocraftApp::setup_context(&cc.egui_ctx, Default::default());
                PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default())
            });
            if with_doc {
                h.state_mut().run("file.new", json!({"width": 64, "height": 64})).unwrap();
                h.state_mut().sync_views();
            }
            let ctx = h.ctx.clone();
            crate::menus::invoke(h.state_mut(), &ctx, "edit.search", json!({})).unwrap();
            h.run_steps(2);
            assert!(h.state().ui.palette_open, "Edit > Search opens the palette (with_doc={with_doc})");
            h.event(Event::Text("keyboard shortcuts".into()));
            h.run_steps(2);
            h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
            h.run_steps(3);
            let app = h.state();
            assert!(!app.ui.palette_open, "Enter closes the palette (with_doc={with_doc})");
            let dialog = app
                .ui
                .dialogs
                .iter()
                .find(|d| d.fields.get("__prefsui").and_then(|v| v.as_str()) == Some("shortcuts"))
                .map(|d| d.id)
                .unwrap_or_else(|| panic!("#536: the Keyboard Shortcuts dialog did not open (with_doc={with_doc})"));
            // The dialog keeps its own Enter: a second press is its OK and closes it.
            h.event(Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
            h.run_steps(3);
            assert!(h.state().ui.dialogs.iter().all(|d| d.id != dialog), "a second Enter closes the dialog as its OK (with_doc={with_doc})");
        }
    }
}
