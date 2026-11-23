//! A Brush, Pencil or Eraser stroke rendered off the window's thread while it is drawn.
//!
//! A large soft tip's dab is millions of pixels (a 1200 px soft tip reaches 2075 px across: 4.3
//! megapixels of coverage, then the same again composited onto the layer), and rendering the
//! frame's dabs in the frame held it for 70 to 130 ms. The frame now only hands the stroke its
//! points: a job on the worker pool renders them, every point in the order it was drawn, and
//! hands back the document with whole dabs. The canvas shows the last document handed back, so
//! the stroke appears as it lands, never part of a dab, and the window is never held.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender};

use photocraft_doc::Document;
use photocraft_engine::brush_cmds::{LiveCommit, LiveStroke};
use photocraft_engine::paint::StrokePoint;

use super::DRect;

/// How long a frame waits for the points it just handed over (milliseconds). A small tip's dabs
/// are rendered in a fraction of this, so they show in the frame of the pointer move that drew
/// them, as when the frame rendered them itself. A frame that finds the points of the frame
/// before still not rendered doesn't wait at all ([`BrushLive::land`]).
const WAIT_MS: f64 = 2.0;

/// The smoothing string's ends (see `LiveStroke::smoothing_string`).
type SmoothingString = Option<([f64; 2], [f64; 2])>;

/// How long [`BrushLive::land`] waits for the stroke's job.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Wait {
    /// Not at all: what is done already.
    No,
    /// A frame's share ([`WAIT_MS`]), for a stroke that keeps up.
    Frame,
    /// Until everything handed over is rendered: another edit is about to run on the document.
    All,
}

/// What the job hands back.
enum Landed {
    /// Points rendered: the document with them, complete.
    Step { doc: Arc<Document>, damage: DRect, points: usize, wants_time: bool, string: SmoothingString },
    /// The stroke, finished for its commit.
    Commit(Box<LiveCommit>),
    /// The stroke couldn't render its points or finish: its commit renders the stroke itself.
    Failed,
}

/// What the frame hands the job.
struct Queue {
    /// The stroke, while no job is rendering with it.
    stroke: Option<Box<LiveStroke>>,
    /// Points to render, in the order they were drawn.
    points: Vec<StrokePoint>,
    /// The stroke ended: finish it for its commit once every point is rendered.
    finish: bool,
    /// A job is at work (it takes what arrives meanwhile before it ends).
    running: bool,
}

/// The engine's live stroke, rendered by a job on the worker pool.
pub(super) struct BrushLive {
    queue: Arc<wasm_sync::Mutex<Queue>>,
    tx: Sender<Landed>,
    rx: Receiver<Landed>,
    /// The document with the points rendered so far.
    doc: Arc<Document>,
    /// The jitter seed the commit must reuse.
    pub(super) seed: u64,
    wants_time: bool,
    string: SmoothingString,
    /// Points handed over, and how many of them the document shows.
    sent: usize,
    shown: usize,
    /// Points handed over when a frame last waited for the stroke.
    mark: usize,
    /// The stroke ended and its commit isn't made yet.
    finishing: bool,
    failed: bool,
    commit: Option<Box<LiveCommit>>,
}

