//! Caller-attributed allocation in an already prepared model. Decoder work is excluded.
use super::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}
static WINDOW: AtomicBool = AtomicBool::new(false);
static OUTSIDE: AtomicUsize = AtomicUsize::new(0);
struct Allocator;
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn record(bytes: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(bytes)));
    } else if WINDOW.load(Ordering::Acquire) {
        OUTSIDE.fetch_max(bytes, Ordering::Relaxed);
    }
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            record(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, bytes) };
        if !pointer.is_null() {
            record(bytes);
        }
        pointer
    }
}

fn measured<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            WINDOW.store(false, Ordering::Release);
            TRACK.set(false);
        }
    }
    LARGEST.set(0);
    OUTSIDE.store(0, Ordering::Relaxed);
    TRACK.set(true);
    WINDOW.store(true, Ordering::Release);
    let reset = Reset;
    let value = run();
    drop(reset);
    (value, LARGEST.get(), OUTSIDE.load(Ordering::Relaxed))
}

#[test]
fn prepared_group_labels_are_admitted_before_model_copy() {
    let large = "group".repeat(210_000);
    let ds = Dataset::memory();
    ds.load_str_into(
        &format!(
            "@prefix sh: <http://www.w3.org/ns/shacl#> . @prefix ex: <http://ex/> .
          ex:R sh:targetNode ex:root ; sh:node ex:Grouped .
          ex:Grouped sh:name \"{large}\" ; sh:property [ sh:path ex:p ] ."
        ),
        crate::io::RdfFormat::Turtle,
        "urn:shapes",
    )
    .unwrap();
    ds.update("INSERT DATA { <http://ex/root> <http://ex/p> \"value\" }")
        .unwrap();
    let forms = ds.forms();
    let request = FormRequest::new(
        "http://ex/root",
        FormSettings {
            shapes: ShapesSource::Graphs,
            graphs: vec!["urn:shapes".into()],
            ..Default::default()
        },
    );
    // Shapes, dictionaries and source prefixes are prepared before this scope.
    let mut built = forms
        .prepare_model(&request, &Default::default(), None)
        .unwrap();
    let (ordinary, baseline, baseline_outside) = measured(|| built.model(true, "main"));
    assert!(
        baseline >= large.len(),
        "the ordinary model did not exercise the label copy"
    );
    drop(ordinary.unwrap());
    built.limits.preparation = Some(nested::Preparation::of(&QueryOptions {
        max_memory_bytes: Some(128_000),
        ..Default::default()
    }));
    let (limited, refused, outside) = measured(|| built.model(true, "main"));
    assert!(
        refused < 16 * 1024,
        "unadmitted model copy allocated {refused} bytes"
    );
    let error = limited.unwrap_err();
    assert!(error.to_string().contains("preparation allowance"));
    eprintln!(
        "prepared adapter allocation: baseline_largest={baseline}, refused_largest={refused}, baseline_outside_thread_largest={baseline_outside}, refused_outside_thread_largest={outside}"
    );
}
