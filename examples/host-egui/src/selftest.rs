//! `--selftest <dir>` (native): the contract run in the real app, frame by frame, with the canvas
//! drawing: open, tool, Remove with AI, one undo, a text, save. Each step leaves a screenshot in
//! `<dir>`; `<dir>/selftest.json` gets the results, and the app exits (code 1 if a check failed).
//! Every frame it also checks that the canvas widget left the host's egui style and fonts alone.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use capishot_host::{DocumentId, Event, JobEnd, LayerId, OpenOptions};
use serde_json::{Value, json};

use crate::app::HostApp;
use crate::script::{self, Removal};

/// Frames each step waits for the canvas to show its result before the screenshot.
const SETTLE: u32 = 6;
/// How long Remove with AI may take (a CPU run of Manga LaMa on a slow machine).
const AI_TIMEOUT: Duration = Duration::from_secs(20 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Open,
    Tool,
    Remove,
    Removing,
    Removed,
    Undone,
    Text,
    /// The whole Capishot interface in the same rect, on the same document.
    Full,
    /// Back to the canvas alone.
    Back,
    Done,
}

/// The run's state.
pub struct SelfTest {
    dir: PathBuf,
    step: Step,
    /// Frames in this step.
    frames: u32,
    frame: u64,
    since: Instant,
    /// The host's style and fonts as it set them, before the canvas drew.
    look: Option<(Arc<egui::Style>, egui::FontDefinitions)>,
    results: serde_json::Map<String, Value>,
    failures: Vec<String>,
    page: Option<LayerId>,
    before: Vec<u8>,
    undo_steps: Option<usize>,
    after_remove: Option<usize>,
    tool_seen: bool,
    job_end: Option<JobEnd>,
    removal: Option<Removal>,
    shots: Arc<Mutex<Vec<String>>>,
    asked: usize,
    /// `DocumentReady` reports.
    ready: Vec<DocumentId>,
    /// The host's styles and theme preference before Capishot's full interface first drew.
    host_look: Option<HostLook>,
}

/// What of the host's look the full interface must leave as it is.
#[derive(PartialEq)]
struct HostLook {
    light: egui::Style,
    dark: egui::Style,
    theme: egui::ThemePreference,
}

impl HostLook {
    fn of(ctx: &egui::Context) -> Self {
        HostLook { light: (*ctx.style_of(egui::Theme::Light)).clone(), dark: (*ctx.style_of(egui::Theme::Dark)).clone(), theme: ctx.options(|o| o.theme_preference) }
    }
}

