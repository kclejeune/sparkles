//! Observers and public cursor regressions for the first history pull.
use super::*;
use crate::access::{
    Caller, GraphAccess, Graphs, Limits, Preparation, Protection, Rule, TripleRules,
};
use crate::branch::BranchOptions;
use crate::history::At;
use crate::sparql::{
    CursorOptions, FallbackPolicy, QueryCursor, QueryOptions, query_source, select_cursor,
    select_source_cursor, update::update,
};
use crate::store::history_query::HistoryQuery;
use crate::task::{Cancellation, Control};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::store) enum Event {
    BeforeFlush,
    HoldingPin,
    ResolvedBounds,
    TimestampIndex,
    TimestampIndexed,
    TimestampRecord,
    IndexReadAttempt,
    IndexReadChunk,
    IndexCrcChunk,
    IndexRecords,
    IndexRecord,
    IndexPosting,
    ReadOnlyTimestamp,
    ReadOnlyReference(u64),
}

type Observer = Box<dyn FnMut(Event)>;

thread_local! {
    static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) };
}

pub(in crate::store) fn observe(event: Event) {
    OBSERVER.with_borrow_mut(|slot| {
        if let Some(observer) = slot {
            observer(event);
        }
    });
}

struct Observe;

impl Observe {
    fn new(observer: impl FnMut(Event) + 'static) -> Self {
        OBSERVER.with_borrow_mut(|slot| {
            assert!(slot.is_none());
            *slot = Some(Box::new(observer));
        });
        Self
    }
}

impl Drop for Observe {
    fn drop(&mut self) {
        OBSERVER.with_borrow_mut(|slot| *slot = None);
    }
}

const LATE: &str = "\"time:2999-01-01T00:00:00Z\"";
const EARLY: &str = "\"time:1970-01-01T00:00:00Z\"";
const CREDIT: Duration = Duration::from_secs(60);

fn query(bounds: &str) -> String {
    format!(
        "PREFIX hist: <urn:x-sparkles:history#>
        SELECT ?s ?o ?c {{ SERVICE hist:changes {{
            << ?s <urn:p> ?o >> hist:commit ?c {bounds}
        }} }}"
    )
}

fn strict() -> CursorOptions {
    CursorOptions {
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

fn write(store: &Store, triples: &str) {
    update(
        store,
        &format!("INSERT DATA {{ {triples} }}"),
        &Default::default(),
    )
    .unwrap();
}

fn write_many(store: &Store) {
    let mut triples = String::new();
    use std::fmt::Write as _;
    for i in 0..8_000 {
        write!(&mut triples, "<urn:big{i}> <urn:p> {i} . ").unwrap();
    }
    write(store, &triples);
}

fn preparation(flag: &Arc<AtomicBool>) -> Preparation {
    Preparation::new(Control::with_cancel(flag.clone()), 64 << 20).unwrap()
}

fn strict_source(
    store: &Store,
    view: &crate::access::PreparedView,
    opts: &QueryOptions,
    bounds: &str,
) -> QueryCursor {
    select_source_cursor(view.read_source(), store, &query(bounds), opts, &strict()).unwrap()
}

fn pull_error(cursor: &mut QueryCursor) -> Error {
    let Err(error) = cursor.next_batch() else {
        panic!("the first pull must fail without returning a batch");
    };
    assert_eq!(cursor.stats().emitted_rows, 0);
    assert!(cursor.next_batch().unwrap().is_none());
    error
}

fn disable(store: &Store) {
    store
        .set_change_log_settings(ChangeLogSettings {
            enabled: Some(false),
            ..Default::default()
        })
        .expect("opening or failing the cursor must release the pin");
}

#[test]
fn checked_open_stops_before_flushing_queued_commits() {
    let store = Store::in_memory(Default::default());
    let log = store.change_log().unwrap();
    log.set_background(false);
    write(&store, "<urn:visible> <urn:p> 1 .");
    store.flush_change_log().unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let prep = preparation(&flag);
    let policy = Arc::new(GraphAccess::all());
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    let baseline = prep.inner_used();
    let mut cursor = strict_source(
        &store,
        &view,
        &QueryOptions {
            graphs: Some(policy),
            ..Default::default()
        },
        &format!("; hist:to {LATE}"),
    );
    write(&store, "<urn:queued> <urn:p> 2 .");
    assert_eq!(log.status().pending, 1);
    let advanced = Rc::new(Cell::new(0));
    let witness = advanced.clone();
    let stop = flag.clone();
    let observer = Observe::new(move |event| match event {
        Event::BeforeFlush => stop.store(true, Ordering::Relaxed),
        Event::HoldingPin | Event::TimestampIndex | Event::ResolvedBounds => {
            witness.set(witness.get() + 1);
        }
        _ => {}
    });
    let error = pull_error(&mut cursor);
    drop(observer);
    let pending = log.status().pending;
    disable(&store);
    drop(cursor);
    assert_eq!(prep.inner_used(), baseline);
    assert!(
        flag.load(Ordering::Relaxed),
        "the flush boundary was reached"
    );
    assert!(matches!(error, Error::Cancelled), "{error:?}");
    assert_eq!(pending, 1, "a cancelled open drained pending commits");
    assert_eq!(
        advanced.get(),
        0,
        "a cancelled open advanced past the flush"
    );
}

fn state_dependent_access() -> Arc<GraphAccess> {
    Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "class".into(),
                    predicates: Some(vec!["urn:p".into()]),
                    classes: Some(vec!["urn:Class".into()]),
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ))
}

