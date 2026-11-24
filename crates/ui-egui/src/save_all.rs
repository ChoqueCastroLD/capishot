//! File › Save All: saves every open document with unsaved changes, one after another, each the
//! way File › Save would (its own file and format, the Save As dialog for one never saved, the
//! format prompts, the flatten and lossy-format warnings, background save jobs, the web's
//! download or File System Access write). Clean documents are left alone.
//!
//! egui is immediate-mode, so the run is a queue polled every frame ([`tick`]): a document's
//! Save is started once nothing else is in the way (a file dialog, a format prompt, a save job,
//! a sleeping page waking), and its outcome is read once its Save is done. Cancelling a Save As
//! dialog or a prompt skips that document only. A notice sums it up at the end.

use photocraft_doc::DocId;
use serde_json::{Value, json};

use crate::PhotocraftApp;

pub const COMMAND: &str = "file.saveAll";

/// A Save All in progress.
#[derive(Default)]
pub struct Run {
    /// Documents still to save, in tab order.
    queue: std::collections::VecDeque<DocId>,
    current: Option<Current>,
    total: usize,
    saved: usize,
    /// (document name, why).
    failed: Vec<(String, String)>,
    skipped: Vec<(String, String)>,
    /// The tab active when Save All started, shown again at the end.
    restore: Option<DocId>,
}

/// The document whose Save is running.
struct Current {
    doc: DocId,
    name: String,
    /// Its Save was started (else it is waiting for its page to wake).
    started: bool,
    /// The save wrote it (a confirmed write, or a web download).
    written: bool,
    error: Option<String>,
    cancelled: bool,
    /// The status bar when its Save started, to tell a new error from an old one.
    status_before: String,
}

/// Whether Save All has anything to do: some open document has unsaved changes.
pub fn available(app: &PhotocraftApp) -> bool {
    app.save_all.is_none() && app.services.export.is_some() && app.session.visible_documents().iter().any(|d| d.is_dirty())
}

/// File › Save All: queues every document with unsaved changes and starts on the first.
pub fn start(app: &mut PhotocraftApp, ctx: &egui::Context) -> Result<Value, String> {
    if app.save_all.is_some() {
        return Err(tl!("Save All is already running").into());
    }
    let queue: std::collections::VecDeque<DocId> = app.session.visible_documents().iter().filter(|d| d.is_dirty()).map(|d| d.doc.id).collect();
    if queue.is_empty() {
        return Err(tl!("No open document has unsaved changes").into());
    }
    let total = queue.len();
    app.save_all = Some(Run { queue, total, restore: app.session.active().map(|d| d.doc.id), ..Run::default() });
    step(app, ctx);
    Ok(json!({"documents": total}))
}

