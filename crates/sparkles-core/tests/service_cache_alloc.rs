//! A SERVICE cache entry read back from disk is admitted before it is decoded (roadmap
//! item 38). The test counts what the reading thread allocates, so it sees an entry
//! decoded before the query refused it.

use oxrdf::Term;
use sparkles_core::Error;
use sparkles_core::sparql::svccache::{Parts, Rows, ServiceCache, Slice, key};
use sparkles_core::sparql::svcdisk::DiskTier;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;

struct Counting;

thread_local! {
    static ALLOCATED: Cell<u64> = const { Cell::new(0) };
}

fn count(n: usize) {
    let _ = ALLOCATED.try_with(|c| c.set(c.get().saturating_add(n as u64)));
}

// SAFETY: every call is passed on to the system allocator unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        // SAFETY: the caller's layout, as `GlobalAlloc::alloc` requires.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        // SAFETY: as above.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count(new_size);
        // SAFETY: the caller's pointer and layout, as `GlobalAlloc::realloc` requires.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's pointer and layout.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

fn allocated() -> u64 {
    ALLOCATED.with(Cell::get)
}

/// Wide rows of unbound slots take a few bytes each in the file and many times that in
/// memory. A query whose budget holds the file and not the decoded rows is refused
/// before the rows are built.
#[test]
fn a_disk_entry_is_admitted_before_it_is_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ServiceCache::new(0);
    cache.set_disk(Some(Arc::new(
        DiskTier::open(dir.path().join("svc"), 1 << 20).unwrap(),
    )));
    let width = 64;
    let rows = Arc::new(Rows {
        vars: (0..width).map(|i| format!("v{i}")).collect(),
        rows: (0..400)
            .map(|_| vec![None::<Term>; width])
            .collect::<Vec<_>>(),
    });
    let url = "http://e/sparql";
    let k = key("a", url, "p", &[]);
    cache.put(
        k.clone(),
        Parts::of("a", url, "p"),
        Slice::ALL,
        rows.clone(),
    );
    let file = cache.disk().unwrap().bytes();
    assert!(file > 0, "the entry is on disk");
    let decoded = rows.rows.len() as u64 * (width * std::mem::size_of::<Option<Term>>()) as u64;
    assert!(decoded > 8 * file, "{decoded} against {file}");
    // the whole entry is read back when the budget allows it
    assert!(cache.get(&k, Slice::ALL).is_some());
    // a budget of the file and a little more
    let admitted = Cell::new(0u64);
    let budget = file + (16 << 10);
    let before = allocated();
    let r = cache.get_admitted(&k, Slice::ALL, &|n| {
        admitted.set(admitted.get().saturating_add(n));
        if admitted.get() > budget {
            return Err(Error::invalid("over the query's budget"));
        }
        Ok(())
    });
    let spent = allocated() - before;
    assert!(r.is_err(), "the decoded rows fit a budget of the file");
    assert!(
        spent < budget + (64 << 10),
        "{spent} bytes allocated under a budget of {budget}, for decoded rows of {decoded}"
    );
}