#[test]
fn checked_open_keeps_enabled_and_access_order() {
    let store = Store::in_memory(Default::default());
    let log = store.change_log().unwrap();
    let check = || Err(Error::Cancelled);
    let denied = HistoryQuery {
        access: Some(state_dependent_access()),
        ..Default::default()
    };
    assert!(matches!(
        log.pages_checked(denied.clone(), 0, &check),
        Err(Error::NotPermitted(_))
    ));
    let allowed = log.pages_checked(HistoryQuery::default(), 0, &check);
    let stopped = matches!(allowed, Err(Error::Cancelled));
    drop(allowed);
    disable(&store);
    assert!(matches!(
        log.pages_checked(denied, 0, &check),
        Err(Error::HistoryUnsupported(_))
    ));
    assert!(
        stopped,
        "an enabled, allowed writable open must poll before flushing"
    );
}

fn assert_provisional_pin_released(phase: Event) {
    let store = Store::in_memory(Default::default());
    let log = store.change_log().unwrap();
    log.set_background(false);
    write(&store, "<urn:visible> <urn:p> 1 .");
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let advanced = Rc::new(Cell::new(false));
    let witness = advanced.clone();
    let observer = Observe::new(move |event| {
        if event == phase {
            stop.store(true, Ordering::Relaxed);
        } else if event == Event::ResolvedBounds {
            witness.set(true);
        }
    });
    let result = log.pages_checked(HistoryQuery::default(), store.head_commit().seq, &|| {
        Control::with_cancel(flag.clone()).check()
    });
    drop(observer);
    assert!(flag.load(Ordering::Relaxed), "did not reach {phase:?}");
    assert!(!advanced.get(), "advanced into bounds after stopping");
    assert!(
        matches!(result, Err(Error::Cancelled)),
        "a stop at {phase:?} must fail before publishing"
    );
    assert!(log.pins.lock().is_empty());
    disable(&store);
}

struct Writable {
    store: Store,
    _dir: tempfile::TempDir,
    first_index: PathBuf,
    second_index: PathBuf,
}

