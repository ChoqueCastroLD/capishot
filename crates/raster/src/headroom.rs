//! How much memory can still be allocated, asked of the allocator itself.
//!
//! On the web (`wasm32-unknown-unknown`) a failed infallible allocation aborts the module and the
//! page freezes with no message, and stable Rust cannot turn that into an error. The allocations
//! made with `try_reserve*` do survive, though: the allocator hands back null when the browser
//! refuses to grow the linear memory, and the caller gets an error. So before a big operation
//! (place, resize, re-render a smart object, save) the operation asks [`ensure`] whether its
//! estimated need can be allocated right now, and fails with a message the user can act on
//! instead of trapping part-way.
//!
//! The answer is a trial reservation: one `Vec::try_reserve_exact`, freed at once. It sees the
//! real ceiling of this browser and device (a phone may stop well below 4 GiB) and the
//! fragmentation of the heap, costs nothing per allocation, and needs no allocator wrapper. The
//! freed block returns to the allocator's free list, so the operation reuses it. A reservation
//! is not written to, so the browser commits no physical pages for it.
//!
//! [`below`], which the low-memory warning asks every few seconds, does not reserve anything:
//! wasm linear memory never shrinks, so a periodic trial would raise the module's high-water
//! mark by the amount it asks about, for nothing. It reads the size of linear memory instead.
//!
//! A browser kills a tab long before its module reaches the 4 GiB wasm32 ceiling (a 4 GB laptop's
//! at about 2.6 GB, a phone's sooner), so the web shell sets the tab's [`limit`] from the device's
//! memory and keeps telling [`set_page_memory`] what the page takes as the browser measures it
//! (the AI worker and the JavaScript heap included): [`ensure`], [`below`] and [`over`] count
//! against that, not the ceiling alone.
//!
//! Native targets overcommit, so the question has no useful answer there: every check passes and
//! desktop behaviour is unchanged.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::AllocationError;

/// Room kept free after an operation's own need: frames, egui, notices and the error path.
pub const RESERVE: u64 = 128 << 20;

/// Marks [`simulate_available`] as off.
const OFF: u64 = u64::MAX;

static SIMULATED: AtomicU64 = AtomicU64::new(OFF);

/// Pretends that only `bytes` can be allocated (`None` stops pretending). The simulation is
/// process-wide and applies on every target. It is for tests and the web build's `?free-mb`
/// diagnostic flag only, so the gates and the low-memory warning can be seen without allocating
/// gigabytes.
pub fn simulate_available(bytes: Option<u64>) {
    SIMULATED.store(bytes.unwrap_or(OFF), Ordering::SeqCst);
}

fn simulated() -> Option<u64> {
    match SIMULATED.load(Ordering::SeqCst) {
        OFF => None,
        n => Some(n),
    }
}

/// The most linear memory a wasm32 module can address.
pub const WASM32_CEILING: u64 = 4 << 30;

/// The tab's memory limit ([`set_limit`]; 0: none set).
static LIMIT: AtomicU64 = AtomicU64::new(0);
/// What the page takes as last measured ([`set_page_memory`]).
static PAGE: AtomicU64 = AtomicU64::new(0);

/// Where the browser would kill this page (bytes; `None`: no limit, as on the desktop). The web
/// shell sets it at start-up from the device's memory; at most [`WASM32_CEILING`].
pub fn set_limit(bytes: Option<u64>) {
    LIMIT.store(bytes.map_or(0, |b| b.clamp(1, WASM32_CEILING)), Ordering::SeqCst);
}

/// The tab's memory limit, if one was set ([`set_limit`]).
pub fn limit() -> Option<u64> {
    match LIMIT.load(Ordering::SeqCst) {
        0 => None,
        n => Some(n),
    }
}

/// What the page takes now as the browser measures it (bytes: the module, its workers, the
/// JavaScript heap), reported by the web shell as measurements come in.
pub fn set_page_memory(bytes: u64) {
    PAGE.store(bytes, Ordering::SeqCst);
}

/// What the page takes: the larger of the module's linear memory and the last measurement.
fn used() -> u64 {
    linear_memory().max(PAGE.load(Ordering::SeqCst))
}

#[cfg(target_arch = "wasm32")]
fn linear_memory() -> u64 {
    (core::arch::wasm32::memory_size::<0>() as u64).saturating_mul(64 << 10)
}

#[cfg(not(target_arch = "wasm32"))]
fn linear_memory() -> u64 {
    0
}

/// `true` when `bytes` can be allocated in one piece right now, within the tab's [`limit`]. Always
/// `true` off the web.
fn can_allocate(bytes: u64) -> bool {
    if let Some(available) = simulated() {
        return bytes <= available;
    }
    if let Some(l) = limit()
        && used().saturating_add(bytes) > l
    {
        return false;
    }
    trial(bytes)
}

