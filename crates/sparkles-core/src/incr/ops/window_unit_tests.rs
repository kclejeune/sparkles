//! The window's construction, refill and refresh under the pass's work, memory and
//! control (spec F13 §4.4, review of item 419).

use super::{CANCEL_AFTER, CMPS, RECONCILED, Window};
use crate::error::{BudgetKind, Error};
use crate::incr::circuit::{Circuit, DeltaSet};
use crate::incr::compile::compile;
use crate::incr::pass::{Pass, Vis};
use crate::sparql::{self, QueryOptions};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

const PREFIX: &str = "PREFIX : <http://ex/> ";

fn upd(s: &Store, u: &str) {
    sparql::update::update(s, &format!("{PREFIX}{u}"), &QueryOptions::default()).expect("update");
}

fn compiled(q: &str) -> Circuit {
    let parsed = sparql::parse_query(&format!("{PREFIX}{q}"), None, &[]).expect("parse");
    Circuit::new(compile(&parsed, None).expect("compile"), None)
}

fn circuit(s: &Store, q: &str) -> Circuit {
    let mut c = compiled(q);
    c.build(s.snapshot(), &Control::none()).expect("build");
    c
}

fn window(c: &mut Circuit) -> &mut Window {
    c.root.result_window_mut().expect("a window")
}

/// Subjects `from..to` with the integer value `v(i)`.
fn ints(from: u32, to: u32, v: impl Fn(u32) -> i64) -> String {
    let d: Vec<String> = (from..to).map(|i| format!(":s{i} :v {}", v(i))).collect();
    d.join(" . ")
}

/// Subjects `0..n`, each with a distinct string of about `len` bytes.
fn strings(n: u32, len: usize) -> String {
    let pad = "x".repeat(len);
    let d: Vec<String> = (0..n)
        .map(|i| format!(":s{i} :v \"{i:06}{pad}\""))
        .collect();
    d.join(" . ")
}

fn over_memory(r: crate::error::Result<()>) -> bool {
    matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
}

/// Run the root window's `delta` over the store's changes since the circuit's commit,
/// under `control`, with a pass built here so that nothing above the window looks at
/// the control first.
fn window_delta(s: &Store, c: &mut Circuit, control: &Control) -> crate::error::Result<()> {
    window_delta_within(s, c, control, None)
}

/// [`window_delta`] with a memory budget.
fn window_delta_within(
    s: &Store,
    c: &mut Circuit,
    control: &Control,
    budget: Option<u64>,
) -> crate::error::Result<()> {
    let d = DeltaSet::read(s, c.applied, &Control::none());
    let old = d.old().expect("old snapshot");
    let vis = Vis::new(None, &d.snapshot);
    let Circuit {
        terms, root, vars, ..
    } = c;
    let mut p = Pass::new(
        d.snapshot.clone(),
        Some(old),
        &d.added,
        &d.removed,
        terms,
        &vis,
        None,
        control,
        vars,
    );
    p.max_memory_bytes = budget;
    let w = root.result_window_mut().expect("a window");
    w.delta(&mut p).map(|_| ())
}

/// A control whose flag [`Window::key_of`] raises after `n` keys on this thread.
fn cancel_after(n: u64) -> Control {
    let flag = Arc::new(AtomicBool::new(false));
    CANCEL_AFTER.with(|c| *c.borrow_mut() = Some((n, flag.clone())));
    Control::with_cancel(flag)
}

fn cancelled() -> Control {
    Control::with_cancel(Arc::new(AtomicBool::new(true)))
}

fn expired() -> Control {
    Control {
        deadline: Some(Instant::now()),
        ..Control::none()
    }
}

/// A fresh build over rows whose keys all tie takes the heap's sorted rows as they come:
/// no row is searched for among the rows placed before it.
#[test]
fn a_fresh_build_over_ties_does_linear_work() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 2000, |_| 1)));
    let mut c = compiled("SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2000");
    CMPS.with(|n| n.set(0));
    c.build(s.snapshot(), &Control::none()).expect("build");
    let n = CMPS.with(|n| n.get());
    assert!(n <= 20_000, "{n} comparisons for 2000 rows");
    assert_eq!(window(&mut c).rows().len(), 2000);
}

