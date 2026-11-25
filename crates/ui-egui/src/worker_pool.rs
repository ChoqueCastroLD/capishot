//! The worker pool keeps a core free for the window's thread.
//!
//! rayon's global pool has a thread per core by default. A job that fills it (the images of a
//! Free Transform box being opened, made off the frame with a parallel composite, while the canvas
//! makes its drag cache ahead) then leaves the window's thread no core of its own: it waits for one
//! whenever it wakes for a frame, for as long as the scheduler lets a worker run. On Windows that
//! is a quantum, 15 to 40 ms, so frames of a few milliseconds of UI took 22 to 41 ms, the time
//! landing in whichever panel was being laid out when the thread was put aside (the owner's PC,
//! 0.13.41-0.13.45: `ui.windows` 33, `ui.status_bar` 22, `dock.properties` 41, `ui.title_bar`
//! 31 ms in the frame Ctrl+T opened the box). With one thread fewer than the cores, the pool's
//! jobs never take the core the window's thread runs on. `RAYON_NUM_THREADS` still sets the size.

/// The pool's threads for a machine with `cores` (logical) cores: all but one, at least one.
pub fn threads(cores: usize) -> usize {
    cores.saturating_sub(1).max(1)
}

/// Make rayon's global pool [`threads`] wide. Call first thing in `main`, before anything uses
/// the pool (it is made once, by whoever uses it first).
pub fn init() {
    let mut pool = rayon::ThreadPoolBuilder::new();
    if std::env::var_os("RAYON_NUM_THREADS").is_none() {
        pool = pool.num_threads(threads(std::thread::available_parallelism().map_or(1, std::num::NonZero::get)));
    }
    if let Err(e) = pool.build_global() {
        log::warn!("Worker pool: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_leaves_one_core_and_has_at_least_one_thread() {
        assert_eq!([1, 2, 4, 12].map(threads), [1, 1, 3, 11]);
    }

    /// What the kernel says of this thread so far: the time it ran and the time it waited for a
    /// core while ready to run (ms).
    fn scheduled() -> (f64, f64) {
        let stat = std::fs::read_to_string("/proc/thread-self/schedstat").expect("schedstat");
        let mut ns = stat.split_whitespace().map(|v| v.parse::<f64>().expect("a number") / 1e6);
        (ns.next().expect("run time"), ns.next().expect("wait time"))
    }

    /// Run in a process of its own by [`the_window_thread_keeps_a_core_while_the_pool_is_full`]
    /// (the pool is the process's): the pool made as the app makes it (`WORKER_POOL_CHILD=app`)
    /// or as rayon makes it alone, every thread of it kept busy, as opening a box keeps it, while
    /// this thread draws 60 frames of 3 ms of work, 5 ms apart. Prints how long it waited for a
    /// core.
    #[test]
    #[ignore = "run in its own process by the_window_thread_keeps_a_core_while_the_pool_is_full"]
    fn child() {
        let Some(mode) = std::env::var_os("WORKER_POOL_CHILD") else { return };
        if mode == "app" {
            init();
        }
        let n = rayon::current_num_threads();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        for _ in 0..=n {
            let stop = stop.clone();
            rayon::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(30));
        let (mut waited, mut worst) = (0.0f64, 0.0f64);
        for _ in 0..60 {
            let (t, before) = (std::time::Instant::now(), scheduled());
            while t.elapsed() < std::time::Duration::from_millis(3) {
                std::hint::spin_loop();
            }
            let w = scheduled().1 - before.1;
            waited += w;
            worst = worst.max(w);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        println!("POOL threads {n} waited {waited:.1} worst {worst:.1}");
    }

    /// On two cores with every pool thread busy: the app's pool has one thread, so the window's
    /// thread has the other core; rayon's own has two and the frames wait for a core.
    #[test]
    #[cfg(target_os = "linux")]
    fn the_window_thread_keeps_a_core_while_the_pool_is_full() {
        let exe = std::env::current_exe().expect("the test binary");
        let run = |mode: &str| -> Option<(usize, f64, f64)> {
            let out = std::process::Command::new("taskset")
                .arg("-c")
                .arg("0,1")
                .arg(&exe)
                .args(["--exact", "worker_pool::tests::child", "--ignored", "--nocapture", "--test-threads=1"])
                .env("WORKER_POOL_CHILD", mode)
                .output()
                .ok()?;
            let text = String::from_utf8_lossy(&out.stdout);
            // (The harness prints the test's name on the same line.)
            let line = text.lines().find_map(|l| l.find("POOL ").map(|at| &l[at..]))?;
            let v: Vec<&str> = line.split_whitespace().collect();
            Some((v[2].parse().ok()?, v[4].parse().ok()?, v[6].parse().ok()?))
        };
        let (Some(rayon_alone), Some(app)) = (run("rayon"), run("app")) else {
            eprintln!("skipping: taskset can't run the test binary here");
            return;
        };
        eprintln!(
            "60 frames of 3 ms, every pool thread busy, 2 cores: rayon's pool {} threads, waited for a core {:.1} ms (worst frame {:.1}); the app's {} thread, {:.1} ms (worst {:.1})",
            rayon_alone.0, rayon_alone.1, rayon_alone.2, app.0, app.1, app.2
        );
        assert_eq!(rayon_alone.0, 2, "rayon alone: a thread per core");
        assert_eq!(app.0, 1, "the app's pool leaves the window's thread a core");
    }
}
