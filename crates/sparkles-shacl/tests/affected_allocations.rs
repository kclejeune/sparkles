//! Actual allocation control for early-bounded affected-target traversal.

use oxrdf::{GraphName, Literal, NamedNode, Quad, Term};
use sparkles_core::io::RdfFormat;
use sparkles_core::sparql::{QueryOptions, update};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_shacl::incremental::{AffectedLimits, validate_affected};
use sparkles_shacl::{Shapes, ValidateOptions};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

struct Counting;

#[derive(Clone, Copy, Default)]
struct Allocations {
    active: bool,
    largest: usize,
    total: usize,
}

thread_local! {
    static CALLER: std::cell::Cell<Allocations> = const {
        std::cell::Cell::new(Allocations { active: false, largest: 0, total: 0 })
    };
}
static WINDOW: AtomicBool = AtomicBool::new(false);
static OUTSIDE_LARGEST: AtomicUsize = AtomicUsize::new(0);
static OUTSIDE_TOTAL: AtomicUsize = AtomicUsize::new(0);
static MEASURE_LOCK: Mutex<()> = Mutex::new(());

fn record(size: usize) {
    let caller = CALLER
        .try_with(|cell| {
            let mut counts = cell.get();
            if counts.active {
                counts.largest = counts.largest.max(size);
                counts.total = counts.total.saturating_add(size);
                cell.set(counts);
            }
            counts.active
        })
        .unwrap_or(false);
    if !caller && WINDOW.load(Ordering::SeqCst) {
        OUTSIDE_LARGEST.fetch_max(size, Ordering::SeqCst);
        OUTSIDE_TOTAL.fetch_add(size, Ordering::SeqCst);
    }
}

// SAFETY: every allocation, reallocation and deallocation is delegated unchanged
// to System. The counters only observe sizes and never inspect or change a pointer.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, size) };
        if !pointer.is_null() {
            record(size);
        }
        pointer
    }
}

fn measure<T>(operation: impl FnOnce() -> T) -> (T, usize, usize, usize, usize) {
    OUTSIDE_LARGEST.store(0, Ordering::SeqCst);
    OUTSIDE_TOTAL.store(0, Ordering::SeqCst);
    CALLER.with(|cell| {
        cell.set(Allocations {
            active: true,
            ..Default::default()
        })
    });
    WINDOW.store(true, Ordering::SeqCst);
    let result = operation();
    WINDOW.store(false, Ordering::SeqCst);
    let counts = CALLER.with(|cell| {
        let counts = cell.get();
        cell.set(Allocations::default());
        counts
    });
    (
        result,
        counts.largest,
        counts.total,
        OUTSIDE_LARGEST.load(Ordering::SeqCst),
        OUTSIDE_TOTAL.load(Ordering::SeqCst),
    )
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[test]
fn dense_delta_reverse_discovery_refuses_before_materializing_the_full_fanout() {
    let _serial = MEASURE_LOCK.lock().unwrap();
    let store = Store::in_memory(StoreOptions::default());
    let mut text =
        String::from("PREFIX ex: <http://ex.org/> INSERT DATA { ex:focus ex:name \"old\" .\n");
    for n in 0..16_000 {
        text.push_str(&format!("ex:root{n} ex:reach ex:focus .\n"));
    }
    text.push('}');
    update::update(&store, &text, &QueryOptions::default()).unwrap();
    let before = store.snapshot();
    assert_eq!(
        before.generation.meta.quads, 0,
        "fixture must isolate delta traversal, not cold base-block decoding"
    );
    assert_eq!(before.delta.inserts(), 16_001);
    update::update(
        &store,
        "INSERT DATA { <http://ex.org/focus> <http://ex.org/name> \"new\" }",
        &QueryOptions::default(),
    )
    .unwrap();
    let after = store.snapshot();
    let shapes = Shapes::parse("@prefix ex: <http://ex.org/> . @prefix sh: <http://www.w3.org/ns/shacl#> . ex:S a sh:NodeShape ; sh:targetSubjectsOf ex:reach ; sh:property [ sh:path (ex:reach ex:name) ; sh:minCount 1 ] .", RdfFormat::Turtle, None).unwrap();
    let focus = NamedNode::new("http://ex.org/focus").unwrap();
    let change = Quad::new(
        focus.clone(),
        NamedNode::new("http://ex.org/name").unwrap(),
        Literal::new_simple_literal("new"),
        GraphName::DefaultGraph,
    );
    let opts = ValidateOptions {
        parallel: false,
        ..Default::default()
    };
    let limits = AffectedLimits {
        additional_focus: 500,
        work: 2_048,
    };
    // Discovery is serial. Attribute its allocations to this caller so unrelated
    // asynchronous store log workers cannot inflate the witness. Total allocated
    // bytes is a conservative upper bound on this traversal's retained heap.
    let (result, largest, total, outside_largest, outside_total) = measure(|| {
        validate_affected(
            &before,
            &after,
            &shapes,
            &[change],
            &[Term::NamedNode(focus)],
            &opts,
            limits,
        )
    });
    let error = result.expect_err("dense discovery must refuse before a partial report");
    assert!(
        matches!(
            error.downcast_ref::<sparkles_core::Error>(),
            Some(sparkles_core::Error::BudgetExceeded(_))
        ),
        "{error:#}"
    );
    eprintln!(
        "affected_discovery_allocation={{\"largest\":{largest},\"total\":{total},\"outside_largest\":{outside_largest},\"outside_total\":{outside_total}}}"
    );
    assert!(
        largest < 64 * 1024,
        "discovery materialized the full predicate fanout: largest={largest}, total={total}"
    );
    assert!(
        total < 256 * 1024,
        "discovery retained unbounded traversal state: largest={largest}, total={total}"
    );
}