/// A refill tracks the kept rows only, however many rows its input holds.
#[test]
fn a_refill_reconciles_only_the_kept_rows() {
    for n in [1000u32, 4000] {
        let s = Store::in_memory(StoreOptions::default());
        upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, n, i64::from)));
        let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 10");
        RECONCILED.with(|r| r.set(0));
        upd(
            &s,
            &format!("DELETE DATA {{ {} }}", ints(0, 101, i64::from)),
        );
        window_delta(&s, &mut c, &Control::none()).expect("delta");
        let w = window(&mut c);
        assert_eq!(w.refills, 1);
        let rows = w.rows();
        assert_eq!(rows.len(), 10);
        let tracked = RECONCILED.with(|r| r.get());
        assert!(tracked as i64 <= w.cap(), "{tracked} rows tracked over {n}");
    }
}

/// A build whose heap of long keys outgrows the memory budget fails while it builds,
/// with fewer rows than any periodic check would wait for.
#[test]
fn a_build_charges_its_keys_strings_as_it_goes() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", strings(2048, 4096)));
    let mut c = compiled("SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 5000");
    c.max_memory_bytes = Some(1 << 20);
    let r = c.build(s.snapshot(), &Control::none());
    assert!(over_memory(r), "the build passed its budget");
}

/// A small heap whose fixed charge fits but whose kept strings do not fails too.
#[test]
fn a_small_heap_of_long_keys_passes_its_budget() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", strings(5000, 4096)));
    let mut c = compiled("SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 10");
    c.max_memory_bytes = Some(256 << 10);
    let r = c.build(s.snapshot(), &Control::none());
    assert!(over_memory(r), "the build passed its budget");
}

/// A guard, which holds before the repair as well: rows the heap evicts leave its
/// charge, so a budget the kept rows fit is not refused.
#[test]
fn evicted_rows_leave_the_charge() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", strings(5000, 4096)));
    let mut c = compiled("SELECT ?s WHERE { ?s :v ?v } ORDER BY DESC(?v) LIMIT 10");
    c.max_memory_bytes = Some(1 << 20);
    c.build(s.snapshot(), &Control::none())
        .expect("within budget");
    let w = window(&mut c);
    assert_eq!(w.rows().len(), 10);
    assert!(w.bytes() < 1 << 20, "{} bytes", w.bytes());
}

/// Run `f` on the circuit's root window with a pass over its snapshot under `control`
/// and `budget`, built here so that no node above the window looks at either first.
fn with_pass<T>(
    c: &mut Circuit,
    control: &Control,
    budget: Option<u64>,
    f: impl FnOnce(&mut Window, &mut Pass) -> T,
) -> T {
    let snap = c.snap.clone().expect("built");
    let vis = Vis::new(None, &snap);
    let Circuit {
        terms, root, vars, ..
    } = c;
    let mut p = Pass::new(
        snap.clone(),
        None,
        &[],
        &[],
        terms,
        &vis,
        None,
        control,
        vars,
    );
    p.max_memory_bytes = budget;
    let w = root.result_window_mut().expect("a window");
    f(w, &mut p)
}

/// Computing the keys of loaded rows obeys a cancelled pass however few rows there are,
/// and leaves the rows stale.
#[test]
fn a_refresh_obeys_a_cancelled_pass() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, i64::from)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2");
    window(&mut c).mark_stale();
    let r = with_pass(&mut c, &cancelled(), None, |w, p| w.refresh(p));
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(
        window(&mut c).stale(),
        "a failed refresh left the keys current"
    );
}

/// The same with a deadline that has passed.
#[test]
fn a_refresh_obeys_an_expired_deadline() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, i64::from)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2");
    window(&mut c).mark_stale();
    let r = with_pass(&mut c, &expired(), None, |w, p| w.refresh(p));
    assert!(matches!(r, Err(Error::Timeout)), "{r:?}");
    assert!(
        window(&mut c).stale(),
        "a failed refresh left the keys current"
    );
}

