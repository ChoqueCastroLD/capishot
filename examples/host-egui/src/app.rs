//! The host app: a host-drawn layer list and the contract's buttons on the left, Capishot's canvas
//! on the right. Its own theme and font stay as they are: the canvas widget styles only itself.

use std::cell::RefCell;
use std::rc::Rc;

use capishot_host::{DocumentId, Event, Host, JobEnd, JobHandle, LayerId};

use crate::script::{self, LayerRow, Removal};
use crate::view::Canvas;

/// The example app.
pub struct HostApp {
    host: Host,
    canvas: Option<Canvas>,
    doc: Option<DocumentId>,
    layers: Vec<LayerRow>,
    /// Remove with AI's running job.
    job: Option<JobHandle>,
    /// Events gathered by the subscription (whoever polls the host: the widget or us).
    events: Rc<RefCell<Vec<Event>>>,
    /// The last few events, for the event log.
    log: Vec<String>,
    status: String,
    saved: Option<usize>,
    /// The layer list needs reading again.
    stale: bool,
}

const LOG_LINES: usize = 12;

impl HostApp {
    /// The app, its canvas on eframe's wgpu device, with the sample page open.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut host = Host::new();
        let events = Rc::new(RefCell::new(Vec::new()));
        let sink = events.clone();
        host.subscribe(move |e| sink.borrow_mut().push(e.clone()));
        let (canvas, status) = match Canvas::new(cc, &mut host) {
            Ok(c) => (Some(c), String::new()),
            Err(e) => (None, format!("No canvas: {e}")),
        };
        let mut app = HostApp { host, canvas, doc: None, layers: Vec::new(), job: None, events, log: Vec::new(), status, saved: None, stale: true };
        app.open_sample();
        app
    }

    /// Step 1 with the generated page (a real host gets the bytes from its server).
    fn open_sample(&mut self) {
        let r = script::sample_psd(&mut self.host).and_then(|bytes| script::open(&mut self.host, "page.psd", bytes));
        match r {
            Ok((doc, warnings)) => {
                self.doc = Some(doc);
                self.status = if warnings.is_empty() { "Opened page.psd".into() } else { format!("Opened page.psd: {}", warnings.join("; ")) };
                if let Some(c) = self.canvas.as_mut() {
                    c.fit();
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
        // Polling twice in a frame is harmless; this applies a finished job when no widget does.
        self.host.poll();
        let events = std::mem::take(&mut *self.events.borrow_mut());
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

    /// The selected layer's bounds, for the host overlay.
    fn highlight(&self) -> Option<[f32; 4]> {
        self.layers.iter().find(|r| r.selected && r.kind != "group").and_then(|r| r.bounds)
    }
}

impl eframe::App for HostApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.take_events();
        egui::Panel::left("host").resizable(true).default_size(280.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.side_panel(ui));
        });
        let highlight = self.highlight();
        egui::CentralPanel::default().show(ui, |ui| match self.canvas.as_mut() {
            Some(c) => {
                c.ui(ui, &mut self.host, highlight);
            }
            None => {
                ui.centered_and_justified(|ui| ui.label("The canvas needs the wgpu renderer."));
            }
        });
        if self.job.is_some() {
            // Keep polling while Remove with AI runs.
            ui.ctx().request_repaint();
        }
    }
}
