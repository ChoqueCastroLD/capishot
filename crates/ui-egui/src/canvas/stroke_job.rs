//! A tool's stroke rendered off the window's thread while it is drawn: the Brush, Pencil and
//! Eraser, the Mixer Brush, and the retouching tools (Clone Stamp, Healing Brush, History Brush,
//! Pattern Stamp, Dodge, Burn, Sponge, Blur, Sharpen, Smudge, Color Replacement, Background
//! Eraser).
//!
//! A large soft tip's dab is millions of pixels (a 1200 px soft tip reaches 2075 px across: 4.3
//! megapixels of coverage, then the same again composited onto the layer), and rendering the
//! frame's dabs in the frame held it for 70 to 130 ms (a 600 px Dodge or a 300 px Sharpen: 160).
//! The frame now only hands the stroke its points: a job on the worker pool renders them, every
//! point in the order it was drawn, and hands back the document with whole dabs. The canvas shows
//! the last document handed back, so the stroke appears as it lands, never part of a dab, and
//! the window is never held.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use photocraft_doc::Document;
use photocraft_engine::brush_cmds::mixer_live::LiveMixer;
use photocraft_engine::brush_cmds::{LiveCommit, LiveStroke};
use photocraft_engine::paint::StrokePoint;
use photocraft_engine::retouch_cmds::{LiveDab, LiveRetouch};

use super::DRect;

/// The engine's live stroke of a tool: the Brush, Pencil and Eraser paint dabs; the Clone Stamp,
/// Healing Brush, History Brush and Pattern Stamp composite their source; Dodge, Burn, Sponge,
/// Blur, Sharpen, Smudge, Color Replacement and the Background Eraser run their effect dab by
/// dab; the Mixer Brush carries paint.
pub(super) enum EngineStroke {
    Brush(Box<LiveStroke>),
    Retouch(Box<LiveRetouch>),
    Dab(Box<LiveDab>),
    Mixer(Box<LiveMixer>),
}

/// A stroke that ended, with everything rendered, for its commit.
pub(super) enum Finished {
    /// The Brush's, Pencil's or Eraser's pixels (`Session::offer_live_commit`).
    Brush(Box<LiveCommit>),
    /// (`Session::offer_live_retouch`.)
    Retouch(Box<LiveRetouch>),
    /// With its finishing dabs run (`Session::offer_live_dab`).
    Dab(Box<LiveDab>),
    /// (`LiveMixer::prepare_commit`.)
    Mixer(Box<LiveMixer>),
}

impl EngineStroke {
    fn doc(&self) -> &Arc<Document> {
        match self {
            Self::Brush(s) => &s.doc,
            Self::Retouch(s) => &s.doc,
            Self::Dab(s) => &s.doc,
            Self::Mixer(s) => &s.doc,
        }
    }

    /// The brush engine's strokes can change while the pointer is held still (see
    /// `LiveStroke::wants_time`); the retouch strokes don't.
    fn wants_time(&self) -> bool {
        match self {
            Self::Brush(s) => s.wants_time(),
            Self::Mixer(s) => s.wants_time(),
            Self::Retouch(_) | Self::Dab(_) => false,
        }
    }

    fn smoothing_string(&self) -> SmoothingString {
        match self {
            Self::Brush(s) => s.smoothing_string(),
            _ => None,
        }
    }

    /// The jitter seed the commit must reuse (the retouching tools' comes from their params).
    fn seed(&self) -> Option<u64> {
        match self {
            Self::Brush(s) => Some(s.seed),
            Self::Mixer(s) => Some(s.seed),
            Self::Retouch(_) | Self::Dab(_) => None,
        }
    }

    /// Render `pts` after the points before them: what changed. The document then shows the
    /// stroke as releasing now would leave it (the Brush's smoothing tail up to date).
    fn step(&mut self, pts: &[StrokePoint]) -> photocraft_engine::Result<DRect> {
        match self {
            Self::Brush(s) => {
                let r = s.push(pts)?;
                Ok(r.union(&s.finalize_tail()?))
            }
            Self::Retouch(s) => s.push(pts),
            Self::Dab(s) => s.push(pts),
            Self::Mixer(s) => s.push(pts),
        }
    }

    /// The stroke ended: what its commit needs rendered beyond the points (a dab-by-dab stroke's
    /// finishing dabs), and what that changed.
    fn end(&mut self) -> photocraft_engine::Result<DRect> {
        match self {
            Self::Dab(s) => s.finish(),
            _ => Ok(DRect::EMPTY),
        }
    }

    fn into_finished(self) -> photocraft_engine::Result<Finished> {
        Ok(match self {
            Self::Brush(s) => Finished::Brush(Box::new(s.into_commit()?)),
            Self::Retouch(s) => Finished::Retouch(s),
            Self::Dab(s) => Finished::Dab(s),
            Self::Mixer(s) => Finished::Mixer(s),
        })
    }
}

