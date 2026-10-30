use super::appearance::{canvas_style, sync_animations, sync_appearance, sync_display_scale, sync_tooltips};
use super::startup::{load, presets_store, recovery, sync_rulers};
use super::{CheckpointStamp, RecoveryContext, recovery_context};
use super::{DocId, HashMap, HashSet, PhotocraftApp, Value, prefs};

/// Per-frame upkeep: theme sync, persistence, autosave and the history log.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.prefs_rt.loaded {
        load(app);
    }
    recovery(app);
    sync_rulers(app);
    sync_display_scale(app, ctx);
    sync_appearance(app, ctx);
    crate::theme::set_ui_font_size(ctx, app.session.prefs().interface.ui_font_size);
    presets_store(app);
    sync_tooltips(app, ctx);
    sync_animations(app, ctx);
    app.sync_recent();
    // Monotonic, not `ctx.input(|i| i.time)`: logic also runs on hidden ticks, where egui's input
    // time is that of the last UI pass.
    if let Some(wait) = persist(app, crate::gpu_canvas::now_ms() / 1000.0) {
        ctx.request_repaint_after(std::time::Duration::try_from_secs_f64(wait).unwrap_or_default());
    }
    let style = canvas_style(app);
    if let Some(gpu) = app.gpu.as_ref()
        && app.prefs_rt.gpu_style != Some(style)
    {
        gpu.set_style(style);
        app.prefs_rt.gpu_style = Some(style);
    }
    autosave(app);
    if let Some(error) = &app.prefs_rt.recovery_error {
        app.ui.status.clone_from(error);
        app.ui.status_error = true;
    }
    if let Some(error) = app.prefs_rt.autosave_errors.values().next() {
        app.ui.status.clone_from(error);
        app.ui.status_error = true;
    }
    if app.services.autosave.is_some() {
        let delay = if app.prefs_rt.pending_autosaves.is_empty() && app.prefs_rt.retiring.is_empty() { 1_000 } else { 100 };
        ctx.request_repaint_after(std::time::Duration::from_millis(delay));
    }
    #[cfg(not(target_arch = "wasm32"))]
    link_updates(app, ctx);
    history_log(app);
}

/// First retry delay after a failed preferences write, in seconds; it doubles with every further
/// failure up to [`SAVE_RETRY_MAX_S`]. A new change retries sooner, but no sooner than this.
pub(super) const SAVE_RETRY_S: f64 = 2.0;

pub(super) const SAVE_RETRY_MAX_S: f64 = 30.0;

/// Failed writes in a row before a notice says preferences aren't being saved.
const SAVE_NOTICE_AFTER: u32 = 3;

/// Retry state of a failed preferences write (times are egui input time, in seconds).
#[derive(Default)]
pub(super) struct SaveRetry {
    failures: u32,
    /// When the next retry is due.
    at: f64,
    /// When the last write was tried, and the revision it carried.
    tried_at: f64,
    tried_rev: u64,
    /// The "not saved" notice, while it's shown.
    notice: Option<u64>,
}

/// Save changed preferences. A failed write stays pending and is retried with a backoff (a newer
/// change retries sooner); the status bar reports the first failure and a notice a persistent
/// one. Returns the seconds until the next retry while one is pending.
pub(super) fn persist(app: &mut PhotocraftApp, now: f64) -> Option<f64> {
    persist_at(app, now, false)
}

/// Wait this long after the last change before writing, at most [`MAX_WAIT_FACTOR`] times it
/// after the first unsaved one (with a debounce set in `Services::prefs_debounce_s`).
const MAX_WAIT_FACTOR: f64 = 4.0;

/// Write pending preference changes now (the page is being hidden, the app closes).
pub fn flush_prefs(app: &mut PhotocraftApp, now: f64) {
    let _ = persist_at(app, now, true);
}

/// Whether preference changes wait to be written.
pub fn prefs_pending(app: &PhotocraftApp) -> bool {
    app.prefs_rt.loaded && app.session.prefs.rev() != app.prefs_rt.saved_rev
}