impl Writable {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), Default::default()).unwrap();
        let log = store.change_log().unwrap();
        log.set_background(false);
        write_many(&store);
        store.flush_change_log().unwrap();
        log.seal(&mut log.inner.lock()).unwrap();
        write(&store, "<urn:second> <urn:p> 1 .");
        store.flush_change_log().unwrap();
        log.seal(&mut log.inner.lock()).unwrap();
        let (first_index, second_index) = {
            let mut inner = log.inner.lock();
            assert_eq!(inner.segs.len(), 2);
            assert!(inner.segs.iter().all(|segment| segment.sealed));
            inner.cache.clear();
            (
                inner.segs[0].idx_path().unwrap(),
                inner.segs[1].idx_path().unwrap(),
            )
        };
        assert!(first_index.metadata().unwrap().len() > 128 << 10);
        Self {
            store,
            _dir: dir,
            first_index,
            second_index,
        }
    }

    fn log(&self) -> &Arc<ChangeLog> {
        self.store.change_log().unwrap()
    }
}

#[test]
fn checked_open_checks_before_bounds_with_the_provisional_pin() {
    assert_provisional_pin_released(Event::HoldingPin);
}

#[test]
fn checked_open_checks_before_publishing_the_final_pin() {
    assert_provisional_pin_released(Event::ResolvedBounds);
}

#[test]
fn checked_open_stops_before_dispatching_the_next_writable_index() {
    let fixture = Writable::new();
    let store = &fixture.store;
    let log = fixture.log();
    std::fs::remove_file(&fixture.second_index).unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let prep = preparation(&flag);
    let policy = Arc::new(GraphAccess::all());
    let view = store.prepare_read(policy.clone(), &prep).unwrap();
    let baseline = prep.inner_used();
    let opts = QueryOptions {
        graphs: Some(policy),
        ..Default::default()
    };
    let bounds = format!("; hist:to {LATE}");
    let mut oracle = strict_source(store, &view, &opts, &bounds);
    let error = pull_error(&mut oracle);
    assert!(
        matches!(error, Error::Io(ref error) if error.kind() == std::io::ErrorKind::NotFound),
        "{error:?}"
    );
    assert!(log.pins.lock().is_empty());
    drop(oracle);
    log.inner.lock().cache.clear();

    let mut cursor = strict_source(store, &view, &opts, &bounds);
    let dispatches = Rc::new(Cell::new(0));
    let witness = dispatches.clone();
    let stop = flag.clone();
    let observer = Observe::new(move |event| match event {
        Event::TimestampIndexed => stop.store(true, Ordering::Relaxed),
        Event::TimestampIndex => witness.set(witness.get() + 1),
        _ => {}
    });
    let error = pull_error(&mut cursor);
    drop(observer);
    assert!(
        flag.load(Ordering::Relaxed),
        "the first segment was not indexed"
    );
    assert!(log.pins.lock().is_empty());
    disable(store);
    drop(cursor);
    assert_eq!(prep.inner_used(), baseline);
    assert_eq!(
        dispatches.get(),
        1,
        "dispatched a second index after stopping"
    );
    assert!(matches!(error, Error::Cancelled), "{error:?}");
}

#[test]
fn writable_timestamp_index_keeps_typed_corruption_and_caches_valid_indexes() {
    let fixture = Writable::new();
    let log = fixture.log();
    let pristine = std::fs::read(&fixture.first_index).unwrap();
    let mut damaged = pristine.clone();
    *damaged.last_mut().unwrap() ^= 1;
    std::fs::write(&fixture.first_index, &damaged).unwrap();
    let result = log.commit_at_time_checked(i64::MAX, &mut || Ok(()));
    assert!(matches!(result, Err(Error::Corrupt(_))), "{result:?}");
    assert!(log.inner.lock().cache.is_empty());
    std::fs::write(&fixture.first_index, &pristine).unwrap();
    assert_eq!(
        log.commit_at_time_checked(i64::MAX, &mut || Ok(()))
            .unwrap(),
        Some(fixture.store.head_commit().seq)
    );
    assert_eq!(log.inner.lock().cache.len(), 2);
}

