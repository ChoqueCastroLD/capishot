//! Help › System Info's Performance section: every frame of the last minute (its stages, from
//! [`crate::frame_stages`]), the input it delivered and what kind of pointer sent it, the window's
//! size and scale. A user who finds the app slow can copy the numbers into a report.

use std::collections::VecDeque;

use crate::frame_stages::FrameStages;

/// How far back the window reaches (ms).
const WINDOW_MS: f64 = 60_000.0;
/// At most this many frames are kept (a 240 Hz display for the whole window).
const MAX_FRAMES: usize = 240 * 60;
/// How many of the slowest frames the section lists with their stages.
const SLOWEST: usize = 10;

/// The kind of pointer that last moved.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PointerKind {
    #[default]
    None,
    Mouse,
    Touch,
    Pen,
}

impl PointerKind {
    fn name(self) -> &'static str {
        match self {
            PointerKind::None => "none",
            PointerKind::Mouse => "mouse",
            PointerKind::Touch => "touch",
            PointerKind::Pen => "pen",
        }
    }
}

/// The last minute of frames and input.
#[derive(Debug, Default)]
pub struct PerfWindow {
    frames: VecDeque<FrameStages>,
    /// (time, input events) per frame.
    input: VecDeque<(f64, usize)>,
    /// Frames of each pointer kind in the window: mouse, touch, pen.
    kinds: [usize; 3],
    last_kind: PointerKind,
    /// The window's size in points, its pixels per point, and the monitor's size when known.
    screen: Option<([f32; 2], f32, Option<[f32; 2]>)>,
}

impl PerfWindow {
    /// A frame ended: its stages.
    pub fn frame(&mut self, s: &FrameStages) {
        if self.frames.len() == MAX_FRAMES {
            self.frames.pop_front();
        }
        self.frames.push_back(s.clone());
        let cutoff = s.t - WINDOW_MS;
        while self.frames.front().is_some_and(|f| f.t < cutoff) {
            self.frames.pop_front();
        }
    }