fn persist_at(app: &mut PhotocraftApp, now: f64, force: bool) -> Option<f64> {
    let rev = app.session.prefs.rev();
    if rev == app.prefs_rt.saved_rev {
        app.prefs_rt.pending_since = None;
        return None;
    }
    // A burst of changes (a slider, a panel drag, the tool memory) is written once, after it.
    let debounce = app.services.prefs_debounce_s;
    if debounce > 0.0 && !force {
        let rt = &mut app.prefs_rt;
        if rt.seen_rev != rev {
            rt.seen_rev = rev;
            rt.changed_at = now;
            rt.pending_since.get_or_insert(now);
        }
        let due = (rt.changed_at + debounce).min(rt.pending_since.unwrap_or(now) + debounce * MAX_WAIT_FACTOR);
        if now < due {
            return Some(due - now);
        }
    }
    let r = &app.prefs_rt.save_retry;
    if r.failures > 0 {
        let due = if rev == r.tried_rev { r.at } else { r.at.min(r.tried_at + SAVE_RETRY_S) };
        if now < due {
            return Some(due - now);
        }
    }
    let ours = app.session.prefs_value();
    // A change that was undone (or set a value to what it already was) leaves nothing to write.
    let written = if app.prefs_rt.saved_value.as_ref() == Some(&ours) { Ok(()) } else { write_prefs(app, &ours) };
    match written {
        Ok(()) => {
            app.prefs_rt.saved_rev = rev;
            app.prefs_rt.saved_value = Some(ours);
            app.prefs_rt.pending_since = None;
            let r = std::mem::take(&mut app.prefs_rt.save_retry);
            if r.failures > 0 {
                let message = "Preferences saved";
                app.ui.status = tl!(message).into();
                app.ui.status_error = false;
                if let Some(id) = r.notice {
                    app.ui.notices.retain(|n| n.id != id);
                }
            }
            None
        }
        Err(e) => {
            let r = &mut app.prefs_rt.save_retry;
            r.failures = r.failures.saturating_add(1);
            (r.tried_at, r.tried_rev) = (now, rev);
            let delay = (SAVE_RETRY_S * 2f64.powi(r.failures.min(16) as i32 - 1)).min(SAVE_RETRY_MAX_S);
            r.at = now + delay;
            let failures = r.failures;
            if failures == 1 {
                let template = "Couldn't save preferences: {error}. Retrying…";
                app.ui.status = crate::i18n::fmt(tl!(template), &[("error", &e)]);
                app.ui.status_error = true;
            } else if failures == SAVE_NOTICE_AFTER {
                let lines = vec![e, "Capishot keeps retrying; until a save succeeds, preference changes are lost when it closes.".into()];
                let id = crate::notices::post(app, "Preferences can't be saved", lines, true, None);
                app.prefs_rt.save_retry.notice = Some(id);
            }
            Some(delay)
        }
    }
}

/// Write `ours` over the stored preferences, keeping what another instance (a second browser
/// tab, another app window) changed there since this one last loaded or saved them.
fn write_prefs(app: &mut PhotocraftApp, ours: &Value) -> Result<(), String> {
    if app.services.save_prefs.is_none() {
        return Ok(());
    }
    let compact = app.services.prefs_compact;
    let to_text = |v: &Value| if compact { serde_json::to_string(v) } else { serde_json::to_string_pretty(v) };
    let stored_text = app.services.load_prefs.as_mut().and_then(|load| load());
    // Storage still holds what this instance last wrote (or loaded): merge into that tree as it
    // is held, rather than parsing the text again.
    let unchanged = stored_text.is_some() && stored_text == app.prefs_rt.saved_text;
    let held = app.prefs_rt.saved_tree.take();
    let stored = if unchanged && held.is_some() { held } else { stored_text.and_then(|t| serde_json::from_str::<Value>(&t).ok()) };
    let (tree, text) = match (stored, &app.prefs_rt.saved_value) {
        (Some(mut theirs @ Value::Object(_)), Some(base)) => {
            let base = &without_stale_migration(base, &theirs);
            merge_changes(base, ours, &mut theirs);
            let text = to_text(&theirs);
            (theirs, text)
        }
        _ => (ours.clone(), to_text(ours)),
    };
    let text = text.map_err(|e| e.to_string())?;
    let Some(save) = app.services.save_prefs.as_mut() else { return Ok(()) };
    save(&text)?;
    app.prefs_rt.saved_text = Some(text);
    app.prefs_rt.saved_tree = Some(tree);
    Ok(())
}

