//! Reasoning over LUBM-style data in a persistent dataset: time, the reasoner's peak
//! heap and a digest of the closure, so that two builds can be compared on the same data.
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example lubm -- gen 10 /tmp/lubm
//! cargo run --release -p sparkles-reasoner --example lubm -- load /tmp/lubm 10 /tmp/lubm/db10
//! cargo run --release -p sparkles-reasoner --example lubm -- loadfile /tmp/owl2bench/db2 owl2bench-2.nt
//! cargo run --release -p sparkles-reasoner --example lubm -- infer /tmp/lubm/db10 rdfs
//! cargo run --release -p sparkles-reasoner --example lubm -- materialize /tmp/lubm/db10 owl-rl
//! cargo run --release -p sparkles-reasoner --example lubm -- keep /tmp/lubm/db10 rdfs 1000
//! cargo run --release -p sparkles-reasoner --example lubm -- churn /tmp/lubm/db100 rdfs 100 100 40
//! cargo run --release -p sparkles-reasoner --example lubm -- diag /tmp/lubm/db10
//! ```
//!
//! `RAYON_NUM_THREADS` sets the number of threads, and `SPARKLES_GENERIC_CLOSURES=1` makes
//! `infer` evaluate transitive and equivalence rules as ordinary rules.
//! `SPARKLES_WITNESSES=N` makes `materialize` and `keep` keep up to `N` witnesses per fact
//! (spec F18 §7.1) and report them. The peak heap counts every allocation
//! of the process from the start of the run, so it includes the data the run returns.
//!
//! `keep` measures the closure kept for incremental runs, as a server keeps it: a full
//! run, a run without changes that keeps the closure in memory, the heap the closure
//! holds, then incremental runs with the kept closure after a change that removes `K`
//! triples of the first university and adds `K` new ones, after it is undone and after it
//! is made again, and one that reads the closure back from the dataset after the change
//! is undone once more. The dataset is changed, so it should be a fresh copy.
//!
//! `churn DB PROFILE K U ROUNDS` keeps a closure the same way and then makes a change of
//! `K` triples to one university after another of the first `U`, each change followed by
//! its undo, the change again and a second undo, which leaves the dataset as it was. It
//! reports the time of each incremental run in microseconds, with the commit, and the
//! quartiles of each step over the rounds. The first round is the first change after the
//! closure is kept, and each round changes a university that no earlier round changed.
//! `K` and `ROUNDS` can be lists such as `100,1000,10000` and `40,20,6`, which run one
//! size after another over the same kept closure. `U` is at least the rounds of all sizes
//! together, and at most the universities of the dataset.
//! `LUBM_PERF_CONTROL` names a control FIFO of `perf record --control`, to which `churn`
//! writes `enable` before the rounds and `disable` after them, so that a record started
//! with `--delay=-1` holds the rounds without the full run.
//!
//! The runs of `keep` and `churn` evaluate on a reasoning pool of `RAYON_NUM_THREADS`
//! threads, as a server's do. `LUBM_POOL=0` runs them on the global pool from the main
//! thread instead, where each parallel section of a run is handed to a worker and waited
//! for, which for changes of 100 triples at 16 threads costs more than the sections save.
//!
//! `diag` runs the check `literal-not-in-datatype` twice on the same snapshot and reports
//! the time and the peak heap of each run.

use oxrdf::{GraphName, NamedNode, Quad, Triple};
use sparkles_core::io::Source;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{
    Cache, Extras, Incremental, Profile, ReasonOptions, ReasonReport, bench_derive, materialize,
    materialize_incremental,
};
use std::alloc::{GlobalAlloc, Layout, System};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[path = "common/lubm.rs"]
mod lubm;

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            let now = CURRENT.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            if now > PEAK.load(Ordering::Relaxed) {
                PEAK.fetch_max(now, Ordering::Relaxed);
            }
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new > l.size() {
                let now = CURRENT.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
                if now > PEAK.load(Ordering::Relaxed) {
                    PEAK.fetch_max(now, Ordering::Relaxed);
                }
            } else {
                CURRENT.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn reset_peak() -> usize {
    let now = CURRENT.load(Ordering::Relaxed);
    PEAK.store(now, Ordering::Relaxed);
    now
}

fn mib(b: usize) -> f64 {
    b as f64 / (1 << 20) as f64
}

fn vm_hwm() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM"))
                .map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string() + " kB")
        })
        .unwrap_or_default()
}

