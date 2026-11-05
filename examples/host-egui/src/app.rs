//! The host app: a host-drawn layer list and the contract's buttons on the left, Capishot's canvas
//! on the right. Its own theme and font stay as they are: the canvas widget styles only itself.

use std::cell::RefCell;
use std::rc::Rc;

use capishot_host::view::{Mode, ViewEvent, ViewOutput, WindowRequest};
use capishot_host::{DocumentId, Event, Host, JobEnd, JobHandle, LayerId};

use crate::script::{self, LayerRow, Removal};
use crate::view::Canvas;

/// The example app.
pub struct HostApp {
    pub(crate) host: Host,
    pub(crate) canvas: Option<Canvas>,
    /// Capishot gets the digit keys (off: the host keeps them).
    digit_keys: bool,
    pub(crate) doc: Option<DocumentId>,
    layers: Vec<LayerRow>,
    /// Remove with AI's running job.
    pub(crate) job: Option<JobHandle>,
    /// Events gathered by the subscription (whoever polls the host: the widget or us).
    events: Rc<RefCell<Vec<Event>>>,
    /// The last few events, for the event log.
    log: Vec<String>,
    /// `--selftest <dir>`: the contract run frame by frame, then the app exits.
    #[cfg(not(target_arch = "wasm32"))]
    selftest: Option<crate::selftest::SelfTest>,
    status: String,
    /// Side by side: the page as it is and as `script::COMPARE_VIEW` (without the selected layer).
    compare: bool,
    saved: Option<usize>,
    /// The layer list needs reading again.
    stale: bool,
}

const LOG_LINES: usize = 12;