#[cfg(target_arch = "wasm32")]
fn trial(bytes: u64) -> bool {
    let Ok(bytes) = usize::try_from(bytes) else { return false };
    let mut probe: Vec<u8> = Vec::new();
    probe.try_reserve_exact(bytes).is_ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn trial(_bytes: u64) -> bool {
    true
}

/// `Ok` when `need` bytes (plus [`RESERVE`]) can be allocated right now. On the web, this tries
/// to reserve that much and frees it at once. On other targets it always passes (unless a
/// simulation is set).
pub fn ensure(need: u64) -> Result<(), AllocationError> {
    let total = need.saturating_add(RESERVE);
    let bytes = usize::try_from(total).map_err(|_| AllocationError::SizeOverflow)?;
    if can_allocate(total) { Ok(()) } else { Err(AllocationError::NotEnoughMemory { bytes }) }
}

/// `true` while the page is within `bytes` of what it may take: the tab's [`limit`] (on the web
/// the 4 GiB wasm32 ceiling without one), less the module's linear memory or the page's last
/// measurement, whichever is larger. Always `false` off the web unless a limit or a simulation is
/// set. Allocates nothing, unlike [`ensure`]. Linear memory is a high-water mark, so this errs
/// towards `true`: room freed inside the allocator after a peak doesn't count.
pub fn below(bytes: u64) -> bool {
    if let Some(available) = simulated() {
        return bytes > available;
    }
    left().is_some_and(|left| left < bytes)
}

/// `true` while the page takes more than `fraction` of the tab's [`limit`] (as [`below`] counts
/// it): the point to let go of what can be made again before the browser kills the tab.
pub fn over(fraction: f64) -> bool {
    let room = |total: u64, left: u64| (left as f64) < total as f64 * (1.0 - fraction);
    if let Some(available) = simulated() {
        return limit().is_some_and(|l| room(l, available));
    }
    let total = if cfg!(target_arch = "wasm32") { Some(limit().unwrap_or(WASM32_CEILING)) } else { limit() };
    total.zip(left()).is_some_and(|(t, l)| room(t, l))
}

/// What the page may still take, where that is known.
fn left() -> Option<u64> {
    match limit() {
        Some(l) => Some(l.saturating_sub(used())),
        None if cfg!(target_arch = "wasm32") => Some(WASM32_CEILING.saturating_sub(used())),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static LOCK: Mutex<()> = Mutex::new(());

    /// Serializes the tests (the simulation is global) and resets it when each one ends.
    struct Guard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);

    impl Guard {
        fn new() -> Self {
            let guard = Guard(LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
            simulate_available(None);
            set_limit(None);
            set_page_memory(0);
            guard
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            simulate_available(None);
            set_limit(None);
            set_page_memory(0);
        }
    }

    const MIB: u64 = 1 << 20;

    #[test]
    fn passes_on_native_without_simulation() {
        let _g = Guard::new();
        assert!(ensure(10 << 30).is_ok());
        assert!(!below(64 << 30));
    }

    #[test]
    fn simulated_room_counts_the_reserve() {
        let _g = Guard::new();
        // 300 MiB of room: a 10 MiB need plus the 128 MiB reserve fits, a 200 MiB need does not.
        simulate_available(Some(300 * MIB));
        assert!(ensure(10 * MIB).is_ok());
        assert!(matches!(ensure(200 * MIB), Err(AllocationError::NotEnoughMemory { .. })));
        assert!(!below(300 * MIB));
        assert!(below(301 * MIB));
    }

    /// A 4 GB laptop's tab is killed at about 2.6 GB: with its limit set, the page's measured
    /// memory counts against that, not the 4 GiB ceiling.
    #[test]
    fn the_tabs_limit_and_the_pages_memory_set_the_room() {
        let _g = Guard::new();
        set_limit(Some(2400 * MIB));
        set_page_memory(1800 * MIB);
        assert!(!below(500 * MIB) && below(700 * MIB), "600 MiB left");
        assert!(over(0.7) && !over(0.8), "75 % used");
        assert!(ensure(400 * MIB).is_ok(), "400 + the 128 reserve fit in 600");
        assert!(ensure(500 * MIB).is_err(), "500 + 128 don't");
        set_limit(None);
        assert!(ensure(10 << 30).is_ok() && !below(1 << 30) && !over(0.0), "no limit off the web: as before");
    }

    #[test]
    fn huge_needs_do_not_overflow() {
        let _g = Guard::new();
        let _ = ensure(u64::MAX);
        simulate_available(Some(100 * MIB));
        assert!(ensure(u64::MAX).is_err());
    }

    #[test]
    fn simulation_can_be_switched_off() {
        let _g = Guard::new();
        simulate_available(Some(0));
        assert!(ensure(0).is_err());
        simulate_available(None);
        assert!(ensure(0).is_ok());
    }
}