/// The save base without the shift tool switch migration (#5647) once storage already holds a
/// version 2 file: another instance wrote it, so its Use Shift Key for Tool Switch is a choice,
/// not the old default this instance migrated. The base then holds the migrated value (on) instead
/// of the old file's off, so only a change made here since loading is written over that choice.
fn without_stale_migration(base: &Value, theirs: &Value) -> Value {
    let mut base = base.clone();
    let new_file = |v: &Value| v.get("version").and_then(Value::as_u64).is_some_and(|v| v >= 2);
    if new_file(theirs) && !new_file(&base) {
        let key = "useShiftKeyForToolSwitch";
        if let Some(tools) = base.get_mut("tools").and_then(Value::as_object_mut)
            && tools.get(key) == Some(&Value::Bool(false))
        {
            tools.insert(key.into(), Value::Bool(true));
        }
    }
    base
}

/// Three-way merge of preference trees: whatever `ours` changed since `base` goes over `theirs`
/// (what storage holds now) and the rest of `theirs` is kept. Objects merge key by key; any
/// other value, arrays included, is replaced whole.
pub(super) fn merge_changes(base: &Value, ours: &Value, theirs: &mut Value) {
    if ours == base {
        return;
    }
    match (base, ours, theirs) {
        (Value::Object(b), Value::Object(o), Value::Object(t)) => {
            for (k, ov) in o {
                match (b.get(k), t.get_mut(k)) {
                    (Some(bv), Some(tv)) => merge_changes(bv, ov, tv),
                    // Removed there, unchanged here: it stays removed.
                    (Some(bv), None) if bv == ov => {}
                    _ => {
                        t.insert(k.clone(), ov.clone());
                    }
                }
            }
            for k in b.keys().filter(|k| !o.contains_key(*k)) {
                t.remove(k);
            }
        }
        (_, _, theirs) => *theirs = ours.clone(),
    }
}

/// Write the preferences now, through the same merge as [`persist`], for recovery choices that
/// need the result immediately. The pending revision stays unsaved when the write fails.
pub(crate) fn save_preferences(app: &mut PhotocraftApp) -> Result<(), String> {
    let rev = app.session.prefs.rev();
    let ours = app.session.prefs_value();
    write_prefs(app, &ours)?;
    app.prefs_rt.saved_rev = rev;
    app.prefs_rt.saved_value = Some(ours);
    if let Some(id) = std::mem::take(&mut app.prefs_rt.save_retry).notice {
        app.ui.notices.retain(|n| n.id != id);
    }
    Ok(())
}

/// Checkpoint every open document, including its history after a normal Save.
/// Retire checkpoints only after the user has accepted the entire quit action.
pub(crate) fn retire_all(app: &mut PhotocraftApp) {
    retire_checkpoints(app, true);
}

fn retire_checkpoints(app: &mut PhotocraftApp, quitting: bool) {
    let now = crate::gpu_canvas::now_ms();
    let live: HashSet<DocId> = if quitting { HashSet::new() } else { app.session.documents().iter().map(|d| d.doc.id).collect() };
    let owned: HashSet<DocId> = app
        .prefs_rt
        .recovery_keys
        .keys()
        .chain(app.prefs_rt.autosaved.keys())
        .chain(app.prefs_rt.pending_autosaves.keys())
        .chain(app.prefs_rt.autosave_errors.keys())
        .copied()
        .collect();
    for id in owned {
        if live.contains(&id) || app.prefs_rt.retiring.contains(&id) || (!quitting && app.prefs_rt.retry_after.get(&id).is_some_and(|time| now < *time)) {
            continue;
        }
        if let Some(discard) = app.services.discard_autosave.as_mut() {
            match discard(id.0, app.prefs_rt.recovery_keys.get(&id).map(String::as_str)) {
                Ok(()) => {
                    app.prefs_rt.retiring.insert(id);
                }
                Err(e) => {
                    app.prefs_rt.retry_after.insert(id, now + 30_000.0);
                    app.prefs_rt.autosave_errors.insert(id, format!("Could not remove recovery checkpoint: {e}"));
                }
            }
        }
    }
}

