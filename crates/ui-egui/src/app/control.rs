use super::{ControlRequest, PhotocraftApp, Receiver, gpu_canvas};

impl PhotocraftApp {
    /// Attach a control channel (requests arrive from a transport thread: TCP, stdin, tests).
    pub fn with_control(mut self, rx: Receiver<ControlRequest>) -> Self {
        self.control_rx = Some(rx);
        self
    }

    /// Close this frame's stage record (`frame_stages`): keep it if it was slow, and hand it to
    /// the stroke trace. `ui_ms` is this pass's UI build. A pass egui will run again
    /// (`request_discard`, such as a sizing pass) leaves the record open, so a frame is one
    /// record across all its passes.
    pub(crate) fn finish_frame(&mut self, ctx: &egui::Context, ui_ms: f64) {
        self.perf.stage.ui_ms += ui_ms;
        if ctx.will_discard() {
            return;
        }
        let stages = std::mem::take(&mut self.perf.stage);
        let stages = self.slow_frames.finish(stages, gpu_canvas::now_ms(), self.perf.gpu, self.perf.gpu_ms);
        self.perf_window.frame(&stages);
        let info = &self.perf.gpu_info;
        self.stroke_trace.frame(&stages, self.live_stroke.is_some(), || {
            let gpu = format!("{} {}", info.backend, info.adapter).trim().to_string();
            if info.present.is_empty() { gpu } else { format!("{gpu}, {}", info.present) }
        });
    }

    pub(crate) fn drain_control(&mut self, ctx: &egui::Context) {
        // Requests held for code that has loaded since (or failed to: they were answered).
        if let Some(held) = crate::code_gate::released(self) {
            for req in held {
                self.dispatch_control(ctx, req);
            }
        }
        let Some(rx) = self.control_rx.take() else { return };
        while let Ok(req) = rx.try_recv() {
            if req.deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) || !req.dispatch.start() {
                let _ = req.reply.send(serde_json::json!({"ok": false, "error": crate::control::expired_error()}));
                continue;
            }
            // A command whose code hasn't loaded yet waits for it, and the requests after it too.
            let Err(req) = crate::code_gate::hold(self, req) else { continue };
            self.dispatch_control(ctx, req);
        }
        self.control_rx = Some(rx);
    }

    fn dispatch_control(&mut self, ctx: &egui::Context, req: ControlRequest) {
        let reply = req.reply.clone();
        match crate::control::handle(self, ctx, &req) {
            crate::control::Outcome::Done(v) => {
                let _ = reply.send(v);
            }
            crate::control::Outcome::AfterInput => self.input_waiters.push(reply),
            crate::control::Outcome::AfterJob(job) => self.jobs.waiters.push((job, reply)),
            crate::control::Outcome::Screenshot { token, path } => {
                // Admission is budgeted here, after this pass's completed captures were
                // drained, so a just-finished request never causes a false rejection.
                let settle = crate::screenshot::settle_ms(ctx.global_style().animation_time);
                let document = self.document_identity();
                if let Err((reply, e)) = self.captures.admit(token, path, reply, document, settle, crate::screenshot::now()) {
                    let _ = reply.send(serde_json::json!({"ok": false, "error": e}));
                }
            }
        }
        // `ui.set` may have changed the edit target: the next request sees its colours.
        self.sync_mask_targets();
    }
}