/// The smoothing string's ends (see `LiveStroke::smoothing_string`).
type SmoothingString = Option<([f64; 2], [f64; 2])>;

/// How long [`StrokeJob::land`] waits for the stroke's job.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wait {
    /// Not at all: what is done already. A frame never waits for the stroke: the points it
    /// hands over show in the frame that finds them rendered. (A frame waited 2 ms for a stroke
    /// that kept up: a whole timer tick on Windows, 17 to 29 ms of `stroke.wait` on the owner's
    /// PC with a 1200 px soft brush.)
    No,
    /// Until everything handed over is rendered: another edit is about to run on the document.
    All,
}

/// What the job hands back.
enum Landed {
    /// Points rendered: the document with them, complete.
    Step { doc: Arc<Document>, damage: DRect, points: usize, wants_time: bool, string: SmoothingString },
    /// The stroke, finished for its commit.
    Finished(Finished),
    /// The stroke couldn't render its points or finish, and why: its commit renders the stroke
    /// itself.
    Failed(String),
}

/// What the frame hands the job.
struct Queue {
    /// The stroke, while no job is rendering with it.
    stroke: Option<EngineStroke>,
    /// Points to render, in the order they were drawn.
    points: Vec<StrokePoint>,
    /// The stroke ended: finish it for its commit once every point is rendered.
    finish: bool,
    /// A job is at work (it takes what arrives meanwhile before it ends).
    running: bool,
}

/// The engine's live stroke, rendered by a job on the worker pool.
pub(super) struct StrokeJob {
    queue: Arc<wasm_sync::Mutex<Queue>>,
    tx: Sender<Landed>,
    rx: Receiver<Landed>,
    /// The document with the points rendered so far.
    doc: Arc<Document>,
    /// The jitter seed the commit must reuse.
    pub(super) seed: Option<u64>,
    wants_time: bool,
    string: SmoothingString,
    /// Points handed over, and how many of them the document shows.
    sent: usize,
    shown: usize,
    /// The stroke ended and its commit isn't made yet.
    finishing: bool,
    /// Why the stroke can't be shown any further.
    failed: Option<String>,
    finished: Option<Finished>,
}

impl StrokeJob {
    pub(super) fn new(stroke: EngineStroke) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            doc: stroke.doc().clone(),
            seed: stroke.seed(),
            wants_time: stroke.wants_time(),
            string: stroke.smoothing_string(),
            queue: Arc::new(wasm_sync::Mutex::new(Queue { stroke: Some(stroke), points: Vec::new(), finish: false, running: false })),
            tx,
            rx,
            sent: 0,
            shown: 0,
            finishing: false,
            failed: None,
            finished: None,
        }
    }

    pub(super) fn doc(&self) -> &Arc<Document> {
        &self.doc
    }

    /// See `LiveStroke::wants_time`, as of the points shown.
    pub(super) fn wants_time(&self) -> bool {
        self.wants_time
    }

    /// See `LiveStroke::smoothing_string`, as of the points shown.
    pub(super) fn smoothing_string(&self) -> SmoothingString {
        self.string
    }

    /// Hand `pts` over to be rendered after the points before them.
    pub(super) fn push(&mut self, pts: &[StrokePoint]) {
        self.sent += pts.len();
        self.hand_over(|q| q.points.extend_from_slice(pts));
    }

    /// The stroke ended: once its points are rendered it is finished for its commit
    /// ([`Self::take_finished`]).
    pub(super) fn finish(&mut self) {
        self.finishing = true;
        self.hand_over(|q| q.finish = true);
    }

    fn hand_over(&mut self, add: impl FnOnce(&mut Queue)) {
        let start = {
            let mut q = lock(&self.queue);
            add(&mut q);
            !std::mem::replace(&mut q.running, true)
        };
        if start {
            let (queue, tx) = (self.queue.clone(), self.tx.clone());
            run(on_pool(), move || work(&queue, &tx));
        }
    }

    /// Is the job still rendering points or finishing the stroke?
    pub(super) fn in_flight(&self) -> bool {
        self.failed.is_none() && (self.shown < self.sent || self.finishing)
    }

    /// The finished stroke, when the job has made it.
    pub(super) fn take_finished(&mut self) -> Option<Finished> {
        self.finished.take()
    }

    /// The stroke as the job last left it, while the job isn't rendering with it (tests).
    #[cfg(test)]
    pub(super) fn with_stroke<R>(&self, f: impl FnOnce(&EngineStroke) -> R) -> Option<R> {
        lock(&self.queue).stroke.as_ref().map(f)
    }

    /// Take what the job finished: the damage of each step the document now shows more, in
    /// order. `Err`: the stroke can't be shown any further, and why (its commit renders it).
    pub(super) fn land(&mut self, wait: Wait) -> Result<Vec<DRect>, String> {
        let mut steps = Vec::new();
        while let Ok(landed) = self.rx.try_recv() {
            self.take(landed, &mut steps);
        }
        if wait == Wait::All {
            while self.in_flight() {
                let Some(landed) = wait_for(&self.rx) else { break };
                self.take(landed, &mut steps);
            }
        }
        match &self.failed {
            Some(why) => Err(why.clone()),
            None => Ok(steps),
        }
    }

    fn take(&mut self, landed: Landed, steps: &mut Vec<DRect>) {
        match landed {
            Landed::Step { doc, damage, points, wants_time, string } => {
                self.doc = doc;
                self.shown += points;
                self.wants_time = wants_time;
                self.string = string;
                steps.push(damage);
            }
            Landed::Finished(f) => {
                self.finished = Some(f);
                self.finishing = false;
            }
            Landed::Failed(why) => self.failed = Some(why),
        }
    }
}