/// Linked smart objects follow their files while the document is open, as in Photoshop: every two
/// seconds, and as soon as the window comes back to the front (after saving in another app), stamp
/// the active document's linked files on a worker thread and re-render the layers of the ones that
/// changed ([`photocraft_engine::link_watch`]).
#[cfg(not(target_arch = "wasm32"))]
fn link_updates(app: &mut PhotocraftApp, ctx: &egui::Context) {
    match app.session.poll_link_scan() {
        Some(Ok(r)) => {
            if let Some(n) = r["updated"].as_array().map(Vec::len).filter(|n| *n > 0) {
                app.ui.status = crate::i18n::trn(
                    crate::i18n::current(),
                    n as u64,
                    "Updated {n} linked smart object changed on disk",
                    "Updated {n} linked smart objects changed on disk",
                );
            }
        }
        Some(Err(e)) => app.ui.status = e.to_string(),
        None => {}
    }
    let (last_key, focus_key) = (egui::Id::unique("linkWatch.lastScan"), egui::Id::unique("linkWatch.focused"));
    let now = crate::gpu_canvas::now_ms() / 1000.0;
    let focused = ctx.input(|i| i.focused);
    let was_focused: bool = ctx.data(|d| d.get_temp(focus_key)).unwrap_or(focused);
    ctx.data_mut(|d| d.insert_temp(focus_key, focused));
    let has_links = app.session.active().is_some_and(|d| !photocraft_engine::link_watch::linked_files(&d.doc).is_empty());
    if !has_links {
        return;
    }
    let last: f64 = ctx.data(|d| d.get_temp(last_key)).unwrap_or(f64::NEG_INFINITY);
    if now - last >= 2.0 || (focused && !was_focused) {
        ctx.data_mut(|d| d.insert_temp(last_key, now));
        app.session.start_link_scan();
    }
    let wait = if app.session.link_scan.is_some() { 100 } else { 2000 };
    ctx.request_repaint_after(std::time::Duration::from_millis(wait));
}

