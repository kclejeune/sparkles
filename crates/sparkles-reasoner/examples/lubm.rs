//! Reasoning over LUBM-style data in a persistent dataset: time, the reasoner's peak
//! heap and a digest of the closure, so that two builds can be compared on the same data.
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example lubm -- gen 10 /tmp/lubm
//! cargo run --release -p sparkles-reasoner --example lubm -- load /tmp/lubm 10 /tmp/lubm/db10
//! cargo run --release -p sparkles-reasoner --example lubm -- infer /tmp/lubm/db10 rdfs
//! cargo run --release -p sparkles-reasoner --example lubm -- materialize /tmp/lubm/db10 owl-rl
//! cargo run --release -p sparkles-reasoner --example lubm -- keep /tmp/lubm/db10 rdfs 1000
//! ```
//!
//! `RAYON_NUM_THREADS` sets the number of threads. The peak heap counts every allocation
//! of the process from the start of the run, so it includes the data the run returns.
//!
//! `keep` measures the closure kept for incremental runs, as a server keeps it: a full
//! run, a run without changes that keeps the closure in memory, the heap the closure
//! holds, then incremental runs with the kept closure after a change that removes `K`
//! triples of the first university and adds `K` new ones, after it is undone and after it
//! is made again, and one that reads the closure back from the dataset after the change
//! is undone once more. The dataset is changed, so it should be a fresh copy.

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

/// `k` triples of the first university, spread over its data, and `k` new triples about
/// new graduate students of its first department.
fn change_sets(k: usize) -> (Vec<Triple>, Vec<Triple>) {
    let text = lubm::university(0);
    let all: Vec<Triple> = oxttl::NTriplesParser::new()
        .for_slice(text.as_bytes())
        .map(|t| t.unwrap())
        .collect();
    let step = (all.len() / k.max(1)).max(1);
    let del: Vec<Triple> = all.iter().step_by(step).take(k).cloned().collect();
    let n = |s: String| NamedNode::new_unchecked(s);
    let ty = n("http://www.w3.org/1999/02/22-rdf-syntax-ns#type".into());
    let dept = "http://www.Department0.University0.edu";
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
                    bench_derive(snap, &profile, &ReasonOptions::default(), &|| {
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
                let r = materialize(&store, &profile, &ReasonOptions::default()).unwrap();
                let peak = PEAK.load(Ordering::Relaxed) - before;
                println!(
                    "materialize {} threads={threads} inferred={} iterations={} ms={} peak_heap_mib={:.1} vm_hwm={}",
                    profile.name(),
                    r.inferred,
                    r.iterations,
                    r.millis,
                    mib(peak),
                    vm_hwm()
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
            let opts = ReasonOptions::default();
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
            let (del, add) = change_sets(k);
            let line = |what: &str, r: &ReasonReport, peak: usize| {
                let source = r.changes.as_ref().map_or("", |c| c.source.as_str());
                println!(
                    "keep {} threads={threads} {what} method={} source={source} ms={} inferred={} peak_heap_mib={:.1} reasoner_peak_mib={:.1}",
                    profile.name(),
                    r.method.as_str(),
                    r.millis,
                    r.inferred,
                    mib(peak),
                    mib(r.peak_bytes as usize),
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
        _ => {
            eprintln!(
                "usage: lubm gen U DIR | load DIR U DB | infer DB PROFILE | materialize DB PROFILE | keep DB PROFILE [K]"
            );
            std::process::exit(2);
        }
    }
}