fn assert_index_stop(phase: Event, stop_at: usize) {
    let fixture = Writable::new();
    let log = fixture.log();
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let progress = Rc::new(Cell::new((0, 0)));
    let witness = progress.clone();
    let observer = Observe::new(move |event| {
        let (mut triggers, mut later) = witness.get();
        if triggers == stop_at {
            if matches!(
                event,
                Event::IndexReadAttempt
                    | Event::IndexCrcChunk
                    | Event::IndexRecord
                    | Event::IndexPosting
                    | Event::TimestampIndexed
            ) {
                later += 1;
            }
        } else if event == phase {
            triggers += 1;
            if triggers == stop_at {
                stop.store(true, Ordering::Relaxed);
            }
        }
        witness.set((triggers, later));
    });
    let result = log.commit_at_time_checked(i64::MAX, &mut || {
        if flag.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    });
    drop(observer);
    assert_eq!(
        progress.get(),
        (stop_at, 0),
        "progress past {phase:?}: {result:?}"
    );
    assert!(
        matches!(result, Err(Error::Cancelled)),
        "{phase:?}: {result:?}"
    );
    assert!(
        log.inner.lock().cache.is_empty(),
        "cached a cancelled miss after {phase:?}"
    );
}

#[test]
fn writable_timestamp_index_polls_during_read() {
    assert_index_stop(Event::IndexReadChunk, 1);
}

#[test]
fn writable_timestamp_index_polls_during_crc() {
    assert_index_stop(Event::IndexCrcChunk, 1);
}

#[test]
fn writable_timestamp_index_polls_during_record() {
    assert_index_stop(Event::IndexRecords, 1);
}

#[test]
fn writable_timestamp_index_polls_during_posting() {
    assert_index_stop(Event::IndexPosting, CHECK_STRIDE);
}

#[test]
fn writable_timestamp_gap_walk_stops_at_the_next_stride() {
    let store = Store::in_memory(Default::default());
    let log = store.change_log().unwrap();
    log.set_background(false);
    write(&store, "<urn:first> <urn:p> 1 .");
    store.flush_change_log().unwrap();
    let first = store.head_commit().seq;
    {
        let mut inner = log.inner.lock();
        for seq in first + 1..=first + (2 * CHECK_STRIDE) as u64 {
            log.append(
                &mut inner,
                Record::Gap {
                    from: seq,
                    through: seq,
                },
            )
            .unwrap();
        }
    }
    assert_eq!(
        log.commit_at_time_checked(i64::MAX, &mut || Ok(()))
            .unwrap(),
        Some(first)
    );
    let count = Rc::new(Cell::new(0));
    let witness = count.clone();
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let observer = Observe::new(move |event| {
        if event == Event::TimestampRecord {
            witness.set(witness.get() + 1);
            if witness.get() == CHECK_STRIDE {
                stop.store(true, Ordering::Relaxed);
            }
        }
    });
    let result =
        log.commit_at_time_checked(i64::MAX, &mut || Control::with_cancel(flag.clone()).check());
    drop(observer);
    assert_eq!(
        count.get(),
        CHECK_STRIDE,
        "walked past the next stride: {result:?}"
    );
    assert!(matches!(result, Err(Error::Cancelled)));
}

#[test]
fn writable_timestamp_checks_after_the_last_segment() {
    let store = Store::in_memory(Default::default());
    let log = store.change_log().unwrap();
    log.set_background(false);
    write(&store, "<urn:first> <urn:p> 1 .");
    store.flush_change_log().unwrap();
    let flag = Arc::new(AtomicBool::new(false));
    let stop = flag.clone();
    let observer = Observe::new(move |event| {
        if event == Event::TimestampIndexed {
            stop.store(true, Ordering::Relaxed);
        }
    });
    let control = Control::with_cancel(flag.clone());
    let result = log.commit_at_time_checked(i64::MAX, &mut || control.check());
    drop(observer);
    assert!(flag.load(Ordering::Relaxed), "the last index was processed");
    assert!(
        matches!(result, Err(Error::Cancelled)),
        "returned a timestamp after stopping: {result:?}"
    );
}

