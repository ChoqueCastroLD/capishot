//! Pages in other tabs give back their memory (Preferences › Performance › Release Inactive
//! Pages): a page not looked at for [`IDLE_S`] goes to sleep ([`photocraft_engine::sleep`]),
//! one at a time, on a worker; activating a sleeping page wakes it, with "Loading page…" over the
//! canvas and in the status bar until it is back. Auto does it on devices of 4 GB or less, and in
//! the browser with more than [`MANY_PAGES`] pages open on any device: a tab is killed well before
//! the device's memory runs out.
//!
//! A page sleeps only once its crash recovery checkpoint is current (where pages are
//! checkpointed), so sleeping never leaves its latest changes without one.

use std::collections::HashMap;

use photocraft_doc::DocId;
use photocraft_engine::jobs::{JobId, Started};
use photocraft_engine::prefs::ReleasePages;
use serde_json::json;

use crate::PhotocraftApp;

/// Seconds a page goes unseen before it sleeps.
pub const IDLE_S: f64 = 6.0;
/// "4 GB or less": devices report a little under their nominal memory.
const SMALL_DEVICE: u64 = 4_400 << 20;
/// Pages open past which the browser releases inactive ones on any device (Auto).
const MANY_PAGES: usize = 8;
const SLEEP: &str = "document.sleep";
const WAKE: &str = "document.wake";

#[derive(Default)]
pub struct PageSleep {
    /// When each page was last the active one (seconds, the app clock).
    seen: HashMap<DocId, f64>,
    sleeping: Option<JobId>,
    waking: Option<(DocId, JobId)>,
    /// Sleeping pages whose canvas caches were freed.
    freed: std::collections::HashSet<DocId>,
    /// When the pages' memory was last weighed, and whether it was over half the budget.
    weighed: (f64, bool),
}

/// Seconds a page goes unseen before it sleeps while the pages' memory is over half the budget
/// (a chapter opened at once on a phone).
const IDLE_PRESSED_S: f64 = 0.5;

/// Whether pages are released on this device.
pub fn enabled(app: &PhotocraftApp) -> bool {
    match app.session.prefs().performance.release_inactive_pages {
        ReleasePages::Always => true,
        ReleasePages::Never => false,
        // A browser tab is killed long before the device runs out (a Chromebook's at about 1 GB):
        // many pages open there sleep whatever the device has.
        ReleasePages::Auto => {
            crate::gpu_canvas::physical_memory().is_some_and(|b| b <= SMALL_DEVICE)
                || (cfg!(target_arch = "wasm32") && app.session.documents().len() > MANY_PAGES)
        }
    }
}

/// Whether the status bar should leave job `command` out: putting a page to sleep is silent.
pub fn silent(command: &str) -> bool {
    command == SLEEP
}

/// The page being woken, for the canvas's "Loading page…".
pub fn waking(app: &PhotocraftApp) -> Option<String> {
    let st = app.session.active()?;
    st.sleeping.is_some().then(|| st.doc.name.clone())
}