impl HostApp {
    /// The app, its canvas on eframe's wgpu device, with the sample page open.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // The host's own look: the canvas widget must leave it as it is.
        cc.egui_ctx.set_visuals(egui::Visuals::light());
        let mut host = Host::new();
        let ai = host.install_ai(&ai_config());
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        host.subscribe(move |e| sink.borrow_mut().push(e.clone()));
        // The canvas needs eframe's wgpu renderer (it draws on the host's device).
        let canvas = cc.wgpu_render_state.is_some().then(|| Canvas::new(cc));
        let status = match &ai.reason {
            Some(why) => format!("No AI: {why}"),
            None => String::new(),
        };
        let mut app = HostApp {
            host,
            canvas,
            digit_keys: false,
            doc: None,
            layers: Vec::new(),
            job: None,
            events,
            log: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            selftest: crate::selftest::SelfTest::from_args(),
            status,
            compare: false,
            saved: None,
            stale: true,
        };
        app.open_sample();
        app
    }

    /// Step 1 with the generated page (a real host gets the bytes from its server; natively,
    /// `CAPISHOT_HOST_PSD=<file>` opens that file's bytes instead).
    fn open_sample(&mut self) {
        let r = sample_bytes(&mut self.host).and_then(|(name, bytes)| script::open(&mut self.host, &name, bytes).map(|r| (name, r)));
        match r {
            Ok((name, (doc, warnings))) => {
                self.doc = Some(doc);
                self.status = if warnings.is_empty() { format!("Opened {name}") } else { format!("Opened {name}: {}", warnings.join("; ")) };
                if let Some(c) = self.canvas.as_mut() {
                    c.fit(doc);
                }
            }
            Err(e) => self.status = format!("Open failed: {e}"),
        }
        self.stale = true;
    }

    fn report(&mut self, what: &str, r: capishot_host::Result<String>) {
        self.status = match r {
            Ok(s) if s.is_empty() => what.to_string(),
            Ok(s) => format!("{what}: {s}"),
            Err(e) => format!("{what} failed: {e}"),
        };
        self.stale = true;
    }

    /// Events since the last frame: the job's end, a refreshed layer list, the log.
    fn take_events(&mut self) {
        // The canvas widget polls the host every frame (and finishes jobs on the canvas); without
        // it, the host polls itself.
        if self.canvas.is_none() {
            self.host.poll();
        }
        let events = std::mem::take(&mut *self.events.borrow_mut());
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(t) = self.selftest.as_mut() {
            t.events(&events);
        }
        if events.is_empty() {
            return;
        }
        if let Some(job) = self.job
            && let Some(end) = script::job_end(&events, job)
        {
            self.job = None;
            self.status = match end {
                JobEnd::Done { .. } => "Remove with AI: done (one undo step)".into(),
                JobEnd::Failed { error } => format!("Remove with AI failed: {error}"),
                JobEnd::Cancelled => "Remove with AI cancelled".into(),
                // `JobEnd` may gain variants.
                #[allow(unreachable_patterns)]
                _ => "Remove with AI ended".into(),
            };
        }
        for e in &events {
            if !matches!(e, Event::ToolChanged { .. }) {
                self.stale = true;
            }
            self.log.push(e.to_json().to_string());
        }
        let extra = self.log.len().saturating_sub(LOG_LINES);
        self.log.drain(..extra);
    }

    fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Host");
        ui.label(&self.status);
        ui.separator();

        // The host's say: which interface, which keys. Same open documents in both modes.
        if let Some(c) = self.canvas.as_mut() {
            let mut mode = c.mode();
            ui.horizontal(|ui| {
                ui.selectable_value(&mut mode, Mode::Embedded, "Canvas");
                #[cfg(feature = "full")]
                ui.selectable_value(&mut mode, Mode::Full, "Full Capishot");
            });
            if mode != c.mode() {
                c.set_mode(mode);
            }
            if ui.checkbox(&mut self.digit_keys, "Capishot gets the 1–9 keys").changed() {
                c.set_digit_keys(self.digit_keys);
            }
        }
        // Two tiles of one page: as it is, and as a named view without the selected layer.
        if ui.checkbox(&mut self.compare, "Side by side, without the selected layer").changed() && self.compare {
            let layer = self.layers.iter().find(|row| row.selected).or_else(|| self.layers.first()).map(|row| row.id);
            if let (Some(doc), Some(layer)) = (self.doc, layer) {
                let r = script::define_compare_view(&mut self.host, doc, layer).map(|()| String::new());
                self.report("Side by side", r);
            }
        }
        ui.separator();

        let doc = self.doc;
        let busy = self.job.is_some();
        ui.add_enabled_ui(doc.is_some() && !busy, |ui| {
            let Some(doc) = doc else { return };
            ui.horizontal_wrapped(|ui| {
                for tool in ["move", "removeAi", "brush", "eraser", "cloneStamp", "eyedropper"] {
                    if ui.selectable_label(self.host.tool() == tool, tool).clicked() {
                        let r = script::set_tool(&mut self.host, tool).map(|()| String::new());
                        self.report(&format!("Tool {tool}"), r);
                    }
                }
            });
            if ui.button("Remove with AI").clicked() {
                match script::remove_ai(&mut self.host, doc, true) {
                    Ok(Removal::Job(job)) => {
                        self.job = Some(job);
                        self.status = "Remove with AI: running".into();
                    }
                    Ok(Removal::Done(_)) => self.report("Remove with AI", Ok(String::new())),
                    Ok(Removal::Plain(_)) => self.report("Remove (no AI on this build)", Ok(String::new())),
                    Err(e) => self.report("Remove with AI", Err(e)),
                }
            }
            if ui.button("Undo").clicked() {
                let r = script::undo(&mut self.host, doc).map(|done| if done { String::new() } else { "nothing to undo".into() });
                self.report("Undo", r);
            }
            if ui.button("Add text").clicked() {
                let r = script::add_text(&mut self.host, doc, "¡Hola, Capikoma!").map(|l| format!("layer {}", l.0));
                self.report("Add text", r);
            }
            if ui.button("Save PSD").clicked() {
                let r = script::save(&mut self.host, doc).map(|b| {
                    self.saved = Some(b.len());
                    format!("{} bytes", b.len())
                });
                self.report("Save", r);
            }
        });
        if let Some(job) = self.job {
            let progress = self.host.job(job).map_or(0.0, |j| j.progress);
            ui.add(egui::ProgressBar::new(progress).show_percentage());
            if ui.button("Cancel").clicked() {
                self.host.cancel(job);
            }
        }
        if let Some(n) = self.saved {
            ui.label(format!("Last save: {n} bytes"));
        }

        ui.separator();
        ui.heading("Layers");
        self.layer_list(ui);

        ui.separator();
        ui.collapsing("Events", |ui| {
            for line in self.log.iter().rev() {
                ui.monospace(line);
            }
        });
    }

    /// The host-drawn layer list: eye and select, both through commands.
    fn layer_list(&mut self, ui: &mut egui::Ui) {
        let Some(doc) = self.doc else { return };
        if self.stale {
            match script::layers(&self.host, doc) {
                Ok(rows) => self.layers = rows,
                Err(e) => self.status = format!("Layers: {e}"),
            }
            self.stale = false;
        }
        let mut toggle: Option<(LayerId, bool)> = None;
        let mut pick: Option<LayerId> = None;
        for row in &self.layers {
            ui.horizontal(|ui| {
                ui.add_space(row.depth as f32 * 12.0);
                let mut visible = row.visible;
                if ui.checkbox(&mut visible, "").changed() {
                    toggle = Some((row.id, visible));
                }
                if ui.selectable_label(row.selected, format!("{} ({})", row.name, row.kind)).clicked() {
                    pick = Some(row.id);
                }
            });
        }
        if let Some((layer, visible)) = toggle {
            let r = script::set_visible(&mut self.host, doc, layer, visible).map(|()| String::new());
            self.report("Visibility", r);
        }
        if let Some(layer) = pick {
            let r = script::select(&mut self.host, doc, layer).map(|()| String::new());
            self.report("Select", r);
        }
    }

    /// What the view reported besides the host's events: the window is this host's to manage.
    fn view_events(&mut self, ctx: &egui::Context, out: &ViewOutput) {
        for e in &out.view_events {
            match e {
                ViewEvent::Window(WindowRequest::Title(title)) => ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{title} — host"))),
                // File › Exit in the full interface: this host goes back to its canvas.
                ViewEvent::Window(WindowRequest::Close) => {
                    if let Some(c) = self.canvas.as_mut() {
                        c.set_mode(Mode::Embedded);
                    }
                }
                ViewEvent::DocumentReady { document, first_frame_ms, final_frame_ms } => {
                    self.log.push(format!("documentReady {} first {first_frame_ms} ms, final {final_frame_ms} ms", document.0));
                    #[cfg(not(target_arch = "wasm32"))]
                    if let Some(t) = self.selftest.as_mut() {
                        t.ready(*document);
                    }
                }
                _ => {}
            }
        }
    }

    /// The selected layer's bounds, for the host overlay.
    fn highlight(&self) -> Option<[f32; 4]> {
        self.layers.iter().find(|r| r.selected && r.kind != "group").and_then(|r| r.bounds)
    }
}