struct ColdFamily {
    child: Arc<Store>,
    main: Store,
    dir: tempfile::TempDir,
    target: crate::commit::CommitInfo,
    later: u64,
    origin_log: PathBuf,
    early: u64,
}

impl ColdFamily {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let main = Store::open(dir.path(), Default::default()).unwrap();
        write(&main, "<urn:main> <urn:p> 0 .");
        main.create_branch("parent", &Default::default()).unwrap();
        let parent = main.branch("parent").unwrap();
        parent.change_log().unwrap().set_background(false);
        let time = parent.head_commit().timestamp_ms + 100_000;
        parent.set_clock(Arc::new(move || time));
        write(&parent, "<urn:early> <urn:p> 1 .");
        let early = parent.head_commit().seq;
        write_many(&parent);
        let target = parent.head_commit();
        assert_eq!(target.timestamp_ms, time, "the two commits must tie");
        parent.set_clock(Arc::new(move || time + 1000));
        write(&parent, "<urn:later> <urn:p> 2 .");
        let later = parent.head_commit().seq;
        main.create_branch(
            "child",
            &BranchOptions {
                from: "parent".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let child = main.branch("child").unwrap();
        write(&child, "<urn:child> <urn:p> 3 .");
        child.flush_change_log().unwrap();
        parent.flush_change_log().unwrap();
        main.flush_change_log().unwrap();
        let origin_log = dir
            .path()
            .join("branches")
            .join(parent.branch_id().to_string())
            .join(CHANGES_DIR);
        drop(child);
        drop(parent);
        drop(main);
        let main = Store::open(dir.path(), Default::default()).unwrap();
        let child = main.branch("child").unwrap().shared().unwrap();
        Self {
            child,
            main,
            dir,
            target,
            later,
            origin_log,
            early,
        }
    }

    fn view(&self, policy: Arc<GraphAccess>, prep: &Preparation) -> crate::access::PreparedView {
        let view = self
            .main
            .prepare_read_with_branch(&self.child, &At::Commit(self.target.seq), policy, prep)
            .unwrap();
        let log = view.snapshot().change_log.as_ref().unwrap();
        assert_eq!(log.read_only_through(), Some(self.target.seq));
        assert!(self.origin_log.starts_with(self.dir.path()));
        view
    }

    fn bytes(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        std::fs::read_dir(&self.origin_log)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            })
            .collect()
    }
}

#[test]
fn checked_open_preserves_read_only_stop_before_access() {
    let fixture = ColdFamily::new();
    let flag = Arc::new(AtomicBool::new(false));
    let prep = preparation(&flag);
    let view = fixture.view(Arc::new(GraphAccess::all()), &prep);
    let log = view.snapshot().change_log.as_ref().unwrap();
    flag.store(true, Ordering::Relaxed);
    assert!(matches!(
        log.pages_checked(
            HistoryQuery {
                access: Some(state_dependent_access()),
                cancel: Some(flag.clone()),
                ..Default::default()
            },
            fixture.target.seq,
            &|| prep.check(),
        ),
        Err(Error::Cancelled)
    ));
}

/// The stop lands after one reference entry passed its check. The next entry
/// must never reach its witness, even if the cursor eventually reports the stop.
fn stop_in_read_only_bound(
    cursor: &mut QueryCursor,
    lookup: usize,
    target: u64,
    stop: impl FnOnce() + 'static,
) -> Error {
    let progress = Rc::new(Cell::new((0, 0)));
    let witness = progress.clone();
    let mut stop = Some(stop);
    let observer = Observe::new(move |event| {
        let (mut pass, mut refs) = witness.get();
        match event {
            Event::ReadOnlyTimestamp => pass += 1,
            Event::ReadOnlyReference(seq) if pass == lookup && seq == target => {
                refs += 1;
                if refs == 1 {
                    stop.take().unwrap()();
                }
            }
            _ => {}
        }
        witness.set((pass, refs));
    });
    let error = pull_error(cursor);
    drop(observer);
    assert_eq!(
        progress.get(),
        (lookup, 1),
        "lookup {lookup} continued reading references after its stop: {error:?}"
    );
    error
}

