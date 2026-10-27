//! Layout-owned allocation admission. Shape parsing occurs before measurement.

use oxrdf::NamedNode;
use sparkles_core::io::RdfFormat;
use sparkles_shacl::Shapes;
use sparkles_shacl::form::{form_layout, form_layout_with};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

thread_local! {
    static TRACK: Cell<bool> = const { Cell::new(false) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}
static OUTSIDE_LARGEST: AtomicUsize = AtomicUsize::new(0);
static MEASURING: AtomicBool = AtomicBool::new(false);

struct Allocator;
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;

fn allocation(bytes: usize) {
    if TRACK.try_with(Cell::get).unwrap_or(false) {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(bytes)));
    } else if MEASURING.load(Ordering::Acquire) {
        OUTSIDE_LARGEST.fetch_max(bytes, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            allocation(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            allocation(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, bytes: usize) -> *mut u8 {
        let pointer = unsafe { System.realloc(pointer, layout, bytes) };
        if !pointer.is_null() {
            allocation(bytes);
        }
        pointer
    }
}

fn measured<T>(run: impl FnOnce() -> T) -> (T, usize, usize) {
    LARGEST.set(0);
    OUTSIDE_LARGEST.store(0, Ordering::Relaxed);
    TRACK.set(true);
    MEASURING.store(true, Ordering::Release);
    let value = run();
    MEASURING.store(false, Ordering::Release);
    TRACK.set(false);
    (
        value,
        LARGEST.get(),
        OUTSIDE_LARGEST.load(Ordering::Relaxed),
    )
}

fn assert_copy_is_admitted(text: &str) {
    let bytes = 1024 * 1024;
    let shapes = Shapes::parse(text, RdfFormat::Turtle, None).unwrap();
    let root = shapes
        .shape_id(&NamedNode::new("http://ex.org/S").unwrap().into())
        .unwrap();
    let (ordinary, old_largest, old_outside) = measured(|| form_layout(&shapes, &[root]));
    assert_eq!(ordinary.fields.len(), 1);
    assert!(
        old_largest >= bytes,
        "ordinary witness did not copy its label"
    );
    drop(ordinary);

    let (_, _, other_thread) = measured(|| {
        std::thread::spawn(|| std::hint::black_box(vec![0_u8; 32 * 1024]))
            .join()
            .unwrap()
    });
    assert!(other_thread >= 32 * 1024);
    // Preparation on the caller outside a measured window is not another thread.
    std::hint::black_box(vec![0_u8; 64 * 1024]);
    assert_eq!(OUTSIDE_LARGEST.load(Ordering::Relaxed), other_thread);

    let mut work = 0;
    let (limited, largest, outside) = measured(|| {
        form_layout_with(&shapes, &[root], &mut || {
            work += 1;
            anyhow::ensure!(work <= 20, "layout allowance exhausted");
            Ok(())
        })
    });
    assert!(
        largest < 16 * 1024,
        "refused producer allocated {largest} bytes under a 20-unit allowance"
    );
    let error = match limited {
        Err(error) => error,
        Ok(_) => panic!("an unadmitted producer returned a layout"),
    };
    assert_eq!(error.to_string(), "layout allowance exhausted");
    eprintln!(
        "layout allocation: ordinary_largest={old_largest}, refused_largest={largest}, ordinary_outside_thread_largest={old_outside}, refused_outside_thread_largest={outside}"
    );
}

#[test]
fn large_labels_and_custom_components_are_admitted_before_copy() {
    let large = "x".repeat(1024 * 1024);
    assert_copy_is_admitted(&format!(
        "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
         <http://ex.org/S> sh:property [ sh:path <http://ex.org/p> ; sh:name \"{large}\" ] ."
    ));
    assert_copy_is_admitted(&format!(
        "@prefix sh: <http://www.w3.org/ns/shacl#> .\n\
         @prefix ex: <http://ex.org/> .\n\
         <http://ex.org/{large}> a sh:ConstraintComponent ;\n\
           sh:parameter [ sh:path ex:flag ] ;\n\
           sh:validator [ a sh:SPARQLAskValidator ; sh:ask \"ASK {{ FILTER(true) }}\" ] .\n\
         ex:S sh:property [ sh:path ex:p ; ex:flag true ] ."
    ));
}
