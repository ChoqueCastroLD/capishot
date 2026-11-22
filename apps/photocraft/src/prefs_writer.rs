//! The preferences file is written off the window's thread.
//!
//! Each write is a durable file replace (a temporary file, fsync, rename, directory fsync). On
//! the window's thread it held a frame for 35 to 40 ms on the owner's NVMe PC, in the middle of
//! a Hand drag or a brush stroke a second after a tool or view change, and 300 ms to over a
//! second on users' disks (usage statistics, `prefs.persist`). The debounce only made it rarer.
//! Here the frame hands the text over and goes on; one thread writes the newest text handed over
//! (a burst is one write), and the app waits for it when it closes.
//!
//! A failed write is reported once ([`PrefsWriter::take_failure`]) so the app's retry and its
//! "couldn't save preferences" notice work as before, and the write after a failure is made on
//! the caller's thread: the retry then gets its real outcome instead of "handed over", and the
//! app says "saved" only when the file is. That fallback runs only while the disk is refusing
//! writes.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};

type Write = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

#[derive(Default)]
struct State {
    /// The newest text handed over and not yet picked up by the writing thread.
    pending: Option<String>,
    /// The text being written now.
    writing: Option<String>,
    /// The error of a write made off the caller's thread, until it is reported.
    failure: Option<String>,
    /// The last write failed: the next is made on the caller's thread.
    failed: bool,
    started: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    write: Write,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Every field is whole between statements: a poisoned lock is still good to use.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Wait until no text is waiting or being written.
    fn idle(&self) -> MutexGuard<'_, State> {
        let mut st = self.lock();
        while st.pending.is_some() || st.writing.is_some() {
            st = self.changed.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        st
    }
}

/// Writes preference texts with `write`, off the caller's thread. Clones share one writer.
#[derive(Clone)]
pub struct PrefsWriter(Arc<Shared>);

impl PrefsWriter {
    pub fn new(write: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(Shared { state: Mutex::default(), changed: Condvar::new(), write: Box::new(write) }))
    }

    /// Hand `text` over to be written; it replaces a text still waiting. `Ok` means handed over.
    /// After a failed write, this one is made here and its own result returned.
    pub fn save(&self, text: &str) -> Result<(), String> {
        let mut st = self.0.lock();
        if st.failed {
            drop(st);
            let mut st = self.0.idle();
            let result = (self.0.write)(text);
            st.failed = result.is_err();
            st.failure = None;
            return result;
        }
        st.pending = Some(text.to_string());
        if !st.started {
            st.started = true;
            let shared = self.0.clone();
            let spawned = std::thread::Builder::new().name("prefs-writer".into()).spawn(move || run(&shared));
            if spawned.is_err() {
                // No thread to be had: this write and the next are made on the caller's thread.
                st.started = false;
                let text = st.pending.take().unwrap_or_default();
                drop(st);
                return (self.0.write)(&text);
            }
        }
        self.0.changed.notify_all();
        Ok(())
    }

    /// The text the file is about to hold (waiting or being written), if any: what a reader of
    /// the file should take instead of its contents.
    pub fn latest(&self) -> Option<String> {
        let st = self.0.lock();
        st.pending.clone().or_else(|| st.writing.clone())
    }

    /// The error of a write that failed off the caller's thread, once.
    pub fn take_failure(&self) -> Option<String> {
        self.0.lock().failure.take()
    }

    /// Wait for everything handed over to be written (the app is closing).
    pub fn flush(&self) {
        drop(self.0.idle());
    }
}

fn run(shared: &Shared) {
    let mut st = shared.lock();
    loop {
        let Some(text) = st.pending.take() else {
            st = shared.changed.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
            continue;
        };
        st.writing = Some(text.clone());
        drop(st);
        let result = (shared.write)(&text);
        st = shared.lock();
        st.writing = None;
        if let Err(e) = result {
            st.failure = Some(e);
            st.failed = true;
        }
        shared.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// A writer whose writes take `ms` and land in `file`; `fail` makes them fail.
    fn slow(ms: u64, file: &Arc<Mutex<Vec<String>>>, fail: &Arc<std::sync::atomic::AtomicBool>, on: &Arc<Mutex<Vec<std::thread::ThreadId>>>) -> PrefsWriter {
        let (file, fail, on) = (file.clone(), fail.clone(), on.clone());
        PrefsWriter::new(move |text| {
            on.lock().unwrap().push(std::thread::current().id());
            std::thread::sleep(std::time::Duration::from_millis(ms));
            if fail.load(Ordering::Relaxed) {
                return Err("disk full".into());
            }
            file.lock().unwrap().push(text.to_string());
            Ok(())
        })
    }

    #[test]
    fn a_save_returns_at_once_and_the_write_runs_on_another_thread() {
        let (file, fail, on) = (Arc::default(), Arc::default(), Arc::default());
        let w = slow(150, &file, &fail, &on);
        let t = std::time::Instant::now();
        w.save("one").unwrap();
        let handed = t.elapsed();
        assert!(handed.as_millis() < 50, "handing the text over took {handed:?}: the write ran on the caller's thread");
        assert_eq!(w.latest().as_deref(), Some("one"), "a reader of the file gets the text on its way");
        w.flush();
        assert!(t.elapsed().as_millis() >= 150, "flush returned before the write ended");
        assert_eq!(*file.lock().unwrap(), ["one"]);
        assert_eq!(w.latest(), None);
        assert!(on.lock().unwrap().iter().all(|id| *id != std::thread::current().id()), "the write ran on the caller's thread");
        assert_eq!(w.take_failure(), None);
    }

    #[test]
    fn a_burst_writes_the_newest_text_and_not_every_one() {
        let (file, fail, on) = (Arc::default(), Arc::default(), Arc::default());
        let w = slow(60, &file, &fail, &on);
        for i in 0..20 {
            w.save(&format!("text {i}")).unwrap();
        }
        w.flush();
        let written = file.lock().unwrap().clone();
        assert_eq!(written.last().map(String::as_str), Some("text 19"), "the file holds the last text");
        assert!(written.len() <= 3, "20 saves in a burst made {} writes", written.len());
    }

    #[test]
    fn a_failed_write_is_reported_once_and_the_retry_gets_its_own_result() {
        let (file, fail, on): (Arc<Mutex<Vec<String>>>, Arc<std::sync::atomic::AtomicBool>, Arc<Mutex<Vec<std::thread::ThreadId>>>) = (Arc::default(), Arc::default(), Arc::default());
        let w = slow(10, &file, &fail, &on);
        fail.store(true, Ordering::Relaxed);
        w.save("lost").unwrap();
        w.flush();
        assert_eq!(w.take_failure().as_deref(), Some("disk full"));
        assert_eq!(w.take_failure(), None, "reported once");
        // Still failing: the retry is told so itself.
        assert_eq!(w.save("again"), Err("disk full".into()));
        // The disk is back: the retry writes, and later saves go off the thread again.
        fail.store(false, Ordering::Relaxed);
        let writes = on.lock().unwrap().len();
        assert_eq!(w.save("kept"), Ok(()));
        assert_eq!(*file.lock().unwrap(), ["kept"], "the retry's Ok is a written file");
        assert_eq!(on.lock().unwrap().last(), Some(&std::thread::current().id()));
        w.save("after").unwrap();
        w.flush();
        assert_eq!(file.lock().unwrap().last().map(String::as_str), Some("after"));
        assert!(on.lock().unwrap().len() > writes);
        assert_ne!(on.lock().unwrap().last(), Some(&std::thread::current().id()));
    }
}
