//! Where runs evaluate and how they share a reasoning pool (F16 review findings F1 and
//! F5): a run called on a rayon worker never lets that worker take another job while it
//! holds the writer lock, and runs that ask for fewer threads than the pool's stay within
//! its capacity together.

use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, Incremental, Profile, ReasonOptions, ReasonReport, materialize_incremental,
    materialize_with, thread_pool,
};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

fn store(n: usize) -> Store {
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> . @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B . ex:B rdfs:subClassOf ex:C .\n",
    );
    for i in 0..n {
        ttl.push_str(&format!("ex:i{i} a ex:A .\n"));
    }
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// The tests of this file one at a time: one counts the threads of the whole process.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Run `f` on a thread of its own and fail when it does not end within `limit`.
fn within<T: Send + 'static>(
    limit: Duration,
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(limit) {
        Ok(v) => v,
        Err(mpsc::RecvTimeoutError::Timeout) => panic!("{what} did not end within {limit:?}"),
        Err(mpsc::RecvTimeoutError::Disconnected) => panic!("{what} failed"),
    }
}

/// A progress callback that, once the run evaluates, waits until `flag` is set or
/// `wait` passed.
fn wait_for(flag: Arc<AtomicBool>, wait: Duration) -> sparkles_reasoner::ProgressFn {
    Arc::new(move |_, msg: &str| {
        if msg == "reasoning" {
            let t0 = Instant::now();
            while !flag.load(Ordering::SeqCst) && t0.elapsed() < wait {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    })
}

/// Two runs on one store joined on a one-thread pool, with a reasoning pool of their
/// own: while the first holds the writer lock and evaluates on the reasoning pool, the
/// caller's only worker must not start the second, which would wait for that lock on
/// the stack of the first.
#[test]
fn f16_foreign_pool_writer_reentry() {
    within(Duration::from_secs(120), "the joined runs", || {
        for incremental in [false, true] {
            let s = Arc::new(store(200));
            let other = Arc::new(store(50));
            let cache = Arc::new(Cache::default());
            let rdfs = Profile::RdfsSimple;
            let before = materialize_with(&s, &rdfs, &Extras::default(), &Default::default())
                .unwrap()
                .receipt
                .unwrap()
                .commit
                .seq;
            let caller = rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .build()
                .unwrap();
            let pool = thread_pool(2).unwrap();
            let second_started = Arc::new(AtomicBool::new(false));
            let first = ReasonOptions {
                pool: Some(pool.clone()),
                // the first run waits, while it evaluates, for the second to start,
                // which can only happen on the caller's worker
                progress: Some(wait_for(second_started.clone(), Duration::from_secs(1))),
                ..Default::default()
            };
            let second = ReasonOptions {
                pool: Some(pool.clone()),
                ..Default::default()
            };
            let run = |s: &Store, o: &ReasonOptions| -> anyhow::Result<ReasonReport> {
                let inc = Incremental {
                    since: incremental.then_some(before),
                    cache: Some(&cache),
                };
                materialize_incremental(s, &rdfs, &Extras::default(), inc, o)
            };
            let (a, b) = caller.install(|| {
                rayon::join(
                    || run(&s, &first),
                    || {
                        second_started.store(true, Ordering::SeqCst);
                        run(&s, &second)
                    },
                )
            });
            let (a, b) = (a.unwrap(), b.unwrap());
            let (ra, rb) = (a.receipt.unwrap(), b.receipt.unwrap());
            assert!(ra.commit.seq >= before && rb.commit.seq >= ra.commit.seq);
            assert_eq!(a.inferred, b.inferred);
            assert_eq!(a.inferred, 401);
            // a different store on the same pools: both complete
            let (c, d) =
                caller.install(|| rayon::join(|| run(&other, &first), || run(&s, &second)));
            assert_eq!(c.unwrap().inferred, 101);
            assert_eq!(d.unwrap().inferred, 401);
        }
    });
}

/// The reasoning threads alive in the process: the shared pool's and those of runs'
/// own pools, by name.
fn reasoning_threads() -> usize {
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return 0;
    };
    dir.filter_map(|e| std::fs::read_to_string(e.ok()?.path().join("comm")).ok())
        .filter(|n| n.starts_with("reason") || n.starts_with("sparkles-reason"))
        .count()
}

/// With a pool of eight, four runs that ask for four threads each and one that asks for
/// none run at once: the threads of the runs' own pools, beside the pool's eight, never
/// exceed eight, and each run keeps its thread count.
#[cfg(target_os = "linux")]
#[test]
fn f16_reduced_requests_share_capacity() {
    within(Duration::from_secs(120), "the pool runs", || {
        let pool = thread_pool(8).unwrap();
        // the pool's own threads start with it
        let base = reasoning_threads();
        let stores: Vec<Arc<Store>> = (0..5).map(|_| Arc::new(store(100))).collect();
        let done = Arc::new(AtomicBool::new(false));
        let peak = Arc::new(AtomicUsize::new(0));
        let sampler = {
            let (done, peak) = (done.clone(), peak.clone());
            std::thread::spawn(move || {
                while !done.load(Ordering::SeqCst) {
                    let extra = reasoning_threads().saturating_sub(base);
                    peak.fetch_max(extra, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
        };
        let slow: sparkles_reasoner::ProgressFn = Arc::new(|_, msg: &str| {
            if msg == "reasoning" {
                std::thread::sleep(Duration::from_millis(300));
            }
        });
        let runs: Vec<_> = stores
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let opts = ReasonOptions {
                    pool: Some(pool.clone()),
                    threads: (i < 4).then_some(4),
                    progress: Some(slow.clone()),
                    ..Default::default()
                };
                let s = s.clone();
                std::thread::spawn(move || {
                    materialize_with(&s, &Profile::RdfsSimple, &Extras::default(), &opts)
                })
            })
            .collect();
        let reports: Vec<ReasonReport> = runs
            .into_iter()
            .map(|h| h.join().unwrap().unwrap())
            .collect();
        done.store(true, Ordering::SeqCst);
        sampler.join().unwrap();
        for (i, r) in reports.iter().enumerate() {
            assert_eq!(r.threads, if i < 4 { 4 } else { 8 }, "run {i}");
            assert_eq!(r.inferred, 201);
        }
        let peak = peak.load(Ordering::SeqCst);
        assert!(peak > 0, "the sampler saw no run's own pool");
        assert!(
            peak <= 8,
            "{peak} threads of runs' own pools beside the pool's eight"
        );
    });
}

/// A run waiting for admission stops when it is cancelled, and a run that panics gives
/// its workers back; requests of zero threads, of the pool's size or more, and runs
/// without a pool are admitted as before.
#[test]
fn f16_admission_cancel_panic_and_sizes() {
    within(Duration::from_secs(120), "the admission runs", || {
        let pool = thread_pool(4).unwrap();
        let s = Arc::new(store(50));
        let other = Arc::new(store(50));
        // a run holds the whole pool while it evaluates
        let hold = Arc::new(AtomicBool::new(false));
        let holder = {
            let (pool, s, hold) = (pool.clone(), s.clone(), hold.clone());
            std::thread::spawn(move || {
                let opts = ReasonOptions {
                    pool: Some(pool),
                    progress: Some(wait_for(hold, Duration::from_secs(60))),
                    ..Default::default()
                };
                materialize_with(&s, &Profile::RdfsSimple, &Extras::default(), &opts)
            })
        };
        let t0 = Instant::now();
        while pool.admitted_workers() < 4 {
            assert!(t0.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(2));
        }
        // a run of its own pool waits, then is cancelled
        let cancel = Arc::new(AtomicBool::new(false));
        let waiter = {
            let (pool, other, cancel) = (pool.clone(), other.clone(), cancel.clone());
            std::thread::spawn(move || {
                let opts = ReasonOptions {
                    pool: Some(pool),
                    threads: Some(2),
                    cancel: Some(cancel),
                    ..Default::default()
                };
                materialize_with(&other, &Profile::RdfsSimple, &Extras::default(), &opts)
            })
        };
        while pool.waiting_runs() == 0 {
            assert!(t0.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(2));
        }
        cancel.store(true, Ordering::SeqCst);
        let e = waiter.join().unwrap().unwrap_err();
        assert!(format!("{e:#}").contains("cancelled"), "{e:#}");
        assert_eq!(pool.waiting_runs(), 0);
        // the cancelled run changed nothing
        assert!(
            other
                .snapshot()
                .lookup_iri(sparkles_reasoner::INFERRED_GRAPH)
                .is_none()
        );
        hold.store(true, Ordering::SeqCst);
        assert_eq!(holder.join().unwrap().unwrap().threads, 4);
        assert_eq!(pool.admitted_workers(), 0);
        // a run that panics while it evaluates gives its workers back
        let boom: sparkles_reasoner::ProgressFn = Arc::new(|_, msg: &str| {
            if msg == "reasoning" {
                panic!("boom");
            }
        });
        for threads in [None, Some(2)] {
            let opts = ReasonOptions {
                pool: Some(pool.clone()),
                threads,
                progress: Some(boom.clone()),
                ..Default::default()
            };
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                materialize_with(&s, &Profile::RdfsSimple, &Extras::default(), &opts)
            }));
            assert!(r.is_err());
            assert_eq!(pool.admitted_workers(), 0, "{threads:?}");
        }
        // sizes
        let seen = Arc::new(Mutex::new(Vec::new()));
        for (threads, want) in [(Some(0), 4), (Some(4), 4), (Some(9), 4), (Some(1), 1)] {
            let opts = ReasonOptions {
                pool: Some(pool.clone()),
                threads,
                ..Default::default()
            };
            let r = materialize_with(&s, &Profile::RdfsSimple, &Extras::default(), &opts).unwrap();
            assert_eq!(r.threads, want, "{threads:?}");
            seen.lock().unwrap().push(r.threads);
        }
        assert_eq!(pool.admitted_workers(), 0);
        let r = materialize_with(
            &s,
            &Profile::RdfsSimple,
            &Extras::default(),
            &ReasonOptions {
                threads: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.threads, 3);
    });
}
