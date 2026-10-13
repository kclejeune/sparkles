//! Reasoning over LUBM-style data in a persistent dataset: time, the reasoner's peak
//! heap and a digest of the closure, so that two builds can be compared on the same data.
//!
//! ```sh
//! cargo run --release -p sparkles-reasoner --example lubm -- gen 10 /tmp/lubm
//! cargo run --release -p sparkles-reasoner --example lubm -- load /tmp/lubm 10 /tmp/lubm/db10
//! cargo run --release -p sparkles-reasoner --example lubm -- infer /tmp/lubm/db10 rdfs
//! cargo run --release -p sparkles-reasoner --example lubm -- materialize /tmp/lubm/db10 owl-rl
//! ```
//!
//! `RAYON_NUM_THREADS` sets the number of threads. The peak heap counts every allocation
//! of the process from the start of the run, so it includes the data the run returns.

use sparkles_core::io::Source;
use sparkles_core::store::{Store, StoreOptions};
use sparkles_reasoner::{Profile, ReasonOptions, bench_derive, materialize};
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
                let (derived, iterations, digest) =
                    bench_derive(snap, &profile, &ReasonOptions::default(), &|| {
                        *done.lock().unwrap() = (
                            PEAK.load(Ordering::Relaxed) - before,
                            t.elapsed().as_millis(),
                        );
                    })
                    .unwrap();
                let (peak, ms) = *done.lock().unwrap();
                println!(
                    "infer {} threads={threads} derived={derived} iterations={iterations} ms={ms} peak_heap_mib={:.1} vm_hwm={} digest={digest:016x}",
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
        _ => {
            eprintln!(
                "usage: lubm gen U DIR | load DIR U DB | infer DB PROFILE | materialize DB PROFILE"
            );
            std::process::exit(2);
        }
    }
}