/// Computing the keys of loaded rows charges their strings to the budget, although the
/// rows without their keys fit it.
#[test]
fn a_refresh_charges_the_keys_it_computes() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", strings(300, 16 << 10)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 300");
    let w = window(&mut c);
    for e in &mut w.kept {
        e.keys.clear();
    }
    w.mark_stale();
    assert!(w.bytes() < 1 << 20, "{} bytes before the keys", w.bytes());
    let r = with_pass(&mut c, &Control::none(), Some(1 << 20), |w, p| w.refresh(p));
    assert!(over_memory(r), "the refresh passed its budget");
    assert!(window(&mut c).stale());
}

/// Computing the keys of a pass's changes obeys the control as it goes.
#[test]
fn keying_the_changes_obeys_a_cancelled_pass() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        &format!("INSERT DATA {{ {} }}", ints(0, 2048, i64::from)),
    );
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2000");
    let r = with_pass(&mut c, &cancelled(), None, |w, p| {
        let rows: Vec<_> = w.kept.iter().map(|e| (e.row.clone(), 1)).collect();
        assert_eq!(rows.len(), 2048);
        w.keyed(p, rows).map(|_| ())
    });
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
}

/// Computing the keys of a pass's changes charges their strings while a small window
/// fits the budget.
#[test]
fn keying_the_changes_charges_their_strings() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", strings(3000, 4096)));
    let all = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 3000");
    let rows: Vec<_> = all
        .root
        .result_window()
        .expect("a window")
        .0
        .kept
        .iter()
        .map(|e| (e.row.clone(), 1))
        .collect();
    assert_eq!(rows.len(), 3000);
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2");
    assert!(window(&mut c).bytes() < 1 << 20);
    let r = with_pass(&mut c, &Control::none(), Some(1 << 20), |w, p| {
        w.keyed(p, rows).map(|_| ())
    });
    assert!(over_memory(r), "the keys passed the budget");
}

/// A build over keys without a total order keeps the heap's own order, which no search
/// in a sorted vector can reproduce.
#[test]
fn a_build_without_a_total_order_keeps_the_heaps_order() {
    use crate::sparql::sortkey::TopK;
    const DT: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
    let s = Store::in_memory(StoreOptions::default());
    // dates with and without a timezone, minutes and hours apart, with repeated values
    let d: Vec<String> = (0..24u32)
        .map(|i| {
            let z = if i % 2 == 0 { "Z" } else { "" };
            let h = 10 + (i * 7) % 5;
            let m = (i * 13) % 4;
            format!(":d{i} :v \"2026-01-01T{h:02}:{m:02}:00{z}\"^^<{DT}>")
        })
        .collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", d.join(" . ")));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 30");
    assert!(window(&mut c).partial());
    let got: Vec<_> = window(&mut c).kept.iter().map(|e| e.row.clone()).collect();
    let want = with_pass(&mut c, &Control::none(), None, |w, p| {
        let rows = w.inner_init(p).expect("input");
        let mut top = TopK::new(usize::try_from(w.cap()).unwrap(), w.asc.clone());
        for (r, _) in rows {
            top.offer(w.key_of(p, &r), r);
        }
        top.into_sorted()
            .into_iter()
            .map(|e| e.payload)
            .collect::<Vec<_>>()
    });
    assert_eq!(got, want);
}

/// A window whose keys were loaded computes them again and keeps its rows, their tie
/// order and the client's moves.
#[test]
fn a_refresh_keeps_the_rows_and_their_ties() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, |_| 1)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 5");
    let before = window(&mut c).rows();
    window(&mut c).mark_stale();
    upd(&s, "INSERT DATA { :n :v 0 }");
    window_delta(&s, &mut c, &Control::none()).expect("delta");
    let w = window(&mut c);
    let after = w.rows();
    assert_eq!(after.len(), 5);
    assert_eq!(&after[1..], &before[..4], "the tied rows keep their order");
    let moves = w.moves.clone().expect("moves");
    assert_eq!(moves.added.len(), 1);
    assert_eq!(moves.added[0].0, 0);
    assert_eq!(moves.removed.len(), 1);
    assert_eq!(moves.removed[0].0, 4);
    assert_eq!(moves.removed[0].1, before[4].0);
}