/// Every frame: wake the active page if it sleeps; put one page unseen for [`IDLE_S`] to sleep.
pub fn tick(app: &mut PhotocraftApp, now: f64) {
    let running = |app: &PhotocraftApp, j: JobId| app.session.job(j).is_some_and(|j| j.state == "running");
    if let Some(st) = app.session.active() {
        app.page_sleep.seen.insert(st.doc.id, now);
    }
    let open: std::collections::HashSet<DocId> = app.session.documents().iter().map(|st| st.doc.id).collect();
    app.page_sleep.seen.retain(|d, _| open.contains(d));
    // A page that just opened counts as seen now: a batch of files doesn't go to sleep as it lands.
    for d in &open {
        app.page_sleep.seen.entry(*d).or_insert(now);
    }
    // A page that just fell asleep lets go of its canvas textures and caches too.
    let asleep: Vec<DocId> = app.session.documents().iter().filter(|st| st.sleeping.is_some()).map(|st| st.doc.id).collect();
    for id in &asleep {
        if app.page_sleep.freed.insert(*id) {
            free_caches(app, *id);
        }
    }
    app.page_sleep.freed.retain(|d| asleep.contains(d));
    // Wake first, whatever the setting: a page asleep is useless until it is back.
    if app.page_sleep.waking.is_some_and(|(_, j)| !running(app, j)) {
        app.page_sleep.waking = None;
    }
    if app.page_sleep.waking.is_none()
        && let Some(st) = app.session.active()
        && let Some(sleeping) = st.sleeping.clone()
    {
        let (id, name) = (st.doc.id, st.doc.name.clone());
        let label = crate::i18n::fmt(tl!("Loading page “{name}”"), &[("name", name.as_str())]);
        let step = label.clone();
        let started = app.session.start_job_in_worker(
            WAKE,
            json!({"document": id.0}),
            &label,
            false,
            move |ctx| {
                ctx.progress(0.1, &step);
                photocraft_engine::sleep::decode(&sleeping)
            },
            move |s, (doc, history)| {
                s.wake(id, doc, history)?;
                Ok(json!({"document": id.0}))
            },
        );
        match started {
            Ok(Started::Job(j)) => app.page_sleep.waking = Some((id, j)),
            Ok(Started::Done(_)) => {}
            Err(e) => crate::notices::error(app, e.to_string()),
        }
    }
    if app.page_sleep.sleeping.is_some_and(|j| !running(app, j)) {
        app.page_sleep.sleeping = None;
    }
    if !enabled(app) || app.page_sleep.sleeping.is_some() || app.page_sleep.waking.is_some() {
        return;
    }
    // Weighing walks every tile: twice a second is enough.
    if now - app.page_sleep.weighed.0 >= 0.5 {
        let budget = app.session.prefs().performance.history_budget_bytes();
        app.page_sleep.weighed = (now, app.session.pixel_bytes() > budget / 2);
    }
    let Some(index) = (0..app.session.documents().len()).find(|&i| sleepy(app, i, now)) else { return };
    let st = &app.session.documents()[index];
    let (id, revision, doc, history) = (st.doc.id, st.revision, st.doc.clone(), st.history.checkpoint());
    let started = app.session.start_job_in_worker(
        SLEEP,
        json!({"document": id.0}),
        tl!("Freeing memory"),
        false,
        move |_| photocraft_engine::sleep::encode(&doc, &history),
        move |s, sleeping| Ok(json!({"asleep": s.put_to_sleep(id, revision, sleeping)})),
    );
    if let Ok(Started::Job(j)) = started {
        app.page_sleep.sleeping = Some(j);
    }
}

/// Everything the app keeps for showing page `id`: canvas textures and caches, the Navigator's,
/// History snapshot thumbnails, the text Text Selection found. Showing it again makes them anew.
fn free_caches(app: &mut PhotocraftApp, id: DocId) {
    crate::doc_switch::evict(app, id);
    if let Some(release) = app.services.release_autosave.as_mut() {
        release(id.0);
    }
    app.navigator_textures.remove(&id);
    app.navigator_pending.remove(&id);
    app.navigator_damage.remove(&id);
    app.navigator_bands.remove(&id);
    app.navigator_gpu.remove(&id);
    app.display_memo.remove(&id);
    app.snapshot_thumbs.retain(|(d, _), _| *d != id);
    app.snapshot_bands.retain(|(d, _), _| *d != id);
    crate::text_select_ui::forget(app, id);
}

/// "Loading page…" over the canvas while the active page wakes: it takes no input meanwhile.
pub fn overlay(app: &PhotocraftApp, ctx: &egui::Context) {
    let Some(name) = waking(app) else { return };
    let rect = app.last_canvas_rect;
    if !rect.is_positive() {
        return;
    }
    let t = crate::theme::Tokens::get(ctx);
    egui::Area::new(egui::Id::unique("page-waking")).order(egui::Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        let (r, _) = ui.allocate_exact_size(rect.size(), egui::Sense::click_and_drag());
        ui.painter().rect_filled(r, 0.0, t.dock.gamma_multiply(0.85));
        let card = egui::Rect::from_center_size(r.center(), egui::vec2(300.0_f32.min(r.width() - 24.0), 96.0));
        ui.painter().rect_filled(card, t.radius, t.card);
        ui.painter().rect_stroke(card, t.radius, egui::Stroke::new(1.0, t.card_border), egui::StrokeKind::Inside);
        let title = crate::i18n::fmt(tl!("Loading page “{name}”"), &[("name", name.as_str())]);
        ui.painter().text(card.center_top() + egui::vec2(0.0, 28.0), egui::Align2::CENTER_CENTER, title, crate::theme::semibold(14.0), t.text);
        ui.painter().text(card.center_top() + egui::vec2(0.0, 52.0), egui::Align2::CENTER_CENTER, tl!("It was freed to save memory"), egui::FontId::proportional(12.0), t.text_dim);
        // An indeterminate bar: decoding has no steps to count.
        let bar = egui::Rect::from_min_size(card.left_bottom() + egui::vec2(24.0, -22.0), egui::vec2(card.width() - 48.0, 4.0));
        ui.painter().rect_filled(bar, 2.0, t.card_border);
        let phase = (ctx.input(|i| i.time) * 0.8).fract() as f32;
        let w = bar.width() * 0.3;
        let x = bar.left() + (bar.width() + w) * phase - w;
        let seg = egui::Rect::from_min_max(egui::pos2(x.max(bar.left()), bar.top()), egui::pos2((x + w).min(bar.right()), bar.bottom()));
        if seg.width() > 0.0 {
            ui.painter().rect_filled(seg, 2.0, t.accent);
        }
        ctx.request_repaint();
    });
}