    /// A frame's raw input: how many events, which pointer, the window's size and scale.
    pub fn input(&mut self, now: f64, raw: &egui::RawInput, ppp: f32, pen: bool) {
        let n = raw.events.len();
        if self.input.len() == MAX_FRAMES {
            self.input.pop_front();
        }
        self.input.push_back((now, n));
        while self.input.front().is_some_and(|(t, _)| *t < now - WINDOW_MS) {
            self.input.pop_front();
        }
        let touch = raw.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }));
        let mouse = raw.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. }));
        let kind = if pen {
            PointerKind::Pen
        } else if touch {
            PointerKind::Touch
        } else if mouse {
            PointerKind::Mouse
        } else {
            PointerKind::None
        };
        if kind != PointerKind::None {
            self.last_kind = kind;
            self.kinds[kind as usize - 1] += 1;
        }
        let vp = raw.viewports.get(&raw.viewport_id);
        let monitor = vp.and_then(|v| v.monitor_size).map(|s| [s.x, s.y]);
        if let Some(r) = raw.screen_rect {
            self.screen = Some(([r.width(), r.height()], ppp, monitor));
        }
    }

    /// The section's lines (English source strings, translated by the caller's language scope).
    pub fn lines(&self, info: &crate::gpu_canvas::GpuInfo) -> Vec<String> {
        let mut v = vec![tl!("Performance (last 60 s)").to_string()];
        let n = self.frames.len();
        if n == 0 {
            v.push(tl!("No frames recorded yet.").to_string());
            return v;
        }
        let mut ms: Vec<f64> = self.frames.iter().map(|f| f.frame_ms).filter(|m| m.is_finite()).collect();
        ms.sort_by(f64::total_cmp);
        let q = |p: f64| ms.get(((ms.len() as f64 - 1.0) * p).round() as usize).copied().unwrap_or(0.0);
        let span = (self.frames.back().map_or(0.0, |f| f.t) - self.frames.front().map_or(0.0, |f| f.t)).max(1.0);
        let fps = (n as f64 - 1.0).max(0.0) * 1000.0 / span;
        let events: usize = self.input.iter().map(|(_, e)| e).sum();
        let ispan = (self.input.back().map_or(0.0, |i| i.0) - self.input.front().map_or(0.0, |i| i.0)).max(1.0);
        v.push(format!(
            "{}: {n}, {:.1} fps; {} p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
            tl!("Frames"),
            fps,
            tl!("frame time"),
            q(0.5),
            q(0.95),
            ms.last().copied().unwrap_or(0.0)
        ));
        let slow = self.frames.iter().filter(|f| f.frame_ms > crate::frame_stages::SLOW_FRAME_MS).count();
        v.push(format!("{}: {slow} (> {} ms)", tl!("Slow frames"), crate::frame_stages::SLOW_FRAME_MS));
        v.push(format!(
            "{}: {:.0}/s; {}: {} ({} mouse, {} touch, {} pen)",
            tl!("Input events"),
            events as f64 * 1000.0 / ispan,
            tl!("pointer"),
            self.last_kind.name(),
            self.kinds[0],
            self.kinds[1],
            self.kinds[2]
        ));
        if let Some((size, ppp, monitor)) = self.screen {
            let monitor = monitor.map_or(String::new(), |m| format!(", {} {:.0}×{:.0}", tl!("monitor"), m[0], m[1]));
            v.push(format!(
                "{}: {:.0}×{:.0} pt × {:.2} = {:.0}×{:.0} px{monitor}",
                tl!("Window"),
                size[0],
                size[1],
                ppp,
                size[0] * ppp,
                size[1] * ppp
            ));
        }
        let paths = |p: &str| self.frames.iter().filter(|f| f.path == p).count();
        v.push(format!(
            "{}: {}; {}: {} gpu / {} cpu; {}: {}",
            tl!("Canvas"),
            if info.canvas == "gpu" { "GPU" } else { "CPU" },
            tl!("frames drawn"),
            paths("gpu"),
            paths("cpu"),
            tl!("presentation"),
            if info.present.is_empty() { "-" } else { info.present.as_str() }
        ));
        let mut worst: Vec<&FrameStages> = self.frames.iter().collect();
        worst.sort_by(|a, b| b.frame_ms.total_cmp(&a.frame_ms));
        v.push(format!("{} {}:", tl!("Slowest frames"), worst.len().min(SLOWEST)));
        for f in worst.into_iter().take(SLOWEST) {
            v.push(format!("  {}", f.line()));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f64, ms: f64, path: &'static str) -> FrameStages {
        let mut s = FrameStages::default();
        s.t = t;
        s.frame_ms = ms;
        s.composite_ms = ms / 2.0;
        s.path = path;
        s
    }

    #[test]
    fn keeps_the_last_minute_and_lists_the_slowest() {
        let mut w = PerfWindow::default();
        for k in 0..2000 {
            let t = f64::from(k) * 50.0;
            w.frame(&frame(t, if k % 100 == 0 { 120.0 } else { 10.0 }, if k % 2 == 0 { "gpu" } else { "cpu" }));
        }
        assert!(w.frames.front().unwrap().t >= 99_950.0 - WINDOW_MS, "older frames dropped");
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0)));
        raw.events = vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0)); 3];
        w.input(99_950.0, &raw, 1.5, false);
        let info = crate::gpu_canvas::GpuInfo { canvas: "gpu".into(), present: "autovsync, latency 1".into(), ..Default::default() };
        let lines = w.lines(&info).join("\n");
        assert!(lines.contains("p50 10.0 ms") && lines.contains("max 120.0 ms"), "{lines}");
        assert!(lines.contains("pointer: mouse") && lines.contains("1280×720 pt × 1.50 = 1920×1080 px"), "{lines}");
        assert!(lines.contains("Slowest frames 10:") && lines.matches("slow frame: 120.0 ms").count() >= 10, "{lines}");
        assert!(lines.contains("presentation: autovsync, latency 1"), "{lines}");
    }

    #[test]
    fn a_pen_is_reported_as_the_pointer() {
        let mut w = PerfWindow::default();
        let mut raw = egui::RawInput::default();
        raw.events = vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0))];
        w.input(0.0, &raw, 1.0, true);
        assert_eq!(w.last_kind, PointerKind::Pen);
    }
}