/// Background autosave of documents with unsaved changes every N minutes (File Handling).
fn autosave(app: &mut PhotocraftApp) {
    if app.allow_close {
        return;
    }
    // A successful queue operation is not a successful disk write. Poll each frame,
    // including frames before the next autosave interval, so failures are visible.
    let now = crate::gpu_canvas::now_ms();
    if let Some(poll) = app.services.poll_autosave.as_mut() {
        for completion in poll() {
            let id = DocId(completion.document_id);
            if completion.retired {
                app.prefs_rt.retiring.remove(&id);
                match completion.result {
                    Ok(()) => {
                        app.prefs_rt.recovery_keys.remove(&id);
                        app.prefs_rt.autosaved.remove(&id);
                        app.prefs_rt.autosaved_at.remove(&id);
                        app.prefs_rt.pending_autosaves.remove(&id);
                        app.prefs_rt.catch_up.remove(&id);
                        app.prefs_rt.retry_after.remove(&id);
                        if let Some(error) = app.prefs_rt.autosave_errors.remove(&id)
                            && app.ui.status == error
                        {
                            app.ui.status = "Recovery checkpoint removed".into();
                            app.ui.status_error = false;
                        }
                    }
                    Err(e) => {
                        app.prefs_rt.retry_after.insert(id, now + 30_000.0);
                        app.prefs_rt.autosave_errors.insert(id, format!("Could not remove recovery checkpoint: {e}"));
                    }
                }
                continue;
            }
            // A result acknowledges exactly the captured RAM snapshot, never the live revision.
            let pending = app.prefs_rt.pending_autosaves.get(&id).filter(|stamp| stamp.revision == completion.revision).cloned();
            if let Some(stamp) = pending {
                app.prefs_rt.pending_autosaves.remove(&id);
                match completion.result {
                    Ok(()) => {
                        app.prefs_rt.autosaved.insert(id, stamp);
                        app.prefs_rt.retry_after.remove(&id);
                        app.prefs_rt.autosaved_at.insert(id, now);
                        if let Some(error) = app.prefs_rt.autosave_errors.remove(&id)
                            && app.ui.status == error
                        {
                            app.ui.status = "Recovery checkpoint saved".into();
                            app.ui.status_error = false;
                        }
                    }
                    Err(e) => {
                        app.prefs_rt.retry_after.insert(id, now + 30_000.0);
                        app.prefs_rt.autosave_errors.insert(id, format!("Autosave failed: {e}. Your document remains open; save a copy or free disk space."));
                    }
                }
            }
        }
    }
    retire_checkpoints(app, false);
    let fh = &app.session.prefs().file_handling;
    if !fh.autosave || app.services.autosave.is_none() {
        return;
    }
    let interval = f64::from(fh.autosave_minutes.max(1)) * 60_000.0;
    if app.prefs_rt.next_autosave_ms == 0.0 || app.prefs_rt.next_autosave_ms > now + interval {
        app.prefs_rt.next_autosave_ms = now + interval;
    }
    let due = now >= app.prefs_rt.next_autosave_ms;
    if due {
        app.prefs_rt.next_autosave_ms = now + interval;
    }
    if due {
        for (index, st) in app.session.documents().iter().enumerate() {
            if app.prefs_rt.pending_autosaves.get(&st.doc.id).is_some_and(|pending| *pending != CheckpointStamp::of(st, recovery_context(app, index, st))) {
                app.prefs_rt.catch_up.insert(st.doc.id);
            }
        }
    }
    if !due && app.prefs_rt.catch_up.is_empty() && !app.prefs_rt.retry_after.values().any(|time| now >= *time) {
        return;
    }
    let jobs: Vec<_> = app
        .session
        .documents()
        .iter()
        .enumerate()
        .filter_map(|(index, st)| {
            // The browser keeps checkpoints of unsaved work only: a document written to its file
            // (or downloaded) at this revision needs none.
            if app.services.autosave_unsaved_only && (!st.is_dirty() || app.prefs_rt.written_at.get(&st.doc.id) == Some(&st.revision)) {
                return None;
            }
            let stamp = CheckpointStamp::of(st, recovery_context(app, index, st));
            let retry_due = app.prefs_rt.retry_after.get(&st.doc.id).is_some_and(|time| now >= *time);
            if (!due && !retry_due && !app.prefs_rt.catch_up.contains(&st.doc.id))
                || app.prefs_rt.pending_autosaves.contains_key(&st.doc.id)
                || app.prefs_rt.autosaved.get(&st.doc.id) == Some(&stamp)
                || app.prefs_rt.retry_after.get(&st.doc.id).is_some_and(|time| now < *time)
            {
                return None;
            }
            Some((st.doc.clone(), st.history.checkpoint(), stamp, st.path.clone()))
        })
        .collect();
    for (doc, history, stamp, path) in jobs {
        if let Some(save) = app.services.autosave.as_mut() {
            match save(&doc, history, stamp.revision, path.as_deref(), app.prefs_rt.recovery_keys.get(&doc.id).map(String::as_str), stamp.context.clone()) {
                Ok(()) => {
                    app.prefs_rt.pending_autosaves.insert(doc.id, stamp);
                    app.prefs_rt.catch_up.remove(&doc.id);
                }
                Err(e) => {
                    app.prefs_rt.retry_after.insert(doc.id, now + 30_000.0);
                    app.prefs_rt.autosave_errors.insert(doc.id, format!("Autosave failed: {e}. Your document remains open; save a copy or free disk space."));
                }
            }
        }
    }
}

