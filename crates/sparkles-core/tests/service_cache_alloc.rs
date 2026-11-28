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

/// Nested RDF 1.2 triple terms allocate a boxed `Triple` and owned names per nesting
/// level. The admission counts them from `<<` markers in the raw text before parsing,
/// so the query budget stops the decoding before the parser reaches its peak (Sol
/// round-2 P1-2, Sol round-3 P3-1, Sol round-4 P3-1).
#[test]
fn a_nested_triple_term_is_admitted_before_it_is_decoded() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ServiceCache::new(0);
    cache.set_disk(Some(Arc::new(
        DiskTier::open(dir.path().join("svc"), 1 << 20).unwrap(),
    )));
    let triple_term = Term::Triple(Box::new(oxrdf::Triple::new(
        oxrdf::NamedNode::new_unchecked("urn:s"),
        oxrdf::NamedNode::new_unchecked("urn:p"),
        Term::Triple(Box::new(oxrdf::Triple::new(
            oxrdf::NamedNode::new_unchecked("urn:s2"),
            oxrdf::NamedNode::new_unchecked("urn:p2"),
            Term::Literal(oxrdf::Literal::new_simple_literal("leaf")),
        ))),
    )));
    let count = 500;
    let url = "http://e/sparql";

    // The nested triple serializes to 52 bytes via Display:
    //   << <urn:s> <urn:p> << <urn:s2> <urn:p2> "leaf" >> >>
    // A simple literal of 50 characters serializes to "x...x" = 52 bytes.
    // Equal serialized byte length and equal key length make the ONLY admission
    // difference the nested triple-opener allowance.
    let plain_term = Term::Literal(oxrdf::Literal::new_simple_literal("x".repeat(50)));
    let plain_rows = Arc::new(Rows {
        vars: vec!["v".to_string()],
        rows: (0..count).map(|_| vec![Some(plain_term.clone())]).collect(),
    });
    let pk = key("a", url, "flat-500-rows", &[]);
    cache.put(
        pk.clone(),
        Parts::of("a", url, "flat-500-rows"),
        Slice::ALL,
        plain_rows,
    );
    let flat = std::cell::Cell::new(0u64);
    let pr = cache.get_admitted(&pk, Slice::ALL, &|n| {
        flat.set(flat.get().saturating_add(n));
        Ok(())
    });
    assert!(pr.unwrap().is_some(), "plain baseline reads back");
    let flat = flat.get();

    let nested_rows = Arc::new(Rows {
        vars: vec!["v".to_string()],
        rows: (0..count)
            .map(|_| vec![Some(triple_term.clone())])
            .collect(),
    });
    let k = key("a", url, "nest-500-rows", &[]);
    cache.put(
        k.clone(),
        Parts::of("a", url, "nest-500-rows"),
        Slice::ALL,
        nested_rows.clone(),
    );
    let file = cache.disk().unwrap().bytes();
    assert!(file > 0, "the entry is on disk");
    let total = std::cell::Cell::new(0u64);
    let _ = cache.get_admitted(&k, Slice::ALL, &|n| {
        total.set(total.get().saturating_add(n));
        Ok(())
    });
    let total = total.get();
    assert!(
        total > flat,
        "nested ({total}) should cost more than flat ({flat})"
    );
    let budget = (flat + total) / 2;
    assert!(
        budget < total,
        "the budget {budget} should be less than the total {total}"
    );
    assert!(
        budget >= flat,
        "the midpoint {budget} should pass the flat baseline ({flat})"
    );
    let admitted = std::cell::Cell::new(0u64);
    let before = allocated();
    let r = cache.get_admitted(&k, Slice::ALL, &|n| {
        admitted.set(admitted.get().saturating_add(n));
        if admitted.get() > budget {
            return Err(Error::invalid("over the query's budget"));
        }
        Ok(())
    });
    let spent = allocated() - before;
    assert!(r.is_err(), "nested triple terms should exceed the budget");
    assert!(
        spent < budget + (64 << 10),
        "{spent} bytes allocated under a budget of {budget}"
    );
    let r = cache.get_admitted(&k, Slice::ALL, &|_| Ok(()));
    let rows = r.unwrap().expect("an ample budget admits the entry");
    assert_eq!(rows.rows.len(), count, "all rows returned");

    // a literal containing many `<<` characters round-trips under a budget that would
    // fail if the parser counted the quoted `<<` as triple openers
    let angle_text = "<<".repeat(50);
    let angle_term = Term::Literal(oxrdf::Literal::new_simple_literal(&angle_text));
    let angle_rows = Arc::new(Rows {
        vars: vec!["v".to_string()],
        rows: (0..count).map(|_| vec![Some(angle_term.clone())]).collect(),
    });
    let ak = key("a", url, "angle-literal", &[]);
    cache.put(
        ak.clone(),
        Parts::of("a", url, "angle-literal"),
        Slice::ALL,
        angle_rows.clone(),
    );
    let angle_total = std::cell::Cell::new(0u64);
    let _ = cache.get_admitted(&ak, Slice::ALL, &|n| {
        angle_total.set(angle_total.get().saturating_add(n));
        Ok(())
    });
    let angle_total = angle_total.get();
    let broken_per_row = 50 * (std::mem::size_of::<oxrdf::Triple>() as u64 + 72);
    let angle_budget = angle_total + count as u64 * broken_per_row / 2;
    let angle_admitted = std::cell::Cell::new(0u64);
    let ar = cache.get_admitted(&ak, Slice::ALL, &|n| {
        angle_admitted.set(angle_admitted.get().saturating_add(n));
        if angle_admitted.get() > angle_budget {
            return Err(Error::invalid("over the angle budget"));
        }
        Ok(())
    });
    let angle_back = ar.unwrap().expect("angle literals under a tight budget");
    assert_eq!(angle_back.rows.len(), count, "angle literal row count");
    match &angle_back.rows[0][0] {
        Some(Term::Literal(l)) => assert_eq!(l.value(), angle_text, "decoded value matches"),
        other => panic!("expected a literal, got {other:?}"),
    }
}
