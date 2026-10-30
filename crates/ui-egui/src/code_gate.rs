//! Commands whose code hasn't arrived yet (the web build loads part of its module after the
//! first frame, see packaging/web-split): instead of the page fetching that code synchronously
//! (a stall, and a dead page if the network drops), such a command waits on a job card ("Loading
//! Capishot…", with progress and Esc to cancel) and runs once the code is in. A failed load
//! drops the command with a notice whose Retry runs it again. Control requests that run such a
//! command are held, in order, until then. Without [`CodeWait`] (the desktop app, tests) every
//! command runs at once.

use std::sync::{Arc, Mutex};

use photocraft_engine::jobs::{Completion, JobCtx, JobEvent, JobId, JobOutcome, Started};
use serde_json::{Value, json};

use crate::PhotocraftApp;
use crate::control::ControlRequest;

/// The job's command id (`jobs.list`).
pub const JOB: &str = "app.loadCode";
/// The notice's Retry: runs the commands the failed load dropped.
pub const RETRY: &str = "app.loadCode.retry";

/// When the rest of the code is in: progress on the job, then `Ok` or the reason it failed.
pub type CodeDone = Box<dyn FnOnce(Result<(), String>)>;

/// The platform side (the web build's chunk loader).
pub struct CodeWait {
    /// Whether command `id` (or `open:<extension>` for opening a file) can run now: the code is
    /// all in, or what it runs is in the first part.
    pub ready: Box<dyn Fn(&str) -> bool>,
    /// Load the rest of the code, reporting progress on `ctx` (already loading: wait for it).
    pub wait: Box<dyn Fn(JobCtx, CodeDone)>,
}

/// The app's waiting commands.
#[derive(Default)]
pub struct Waiting {
    /// The load job, while it runs.
    job: Option<JobId>,
    /// Interactive commands to run once it is done.
    commands: Vec<(String, Value)>,
    /// Control requests held until it is done, in arrival order.
    pub(crate) requests: Vec<ControlRequest>,
    /// What a failed load dropped, for the notice's Retry.
    dropped: Vec<(String, Value)>,
    /// Some file opens wait for the load (the platform keeps the files; [`files_ready`]). A
    /// failed load leaves them waiting: the next frame starts another.
    files: bool,
}

/// Whether `id` can run now (always without a [`CodeWait`]).
pub fn ready(app: &PhotocraftApp, id: &str) -> bool {
    app.services.code_wait.as_ref().is_none_or(|w| (w.ready)(id))
}

/// Opening a file named `name`: whether it can be decoded now. When not, the load starts and
/// the caller keeps the file until [`files_ready`] says so.
pub fn open_ready(app: &mut PhotocraftApp, name: &str) -> bool {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if ready(app, &format!("open:{ext}")) {
        return true;
    }
    app.code_waiting.files = true;
    start(app).is_err()
}

/// After [`open_ready`] said no: whether the files may go now (the load ended, either way).
pub fn files_ready(app: &mut PhotocraftApp) -> bool {
    if app.code_waiting.files && app.code_waiting.job.is_none() {
        app.code_waiting.files = false;
    }
    !app.code_waiting.files
}

/// A command from the menus, a shortcut or the palette ([`crate::menus::invoke`]): `Some` when it
/// waits for the code (its result is the job's `{job, pending}`).
pub(crate) fn intercept(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == RETRY {
        let dropped = std::mem::take(&mut app.code_waiting.dropped);
        if dropped.is_empty() {
            return Some(Ok(Value::Null));
        }
        return Some(start(app).map(|job| {
            app.code_waiting.commands.extend(dropped);
            json!({"job": job.0, "pending": true})
        }));
    }
    // Control requests were held before they got here. Picking a tool, zooming and showing a
    // panel only change the interface: never kept waiting.
    if app.automation_input || ui_only(id) || ready(app, id) {
        return None;
    }
    Some(start(app).map(|job| {
        app.code_waiting.commands.push((id.to_string(), params.clone()));
        json!({"job": job.0, "pending": true})
    }))
}

fn ui_only(id: &str) -> bool {
    crate::tool_shortcuts::tool(id).is_some() || id.starts_with("view.") || id.starts_with("window.")
}