fn data_files(dir: &Path, u: usize) -> (PathBuf, PathBuf) {
    (dir.join("univ-bench.ttl"), dir.join(format!("lubm-{u}.nt")))
}

/// Remove `del` from the default graph and add `add`, in one commit.
fn change(s: &Store, add: &[Triple], del: &[Triple]) {
    let mut txn = s.write();
    let mut labels = Default::default();
    let q = |t: &Triple| {
        Quad::new(
            t.subject.clone(),
            t.predicate.clone(),
            t.object.clone(),
            GraphName::DefaultGraph,
        )
    };
    for t in del {
        let x = txn.encode_quad(&q(t), &mut labels).unwrap();
        txn.delete(x).unwrap();
    }
    for t in add {
        let x = txn.encode_quad(&q(t), &mut labels).unwrap();
        txn.insert(x).unwrap();
    }
    txn.commit().unwrap();
}

/// `k` triples of university `u`, spread over its data, and `k` new triples about new
/// graduate students of its first department.
fn change_sets(u: usize, k: usize) -> (Vec<Triple>, Vec<Triple>) {
    let text = lubm::university(u);
    let all: Vec<Triple> = oxttl::NTriplesParser::new()
        .for_slice(text.as_bytes())
        .map(|t| t.unwrap())
        .collect();
    let step = (all.len() / k.max(1)).max(1);
    let del: Vec<Triple> = all.iter().step_by(step).take(k).cloned().collect();
    let n = |s: String| NamedNode::new_unchecked(s);
    let ty = n("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into());
    let dept = format!("http://www.Department0.University{u}.edu");
    let dept = dept.as_str();
    let add = (0..k)
        .map(|j| {
            let s = n(format!("{dept}/NewStudent{}", j / 3));
            match j % 3 {
                0 => Triple::new(s, ty.clone(), n(format!("{}GraduateStudent", lubm::UB))),
                1 => Triple::new(s, n(format!("{}memberOf", lubm::UB)), n(dept.into())),
                _ => Triple::new(
                    s,
                    n(format!("{}takesCourse", lubm::UB)),
                    n(format!("{dept}/GraduateCourse{}", j % 10)),
                ),
            }
        })
        .collect();
    (del, add)
}

/// The options of `infer`: `SPARKLES_GENERIC_CLOSURES=1` evaluates transitive and
/// equivalence rules as ordinary rules, for a comparison with the closure operators.
fn run_options() -> ReasonOptions {
    ReasonOptions {
        closure_operators: std::env::var("SPARKLES_GENERIC_CLOSURES").as_deref() != Ok("1"),
        ..ReasonOptions::default()
    }
}

/// The universities that the rounds of `churn` change: one for each round of each size in
/// `batches`, none of them twice, among the first `universities`. `None` when the rounds
/// need more universities than that.
fn fresh_universities(
    batches: &[usize],
    universities: usize,
) -> Option<Vec<std::ops::Range<usize>>> {
    let mut next = 0usize;
    let mut ranges = Vec::new();
    for &rounds in batches {
        let end = next.checked_add(rounds).filter(|&e| e <= universities)?;
        ranges.push(next..end);
        next = end;
    }
    Some(ranges)
}

/// The pool the runs of `keep` and `churn` evaluate on: a reasoning pool of `threads`
/// threads, as a server's, or none with `LUBM_POOL=0`, which leaves them on the global
/// pool.
fn reasoning_pool(threads: usize) -> Option<std::sync::Arc<sparkles_reasoner::ReasoningPool>> {
    (std::env::var("LUBM_POOL").as_deref() != Ok("0"))
        .then(|| sparkles_reasoner::thread_pool(threads).unwrap())
}

/// The witnesses `SPARKLES_WITNESSES` asks for.
fn witnesses() -> Option<usize> {
    std::env::var("SPARKLES_WITNESSES")
        .ok()
        .map(|v| v.parse().expect("SPARKLES_WITNESSES"))
}

/// The witness fields of a report line.
fn witness_fields(r: &ReasonReport) -> String {
    r.witnesses.as_ref().map_or(String::new(), |w| {
        format!(
            " witnesses_ms={} witness_facts={} witness_truncated={} witnesses={} witness_evaluated={} witness_mib={:.1}",
            w.millis,
            w.facts,
            w.truncated,
            w.witnesses,
            w.evaluated,
            mib(w.bytes as usize)
        )
    })
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "gen" => {
            let u: usize = args[2].parse().expect("universities");
            let dir = PathBuf::from(&args[3]);
            std::fs::create_dir_all(&dir).unwrap();
            let (onto, data) = data_files(&dir, u);
            std::fs::write(&onto, lubm::ontology()).unwrap();
            let mut f = std::io::BufWriter::new(std::fs::File::create(&data).unwrap());
            let mut lines = 0usize;
            for i in 0..u {
                let s = lubm::university(i);
                lines += s.lines().count();
                f.write_all(s.as_bytes()).unwrap();
            }
            f.flush().unwrap();
            println!("{lines} triples in {}", data.display());
        }
        "load" => {
            let dir = PathBuf::from(&args[2]);
            let u: usize = args[3].parse().expect("universities");
            let db = PathBuf::from(&args[4]);
            let _ = std::fs::remove_dir_all(&db);
            let (onto, data) = data_files(&dir, u);
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let t = Instant::now();
            store
                .load(&[
                    Source::from_path(&onto, None).unwrap(),
                    Source::from_path(&data, None).unwrap(),
                ])
                .unwrap();
            println!(
                "{} quads loaded in {} ms",
                store.snapshot().len(),
                t.elapsed().as_millis()
            );
        }
        "loadfile" => {
            let db = PathBuf::from(&args[2]);
            let _ = std::fs::remove_dir_all(&db);
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let t = Instant::now();
            let sources: Vec<Source> = args[3..]
                .iter()
                .map(|f| Source::from_path(Path::new(f), None).unwrap())
                .collect();
            store.load(&sources).unwrap();
            println!(
                "{} quads loaded in {} ms",
                store.snapshot().len(),
                t.elapsed().as_millis()
            );
        }
        "infer" | "materialize" => {
            let db = PathBuf::from(&args[2]);
            let profile: Profile = args[3].parse().expect("profile");
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let snap = store.snapshot();
            let threads = rayon::current_num_threads();
            let before = reset_peak();
            let t = Instant::now();
            if cmd == "infer" {
                let done = std::sync::Mutex::new((0usize, 0u128));
                let (derived, iterations, digest, order) =
                    bench_derive(snap, &profile, &run_options(), &|| {
                        *done.lock().unwrap() = (
                            PEAK.load(Ordering::Relaxed) - before,
                            t.elapsed().as_millis(),
                        );
                    })
                    .unwrap();
                let (peak, ms) = *done.lock().unwrap();
                println!(
                    "infer {} threads={threads} derived={derived} iterations={iterations} ms={ms} peak_heap_mib={:.1} vm_hwm={} digest={digest:016x} order={order:016x}",
                    profile.name(),
                    mib(peak),
                    vm_hwm()
                );
            } else {
                let opts = ReasonOptions {
                    witnesses: witnesses(),
                    ..ReasonOptions::default()
                };
                let r = materialize(&store, &profile, &opts).unwrap();
                let peak = PEAK.load(Ordering::Relaxed) - before;
                println!(
                    "materialize {} threads={threads} inferred={} iterations={} ms={} peak_heap_mib={:.1} vm_hwm={}{}",
                    profile.name(),
                    r.inferred,
                    r.iterations,
                    r.millis,
                    mib(peak),
                    vm_hwm(),
                    witness_fields(&r)
                );
            }
        }
        "keep" => {
            let db = PathBuf::from(&args[2]);
            let profile: Profile = args[3].parse().expect("profile");
            let k: usize = args.get(4).map_or(1000, |a| a.parse().expect("triples"));
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let threads = rayon::current_num_threads();
            let cache = Cache::new(usize::MAX);
            let opts = ReasonOptions {
                witnesses: witnesses(),
                pool: reasoning_pool(threads),
                ..ReasonOptions::default()
            };
            let run = |since: Option<u64>, cache: Option<&Cache>| -> (ReasonReport, usize) {
                let before = reset_peak();
                let r = materialize_incremental(
                    &store,
                    &profile,
                    &Extras::default(),
                    Incremental { since, cache },
                    &opts,
                )
                .unwrap();
                (r, PEAK.load(Ordering::Relaxed) - before)
            };
            let (del, add) = change_sets(0, k);
            let line = |what: &str, r: &ReasonReport, peak: usize| {
                let source = r.changes.as_ref().map_or("", |c| c.source.as_str());
                println!(
                    "keep {} threads={threads} {what} method={} source={source} ms={} inferred={} peak_heap_mib={:.1} reasoner_peak_mib={:.1}{}",
                    profile.name(),
                    r.method.as_str(),
                    r.millis,
                    r.inferred,
                    mib(peak),
                    mib(r.peak_bytes as usize),
                    witness_fields(r),
                );
            };
            let (r, peak) = run(None, Some(&cache));
            line("full", &r, peak);
            let mut since = r.receipt.unwrap().commit.seq;
            // the full run's commit rebuilt the generation, so its closure is not kept: a
            // run without changes keeps it
            let (r, peak) = run(Some(since), Some(&cache));
            line("settle", &r, peak);
            since = r.receipt.unwrap().commit.seq;
            let kept = cache.kept().map_or(0, |k| k.0);
            let with = CURRENT.load(Ordering::Relaxed);
            cache.clear();
            let closure = with.saturating_sub(CURRENT.load(Ordering::Relaxed));
            println!(
                "keep {} threads={threads} kept_facts={kept} kept_closure_mib={:.1}",
                profile.name(),
                mib(closure)
            );
            let (r, _) = run(Some(since), Some(&cache));
            since = r.receipt.unwrap().commit.seq;
            change(&store, &add, &del);
            let (r, peak) = run(Some(since), Some(&cache));
            line("change", &r, peak);
            since = r.receipt.unwrap().commit.seq;
            // the same changes again, undone and redone, with the kept closure and the
            // blocks the first change read
            change(&store, &del, &add);
            let (r, peak) = run(Some(since), Some(&cache));
            line("revert", &r, peak);
            since = r.receipt.unwrap().commit.seq;
            change(&store, &add, &del);
            let (r, peak) = run(Some(since), Some(&cache));
            line("change-again", &r, peak);
            since = r.receipt.unwrap().commit.seq;
            cache.clear();
            change(&store, &del, &add);
            let (r, peak) = run(Some(since), None);
            line("revert-from-store", &r, peak);
            println!(
                "keep {} threads={threads} vm_hwm={}",
                profile.name(),
                vm_hwm()
            );
        }
        "churn" => {
            let db = PathBuf::from(&args[2]);
            let profile: Profile = args[3].parse().expect("profile");
            let list = |a: &str, what: &str| -> Vec<usize> {
                a.split(',').map(|x| x.parse().expect(what)).collect()
            };
            let sizes = list(&args[4], "triples");
            let universities: usize = args[5].parse().expect("universities");
            let batches = list(&args[6], "rounds");
            assert_eq!(sizes.len(), batches.len(), "a number of rounds for each K");
            let fresh = fresh_universities(&batches, universities).unwrap_or_else(|| {
                panic!("the rounds {batches:?} need a university each, and U is {universities}")
            });
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let threads = rayon::current_num_threads();
            let cache = Cache::new(usize::MAX);
            let opts = ReasonOptions {
                pool: reasoning_pool(threads),
                ..ReasonOptions::default()
            };
            let run = |since: Option<u64>| -> (ReasonReport, u128) {
                let t = Instant::now();
                let r = materialize_incremental(
                    &store,
                    &profile,
                    &Extras::default(),
                    Incremental {
                        since,
                        cache: Some(&cache),
                    },
                    &opts,
                )
                .unwrap();
                (r, t.elapsed().as_micros())
            };
            let (r, full) = run(None);
            let mut since = r.receipt.unwrap().commit.seq;
            let (r, settle) = run(Some(since));
            since = r.receipt.unwrap().commit.seq;
            println!(
                "churn {} threads={threads} full_ms={} settle_ms={} inferred={}",
                profile.name(),
                full / 1000,
                settle / 1000,
                r.inferred
            );
            // a profiler started with its events off records the rounds only
            let control = |word: &str| {
                if let Ok(path) = std::env::var("LUBM_PERF_CONTROL") {
                    let mut f = std::fs::OpenOptions::new().write(true).open(path).unwrap();
                    writeln!(f, "{word}").unwrap();
                }
            };
            let steps = ["change", "revert", "change-again", "revert-again"];
            // each round of each size changes a university of its own
            for ((&k, &rounds), range) in sizes.iter().zip(&batches).zip(fresh) {
                let sets: Vec<(Vec<Triple>, Vec<Triple>)> =
                    range.map(|u| change_sets(u, k)).collect();
                control("enable");
                let mut times: Vec<Vec<u128>> = vec![Vec::new(); steps.len()];
                for (round, (del, add)) in sets.iter().enumerate() {
                    let mut line = format!(
                        "churn {} threads={threads} k={k} round={round}",
                        profile.name()
                    );
                    for (i, step) in steps.iter().enumerate() {
                        if i % 2 == 0 {
                            change(&store, add, del);
                        } else {
                            change(&store, del, add);
                        }
                        let (r, us) = run(Some(since));
                        since = r.receipt.unwrap().commit.seq;
                        let source = r.changes.as_ref().map_or("", |c| c.source.as_str());
                        assert!(
                            r.method.as_str() == "incremental" && source == "memory",
                            "round {round} {step}: method={} source={source}",
                            r.method.as_str()
                        );
                        times[i].push(us);
                        line.push_str(&format!(" {step}_us={us}"));
                        if i == 3 {
                            line.push_str(&format!(" inferred={}", r.inferred));
                        }
                    }
                    println!("{line}");
                }
                control("disable");
                for (step, t) in steps.iter().zip(&mut times) {
                    let first = t.first().copied().unwrap_or(0);
                    t.sort_unstable();
                    let q = |f: f64| t[((t.len() - 1) as f64 * f).round() as usize];
                    println!(
                        "churn {} threads={threads} k={k} rounds={rounds} {step} first_us={first} min_us={} p25_us={} median_us={} p75_us={} max_us={}",
                        profile.name(),
                        q(0.0),
                        q(0.25),
                        q(0.5),
                        q(0.75),
                        q(1.0)
                    );
                }
            }
        }
        "diag" => {
            let db = PathBuf::from(&args[2]);
            let store = Store::open(&db, StoreOptions::default()).unwrap();
            let opts = sparkles_reasoner::diagnostics::DiagnoseOptions {
                checks: vec!["literal-not-in-datatype".into()],
                ..Default::default()
            };
            for round in ["first", "second"] {
                let before = reset_peak();
                let t = Instant::now();
                let r = sparkles_reasoner::diagnostics::diagnose(store.snapshot(), &opts).unwrap();
                println!(
                    "diag {round} ms={} findings={} peak_heap_mib={:.1}",
                    t.elapsed().as_millis(),
                    r.findings.len(),
                    mib(PEAK.load(Ordering::Relaxed) - before)
                );
            }
        }
        _ => {
            eprintln!(
                "usage: lubm gen U DIR | load DIR U DB | infer DB PROFILE | materialize DB PROFILE | keep DB PROFILE [K] | churn DB PROFILE K U ROUNDS | diag DB"
            );
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fresh_universities;

    #[test]
    fn every_round_of_churn_changes_a_university_of_its_own() {
        // one size
        assert_eq!(fresh_universities(&[40], 100), Some(vec![0..40]));
        assert_eq!(fresh_universities(&[40], 40), Some(vec![0..40]));
        assert_eq!(fresh_universities(&[40], 10), None);
        // several sizes do not share a university
        assert_eq!(
            fresh_universities(&[40, 20, 6], 100),
            Some(vec![0..40, 40..60, 60..66])
        );
        assert_eq!(fresh_universities(&[40, 20, 6], 65), None);
        assert_eq!(fresh_universities(&[], 0), Some(vec![]));
        // rounds whose sum passes the largest number
        assert_eq!(fresh_universities(&[usize::MAX, 1], 10), None);
        assert_eq!(fresh_universities(&[1, usize::MAX], usize::MAX), None);
    }
}