fn assert_read_only_timestamp_stop(lookup: usize, composite: bool) {
    let fixture = ColdFamily::new();
    let before = fixture.bytes();
    let flag = Arc::new(AtomicBool::new(false));
    let prep = if composite {
        Preparation::for_request(&QueryOptions {
            cancellation: Some(Cancellation::from_flags([
                Arc::new(AtomicBool::new(false)),
                flag.clone(),
            ])),
            max_memory_bytes: Some(64 << 20),
            ..Default::default()
        })
        .unwrap()
    } else {
        preparation(&flag)
    };
    let policy = Arc::new(GraphAccess::all());
    let view = fixture.view(policy.clone(), &prep);
    let baseline = prep.inner_used();
    let opts = QueryOptions {
        graphs: Some(policy),
        ..Default::default()
    };
    let mut cursor = strict_source(
        &fixture.child,
        &view,
        &opts,
        &format!("; hist:from {EARLY} ; hist:to {LATE}"),
    );
    let error = stop_in_read_only_bound(&mut cursor, lookup, fixture.target.seq, move || {
        flag.store(true, Ordering::Relaxed);
    });
    assert!(matches!(error, Error::Cancelled), "{error:?}");
    drop(cursor);
    assert_eq!(prep.inner_used(), baseline);
    assert_eq!(fixture.bytes(), before);
}

#[test]
fn checked_open_stops_inside_read_only_from_timestamp() {
    assert_read_only_timestamp_stop(1, false);
}

#[test]
fn checked_open_stops_inside_read_only_to_timestamp() {
    assert_read_only_timestamp_stop(2, false);
}

#[test]
fn checked_open_honors_original_composed_cancellation_during_read_only_timestamp() {
    assert_read_only_timestamp_stop(2, true);
}

fn assert_absolute_timestamp_deadline(source_expires: bool) {
    let fixture = ColdFamily::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let prep = Preparation::new(
        Control {
            deadline: source_expires.then_some(deadline),
            ..Control::none()
        },
        64 << 20,
    )
    .unwrap();
    let policy = Arc::new(GraphAccess::all());
    let view = fixture.view(policy.clone(), &prep);
    let baseline = prep.inner_used();
    let opts = QueryOptions {
        graphs: Some(policy),
        deadline: Some(if source_expires {
            deadline + CREDIT
        } else {
            deadline
        }),
        ..Default::default()
    };
    let mut cursor = strict_source(&fixture.child, &view, &opts, &format!("; hist:to {LATE}"));
    cursor
        .deadline_pause()
        .store(CREDIT.as_nanos() as u64, Ordering::Relaxed);
    let error = stop_in_read_only_bound(&mut cursor, 1, fixture.target.seq, move || {
        assert!(
            Instant::now() < deadline,
            "the deadline expired before the witness"
        );
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        assert!(Instant::now() >= deadline);
    });
    assert!(matches!(error, Error::Timeout), "{error:?}");
    drop(cursor);
    assert_eq!(prep.inner_used(), baseline);
}

#[test]
fn checked_open_keeps_absolute_source_deadline_during_timestamp_lookup() {
    assert_absolute_timestamp_deadline(true);
}

#[test]
fn checked_open_keeps_absolute_query_deadline_during_timestamp_lookup() {
    assert_absolute_timestamp_deadline(false);
}

