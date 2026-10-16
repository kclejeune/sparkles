use super::*;
use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

fn isolated(name: &str, body: impl FnOnce()) {
    if std::env::var("F14_PAGERANK_C1_CASE").as_deref() == Ok(name) {
        body();
        return;
    }
    let logs = tempfile::tempdir().unwrap();
    let out = logs.path().join("stdout");
    let err = logs.path().join("stderr");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("algo::pagerank::polling_tests::{name}"),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("F14_PAGERANK_C1_CASE", name)
        .stdout(std::fs::File::create(&out).unwrap())
        .stderr(std::fs::File::create(&err).unwrap())
        .spawn()
        .unwrap();
    let until = Instant::now() + Duration::from_secs(120);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= until {
            let _ = child.kill();
            let status = child.wait().unwrap();
            panic!(
                "owned C1 child timed out and was reaped: {status:?}\n{}\n{}",
                std::fs::read_to_string(&out).unwrap(),
                std::fs::read_to_string(&err).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = std::fs::read_to_string(out).unwrap();
    let stderr = std::fs::read_to_string(err).unwrap();
    println!("C1 isolated {name} actual_status={status:?}\n{stdout}\n{stderr}");
    assert!(
        status.success(),
        "owned C1 child failed: {status:?}\n{stdout}\n{stderr}"
    );
}

#[derive(Default)]
struct Observed {
    entered: usize,
    injected: bool,
    arrivals: HashSet<std::thread::ThreadId>,
    workers: HashSet<std::thread::ThreadId>,
    counts: HashMap<std::thread::ThreadId, usize>,
    per_pass: [usize; 4],
}
fn sparse() -> Graph {
    let n = 2 * super::super::CHECK_EVERY;
    Graph::build(
        super::super::RawEdges {
            nodes: (0..n as u64).collect(),
            src: (0..n as u64).collect(),
            dst: (0..n as u64).map(|u| (u + 1) % n as u64).collect(),
            ..Default::default()
        },
        super::super::Aggregate::None,
        &Par::single(),
        &Control::unlimited(),
    )
    .unwrap()
}
fn exercise(g: &Graph, threads: usize, phase: Option<usize>) -> bool {
    let par = if threads == 1 {
        Par::single()
    } else {
        Par::with_threads(threads)
    };
    let flag = Arc::new(AtomicBool::new(false));
    let expires = phase
        .is_none()
        .then(|| Instant::now() + Duration::from_secs(3));
    let limit = 64 * 1024 * 1024;
    let ctl = Control::new(flag.clone(), expires, limit);
    let state = Arc::new((Mutex::new(Observed::default()), Condvar::new()));
    let observed = state.clone();
    let caller = std::thread::current().id();
    let hook = c1_probe::install(
        &ctl,
        Arc::new(move |event| {
            let (lock, ready) = &*observed;
            match event {
                c1_probe::Event::Iteration => {
                    let mut state = lock.lock().unwrap();
                    state.entered += 1;
                    assert_eq!(state.entered, 1);
                    if phase == Some(4) {
                        state.injected = true;
                        flag.store(true, Ordering::SeqCst);
                    }
                    if let Some(expires) = expires {
                        drop(state);
                        while Instant::now() < expires {
                            std::thread::park_timeout(
                                expires
                                    .saturating_duration_since(Instant::now())
                                    .min(Duration::from_millis(10)),
                            );
                        }
                        let mut state = lock.lock().unwrap();
                        assert!(Instant::now() >= expires);
                        state.injected = true;
                    }
                }
                c1_probe::Event::Node(pass) => {
                    let id = std::thread::current().id();
                    let mut state = lock.lock().unwrap();
                    assert_eq!(state.entered, 1);
                    if phase == Some(pass) && !state.injected {
                        state.arrivals.insert(id);
                        if id != caller && rayon::current_thread_index().is_some() {
                            state.workers.insert(id);
                        }
                        if state.arrivals.len() == threads {
                            state.injected = true;
                            flag.store(true, Ordering::SeqCst);
                            ready.notify_all();
                        } else {
                            let (next, timeout) = ready
                                .wait_timeout_while(state, Duration::from_secs(5), |state| {
                                    !state.injected
                                })
                                .unwrap();
                            assert!(
                                !timeout.timed_out() && next.injected,
                                "actual pass workers did not all reach injection"
                            );
                            state = next;
                        }
                    }
                    if state.injected {
                        *state.counts.entry(id).or_default() += 1;
                        state.per_pass[pass] += 1;
                    }
                }
            }
        }),
    );
    let result = run(
        g,
        &Params {
            max_iterations: 1,
            fixed: true,
            ..Default::default()
        },
        &par,
        &ctl,
    );
    drop(hook);
    let typed = if phase.is_some() {
        matches!(&result, Err(crate::Error::Cancelled))
    } else {
        matches!(&result, Err(crate::Error::Timeout))
    };
    let returned_output = result.is_ok();
    drop(result);
    let state = state.0.lock().unwrap();
    assert_eq!(
        state.entered, 1,
        "setup must reach successful iteration entry"
    );
    assert!(
        state.injected && typed && !returned_output,
        "must retain typed failure and return no Output"
    );
    assert_eq!(
        ctl.remaining(),
        limit,
        "64 MiB ledger must fully roll back before the node-bound assertion"
    );
    if phase.is_some_and(|pass| pass < 4) {
        assert_eq!(state.arrivals.len(), threads);
        if threads > 1 {
            assert_eq!(
                state.workers.len(),
                threads,
                "require every actual non-caller Rayon worker"
            );
        }
    }
    let visits: usize = state.counts.values().sum();
    println!(
        "C1 threads={threads} phase={phase:?} entered={} post_injection_visits={visits} per_pass={:?} per_worker={:?} actual_workers={} typed_error={typed} output={returned_output} remaining={} limit={limit}",
        state.entered,
        state.per_pass,
        state.counts,
        state.workers.len(),
        ctl.remaining()
    );
    visits <= super::super::CHECK_EVERY
        && state
            .counts
            .values()
            .all(|&n| n <= super::super::CHECK_EVERY)
}
#[test]
fn c1_cancel_each_iteration_pass_threads_one_two_four() {
    isolated("c1_cancel_each_iteration_pass_threads_one_two_four", || {
        let g = sparse();
        let mut failures = Vec::new();
        for threads in [1, 2, 4] {
            // Phase 4 cancels immediately after successful iteration entry.
            for phase in 0..=4 {
                if !exercise(&g, threads, Some(phase)) {
                    failures.push((threads, phase));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "iteration node polling bound exceeded: {failures:?}"
        );
    });
}
#[test]
fn c1_real_deadline_after_iteration_entry_threads_one_two_four() {
    isolated(
        "c1_real_deadline_after_iteration_entry_threads_one_two_four",
        || {
            let g = sparse();
            let mut failures = Vec::new();
            for threads in [1, 2, 4] {
                if !exercise(&g, threads, None) {
                    failures.push(threads);
                }
            }
            assert!(
                failures.is_empty(),
                "actual iteration deadline node bound exceeded: {failures:?}"
            );
        },
    );
}
#[test]
fn c1_complete_weighted_multiblock_score_bits_threads_one_two_four() {
    isolated(
        "c1_complete_weighted_multiblock_score_bits_threads_one_two_four",
        || {
            let n = 3 * BLOCK + 17;
            let mut raw = super::super::RawEdges {
                nodes: (0..n as u64).collect(),
                w: Some(Vec::new()),
                ..Default::default()
            };
            for u in 0..n {
                if u % 7 == 0 {
                    continue;
                }
                for (v, w) in [
                    ((u + 1) % n, (u % 5 + 1) as f32 / 4.0),
                    ((u * 17 + 11) % n, (u % 3 + 1) as f32 / 7.0),
                ] {
                    raw.src.push(u as u64);
                    raw.dst.push(v as u64);
                    raw.w.as_mut().unwrap().push(w);
                }
            }
            let g = Graph::build(
                raw,
                super::super::Aggregate::None,
                &Par::single(),
                &Control::unlimited(),
            )
            .unwrap();
            let p = Params {
                max_iterations: 20,
                fixed: true,
                sources: vec![0, (n - 1) as u32],
                ..Default::default()
            };
            let expected = include_bytes!("../../../tests/data/pagerank-c1-positive.bits");
            assert_eq!(expected.len(), n * 8);
            for threads in [1, 2, 4] {
                let par = if threads == 1 {
                    Par::single()
                } else {
                    Par::with_threads(threads)
                };
                let out = run(
                    &g,
                    &p,
                    &par,
                    &Control::new(Default::default(), None, 64 * 1024 * 1024),
                )
                .unwrap();
                assert!(out.score.iter().all(|s| s.is_finite() && *s >= 0.0));
                assert!((out.score.iter().sum::<f64>() - 1.0).abs() < 1e-12);
                let actual: Vec<_> = out
                    .score
                    .iter()
                    .flat_map(|s| s.to_bits().to_le_bytes())
                    .collect();
                assert_eq!(
                    actual, expected,
                    "complete signed-before score bits, threads={threads}"
                );
                println!(
                    "C1 complete score bits n={n} threads={threads} fixed=20 all_bytes={}",
                    actual.len()
                );
            }
        },
    );
}
