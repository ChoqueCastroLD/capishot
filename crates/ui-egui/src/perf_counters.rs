//! Counts of the app's per-frame work that should only happen when something changed: thumbnails
//! rebuilt, Layers panel rows laid out, panels recomputed. Benchmarks (`examples/tool_frames.rs`)
//! and tests read them to prove that a frame's work is proportional to what changed, not to the
//! layer count; `?perf=1` on the web reports them per gesture.

/// Monotonic counters (never reset; read differences).
#[derive(Clone, Debug, Default)]
pub struct PerfCounters {
    /// Layer content thumbnails rendered (Layers panel).
    pub thumbs: u64,
    /// Layer / vector mask thumbnails rendered.
    pub mask_thumbs: u64,
    /// Thumbnails whose rebuild was put off to a later frame (throttled while drawing).
    pub thumbs_deferred: u64,
    /// Layers panel rows laid out in full (visible rows; off-screen rows only keep their place).
    pub rows_drawn: u64,
    /// Layers panel rows that only kept their place (scrolled out of view).
    pub rows_skipped: u64,
    /// Navigator thumbnail recomputes.
    pub navigator: u64,
    /// Histogram panel recomputes.
    pub histogram: u64,
    /// Move drag frames whose preview reused the last frame's document (only the moving layers
    /// copied again).
    pub move_preview_reused: u64,
    /// Snap target sets built from the document (drag starts on a changed document).
    pub snap_builds: u64,
    /// Frames that repainted the chrome from its last shapes during a canvas gesture.
    pub chrome_reused: u64,
    /// Frames run.
    pub frames: u64,
}

impl PerfCounters {
    /// Every counter by name.
    pub fn snapshot(&self) -> Vec<(&'static str, u64)> {
        vec![
            ("thumbs", self.thumbs),
            ("mask_thumbs", self.mask_thumbs),
            ("thumbs_deferred", self.thumbs_deferred),
            ("rows_drawn", self.rows_drawn),
            ("rows_skipped", self.rows_skipped),
            ("navigator", self.navigator),
            ("histogram", self.histogram),
            ("move_preview_reused", self.move_preview_reused),
            ("snap_builds", self.snap_builds),
            ("chrome_reused", self.chrome_reused),
            ("frames", self.frames),
        ]
    }
}