/// Every frame: moves the run on once the document being saved is done.
pub fn tick(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.save_all.is_some() {
        step(app, ctx);
        if app.save_all.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

/// A save of `doc` was written (`PhotocraftApp::saved`).
pub(crate) fn written(app: &mut PhotocraftApp, doc: DocId) {
    if let Some(c) = current_for(app, doc) {
        c.written = true;
    }
}

/// A save of `doc` failed after it started (a background job, a write after a format prompt).
pub(crate) fn failed(app: &mut PhotocraftApp, doc: DocId, error: &str) {
    if let Some(c) = current_for(app, doc) {
        c.error = Some(error.to_string());
    }
}

/// A background save of `doc` was cancelled.
pub(crate) fn cancelled(app: &mut PhotocraftApp, doc: DocId) {
    if let Some(c) = current_for(app, doc) {
        c.cancelled = true;
    }
}

fn current_for(app: &mut PhotocraftApp, doc: DocId) -> Option<&mut Current> {
    app.save_all.as_mut()?.current.as_mut().filter(|c| c.doc == doc)
}

/// Something a Save is still waiting on: a file dialog (or its replace question), a format or
/// warning prompt, a background save, the prompt about a file changed on disk.
fn busy(app: &PhotocraftApp) -> bool {
    app.file_dialog_open() || app.save_options_open() || app.saving() || app.ui.dialogs.iter().any(|d| crate::disk_change_prompt::owns(&d.fields))
}

fn index_of(app: &PhotocraftApp, doc: DocId) -> Option<usize> {
    app.session.visible_documents().iter().position(|d| d.doc.id == doc)
}

fn step(app: &mut PhotocraftApp, ctx: &egui::Context) {
    loop {
        if busy(app) {
            return;
        }
        let Some(run) = app.save_all.as_mut() else { return };
        if let Some(c) = run.current.as_ref() {
            if !c.started {
                let doc = c.doc;
                // A page put to sleep wakes once it is the active one (`page_sleep`); Save runs on
                // the woken document, never on its stub.
                match index_of(app, doc) {
                    None => {}
                    Some(i) if app.session.documents()[i].sleeping.is_some() => {
                        if app.session.active_index() != Some(i) {
                            app.session.set_active(i);
                        }
                        return;
                    }
                    Some(_) => {
                        begin(app, ctx, doc);
                        continue;
                    }
                }
            }
            finish_current(app);
            continue;
        }
        let Some(doc) = run.queue.pop_front() else {
            end(app);
            return;
        };
        let Some(i) = index_of(app, doc) else {
            // Closed meanwhile: nothing to save.
            continue;
        };
        let st = &app.session.documents()[i];
        if !st.is_dirty() {
            // Saved meanwhile (by hand, or by a hook).
            continue;
        }
        let name = st.doc.name.clone();
        let asleep = st.sleeping.is_some();
        if let Some(run) = app.save_all.as_mut() {
            run.current = Some(Current { doc, name, started: false, written: false, error: None, cancelled: false, status_before: String::new() });
        }
        app.session.set_active(i);
        if asleep {
            return;
        }
        begin(app, ctx, doc);
    }
}

/// Runs File › Save on `doc`, now the active document.
fn begin(app: &mut PhotocraftApp, ctx: &egui::Context, doc: DocId) {
    let status_before = app.ui.status.clone();
    if let Some(c) = current_for(app, doc) {
        c.started = true;
        c.status_before = status_before;
    }
    let r = crate::menus::invoke_unguarded(app, ctx, "file.save", json!({}));
    match r {
        Err(e) if e == crate::file_dialog::CANCELLED => cancelled(app, doc),
        Err(e) => failed(app, doc, &e),
        Ok(_) if app.file_dialog_open() => {
            // A document never saved (or whose file can't hold it) asks where, as Save does.
            app.after_file_dialog(move |app, r| {
                match &r {
                    Err(e) if e == crate::file_dialog::CANCELLED => cancelled(app, doc),
                    Err(e) => failed(app, doc, e),
                    Ok(_) => {}
                }
                r
            });
            if let Some(run) = app.save_all.as_ref() {
                let n = run.total - run.queue.len();
                let name = run.current.as_ref().map(|c| c.name.clone()).unwrap_or_default();
                let template = "Save All ({n} of {total}): choose where to save \u{201c}{name}\u{201d}";
                app.ui.status = crate::i18n::fmt(tl!(template), &[("n", &n.to_string()), ("total", &run.total.to_string()), ("name", &name)]);
                app.ui.status_error = false;
            }
        }
        Ok(_) => {}
    }
}

/// Reads how the current document's Save went.
fn finish_current(app: &mut PhotocraftApp) {
    let Some(c) = app.save_all.as_mut().and_then(|r| r.current.take()) else { return };
    let clean = index_of(app, c.doc).is_some_and(|i| !app.session.documents()[i].is_dirty());
    let open = index_of(app, c.doc).is_some();
    // An error the Save reported only in the status bar (a prompt's Replace that failed).
    let reported = (app.ui.status_error && !app.ui.status.is_empty() && app.ui.status != c.status_before).then(|| app.ui.status.clone());
    let Some(run) = app.save_all.as_mut() else { return };
    if c.written || clean {
        run.saved += 1;
    } else if let Some(e) = c.error.or(reported) {
        run.failed.push((c.name, e));
    } else if !open {
        run.skipped.push((c.name, tl!("it was closed").into()));
    } else if c.cancelled {
        run.skipped.push((c.name, tl!("cancelled").into()));
    } else {
        run.skipped.push((c.name, tl!("not saved").into()));
    }
}

/// The run is over: the first tab comes back and a notice says what happened.
fn end(app: &mut PhotocraftApp) {
    let Some(run) = app.save_all.take() else { return };
    if let Some(i) = run.restore.and_then(|d| index_of(app, d)) {
        app.session.set_active(i);
    }
    let lang = crate::i18n::current();
    let n = run.saved.to_string();
    let title = crate::i18n::fmt(&crate::i18n::trn(lang, run.saved as u64, "Saved {n} document", "Saved {n} documents"), &[("n", &n)]);
    let mut lines = Vec::new();
    for (name, why) in &run.failed {
        lines.push(crate::i18n::fmt(tl!("\u{201c}{name}\u{201d} failed: {reason}"), &[("name", name), ("reason", why)]));
    }
    for (name, why) in &run.skipped {
        lines.push(crate::i18n::fmt(tl!("\u{201c}{name}\u{201d} skipped: {reason}"), &[("name", name), ("reason", why)]));
    }
    let mut status = title.clone();
    if !run.failed.is_empty() {
        let f = run.failed.len().to_string();
        status = format!("{status}; {}", crate::i18n::fmt(tl!("{n} failed"), &[("n", &f)]));
    }
    if !run.skipped.is_empty() {
        let s = run.skipped.len().to_string();
        status = format!("{status}; {}", crate::i18n::fmt(tl!("{n} skipped"), &[("n", &s)]));
    }
    let error = !run.failed.is_empty();
    app.ui.status = status;
    app.ui.status_error = error;
    crate::notices::post(app, title, lines, error, None);
}

#[cfg(test)]
mod tests;
