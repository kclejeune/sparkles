//! Opt-in cost diagnostic for controlled permission preparation (roadmap item 398).
//! It reports latency, caller-thread allocation and the smallest memory cap that
//! admits each preparation, for stores whose data sits in an uncompacted delta and
//! for compacted stores. Run it in a release build with `--ignored --nocapture`, with
//! `--test-threads=1` left implicit since it is the only test in this binary.
use sparkles_core::access::{
    Caller, GraphAccess, Graphs, Limits, Preparation, Protection, Rule, TripleRules,
};
use sparkles_core::io::{RdfFormat, Source};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::task::Control;
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct Counting;
// Every thread counts, since preparation writes permutations on worker threads. The
// diagnostic runs alone, so the other threads of the process stay idle.
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

// SAFETY: every call forwards to System with the caller's layout and only counts
// the bytes requested.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocated() -> u64 {
    ALLOCATED.load(Ordering::Relaxed)
}

fn policy(name: &str, predicate: &str) -> GraphAccess {
    GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: name.into(),
                    predicates: Some(vec![predicate.into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: BTreeMap::new(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    )
}

/// `rows` subjects, every tenth with the protected predicate, written as updates so
/// that the terms and quads stay in the delta unless `compact` is set.
fn store(rows: usize, compact: bool) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let data = (0..rows)
        .map(|row| {
            let p = if row % 10 == 0 {
                "urn:secret"
            } else {
                "urn:visible"
            };
            format!("<urn:s{row}> <{p}> \"value {row}\" .\n")
        })
        .collect::<String>();
    if compact {
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::NTriples,
                None,
            )])
            .unwrap();
        store.compact().unwrap();
    } else {
        for chunk in data.lines().collect::<Vec<_>>().chunks(5000) {
            sparkles_core::sparql::update::update(
                &store,
                &format!("INSERT DATA {{ {} }}", chunk.join("\n")),
                &Default::default(),
            )
            .unwrap();
        }
    }
    store
}

/// The smallest cap, to 1 KiB, that admits a cold preparation of a fresh policy.
fn smallest_cap(snap: &Arc<Snapshot>, probe: &mut usize) -> u64 {
    let (mut lo, mut hi) = (1u64 << 10, 1u64 << 40);
    while hi - lo > 1024 {
        let mid = lo + (hi - lo) / 2;
        *probe += 1;
        let view = policy(&format!("probe{probe}"), "urn:secret");
        let ok = Preparation::new(Control::none(), mid)
            .and_then(|p| view.masked_controlled(snap, &p).map(drop))
            .is_ok();
        if ok { hi = mid } else { lo = mid }
    }
    hi
}

fn median(mut values: Vec<u128>) -> u128 {
    values.sort_unstable();
    values[values.len() / 2]
}

#[test]
#[ignore = "local controlled preparation cost diagnostic"]
fn controlled_preparation_cost_diagnostic() {
    let rounds = 7;
    let mut probe = 0usize;
    for (rows, compact) in [(20_000, false), (100_000, false), (100_000, true)] {
        let store = store(rows, compact);
        let snap = store.snapshot();
        let layout = format!(
            "rows={rows} compact={compact} delta_inserts={}",
            snap.delta.inserts()
        );

        // vocabulary lookup: a present delta IRI and an absent one
        let prep = Preparation::new(Control::none(), 1 << 30).unwrap();
        for (label, iri) in [
            ("present", format!("urn:s{}", rows - 1)),
            ("absent", "urn:missing".into()),
        ] {
            let mut times = Vec::new();
            let mut bytes = 0;
            for _ in 0..rounds {
                let a = allocated();
                let start = Instant::now();
                let found = prep.lookup_iri(&snap, &iri).unwrap();
                times.push(start.elapsed().as_nanos());
                bytes = allocated() - a;
                assert_eq!(found.is_some(), label == "present");
            }
            eprintln!(
                "COST {layout} op=lookup_{label} median_ns={} alloc_bytes={bytes}",
                median(times)
            );
        }

        // cold and cached mask preparation under an ample cap
        let mut cold = Vec::new();
        let mut cold_bytes = 0;
        let mut cached = Vec::new();
        let mut cached_bytes = 0;
        let mut hidden = 0;
        let mut views = Vec::new();
        for _ in 0..rounds {
            probe += 1;
            let view = policy(&format!("timed{probe}"), "urn:secret");
            let p = Preparation::new(Control::none(), 1 << 34).unwrap();
            let a = allocated();
            let start = Instant::now();
            let prepared = view.masked_controlled(&snap, &p).unwrap();
            cold.push(start.elapsed().as_nanos());
            cold_bytes = allocated() - a;
            hidden = snap.len() - prepared.snapshot().len();
            drop(prepared);
            views.push((view, p));
        }
        // cached calls run apart from the cold builds and the worker threads they wake
        std::thread::sleep(std::time::Duration::from_millis(50));
        for (view, p) in &views {
            let a = allocated();
            let start = Instant::now();
            let prepared = view.masked_controlled(&snap, p).unwrap();
            cached.push(start.elapsed().as_nanos());
            cached_bytes = allocated() - a;
            drop(prepared);
        }
        assert_eq!(hidden, rows.div_ceil(10) as u64);
        eprintln!(
            "COST {layout} op=mask_cold median_ns={} alloc_bytes={cold_bytes} hidden={hidden}",
            median(cold)
        );
        eprintln!(
            "COST {layout} op=mask_cached median_ns={} alloc_bytes={cached_bytes}",
            median(cached)
        );
        // where one cold preparation spends its time, by reported stage
        {
            probe += 1;
            let view = policy(&format!("stages{probe}"), "urn:secret");
            let stages = Arc::new(std::sync::Mutex::new((
                Instant::now(),
                String::new(),
                BTreeMap::<String, u128>::new(),
            )));
            let record = stages.clone();
            let control = Control {
                progress: sparkles_core::task::Progress::new(move |_, stage| {
                    let mut s = record.lock().unwrap();
                    let now = Instant::now();
                    let elapsed = now.duration_since(s.0).as_nanos();
                    let previous = std::mem::replace(&mut s.1, stage.to_string());
                    *s.2.entry(previous).or_default() += elapsed;
                    s.0 = now;
                }),
                ..Control::none()
            };
            let p = Preparation::new(control, 1 << 34).unwrap();
            stages.lock().unwrap().0 = Instant::now();
            drop(view.masked_controlled(&snap, &p).unwrap());
            let s = stages.lock().unwrap();
            let tail = s.0.elapsed().as_nanos();
            for (stage, ns) in &s.2 {
                eprintln!("STAGE {layout} stage={stage:?} ns={ns}");
            }
            eprintln!("STAGE {layout} stage={:?} ns={tail}", s.1);
        }
        // the legacy entry point, for reference
        let mut legacy = Vec::new();
        for _ in 0..rounds {
            probe += 1;
            let view = policy(&format!("legacy{probe}"), "urn:secret");
            let start = Instant::now();
            drop(view.masked(&snap).unwrap());
            legacy.push(start.elapsed().as_nanos());
        }
        eprintln!("COST {layout} op=mask_legacy median_ns={}", median(legacy));
        eprintln!(
            "COST {layout} op=smallest_cap bytes={}",
            smallest_cap(&snap, &mut probe)
        );
    }
}