impl SelfTest {
    /// `Some` when the command line has `--selftest <dir>`.
    pub fn from_args() -> Option<SelfTest> {
        let mut args = std::env::args().skip_while(|a| a != "--selftest");
        args.next()?;
        let dir = PathBuf::from(args.next().unwrap_or_else(|| "selftest".into()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("{}: {e}", dir.display());
        }
        Some(SelfTest {
            dir,
            step: Step::Open,
            frames: 0,
            frame: 0,
            since: Instant::now(),
            look: None,
            results: serde_json::Map::new(),
            failures: Vec::new(),
            page: None,
            before: Vec::new(),
            undo_steps: None,
            after_remove: None,
            tool_seen: false,
            job_end: None,
            removal: None,
            shots: Arc::default(),
            asked: 0,
            ready: Vec::new(),
            host_look: None,
        })
    }

    /// The host's look before anything of the frame is drawn (the first frame: as it set it).
    pub fn before_frame(&mut self, ctx: &egui::Context) {
        if self.look.is_none() {
            self.look = Some((ctx.global_style(), ctx.fonts(|f| f.definitions().clone())));
        }
    }

    /// The view reported `DocumentReady`.
    pub fn ready(&mut self, document: DocumentId) {
        self.ready.push(document);
    }

    /// Events the host reported this frame.
    pub fn events(&mut self, events: &[Event]) {
        for e in events {
            match e {
                Event::HistoryChanged { undo_steps, .. } => self.undo_steps = Some(*undo_steps),
                Event::ToolChanged { tool } if tool == "removeAi" => self.tool_seen = true,
                Event::JobFinished { command, end, .. } if command == "edit.removeAi" => self.job_end = Some(end.clone()),
                _ => {}
            }
        }
    }

    fn check(&mut self, ok: bool, what: &str) {
        if !ok {
            self.failures.push(what.to_string());
        }
        self.results.insert(what.to_string(), json!(ok));
    }

    fn next(&mut self, step: Step) {
        self.step = step;
        self.frames = 0;
        self.since = Instant::now();
    }

    fn screenshot(&mut self, ctx: &egui::Context, name: &str) {
        let path = self.dir.join(format!("{name}.png"));
        let shots = self.shots.clone();
        self.asked += 1;
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
            let pixels: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
            let [w, h] = image.size;
            let rgba = capishot_host::render::Rgba { width: w as u32, height: h as u32, rect: [0, 0, w as i32, h as i32], pixels };
            let written = rgba.to_png().map_err(|e| e.to_string()).and_then(|png| std::fs::write(&path, png).map_err(|e| e.to_string()));
            let line = match written {
                Ok(()) => path.display().to_string(),
                Err(e) => format!("{}: {e}", path.display()),
            };
            shots.lock().unwrap_or_else(PoisonError::into_inner).push(line);
        })));
    }

    /// One frame of the run, after the app drew it.
    pub fn after_frame(&mut self, ctx: &egui::Context, app: &mut HostApp, drawn: Option<DocumentId>, gpu: bool) {
        self.frame += 1;
        self.frames += 1;
        if let Some((style, fonts)) = &self.look {
            let same = *ctx.global_style() == **style && ctx.fonts(|f| f.definitions() == fonts);
            if !same && !self.results.contains_key("host style and fonts unchanged") {
                self.check(false, "host style and fonts unchanged");
                self.results.insert("look changed at frame".into(), json!(self.frame));
            }
        }
        let Some(doc) = app.doc else {
            self.check(false, "the page opened");
            return self.finish(ctx);
        };
        match self.step {
            Step::Open if drawn == Some(doc) && self.frames >= SETTLE => {
                self.check(true, "the page opened and the canvas draws it");
                self.results.insert("gpu canvas".into(), json!(gpu));
                let rows = script::layers(&app.host, doc).unwrap_or_default();
                self.page = rows.iter().find(|r| r.kind == "pixel").map(|r| r.id);
                self.check(self.page.is_some(), "the page has a pixel layer");
                if let Some(page) = self.page {
                    self.before = script::blob_pixels(&mut app.host, doc, page).unwrap_or_default();
                }
                self.screenshot(ctx, "1-open");
                self.next(Step::Tool);
            }
            Step::Open if self.frames > 600 => {
                self.check(false, "the page opened and the canvas draws it");
                self.finish(ctx);
            }
            Step::Tool if self.frames == 1 => {
                let r = script::set_tool(&mut app.host, "removeAi");
                self.check(r.is_ok(), "the host sets the tool");
            }
            Step::Tool if self.tool_seen || self.frames > 60 => {
                self.check(self.tool_seen && app.host.tool() == "removeAi", "toolChanged reported");
                self.next(Step::Remove);
            }
            Step::Remove => {
                let download = std::env::var("CAPISHOT_ALLOW_DOWNLOAD").is_ok_and(|v| v == "1");
                match script::remove_ai(&mut app.host, doc, download) {
                    Ok(r) => {
                        if let Removal::Job(job) = &r {
                            app.job = Some(*job);
                        }
                        self.results.insert("remove".into(), json!(format!("{r:?}").split('(').next().unwrap_or_default()));
                        let job = matches!(r, Removal::Job(_));
                        self.removal = Some(r);
                        self.next(if job { Step::Removing } else { Step::Removed });
                    }
                    Err(e) => {
                        self.check(false, &format!("remove ran: {e}"));
                        self.finish(ctx);
                    }
                }
            }
            Step::Removing => match self.job_end.clone() {
                Some(JobEnd::Done { result }) => {
                    let reply = capishot_host::ai::AiReply::from_value(&result);
                    self.results.insert("ai".into(), json!({"device": reply.device, "cpu": reply.cpu, "fellBack": reply.fell_back, "ms": reply.ms, "model": reply.model}));
                    self.results.insert("remove seconds".into(), json!(self.since.elapsed().as_secs_f32()));
                    self.next(Step::Removed);
                }
                Some(end) => {
                    self.check(false, &format!("Remove with AI ended: {end:?}"));
                    self.finish(ctx);
                }
                None if self.since.elapsed() > AI_TIMEOUT => {
                    self.check(false, "Remove with AI finished in time");
                    self.finish(ctx);
                }
                None => {}
            },
            // The screenshot is of the next frame drawn: the next action waits a few frames.
            Step::Removed if self.frames == SETTLE => {
                let after = self.page.map(|p| script::blob_pixels(&mut app.host, doc, p).unwrap_or_default()).unwrap_or_default();
                self.check(!after.is_empty() && after != self.before, "the removal changed the pixels");
                self.after_remove = self.undo_steps;
                self.screenshot(ctx, "2-removed");
            }
            Step::Removed if self.frames >= 2 * SETTLE => {
                let undone = script::undo(&mut app.host, doc);
                self.check(matches!(undone, Ok(true)), "undo ran");
                self.next(Step::Undone);
            }
            Step::Undone if self.frames == SETTLE => {
                let now = self.page.map(|p| script::blob_pixels(&mut app.host, doc, p).unwrap_or_default()).unwrap_or_default();
                self.check(now == self.before, "one undo restores the pixels exactly");
                let steps = (self.after_remove, self.undo_steps);
                self.results.insert("undo steps after remove, after undo".into(), json!([steps.0, steps.1]));
                self.check(matches!(steps, (Some(a), Some(b)) if a == b + 1), "the removal is one undo step");
                self.screenshot(ctx, "3-undone");
            }
            Step::Undone if self.frames >= 2 * SETTLE => {
                let text = script::add_text(&mut app.host, doc, "¡Hola, Capikoma! ñ á é");
                self.check(text.is_ok(), "a text layer was added");
                self.next(Step::Text);
            }
            Step::Text if self.frames >= SETTLE => {
                self.screenshot(ctx, "4-text");
                match script::save(&mut app.host, doc) {
                    Ok(bytes) => {
                        self.results.insert("saved bytes".into(), json!(bytes.len()));
                        let again = app.host.open(bytes, OpenOptions { name: Some("again.psd".into()), hidden: true });
                        let kinds: Vec<String> = again
                            .ok()
                            .and_then(|o| script::layers(&app.host, o.document).ok())
                            .map(|rows| rows.into_iter().map(|r| r.kind).collect())
                            .unwrap_or_default();
                        self.check(kinds.iter().any(|k| k == "type"), "the saved PSD opens again with the text");
                        self.check(app.host.active_document() == Some(doc), "a hidden open leaves the page shown");
                    }
                    Err(e) => self.check(false, &format!("saved: {e}")),
                }
                self.check(self.ready.contains(&doc), "DocumentReady reported for the shown page");
                self.host_look = Some(HostLook::of(ctx));
                // The whole interface in the same rect, then back (a canvas-only build skips it).
                #[cfg(feature = "full")]
                if let Some(c) = app.canvas.as_mut() {
                    c.set_mode(capishot_host::view::Mode::Full);
                }
                self.next(if cfg!(feature = "full") { Step::Full } else { Step::Done });
            }
            Step::Full if self.frames == 3 * SETTLE => {
                self.check(drawn == Some(doc) && app.host.documents().iter().filter(|d| d.id == doc).count() == 1, "the full interface draws the same document");
                let same = self.host_look.as_ref().is_some_and(|l| *l == HostLook::of(ctx));
                self.check(same, "the host's style and theme are unchanged with the full interface");
                self.screenshot(ctx, "5-full");
            }
            Step::Full if self.frames >= 4 * SETTLE => {
                if let Some(c) = app.canvas.as_mut() {
                    c.set_mode(capishot_host::view::Mode::Embedded);
                }
                self.next(Step::Back);
            }
            Step::Back if self.frames == SETTLE => {
                self.check(drawn == Some(doc), "back on the canvas, same document");
                let same = self.host_look.as_ref().is_some_and(|l| *l == HostLook::of(ctx));
                self.check(same, "the host's style and theme are unchanged after the full interface");
                self.screenshot(ctx, "6-back");
                self.next(Step::Done);
            }
            Step::Done => {
                let taken = self.shots.lock().unwrap_or_else(PoisonError::into_inner).len();
                if taken >= self.asked || self.frames > 120 {
                    self.finish(ctx);
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, ctx: &egui::Context) {
        if !self.results.contains_key("host style and fonts unchanged") {
            self.check(true, "host style and fonts unchanged");
        }
        self.results.insert("frames".into(), json!(self.frame));
        self.results.insert("screenshots".into(), json!(*self.shots.lock().unwrap_or_else(PoisonError::into_inner)));
        self.results.insert("failures".into(), json!(self.failures));
        let report = serde_json::to_string_pretty(&Value::Object(std::mem::take(&mut self.results))).unwrap_or_default();
        println!("{report}");
        if let Err(e) = std::fs::create_dir_all(&self.dir).and_then(|()| std::fs::write(self.dir.join("selftest.json"), &report)) {
            eprintln!("selftest.json: {e}");
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        std::process::exit(i32::from(!self.failures.is_empty()));
    }
}
