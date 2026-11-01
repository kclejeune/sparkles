//! `serve --prewarm` (spec X07 §7): record what each persistent dataset's index used, and
//! read it again in the background after the next start is ready.
//!
//! The worker thread waits until the server is ready, prewarms each dataset in turn at a
//! low CPU priority, and then records the lists every `interval`. The shutdown records
//! them once more after the requests have drained ([`record_all`]).

use crate::state::{AppState, DbType};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Start the worker. It ends when the server begins to drain.
pub fn start(st: Arc<AppState>, interval: Duration) {
    let spawned = std::thread::Builder::new()
        .name("prewarm".into())
        .spawn(move || run(&st, interval));
    if let Err(e) = spawned {
        tracing::warn!("--prewarm: the worker thread did not start: {e}");
    }
}

fn draining(st: &AppState) -> bool {
    st.phase() == crate::obs::Phase::Draining
}

fn run(st: &AppState, interval: Duration) {
    while st.phase() == crate::obs::Phase::Starting {
        std::thread::sleep(Duration::from_millis(20));
    }
    if draining(st) {
        return;
    }
    lower_priority();
    let t0 = Instant::now();
    for (name, ds) in st.datasets() {
        if draining(st) {
            return;
        }
        if ds.kind != DbType::Persistent {
            continue;
        }
        match ds.store.prewarm(&|| draining(st)) {
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
            if draining(st) {
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
    // SAFETY: gettid and setpriority take no pointers; on Linux a thread id names the
    // calling thread alone for PRIO_PROCESS
    unsafe {
        let tid = libc::gettid();
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid as libc::id_t, 10);
    }
}
