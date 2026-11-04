//! `serve --prewarm` (spec X07 §7): record what each persistent dataset's index used, and
//! read it again in the background after the next start is ready.
//!
//! The worker thread waits until the server is ready, prewarms each dataset in turn at a
//! low CPU priority, and then records the lists every `interval`. The shutdown stops and
//! joins it, and then records the lists once more after the requests have drained
//! ([`record_all`]).

use crate::state::{AppState, DbType};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// The worker thread, which [`Worker::stop_and_join`] ends before the server tears down.
/// The thread holds the server's state, so a detached one would keep the datasets from
/// closing when the process exits.
pub struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    /// Tell the worker to stop and wait for it. A prewarm or recording in progress stops at
    /// its next check.
    pub fn stop_and_join(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Start the worker. It stops when the server begins to drain or [`Worker::stop_and_join`]
/// asks it to.
pub fn start(st: Arc<AppState>, interval: Duration) -> Option<Worker> {
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let spawned = std::thread::Builder::new()
        .name("prewarm".into())
        .spawn(move || run(&st, interval, &flag));
    match spawned {
        Ok(handle) => Some(Worker {
            stop,
            handle: Some(handle),
        }),
        Err(e) => {
            tracing::warn!("--prewarm: the worker thread did not start: {e}");
            None
        }
    }
}

fn stopped(st: &AppState, stop: &AtomicBool) -> bool {
    stop.load(Ordering::SeqCst) || st.phase() == crate::obs::Phase::Draining
}

fn run(st: &AppState, interval: Duration, stop: &AtomicBool) {
    while st.phase() == crate::obs::Phase::Starting {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if stopped(st, stop) {
        return;
    }
    lower_priority();
    let t0 = Instant::now();
    for (name, ds) in st.datasets() {
        if stopped(st, stop) {
            return;
        }
        if ds.kind != DbType::Persistent {
            continue;
        }
        match ds.store.prewarm(&|| stopped(st, stop)) {
            Ok(done) => match &done.skipped {
                None => tracing::info!(
                    "prewarm of /{name} finished: {} block parts and {:.1} MiB of pages in {:.1} ms",
                    done.parts,
                    done.page_bytes as f64 / (1 << 20) as f64,
                    done.elapsed_ms
                ),
                Some(why) => tracing::info!(
                    "prewarm of /{name} read {} block parts and {:.1} MiB of pages in {:.1} ms: {why}",
                    done.parts,
                    done.page_bytes as f64 / (1 << 20) as f64,
                    done.elapsed_ms
                ),
            },
            Err(e) => tracing::warn!("prewarm of /{name} skipped: {e}"),
        }
    }
    tracing::info!("prewarm finished in {:.1?}", t0.elapsed());
    loop {
        let next = Instant::now() + interval;
        while Instant::now() < next {
            if stopped(st, stop) {
                return;
            }
            std::thread::sleep(Duration::from_millis(250).min(interval));
        }
        record_all(st);
    }
}

/// Record the prewarm list of every persistent dataset.
pub fn record_all(st: &AppState) {
    for (name, ds) in st.datasets() {
        if ds.kind != DbType::Persistent {
            continue;
        }
        match ds.store.record_prewarm() {
            Ok(Some(r)) => tracing::debug!(
                "recorded the prewarm list of /{name}: {} block parts, {} bytes of pages",
                r.parts,
                r.page_bytes
            ),
            Ok(None) => {}
            Err(e) => tracing::warn!("recording the prewarm list of /{name} failed: {e}"),
        }
    }
}

/// Run this thread at a lower CPU priority than the request threads, since its reads
/// and decodes only save work for later requests.
fn lower_priority() {
    #[cfg(target_os = "linux")]
    // SAFETY: the gettid system call and setpriority take no pointers, and on Linux a
    // thread id names the calling thread alone for PRIO_PROCESS. The raw system call
    // keeps the server linking against glibc before 2.30, which has no gettid wrapper.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid);
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 10);
    }
}
