//! The worker pool held busy, for tests of work the window's thread does that must not wait for
//! the pool: on a weak machine every worker can be compositing an edit for seconds, and a
//! parallel iterator started from outside the pool waits for one of them to be free.

use std::sync::mpsc;
use std::time::Duration;

/// Run `f` on a thread of its own while every worker of the global pool is held, and give its
/// result if it came within `within` (`None`: it waited for the pool). The workers are let go
/// either way.
pub fn while_busy<T: Send + 'static>(within: Duration, f: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let workers = rayon::current_num_threads();
    let release = std::sync::Arc::new(std::sync::Barrier::new(workers + 1));
    let (started, taken) = mpsc::channel();
    for _ in 0..workers {
        let (started, release) = (started.clone(), release.clone());
        rayon::spawn(move || {
            let _ = started.send(());
            release.wait();
        });
    }
    let held = (0..workers).all(|_| taken.recv_timeout(Duration::from_secs(120)).is_ok());
    assert!(held, "the pool's workers never took the jobs that hold them");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    let made = rx.recv_timeout(within).ok();
    release.wait();
    made
}