fn lock(queue: &wasm_sync::Mutex<Queue>) -> impl std::ops::DerefMut<Target = Queue> + '_ {
    queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The next thing the job finishes, waiting until it comes.
#[cfg(not(target_arch = "wasm32"))]
fn wait_for(rx: &Receiver<Landed>) -> Option<Landed> {
    let _s = crate::perf_span::span("stroke.wait");
    rx.recv().ok()
}

/// The page's own thread can't sleep in the browser: it looks until the job's result comes (the
/// job runs on the pool's workers).
#[cfg(target_arch = "wasm32")]
fn wait_for(rx: &Receiver<Landed>) -> Option<Landed> {
    loop {
        match rx.try_recv() {
            Ok(l) => return Some(l),
            Err(std::sync::mpsc::TryRecvError::Empty) => std::hint::spin_loop(),
            Err(_) => return None,
        }
    }
}

/// The job: render what the frames handed over, in order, until nothing is left.
fn work(queue: &wasm_sync::Mutex<Queue>, tx: &Sender<Landed>) {
    loop {
        let (mut stroke, points, finish) = {
            let mut q = lock(queue);
            let finish = q.finish && q.points.is_empty();
            let stroke = if q.points.is_empty() && !finish { None } else { q.stroke.take() };
            let Some(stroke) = stroke else {
                q.running = false;
                return;
            };
            if finish {
                q.finish = false;
                q.running = false;
            }
            (stroke, std::mem::take(&mut q.points), finish)
        };
        #[cfg(test)]
        let pace = crate::cpu_canvas::Pace::start();
        let step = if finish { stroke.end() } else { stroke.step(&points) };
        #[cfg(test)]
        pace.hold();
        let damage = match step {
            Ok(damage) => damage,
            Err(e) => {
                // (The stroke may be dropped by now: nothing to tell.)
                let _ = tx.send(Landed::Failed(e.to_string()));
                lock(queue).running = false;
                return;
            }
        };
        if !finish || !damage.is_empty() {
            let _ = tx.send(Landed::Step { doc: stroke.doc().clone(), damage, points: points.len(), wants_time: stroke.wants_time(), string: stroke.smoothing_string() });
        }
        if finish {
            let _ = tx.send(stroke.into_finished().map_or_else(|e| Landed::Failed(e.to_string()), Landed::Finished));
            return;
        }
        lock(queue).stroke = Some(stroke);
    }
}

/// Run the job: on the worker pool, or in the call where there is none or strokes are rendered
/// in the frame ([`on_pool`]).
fn run(pool: bool, f: impl FnOnce() + Send + 'static) {
    if pool {
        crate::cpu_canvas::spawn(f);
    } else {
        f();
    }
}

/// Are strokes rendered on the worker pool (and so is what follows them: the footprint trail of
/// the tools that show one)? Else they are rendered in the frame, where there is no pool.
pub(crate) fn on_pool() -> bool {
    #[cfg(test)]
    {
        OFF_FRAME.with(std::cell::Cell::get)
    }
    #[cfg(not(test))]
    {
        crate::cpu_canvas::POOL
    }
}

#[cfg(test)]
thread_local! {
    /// Tests render strokes in the frame, so they read a stroke's pixels the frame they draw it;
    /// one that measures frames renders them on the pool, as the app does ([`set_off_frame`]).
    static OFF_FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Render strokes on the worker pool, as the app does (tests).
#[cfg(test)]
pub(crate) fn set_off_frame(on: bool) {
    OFF_FRAME.with(|o| o.set(on));
}

#[cfg(test)]
mod tests;