impl eframe::App for HostApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(t) = self.selftest.as_mut() {
            t.before_frame(ui.ctx());
        }
        egui::Panel::left("host").resizable(true).default_size(280.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ui));
        });
        let highlight = self.highlight();
        let mut out = None;
        let compare = self.doc.filter(|_| self.compare && self.canvas.as_ref().is_some_and(|c| c.mode() == Mode::Embedded));
        egui::CentralPanel::default().show(ui, |ui| match self.canvas.as_mut() {
            Some(c) => {
                out = Some(match compare {
                    Some(doc) => c.side_by_side(ui, &mut self.host, doc, script::COMPARE_VIEW, highlight),
                    None => c.ui(ui, &mut self.host, highlight),
                })
            }
            None => {
                ui.centered_and_justified(|ui| ui.label("The canvas needs the wgpu renderer."));
            }
        });
        #[cfg(not(target_arch = "wasm32"))]
        let drawn = out.as_ref().and_then(|o| o.document);
        if let Some(out) = &out {
            self.view_events(ui.ctx(), out);
        }
        // The events the view's poll just gave (job ends, the person's tool…).
        self.take_events();
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(mut t) = self.selftest.take() {
            let gpu = self.canvas.as_ref().is_some_and(Canvas::gpu_active);
            t.after_frame(ui.ctx(), self, drawn, gpu);
            self.selftest = Some(t);
        }
        #[cfg(not(target_arch = "wasm32"))]
        let scripted = self.selftest.is_some();
        #[cfg(target_arch = "wasm32")]
        let scripted = false;
        if self.job.is_some() || scripted {
            // Keep polling while Remove with AI runs.
            ui.ctx().request_repaint();
        }
    }
}

/// The page to open: natively `CAPISHOT_HOST_PSD=<file>` if set, else the generated sample.
fn sample_bytes(host: &mut Host) -> capishot_host::Result<(String, Vec<u8>)> {
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(path) = std::env::var_os("CAPISHOT_HOST_PSD") {
        let path = std::path::PathBuf::from(path);
        let name = path.file_name().map_or_else(|| "page.psd".into(), |n| n.to_string_lossy().into_owned());
        let bytes = std::fs::read(&path).map_err(|e| capishot_host::Error::File(format!("{}: {e}", path.display())))?;
        return Ok((name, bytes));
    }
    Ok(("page.psd".into(), script::sample_psd(host)?))
}

/// Where AI models live and which device runs them. Natively: `CAPISHOT_MODELS_DIR` (no AI
/// without it), `CAPISHOT_MODELS_URL` (`""`: no downloads) and `CAPISHOT_AI_DEVICE`
/// (`cpu`/`gpu`/`auto`). In the browser a host would set `worker_url` to its AI worker.
fn ai_config() -> capishot_host::ai::AiConfig {
    #[allow(unused_mut)]
    let mut config = capishot_host::ai::AiConfig::default();
    #[cfg(not(target_arch = "wasm32"))]
    {
        config.models_dir = std::env::var_os("CAPISHOT_MODELS_DIR").map(Into::into);
        config.models_url = std::env::var("CAPISHOT_MODELS_URL").ok();
        config.device = match std::env::var("CAPISHOT_AI_DEVICE").as_deref() {
            Ok("cpu") => capishot_host::ai::AiDevice::Cpu,
            Ok("gpu") => capishot_host::ai::AiDevice::Gpu,
            _ => capishot_host::ai::AiDevice::Auto,
        };
    }
    config
}
