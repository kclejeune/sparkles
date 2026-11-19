//! An audit of the text reader's memory ledger against the allocator (item 829). A
//! counting allocator measures the peak heap of a streamed text search without a limit
//! above what was live before the query. A budget too small for the hit list refuses the
//! search while the list grows, so the peak stays near the budget rather than reaching
//! the whole list. The allocator counts every thread, so each test holds a lock for its
//! whole body.
#![cfg(feature = "text")]

use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{CursorOptions, FallbackPolicy, QueryOptions, select_cursor};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::text::TextConfig;
use sparkles_core::{BudgetKind, Error};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
        PEAK.fetch_max(now, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // The old and the new block can be live together while the bytes move.
        let now = LIVE.fetch_add(new, Ordering::Relaxed) + new;
        PEAK.fetch_max(now, Ordering::Relaxed);
        let q = unsafe { System.realloc(p, l, new) };
        LIVE.fetch_sub(if q.is_null() { new } else { l.size() }, Ordering::Relaxed);
        q
    }
}

#[global_allocator]
static A: Counting = Counting;

/// What a query may hold outside its ledger: the parsed query and plan, the cursor, and
/// Tantivy's scorer scratch (row 1156), of which the registry of the automaton built for
/// the predicate set, about 1 MB, is the largest.
const ALLOWANCE: usize = (1 << 20) + (256 << 10);

/// A budget too small for 100,000 hits of 12 bytes refuses the search with a memory
/// error, and the heap the query held at its peak stays within the budget and the
/// allowance, which an uncharged list of every hit, 1.5 MiB, would pass.
#[test]
fn a_refused_hit_list_stops_growing() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..100_000 {
        data.push_str(&format!(
            "<http://example.org/s{i}> <http://www.w3.org/2000/01/rdf-schema#label> \"fox {i}\" .\n"
        ));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.enable_text(TextConfig::default()).unwrap();
    let snap = s.snapshot();
    let q = "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#> PREFIX text: <http://jena.apache.org/text#> SELECT ?s { ?s text:query (rdfs:label \"fox\") }";
    let budget = 64 << 10;
    let o = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(budget as u64),
        ..Default::default()
    };
    let c = CursorOptions {
        batch_rows: 16,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    };
    // a first run warms what the process keeps once, such as the searcher's caches
    let warm = |o: &QueryOptions| {
        let mut cursor = select_cursor(snap.clone(), q, o, &c)?;
        while cursor.next_batch()?.is_some() {}
        Ok::<(), Error>(())
    };
    let _ = warm(&o);
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    match warm(&o) {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        other => panic!("{other:?}"),
    }
    let peak = PEAK.load(Ordering::Relaxed) - base;
    assert!(
        peak <= budget + ALLOWANCE,
        "peak {peak} over {budget} + {ALLOWANCE}"
    );
}