#[test]
fn ordinary_cursor_applies_consumer_credit_during_timestamp_open() {
    let store = Store::in_memory(Default::default());
    write(&store, "<urn:one> <urn:p> 1 .");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut cursor = select_cursor(
        store.snapshot(),
        &query(&format!("; hist:to {LATE}")),
        &QueryOptions {
            deadline: Some(deadline),
            ..Default::default()
        },
        &strict(),
    )
    .unwrap();
    cursor
        .deadline_pause()
        .store(CREDIT.as_nanos() as u64, Ordering::Relaxed);
    assert!(Instant::now() < deadline);
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(Instant::now() >= deadline);
    assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 1);
    assert!(cursor.next_batch().unwrap().is_none());
}

fn rows(cursor: &mut QueryCursor) -> Vec<Vec<Option<oxrdf::Term>>> {
    let mut rows = Vec::new();
    while let Some(batch) = cursor.next_batch().unwrap() {
        for idx in 0..batch.len() {
            rows.push(batch.row(idx).unwrap().values().to_vec());
        }
    }
    rows
}

#[test]
fn checked_open_preserves_writable_and_cold_selector_rows_and_order() {
    let cold = ColdFamily::new();
    let writable = Store::in_memory(Default::default());
    let time = cold.target.timestamp_ms;
    writable.set_clock(Arc::new(move || time));
    write(&writable, "<urn:early> <urn:p> 1 .");
    let first = writable.head_commit().seq;
    write_many(&writable);
    let target = writable.head_commit().seq;
    let policy = Arc::new(GraphAccess::all());
    let prep = Preparation::new(Control::none(), 128 << 20).unwrap();
    let cold_view = cold.view(policy.clone(), &prep);
    let log = cold_view.snapshot().change_log.as_ref().unwrap();
    assert_eq!(
        log.commit_at_time_checked(i64::MAX, &mut || Ok(()))
            .unwrap(),
        Some(cold.later),
        "the time lookup sees the captured head beyond the row ceiling"
    );
    let warm_view = writable.prepare_read(policy.clone(), &prep).unwrap();
    writable.set_clock(Arc::new(move || time + 1000));
    write(&writable, "<urn:later> <urn:p> 2 .");
    assert!(
        warm_view
            .snapshot()
            .change_log
            .as_ref()
            .unwrap()
            .readonly
            .is_none()
    );
    let timestamp = cold.target.timestamp();
    let opts = QueryOptions {
        graphs: Some(policy),
        ..Default::default()
    };
    for (store, view, first, target, later) in [
        (
            cold.child.as_ref(),
            &cold_view,
            cold.early,
            cold.target.seq,
            cold.later,
        ),
        (&writable, &warm_view, first, target, target + 1),
    ] {
        for (bounds, expected) in [
            (
                format!(
                    "; hist:from \"{timestamp}\"^^<http://www.w3.org/2001/XMLSchema#dateTime> \
                     ; hist:to \"time:{timestamp}\""
                ),
                8001,
            ),
            (
                format!("; hist:from \"commit:{first}\" ; hist:to \"head\""),
                8001,
            ),
            (
                format!("; hist:from \"commit:{target}\" ; hist:to {LATE}"),
                8000,
            ),
            (
                format!("; hist:from \"commit:{later}\" ; hist:to \"head\""),
                0,
            ),
        ] {
            for order in ["", "; hist:order hist:descending"] {
                let bounds = format!("{bounds} {order}");
                // Eager uses the exact access already bound to this prepared view.
                let eager = query_source(
                    view.read_source(),
                    store,
                    &query(&bounds),
                    &QueryOptions::default(),
                )
                .unwrap();
                let eager: Vec<_> = eager
                    .rows()
                    .unwrap()
                    .iter()
                    .map(|r| r.values().to_vec())
                    .collect();
                let mut cursor = strict_source(store, view, &opts, &bounds);
                let streamed = rows(&mut cursor);
                assert_eq!(streamed.len(), expected, "{bounds}");
                assert_eq!(streamed, eager, "{bounds}");
            }
        }
    }
}