/// A cancellation that arrives while the keys of a pass's changes are computed fails
/// the phase, though too few rows follow for a periodic check.
#[test]
fn keying_notices_a_cancellation_during_the_phase() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, i64::from)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 20");
    let control = cancel_after(5);
    let r = with_pass(&mut c, &control, None, |w, p| {
        let rows: Vec<_> = w.kept.iter().map(|e| (e.row.clone(), 1)).collect();
        w.keyed(p, rows).map(|_| ())
    });
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
}

/// The same while the keys of loaded rows are computed, which leaves them stale.
#[test]
fn a_refresh_notices_a_cancellation_during_the_phase() {
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, i64::from)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 20");
    window(&mut c).mark_stale();
    let control = cancel_after(3);
    let r = with_pass(&mut c, &control, None, |w, p| w.refresh(p));
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert!(window(&mut c).stale());
}

/// A window without a total order that rebuilds holds its old rows while it builds the
/// new ones, and both are charged.
#[test]
fn a_rebuild_charges_the_rows_it_replaces() {
    const DT: &str = "http://www.w3.org/2001/XMLSchema#dateTime";
    let s = Store::in_memory(StoreOptions::default());
    let pad = "x".repeat(80 << 10);
    let row =
        |i: u32| format!(":s{i} :d \"2026-01-01T12:{i:02}:00\"^^<{DT}> . :s{i} :t \"{i:03}{pad}\"");
    let d: Vec<String> = (0..8).map(row).collect();
    upd(&s, &format!("INSERT DATA {{ {} }}", d.join(" . ")));
    let q = "SELECT ?s WHERE { ?s :d ?d ; :t ?t } ORDER BY ?d ?t LIMIT 10";
    let mut c = circuit(&s, q);
    assert!(window(&mut c).partial());
    let before = window(&mut c).bytes();
    assert!(before < 1 << 20 && before > 600 << 10, "{before} bytes");
    upd(&s, &format!("INSERT DATA {{ {} }}", row(8)));
    let r = window_delta_within(&s, &mut c, &Control::none(), Some(1 << 20));
    assert!(over_memory(r), "the rebuild passed its budget");
}

/// A guard: a pass's changes whose slots alone pass the budget fail before any long key.
#[test]
fn keying_many_changes_charges_their_slots() {
    let s = Store::in_memory(StoreOptions::default());
    upd(
        &s,
        &format!("INSERT DATA {{ {} }}", ints(0, 20_000, i64::from)),
    );
    let all = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 20000");
    let rows: Vec<_> = all
        .root
        .result_window()
        .expect("a window")
        .0
        .kept
        .iter()
        .map(|e| (e.row.clone(), 1))
        .collect();
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 2");
    let r = with_pass(&mut c, &Control::none(), Some(256 << 10), |w, p| {
        w.keyed(p, rows).map(|_| ())
    });
    assert!(over_memory(r), "the slots passed the budget");
}

/// A checkpoint whose window holds a row twice is refused as corrupt.
#[test]
fn a_window_that_holds_a_row_twice_does_not_load() {
    use crate::incr::persist::{Reader, Writer};
    let s = Store::in_memory(StoreOptions::default());
    upd(&s, &format!("INSERT DATA {{ {} }}", ints(0, 10, i64::from)));
    let mut c = circuit(&s, "SELECT ?s WHERE { ?s :v ?v } ORDER BY ?v LIMIT 5");
    let snap = c.snap.clone().expect("built");
    let mut w = window(&mut c).clone();
    let first = w.kept[0].clone();
    w.kept[1] = first;
    let mut out = Writer::new(&snap, &c.terms);
    w.save(&mut out).expect("save");
    let data = out.finish();
    let mut terms = c.terms.clone();
    let mut r = Reader::new(&data, &snap, &mut terms).expect("reader");
    let r = window(&mut c).load(&mut r);
    assert!(matches!(r, Err(Error::Corrupt(_))), "{r:?}");
}