/// The status bar's autosave note for the active document: when its last checkpoint was stored,
/// while it has one.
pub(crate) fn autosave_hint(app: &PhotocraftApp) -> Option<String> {
    app.services.autosave.as_ref()?;
    let st = app.session.active()?;
    let at = *app.prefs_rt.autosaved_at.get(&st.doc.id)?;
    let now = crate::gpu_canvas::now_ms();
    Some(crate::i18n::fmt(tl!("Autosaved {when}"), &[("when", &crate::recovery_ui::ago(at, now))]))
}

/// Document `doc` was written to its own file at `revision` (a Save, not a copy). Where only
/// unsaved work is checkpointed ([`crate::Services::autosave_unsaved_only`], the browser), its
/// checkpoint is removed now and none is written until it changes again.
pub(crate) fn document_written(app: &mut PhotocraftApp, doc: DocId, revision: u64) {
    app.prefs_rt.written_at.insert(doc, revision);
    if !app.services.autosave_unsaved_only {
        return;
    }
    let owned = app.prefs_rt.recovery_keys.contains_key(&doc)
        || app.prefs_rt.autosaved.contains_key(&doc)
        || app.prefs_rt.pending_autosaves.contains_key(&doc)
        || app.prefs_rt.autosave_errors.contains_key(&doc);
    if !owned || app.prefs_rt.retiring.contains(&doc) {
        return;
    }
    if let Some(discard) = app.services.discard_autosave.as_mut() {
        match discard(doc.0, app.prefs_rt.recovery_keys.get(&doc).map(String::as_str)) {
            Ok(()) => {
                app.prefs_rt.retiring.insert(doc);
            }
            Err(e) => {
                app.prefs_rt.retry_after.insert(doc, crate::gpu_canvas::now_ms() + 30_000.0);
                app.prefs_rt.autosave_errors.insert(doc, format!("Could not remove recovery checkpoint: {e}"));
            }
        }
    }
}

/// Names of the documents with unsaved changes whose recovery checkpoint isn't confirmed on disk
/// at their current revision.
pub(crate) fn recovery_pending(app: &PhotocraftApp) -> Vec<String> {
    app.session
        .documents()
        .iter()
        .filter(|d| d.is_dirty() && app.prefs_rt.autosaved.get(&d.doc.id).is_none_or(|s| s.revision < d.revision))
        .map(|d| d.doc.name.clone())
        .collect()
}

/// Force the autosave timer to fire on the next tick (tests, `prefs` changes).
pub fn autosave_now(app: &mut PhotocraftApp) {
    app.prefs_rt.next_autosave_ms = f64::MIN_POSITIVE;
}

/// History Log preference: append executed commands to a text file.
fn history_log(app: &mut PhotocraftApp) {
    let n = app.session.journal.len();
    if n <= app.prefs_rt.log_len {
        app.prefs_rt.log_len = n;
        return;
    }
    let hl = app.session.prefs().history_log.clone();
    let start = app.prefs_rt.log_len;
    app.prefs_rt.log_len = n;
    if !hl.enabled || hl.file_path.is_empty() || hl.destination == prefs::LogDestination::Metadata {
        return;
    }
    let Some(append) = app.services.append_text.as_mut() else { return };
    let mut text = String::new();
    for (id, params) in &app.session.journal[start..] {
        let label = photocraft_engine::commands::find(id).map_or(id.as_str(), |c| c.label).trim_end_matches('…');
        match hl.detail {
            prefs::LogDetail::SessionsOnly => {
                if matches!(id.as_str(), "file.new" | "file.open" | "file.close" | "file.openAs") {
                    text.push_str(&format!("{label}\n"));
                }
            }
            prefs::LogDetail::Concise => text.push_str(&format!("{label}\n")),
            prefs::LogDetail::Detailed => text.push_str(&format!("{label}  {id} {params}\n")),
        }
    }
    if !text.is_empty() {
        let _ = append(&hl.file_path, &text);
    }
}
