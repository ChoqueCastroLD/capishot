//! The planner's 4096-entry tables (gradient stops, Curves, Levels, Gradient Map), made once.
//!
//! A plan is made for every composite, and each one sampled every gradient of the document
//! 4096 times again and evaluated every tone curve again: on the owner's credits PSD (20 layers,
//! a few gradient strokes and overlays) that was 12 to 31 ms of `gpu.plan` in every frame that
//! drew, whatever its size, and the floor under every brush dab, keystroke and drag release.
//! A table depends on nothing but what it is made from, so the plans share it: [`shared`] keeps
//! the last [`CAPACITY`] by their source and hands out the same rows, which the compositor's
//! texture cache then knows by address (`Compositor::frame_res`).

use std::sync::{Arc, Mutex};

use photocraft_color::SampleType;
use photocraft_compose::adjust::Transfer;
use photocraft_doc::{Adjustment, Color, Fill};

/// One row of a table.
pub type Row = [f32; 4096];
/// The rows of a table, shared by the plans that use it.
pub type Lut = Arc<Vec<Row>>;
/// An adjustment's kernel kind and parameters, with its table.
pub type Program = (i32, [[f32; 4]; 4], Option<Lut>);

/// The tables kept: a page's gradients and curves many times over (a table is 16 to 64 KB).
const CAPACITY: usize = 256;

/// What a table is made from.
#[derive(Clone, PartialEq)]
pub(crate) enum Source {
    /// A layer effect's gradient: its colour and opacity stops.
    Gradient(Vec<(f32, Color)>, Vec<(f32, f32)>),
    /// A gradient fill layer.
    Fill(Fill),
    /// An adjustment in a document of this tone transfer and depth.
    Adjust(Adjustment, Transfer, SampleType),
}

struct Entry {
    source: Source,
    /// The CMYK profile the stops were converted through (`CmykSpace::id`, 0: the built-in).
    cmyk: u64,
    kind: i32,
    params: [[f32; 4]; 4],
    rows: Option<Lut>,
    used: u64,
}

struct Memo {
    entries: Vec<Entry>,
    clock: u64,
}

static MEMO: Mutex<Memo> = Mutex::new(Memo { entries: Vec::new(), clock: 0 });

thread_local! {
    /// Tables computed by this thread's plans.
    static BUILT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many tables this thread's plans computed so far: the plan of a document planned before
/// computes none (per thread, so tests running side by side don't count each other's).
pub fn built_here() -> u64 {
    BUILT.with(std::cell::Cell::get)
}

/// The program and table of the source `is` recognises, computed by `build` (and its source
/// kept, from `source`) only when no plan made it before under the CMYK profile active now.
pub(crate) fn shared(is: impl Fn(&Source) -> bool, source: impl FnOnce() -> Source, build: impl FnOnce() -> (i32, [[f32; 4]; 4], Option<Vec<Row>>)) -> Program {
    let cmyk = photocraft_color::convert::active_cmyk_space().map_or(0, |s| s.id);
    {
        // A poisoned lock still holds whole entries (they are pushed complete).
        let mut memo = MEMO.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        memo.clock += 1;
        let now = memo.clock;
        if let Some(e) = memo.entries.iter_mut().find(|e| e.cmyk == cmyk && is(&e.source)) {
            e.used = now;
            return (e.kind, e.params, e.rows.clone());
        }
    }
    // Computed outside the lock: two threads planning the same new table both compute it once.
    let (kind, params, rows) = build();
    BUILT.with(|b| b.set(b.get() + 1));
    let rows = rows.map(Arc::new);
    let mut memo = MEMO.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    memo.clock += 1;
    let used = memo.clock;
    if memo.entries.len() >= CAPACITY
        && let Some(oldest) = memo.entries.iter().enumerate().min_by_key(|(_, e)| e.used).map(|(i, _)| i)
    {
        memo.entries.swap_remove(oldest);
    }
    memo.entries.push(Entry { source: source(), cmyk, kind, params, rows: rows.clone(), used });
    (kind, params, rows)
}