impl BrushLive {
    pub(super) fn new(stroke: LiveStroke) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            doc: stroke.doc.clone(),
            seed: stroke.seed,
            wants_time: stroke.wants_time(),
            string: stroke.smoothing_string(),
            queue: Arc::new(wasm_sync::Mutex::new(Queue { stroke: Some(Box::new(stroke)), points: Vec::new(), finish: false, running: false })),
            tx,
            rx,
            sent: 0,
            shown: 0,
            mark: 0,
            finishing: false,
            failed: false,
            commit: None,
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
    /// ([`Self::take_commit`]).
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
            run(wait_ms().is_some(), move || work(&queue, &tx));
        }
    }

    /// Is the job still rendering points or finishing the stroke?
    pub(super) fn in_flight(&self) -> bool {
        !self.failed && (self.shown < self.sent || self.finishing)
    }

    /// The finished stroke, when the job has made it.
    pub(super) fn take_commit(&mut self) -> Option<Box<LiveCommit>> {
        self.commit.take()
    }

    /// Take what the job finished: the damage of each step the document now shows more, in
    /// order. `Err`: the stroke can't be shown (its commit renders it).
    pub(super) fn land(&mut self, wait: Wait) -> Result<Vec<DRect>, ()> {
        let mut steps = Vec::new();
        while let Ok(landed) = self.rx.try_recv() {
            self.take(landed, &mut steps);
        }
        // A stroke that keeps up shows each move in its own frame: the frame waits its share for
        // the points it handed over. One that hadn't rendered what the frame before handed it
        // lands when it is ready, and the frame goes on.
        let until = match wait {
            Wait::No => None,
            Wait::All => Some(f64::INFINITY),
            Wait::Frame => wait_ms().filter(|_| self.shown >= self.mark).map(|ms| crate::gpu_canvas::now_ms() + ms),
        };
        if let Some(until) = until {
            while self.in_flight() {
                let Some(landed) = wait_for(&self.rx, until - crate::gpu_canvas::now_ms()) else { break };
                self.take(landed, &mut steps);
            }
        }
        if wait == Wait::Frame {
            self.mark = self.sent;
        }
        if self.failed { Err(()) } else { Ok(steps) }
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
            Landed::Commit(c) => {
                self.commit = Some(c);
                self.finishing = false;
            }
            Landed::Failed => self.failed = true,
        }
    }
}

fn lock(queue: &wasm_sync::Mutex<Queue>) -> impl std::ops::DerefMut<Target = Queue> + '_ {
    queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The next thing the job finished, waiting up to `ms` for it (infinite: until it comes).
#[cfg(not(target_arch = "wasm32"))]
fn wait_for(rx: &Receiver<Landed>, ms: f64) -> Option<Landed> {
    let _s = crate::perf_span::span("stroke.wait");
    if ms.is_finite() { rx.recv_timeout(std::time::Duration::from_secs_f64(ms.max(0.0) / 1000.0)).ok() } else { rx.recv().ok() }
}

/// The page's own thread can't sleep in the browser: a frame's share is what is done already,
/// and waiting for all of it looks until it comes (the job runs on the pool's workers).
#[cfg(target_arch = "wasm32")]
fn wait_for(rx: &Receiver<Landed>, ms: f64) -> Option<Landed> {
    loop {
        match rx.try_recv() {
            Ok(l) => return Some(l),
            Err(std::sync::mpsc::TryRecvError::Empty) if !ms.is_finite() => std::hint::spin_loop(),
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
        if finish {
            let commit = stroke.into_commit().map_or(Landed::Failed, |c| Landed::Commit(Box::new(c)));
            #[cfg(test)]
            pace.hold();
            // (The stroke may be dropped by now: nothing to tell.)
            let _ = tx.send(commit);
            return;
        }
        // The stroke as releasing now would leave it: the smoothing tail up to date in every
        // document handed back.
        let step = stroke.push(&points).and_then(|r| Ok(r.union(&stroke.finalize_tail()?)));
        #[cfg(test)]
        pace.hold();
        let Ok(damage) = step else {
            let _ = tx.send(Landed::Failed);
            lock(queue).running = false;
            return;
        };
        let _ = tx.send(Landed::Step { doc: stroke.doc.clone(), damage, points: points.len(), wants_time: stroke.wants_time(), string: stroke.smoothing_string() });
        lock(queue).stroke = Some(stroke);
    }
}

/// Run the job: on the worker pool, or in the call where there is none or strokes are rendered
/// in the frame ([`wait_ms`]).
fn run(pool: bool, f: impl FnOnce() + Send + 'static) {
    if pool {
        crate::cpu_canvas::spawn(f);
    } else {
        f();
    }
}

/// How long a frame waits for the points it handed over (`None`: they are rendered in the frame,
/// where there is no worker pool).
fn wait_ms() -> Option<f64> {
    #[cfg(test)]
    {
        OFF_FRAME.with(std::cell::Cell::get).then_some(WAIT_MS)
    }
    #[cfg(not(test))]
    {
        crate::cpu_canvas::POOL.then_some(WAIT_MS)
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