/// A control request that runs a command whose code isn't in: hold it (and every request after
/// it, so they keep their order). `Err` gives the request back to run now.
pub(crate) fn hold(app: &mut PhotocraftApp, req: ControlRequest) -> Result<(), ControlRequest> {
    if app.code_waiting.requests.is_empty() {
        let id = matches!(req.method.as_str(), "engine.execute" | "ui.menu.invoke")
            .then(|| req.params.get("command").or_else(|| req.params.get("id")).and_then(Value::as_str))
            .flatten();
        if id.is_none_or(|id| id == RETRY || ui_only(id) || ready(app, id)) || start(app).is_err() {
            return Err(req);
        }
    }
    app.code_waiting.requests.push(req);
    Ok(())
}

/// The held control requests once the load has ended (`None` while it runs).
pub(crate) fn released(app: &mut PhotocraftApp) -> Option<Vec<ControlRequest>> {
    if app.code_waiting.requests.is_empty() || app.code_waiting.job.is_some() {
        return None;
    }
    Some(std::mem::take(&mut app.code_waiting.requests))
}

/// The load job (started if none runs).
fn start(app: &mut PhotocraftApp) -> Result<JobId, String> {
    if let Some(job) = app.code_waiting.job {
        return Ok(job);
    }
    let Some(wait) = app.services.code_wait.as_ref() else { return Err("no code loader".into()) };
    // The job's work hands its context and completion back here; the loader runs on this thread.
    let slot: Arc<Mutex<Option<(JobCtx, Completion<()>)>>> = Arc::default();
    let s = slot.clone();
    let started = app.session.start_async_job(JOB, json!({}), &tl!("Loading Capishot…"), move |ctx, done| {
        if let Ok(mut s) = s.lock() {
            *s = Some((ctx.clone(), done));
        }
    }, |_, ()| Ok(Value::Null));
    let job = match started {
        Ok(Started::Job(job)) => job,
        Ok(Started::Done(_)) => return Err("the load ended at once".into()),
        Err(e) => return Err(e.to_string()),
    };
    // In the browser the work ran inline; natively (tests) on a thread that is about to.
    let mut taken = slot.lock().ok().and_then(|mut s| s.take());
    #[cfg(not(target_arch = "wasm32"))]
    for _ in 0..2000 {
        if taken.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
        taken = slot.lock().ok().and_then(|mut s| s.take());
    }
    if let Some((ctx, done)) = taken {
        (wait.wait)(ctx, Box::new(move |r: Result<(), String>| done.finish(r.map_err(photocraft_engine::EngineError::Other))));
    }
    app.code_waiting.job = Some(job);
    Ok(job)
}

/// A job ended: the load's end runs (or drops) what waited for it. True when it was the load.
pub(crate) fn on_event(app: &mut PhotocraftApp, ctx: &egui::Context, e: &JobEvent) -> bool {
    if app.code_waiting.job != Some(e.id) {
        return false;
    }
    app.code_waiting.job = None;
    ctx.request_repaint();
    let commands = std::mem::take(&mut app.code_waiting.commands);
    match &e.outcome {
        JobOutcome::Done(_) => {
            for (id, params) in commands {
                if let Err(err) = crate::menus::invoke(app, ctx, &id, params) {
                    app.ui.status = err;
                    app.ui.status_error = true;
                }
            }
        }
        JobOutcome::Failed(err) => {
            log::warn!("Capishot: a part of the app didn't load: {err}");
            app.code_waiting.dropped = commands;
            // One notice however many loads fail (file opens keep retrying).
            if !app.ui.notices.iter().any(|n| n.tag.as_deref() == Some(JOB)) {
                let id = crate::notices::post(app, "Couldn't load a part of Capishot (network). Try again", Vec::new(), true, None);
                if let Some(n) = app.ui.notices.iter_mut().find(|n| n.id == id) {
                    n.actions.push(crate::notices::NoticeAction { label: "Retry".into(), command: RETRY.into() });
                    n.tag = Some(JOB.into());
                }
            }
        }
        JobOutcome::Cancelled => {}
    }
    // Held control requests get the load's error rather than running code that isn't there.
    if !matches!(e.outcome, JobOutcome::Done(_)) {
        let error = match &e.outcome {
            JobOutcome::Failed(err) => format!("a part of the app didn't load: {err}"),
            _ => "cancelled".to_string(),
        };
        for req in std::mem::take(&mut app.code_waiting.requests) {
            let _ = req.reply.send(json!({"ok": false, "error": error}));
        }
    }
    true
}

#[cfg(test)]
#[path = "code_gate_tests.rs"]
mod tests;