/// Whether page `index` should go to sleep now.
fn sleepy(app: &PhotocraftApp, index: usize, now: f64) -> bool {
    if !app.session.can_sleep(index) {
        return false;
    }
    let st = &app.session.documents()[index];
    let idle = if app.page_sleep.weighed.1 { IDLE_PRESSED_S } else { IDLE_S };
    let unseen = app.page_sleep.seen.get(&st.doc.id).is_some_and(|t| now - t >= idle);
    // Its latest changes have a crash recovery checkpoint, where pages are checkpointed.
    let checkpointed = app.services.autosave.is_none()
        || !app.session.prefs().file_handling.autosave
        || (app.services.autosave_unsaved_only && !st.is_dirty())
        || crate::prefs_ui::checkpointed_at(app, st.doc.id) == Some(st.revision);
    unseen && checkpointed && app.session.job_on(st.doc.id).is_none() && !app.session.has_stepped_jobs()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn app() -> PhotocraftApp {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), crate::Services::default());
        app.run("file.new", json!({"width": 40, "height": 30, "name": "uno"})).unwrap();
        app.run("layer.new.layer", json!({})).unwrap();
        app.run("select.rect", json!({"x": 2, "y": 2, "width": 10, "height": 8})).unwrap();
        app.run("edit.fill", json!({"color": "#336699"})).unwrap();
        app.run("file.new", json!({"width": 20, "height": 20, "name": "dos"})).unwrap();
        app.session.edit_prefs(|p| p.performance.release_inactive_pages = ReleasePages::Always);
        app
    }

    /// Runs `tick` and lets the job it started finish.
    fn step(app: &mut PhotocraftApp, now: f64) {
        tick(app, now);
        for j in [app.page_sleep.sleeping, app.page_sleep.waking.map(|w| w.1)].into_iter().flatten() {
            let _ = app.session.wait_job(j);
        }
        let _ = app.session.poll_jobs();
    }

    #[test]
    fn an_unseen_page_sleeps_and_activating_it_wakes_it() {
        let mut app = app();
        let layers = app.session.documents()[0].doc.layer_count();
        step(&mut app, 0.0);
        assert!(!app.session.is_asleep(0), "seen a moment ago");
        app.page_sleep.seen.insert(app.session.documents()[0].doc.id, 0.0);
        step(&mut app, IDLE_S + 1.0);
        assert!(app.session.is_asleep(0), "unseen for a while");
        assert!(!app.session.is_asleep(1), "never the active one");
        assert!(waking(&app).is_none());
        app.session.set_active(0);
        assert_eq!(waking(&app).as_deref(), Some("uno"));
        step(&mut app, IDLE_S + 2.0);
        assert!(!app.session.is_asleep(0), "woken");
        assert_eq!(app.session.documents()[0].doc.layer_count(), layers);
    }

    #[test]
    fn a_sleeping_page_is_let_go() {
        let mut app = app();
        let st = &app.session.documents()[0];
        let doc = std::sync::Arc::downgrade(&st.doc);
        let layer = st.doc.layers.last().and_then(|l| l.surface()).map(|s| s.tiles().next().map(|(_, t)| std::sync::Arc::downgrade(t))).flatten();
        app.page_sleep.seen.insert(st.doc.id, 0.0);
        step(&mut app, IDLE_S + 1.0);
        assert!(app.session.is_asleep(0));
        step(&mut app, IDLE_S + 2.0);
        eprintln!("doc refs left: {}, tile refs left: {:?}", doc.strong_count(), layer.as_ref().map(std::sync::Weak::strong_count));
        assert_eq!(doc.strong_count(), 0, "something still holds the page");
        assert!(layer.is_none_or(|t| t.strong_count() == 0), "something still holds its pixels");
    }

    #[test]
    fn never_means_never() {
        let mut app = app();
        app.session.edit_prefs(|p| p.performance.release_inactive_pages = ReleasePages::Never);
        step(&mut app, 0.0);
        step(&mut app, IDLE_S * 10.0);
        assert!(!app.session.is_asleep(0));
    }

    #[test]
    fn putting_a_page_to_sleep_is_silent() {
        assert!(silent(SLEEP) && !silent(WAKE));
    }
}
