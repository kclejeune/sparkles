use super::*;
use crate::access::{GraphAccess, GraphRule, Graphs, Preparation};
use crate::store::{Store, StoreOptions};
use crate::task::Control;
use std::sync::atomic::Ordering;

fn store() -> Store {
    let store = Store::in_memory(StoreOptions::default());
    store.load(&[crate::io::Source::from_bytes(
        b"<urn:s> <urn:p> 1 . <urn:allowed> { <urn:a> <urn:p> 2 . } <urn:hidden> { <urn:h> <urn:p> 3 . }".to_vec(),
        crate::io::RdfFormat::TriG, None,
    )]).unwrap();
    store
}

fn opts() -> QueryOptions {
    QueryOptions {
        no_cache: true,
        ..Default::default()
    }
}

#[test]
fn prepared_query_retains_genuine_source_credit_and_releases_on_final_result() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 2 << 20).unwrap();
    let baseline = preparation.inner_used();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let physical = Arc::downgrade(view.query_input().0);
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(Arc::ptr_eq(
        &result.test_context().snap,
        view.query_input().0
    ));
    assert!(result.test_context().read_source.is_some());
    assert!(preparation.inner_used() > baseline);
    let context = Arc::downgrade(&result.test_context());
    drop(view);
    drop(store);
    assert!(physical.upgrade().is_some());
    assert_eq!(
        result.test_application_rows()[0][0]
            .as_ref()
            .unwrap()
            .to_string(),
        "<urn:s>"
    );
    drop(result);
    assert!(context.upgrade().is_none());
    assert!(physical.upgrade().is_none());
    assert_eq!(preparation.inner_used(), baseline);
}

#[test]
fn two_source_queries_share_the_original_preparation_cap_without_credit_gap() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let first = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    let used = preparation.inner_used();
    let pressure = preparation.reserve(preparation.room()).unwrap();
    assert!(matches!(
        query_source(
            view.read_source(),
            &store,
            "SELECT ?s WHERE { ?s <urn:p> ?o }",
            &opts()
        ),
        Err(Error::BudgetExceeded(_))
    ));
    assert_eq!(preparation.inner_used(), 2 << 20);
    assert_eq!(first.len(), 1);
    drop(pressure);
    assert_eq!(preparation.inner_used(), used);
    let second = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert!(preparation.inner_used() > used);
    drop(second);
    assert_eq!(preparation.inner_used(), used);
    drop(first);
}

#[test]
fn source_query_uses_original_pool_and_refuses_foreign_domain_without_leak() {
    let store = store();
    let pool = pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let preparation = Preparation::with_grant(Control::none(), 2 << 20, grant.clone()).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let source_bytes = preparation.inner_used();
    let foreign = pool::QueryPool::new(64 << 20);
    let options = QueryOptions {
        pool: Some(foreign.clone()),
        ..opts()
    };
    let error = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .err()
    .unwrap();
    assert!(matches!(error, Error::Component(c) if c.code == "foreign-domain"));
    assert_eq!(
        (preparation.inner_used(), grant.used(), foreign.granted()),
        (source_bytes, source_bytes, 0)
    );
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert!(Arc::ptr_eq(result.test_context().grant().unwrap(), &grant));
    assert_eq!(grant.used(), preparation.inner_used());
    drop(result);
    assert_eq!(
        (grant.used(), preparation.inner_used()),
        (source_bytes, source_bytes)
    );
    drop(view);
    drop(preparation);
    assert_eq!(grant.used(), 0);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn checked_query_cannot_replace_prepared_policy_and_applies_graph_restrictions() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 4 << 20).unwrap();
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    let view = access
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let source_bytes = preparation.inner_used();
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    assert!(matches!(
        query_source(
            view.read_source(),
            &store,
            "SELECT ?g WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
            &options
        ),
        Err(Error::Invalid(_))
    ));
    assert_eq!(preparation.inner_used(), source_bytes);
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?g WHERE { GRAPH ?g { ?s <urn:p> ?o } }",
        &opts(),
    )
    .unwrap();
    assert_eq!(
        result.test_application_rows(),
        vec![vec![Some(
            oxrdf::NamedNode::new("urn:allowed").unwrap().into()
        )]]
    );
    drop(result);
    let options = QueryOptions {
        default_graph_uris: vec!["urn:hidden".into()],
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert!(result.is_empty());
    drop(result);
    let options = QueryOptions {
        union_default_graph: Some(true),
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert_eq!(
        result.test_application_rows(),
        vec![vec![Some(oxrdf::NamedNode::new("urn:a").unwrap().into())]]
    );
    drop(result);
    assert_eq!(preparation.inner_used(), source_bytes);
}

#[test]
fn checked_query_preserves_source_and_option_cancellation_before_policy_work() {
    let store = store();
    let control = Control::none();
    let preparation = Preparation::new(control.clone(), 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let options = QueryOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &options),
        Err(Error::Cancelled)
    ));
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    control.cancel.cancel();
    assert!(matches!(
        result.test_context().check(),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &opts()),
        Err(Error::Cancelled)
    ));
}

#[test]
fn checked_source_deadline_cannot_be_extended_by_cursor_wait_credit() {
    let store = store();
    let deadline = Instant::now() + Duration::from_secs(2);
    let control = Control {
        deadline: Some(deadline),
        ..Control::none()
    };
    let preparation = Preparation::new(control, 2 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    result
        .test_context()
        .deadline_pause
        .store(60_000_000_000, Ordering::Relaxed);
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(matches!(result.test_context().check(), Err(Error::Timeout)));
}

#[test]
fn historical_source_requires_current_policy_and_real_runtime_instance() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let view = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let foreign = Store::in_memory(Default::default());
    assert!(query_source(view.read_source(), &foreign, "ASK {}", &opts()).is_err());
    assert!(matches!(
        query_source(view.read_source(), &store, "ASK {}", &opts()),
        Err(Error::NotPermitted(_))
    ));
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &options,
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(result.test_context().source_view.is_some());
    assert!(
        result
            .test_context()
            .read_source
            .as_ref()
            .unwrap()
            .query_input()
            .1
            .is_none()
    );
    drop(view);
    drop(store);
    assert_eq!(
        result.test_application_rows()[0][0]
            .as_ref()
            .unwrap()
            .to_string(),
        "<urn:s>"
    );
}

// Observation hooks are thread-local and test-only. They never alter admission.
type ReleaseObserver = Box<dyn Fn(&Preparation, u64)>;
type UnionObserver = Box<dyn Fn(&Preparation, Option<(usize, u64)>)>;
type ContextObserver = Box<dyn Fn(&Ctx)>;
thread_local! {
    static RELEASE_OBSERVER: std::cell::RefCell<Option<ReleaseObserver>> = const { std::cell::RefCell::new(None) };
    static UNION_OBSERVER: std::cell::RefCell<Option<UnionObserver>> = const { std::cell::RefCell::new(None) };
    static CONTEXT_OBSERVER: std::cell::RefCell<Option<ContextObserver>> = const { std::cell::RefCell::new(None) };
    // Watched allocation, original live-counter address, observed value and event count.
    static DEALLOCATION: std::cell::Cell<(usize, usize, u64, usize)> = const { std::cell::Cell::new((0, 0, 0, 0)) };
}
pub(super) fn observe_source_context(ctx: &Ctx) {
    CONTEXT_OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow().as_ref() {
            observer(ctx);
        }
    });
}
pub(crate) fn observe_release(preparation: &Preparation, bytes: u64) {
    RELEASE_OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow().as_ref() {
            observer(preparation, bytes);
        }
    });
}
pub(super) fn observe_union(preparation: &Preparation, allocation: Option<(usize, u64)>) {
    UNION_OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow().as_ref() {
            observer(preparation, allocation);
        }
    });
}
struct Observe;
impl Drop for Observe {
    fn drop(&mut self) {
        RELEASE_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        UNION_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        CONTEXT_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        DEALLOCATION.with(|observer| observer.set((0, 0, 0, 0)));
    }
}

// Retain the genuine counter owner throughout a selected-allocation watch.
// The allocator only reads its AtomicU64 and records Copy TLS data.
pub(crate) struct AllocationWatch {
    _preparation: Preparation,
    _thread: std::marker::PhantomData<std::rc::Rc<()>>,
}
impl AllocationWatch {
    pub(crate) fn new(pointer: usize, preparation: &Preparation) -> Self {
        DEALLOCATION.with(|watch| {
            assert_eq!(watch.get().0, 0, "one selected allocation per test thread");
            watch.set((
                pointer,
                preparation.test_used_counter() as *const _ as usize,
                0,
                0,
            ));
        });
        Self {
            _preparation: preparation.clone(),
            _thread: std::marker::PhantomData,
        }
    }
    pub(crate) fn observed(&self) -> (u64, usize) {
        DEALLOCATION.with(|watch| {
            let (_, _, used, count) = watch.get();
            (used, count)
        })
    }
}
impl Drop for AllocationWatch {
    fn drop(&mut self) {
        DEALLOCATION.with(|watch| watch.set((0, 0, 0, 0)));
    }
}

struct SourceAllocationObserver;
// SAFETY: System receives exactly the original allocation layouts. Only the
// selected test thread watches one actual backing allocation. Its Preparation stays
// alive until the watch is cleared; the counter points into that retained Inner.
// The allocator only reads an atomic and updates Copy TLS cells, never allocates,
// invokes a callback or panics. Other unit-test allocations pass through unchanged.
unsafe impl std::alloc::GlobalAlloc for SourceAllocationObserver {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        let _ = DEALLOCATION.try_with(|watch| {
            let (watched, counter, _, count) = watch.get();
            if watched != 0 && watched == pointer as usize {
                let used = unsafe { &*(counter as *const std::sync::atomic::AtomicU64) }
                    .load(Ordering::Relaxed);
                watch.set((0, counter, used, count + 1));
            }
        });
        unsafe { std::alloc::System.dealloc(pointer, layout) };
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        // A watched union vector is filled to its admitted capacity and never grows.
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static SOURCE_ALLOCATION_OBSERVER: SourceAllocationObserver = SourceAllocationObserver;

fn secret_policy() -> GraphAccess {
    use crate::access::{Caller, Limits, Protection, Rule, TripleRules};
    GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
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
    )
}
fn secret_store() -> Store {
    let store = Store::in_memory(Default::default());
    update::update(
        &store,
        "INSERT DATA { <urn:s> <urn:secret> 1 ; <urn:visible> 2 }",
        &opts(),
    )
    .unwrap();
    store
}
fn assert_redacted(plan: &exec::PlanInfo) {
    assert_eq!(
        plan.estimated_rows, -1.0,
        "actual protected plan must redact estimates"
    );
    assert_eq!(plan.estimated_cost, -1.0);
    assert!(plan.counters.is_none());
    for child in &plan.children {
        assert_redacted(child);
    }
}
#[test]
fn checked_source_all_graph_triple_policy_is_retained_and_redacts_plan() {
    let store = secret_store();
    let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
    let policy = secret_policy();
    assert!(policy.reads_all() && !policy.reads_everything());
    let view = policy
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?p WHERE { <urn:s> ?p ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(
        result.test_application_rows(),
        vec![vec![Some(
            oxrdf::NamedNode::new("urn:visible").unwrap().into()
        )]]
    );
    assert!(
        result
            .test_context()
            .graphs
            .as_ref()
            .is_some_and(|policy| Arc::ptr_eq(policy, view.query_input().1)),
        "actual bound triple policy must survive even when all graph names are readable"
    );
    assert_redacted(&result.plan());
    let all = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let unrestricted = query_source(
        all.read_source(),
        &store,
        "SELECT ?p WHERE { <urn:s> ?p ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(unrestricted.len(), 2);
    assert!(unrestricted.test_context().graphs.is_none());
}
#[test]
fn checked_source_all_graph_triple_policy_filters_history_changes() {
    let store = secret_store();
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    let policy = Arc::new(secret_policy());
    let query_text = "PREFIX hist: <urn:x-sparkles:history#> SELECT ?p WHERE { SERVICE hist:changes { << <urn:s> ?p ?o >> hist:op \"add\" } }";
    let legacy = query(
        store.snapshot(),
        query_text,
        &QueryOptions {
            graphs: Some(policy.clone()),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(
        legacy.test_application_rows(),
        vec![vec![Some(
            oxrdf::NamedNode::new("urn:visible").unwrap().into()
        )]]
    );
    let view = policy
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let actual = query_source(view.read_source(), &store, query_text, &opts()).unwrap();
    assert_eq!(
        actual.test_application_rows(),
        legacy.test_application_rows(),
        "checked source must hide the same protected recorded changes as the actual policy"
    );
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let evidence = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let historical = query_source(
        evidence.read_source(),
        &store,
        query_text,
        &QueryOptions {
            graphs: Some(policy),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(
        historical.test_application_rows(),
        legacy.test_application_rows()
    );
}
#[derive(Debug)]
struct PolicyView(Arc<std::sync::atomic::AtomicUsize>);
impl viewsvc::ViewSource for PolicyView {
    fn read_view(&self, read: &viewsvc::ViewRead<'_>) -> Result<viewsvc::ViewRows> {
        self.0.store(
            if read
                .caller
                .is_some_and(|caller| caller.reads_all() && !caller.reads_everything())
            {
                1
            } else {
                2
            },
            Ordering::Relaxed,
        );
        Ok(viewsvc::ViewRows {
            columns: vec!["x".into()],
            rows: vec![(vec![Some(oxrdf::Literal::from(7).into())], 1)],
            commit: read.at,
        })
    }
}
#[test]
fn checked_source_all_graph_triple_policy_reaches_view_provider() {
    let store = secret_store();
    let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
    let view = secret_policy()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let called = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let result = query_source(view.read_source(), &store, "PREFIX view: <urn:x-sparkles:view#> SELECT ?x WHERE { SERVICE view:read { [] view:name \"guarded\" ; view:bind ?x } }", &QueryOptions { views: Some(Arc::new(PolicyView(called.clone()))), ..opts() }).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        called.load(Ordering::Relaxed),
        1,
        "the actual ViewRead caller must retain triple protections"
    );
}

#[test]
fn checked_union_error_frees_payload_before_returning_source_credit() {
    let _observe = Observe;
    let store = store();
    let limit = 4 << 20;
    let preparation = Preparation::new(Control::none(), limit).unwrap();
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    let view = access
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let (mut ctx, access) = make_source_ctx(view.read_source(), &opts()).unwrap();
    ctx.dataset.union_default = true;
    let pressure = std::rc::Rc::new(std::cell::RefCell::new(None));
    let retained = pressure.clone();
    UNION_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |preparation, allocation| match allocation {
            None => {
                *retained.borrow_mut() = Some(preparation.reserve(preparation.room() - 8).unwrap())
            }
            Some((pointer, bytes)) => {
                assert_eq!(bytes, 8, "one real visible graph");
                DEALLOCATION.with(|watch| {
                    watch.set((
                        pointer,
                        preparation.test_used_counter() as *const _ as usize,
                        0,
                        0,
                    ))
                });
            }
        }))
    });
    let error = restrict_source_ctx(&mut ctx, access.as_ref()).unwrap_err();
    assert!(matches!(error, Error::BudgetExceeded(_)), "{error}");
    let (_, _, used_at_deallocation, events) = DEALLOCATION.with(std::cell::Cell::get);
    assert_eq!(
        events, 1,
        "must observe actual first union-vector deallocation"
    );
    assert_eq!(
        used_at_deallocation, limit,
        "first union payload must retire before its eight-byte credit returns"
    );
    drop(pressure.borrow_mut().take());
}
#[test]
fn checked_source_policy_drops_before_original_source_credit_returns() {
    let _observe = Observe;
    let store = store();
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    let baseline = preparation.inner_used();
    let view = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    )
    .masked_controlled(&store.snapshot(), &preparation)
    .unwrap();
    let source_bytes = preparation.inner_used() - baseline;
    let weak = Arc::downgrade(view.query_input().1);
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { GRAPH <urn:allowed> { ?s ?p ?o } }",
        &opts(),
    )
    .unwrap();
    let extra_context = result.test_context().clone();
    drop(view);
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = observations.clone();
    let counter = preparation.test_used_counter() as *const _ as usize;
    RELEASE_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |preparation, bytes| {
            if preparation.test_used_counter() as *const _ as usize == counter
                && bytes == source_bytes
            {
                seen.borrow_mut().push(weak.upgrade().is_none());
            }
        }))
    });
    drop(result);
    assert!(
        observations.borrow().is_empty(),
        "another real Ctx still owns the source"
    );
    drop(extra_context);
    assert_eq!(
        &*observations.borrow(),
        &[true],
        "actual original credit return must follow the final engine-held policy Arc"
    );
    assert_eq!(preparation.inner_used(), baseline);
}
#[test]
fn checked_source_option_cancel_stops_current_policy_mid_preparation() {
    for composed in [false, true] {
        let store = secret_store();
        let cancel = Arc::new(AtomicBool::new(false));
        let armed = Arc::new(AtomicBool::new(false));
        let later = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let control = Control {
            progress: crate::task::Progress::new({
                let (cancel, armed, later) = (cancel.clone(), armed.clone(), later.clone());
                move |_, phase| {
                    if armed.load(Ordering::Relaxed) {
                        if phase == "preparing permission view" {
                            cancel.store(true, Ordering::Relaxed);
                        } else {
                            later.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            }),
            ..Control::none()
        };
        let preparation = Preparation::new(control.clone(), 32 << 20).unwrap();
        store
            .activate_memory_configuration_history(&preparation)
            .unwrap();
        let view = store
            .evidence_view(&crate::history::At::Head, &preparation)
            .unwrap();
        let used = preparation.inner_used();
        armed.store(true, Ordering::Relaxed);
        let options = QueryOptions {
            graphs: Some(Arc::new(secret_policy())),
            cancel: (!composed).then(|| cancel.clone()),
            cancellation: composed.then(|| crate::task::Cancellation::from_flags([cancel.clone()])),
            ..opts()
        };
        assert!(matches!(
            query_source(view.read_source(), &store, "ASK {}", &options),
            Err(Error::Cancelled)
        ));
        assert_eq!(
            later.load(Ordering::Relaxed),
            0,
            "query cancellation at the real preparation callback must stop subsequent policy work"
        );
        assert!(!control.cancel.is_cancelled());
        assert_eq!(preparation.inner_used(), used);
    }
}

struct WaitingPolicy {
    lock: Arc<parking_lot::Mutex<()>>,
    started: std::sync::mpsc::SyncSender<()>,
    acquired: Arc<AtomicBool>,
}
impl crate::access::GuardDecision for WaitingPolicy {
    fn exempt(&self, _: &Arc<Snapshot>) -> Vec<String> {
        panic!("uncontrolled provider must not run")
    }
    fn exempt_controlled(
        &self,
        _: &Arc<Snapshot>,
        preparation: &Preparation,
    ) -> Result<crate::access::Exemptions> {
        self.started.send(()).unwrap();
        let _lock = preparation.lock(&self.lock)?;
        self.acquired.store(true, Ordering::Relaxed);
        crate::access::Exemptions::new(preparation)
    }
}
#[test]
fn checked_source_option_deadline_stops_preparation_wait() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let view = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let lock = Arc::new(parking_lot::Mutex::new(()));
    let held = lock.lock();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let acquired = Arc::new(AtomicBool::new(false));
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::All.guarded_by(
                "urn:*",
                crate::access::Decider(Arc::new(WaitingPolicy {
                    lock: lock.clone(),
                    started: tx,
                    acquired: acquired.clone(),
                })),
            ),
            Graphs::none(),
        ))),
        deadline: Some(Instant::now() + Duration::from_millis(150)),
        ..opts()
    };
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| query_source(view.read_source(), &store, "ASK {}", &options));
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let stopped_before_unlock = worker.is_finished();
        drop(held);
        assert!(matches!(worker.join().unwrap(), Err(Error::Timeout)));
        assert!(
            stopped_before_unlock && !acquired.load(Ordering::Relaxed),
            "the real controlled provider wait must observe the query deadline before lock release"
        );
    });
}

#[test]
fn checked_source_early_cancel_retires_policy_before_original_credit() {
    let _observe = Observe;
    let store = store();
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    let baseline = preparation.inner_used();
    let view = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    )
    .masked_controlled(&store.snapshot(), &preparation)
    .unwrap();
    let source_bytes = preparation.inner_used() - baseline;
    let weak = Arc::downgrade(view.query_input().1);
    let source = view.read_source();
    drop(view);
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = observations.clone();
    let counter = preparation.test_used_counter() as *const _ as usize;
    RELEASE_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |preparation, bytes| {
            if preparation.test_used_counter() as *const _ as usize == counter
                && bytes == source_bytes
            {
                seen.borrow_mut().push(weak.upgrade().is_none());
            }
        }))
    });
    CONTEXT_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(|ctx| ctx.cancel.store(true, Ordering::Relaxed)))
    });
    let cancel = Arc::new(AtomicBool::new(false));
    assert!(matches!(
        make_source_ctx(
            source,
            &QueryOptions {
                cancel: Some(cancel),
                ..opts()
            }
        ),
        Err(Error::Cancelled)
    ));
    assert_eq!(
        &*observations.borrow(),
        &[true],
        "initial Ctx refusal must not leave an earlier local policy clone past original source credit"
    );
    assert_eq!(preparation.inner_used(), baseline);
}

#[test]
fn checked_source_control_view_shares_original_cap_and_decoder_controls() {
    use crate::history::ReadLimit;
    for composed in [false, true] {
        let control = Control::none();
        let original = Preparation::new(control.clone(), 4096)
            .unwrap()
            .with_no_wait(true);
        let query_cancel = Arc::new(AtomicBool::new(false));
        let view = original
            .with_query_controls(
                (!composed).then(|| query_cancel.clone()),
                composed.then(|| crate::task::Cancellation::from_flags([query_cancel.clone()])),
                None,
            )
            .unwrap();
        let other = original.with_query_controls(None, None, None).unwrap();
        assert!(std::ptr::eq(
            original.test_used_counter(),
            view.test_used_counter()
        ));
        let held = view.reserve(original.room()).unwrap();
        assert!(matches!(other.reserve(1), Err(Error::BudgetExceeded(_))));
        drop(held);
        let limit = ReadLimit {
            options: &view,
            memory: Some(4096),
            held: 0,
        };
        let decoder = crate::store::cold::ColdBudget::new(limit).unwrap();
        query_cancel.store(true, Ordering::Relaxed);
        assert!(matches!(decoder.check(), Err(Error::Cancelled)));
        assert!(original.check().is_ok() && other.check().is_ok());
        assert!(!control.cancel.is_cancelled());
        control.cancel.cancel();
        assert!(matches!(other.check(), Err(Error::Cancelled)));
        assert_eq!(original.inner_used(), 256);
    }
    let original = Preparation::new(Control::none(), 4096)
        .unwrap()
        .with_no_wait(true);
    let view = original.with_query_controls(None, None, None).unwrap();
    let lock = parking_lot::Mutex::new(());
    let _held = lock.lock();
    assert!(matches!(view.lock(&lock), Err(Error::Invalid(_))));
    let expired = Preparation::new(Control::none(), 4096)
        .unwrap()
        .with_query_controls(None, None, Some(Instant::now() + Duration::from_millis(10)))
        .unwrap();
    let limit = ReadLimit {
        options: &expired,
        memory: None,
        held: 0,
    };
    let decoder = crate::store::cold::ColdBudget::new(limit).unwrap();
    std::thread::sleep(Duration::from_millis(15));
    assert!(matches!(decoder.check(), Err(Error::Timeout)));
}

#[test]
fn checked_source_query_deadline_stays_absolute_after_cursor_pause() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 4 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let options = QueryOptions {
        timeout: Some(Duration::from_millis(150)),
        ..opts()
    };
    let result = query_source(view.read_source(), &store, "ASK {}", &options).unwrap();
    let original_deadline = result.test_context().deadline.unwrap();
    result
        .test_context()
        .deadline_pause
        .store(60_000_000_000, Ordering::Relaxed);
    std::thread::sleep(original_deadline.saturating_duration_since(Instant::now()));
    assert!(matches!(result.test_context().check(), Err(Error::Timeout)));
    assert!(
        preparation.check().is_ok(),
        "one query must not mutate original source controls"
    );
    let next = query_source(view.read_source(), &store, "ASK {}", &opts()).unwrap();
    assert!(next.boolean);
}

#[test]
fn checked_source_union_cancel_retires_its_actual_partial_payload() {
    let _observe = Observe;
    let store = store();
    let control = Control::none();
    let preparation = Preparation::new(control.clone(), 4 << 20).unwrap();
    let view = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    )
    .masked_controlled(&store.snapshot(), &preparation)
    .unwrap();
    let (mut ctx, access) = make_source_ctx(view.read_source(), &opts()).unwrap();
    ctx.dataset.union_default = true;
    let expected = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = expected.clone();
    UNION_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |preparation, allocation| {
            if let Some((pointer, bytes)) = allocation {
                assert_eq!(bytes, 8);
                seen.set(preparation.inner_used());
                DEALLOCATION.with(|watch| {
                    watch.set((
                        pointer,
                        preparation.test_used_counter() as *const _ as usize,
                        0,
                        0,
                    ))
                });
                control.cancel.cancel();
            }
        }))
    });
    assert!(matches!(
        restrict_source_ctx(&mut ctx, access.as_ref()),
        Err(Error::Cancelled)
    ));
    let (_, _, used_at_deallocation, events) = DEALLOCATION.with(std::cell::Cell::get);
    assert_eq!(events, 1);
    assert_eq!(used_at_deallocation, expected.get());
    assert!(
        ctx.dataset.default.is_none() && ctx.dataset.named.is_none(),
        "no partial dataset is published"
    );
}

#[test]
fn checked_source_empty_visible_union_preserves_policy_and_releases_credit() {
    let store = Store::in_memory(Default::default());
    let preparation = Preparation::new(Control::none(), 4 << 20).unwrap();
    let view = GraphAccess::graphs(Graphs::none(), Graphs::none())
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let before = preparation.inner_used();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT * WHERE { ?s ?p ?o }",
        &QueryOptions {
            union_default_graph: Some(true),
            ..opts()
        },
    )
    .unwrap();
    assert!(result.is_empty());
    assert!(result.test_context().graphs.is_some());
    drop(result);
    assert_eq!(preparation.inner_used(), before);
}

#[test]
fn checked_source_protected_failed_plan_remains_redacted() {
    let store = secret_store();
    let preparation = Preparation::new(Control::none(), 16 << 20).unwrap();
    let view = secret_policy()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let options = QueryOptions {
        max_rows: Some(0),
        ..opts()
    };
    let parsed = parse_query(
        "SELECT ?s WHERE { ?s <urn:visible> ?o }",
        None,
        &options.prefixes,
    )
    .unwrap();
    let source = view.read_source();
    let snapshot = source.query_input().0.clone();
    let prepared =
        prepare_query_input(snapshot, &parsed, &options, None, None, Some(source)).unwrap();
    let failure = execute_prepared(&parsed, &options, 0.0, 0.0, prepared)
        .err()
        .expect("zero-row cap refuses a real nonempty scan");
    assert!(matches!(failure.error, Error::BudgetExceeded(_)));
    assert_redacted(failure.plan.as_ref().expect("real failing operator plan"));
}

#[test]
fn checked_source_new_mask_policy_retires_before_original_evidence_credit() {
    let _observe = Observe;
    let store = secret_store();
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let view = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let source_bytes = view.test_source_bytes();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?p WHERE { <urn:s> ?p ?o }",
        &QueryOptions {
            graphs: Some(Arc::new(secret_policy())),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    let weak_policy = Arc::downgrade(
        result
            .test_context()
            .source_view
            .as_ref()
            .unwrap()
            .query_input()
            .1,
    );
    let extra = result.test_context().clone();
    drop(view);
    let observations = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = observations.clone();
    let counter = preparation.test_used_counter() as *const _ as usize;
    RELEASE_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |preparation, bytes| {
            if preparation.test_used_counter() as *const _ as usize == counter
                && bytes == source_bytes
            {
                seen.borrow_mut().push(weak_policy.upgrade().is_none());
            }
        }))
    });
    drop(result);
    assert!(observations.borrow().is_empty());
    drop(extra);
    assert_eq!(
        &*observations.borrow(),
        &[true],
        "new controlled mask and its internal policy references must retire before original evidence credit"
    );
}

#[test]
fn same_pool_options_keep_the_actual_original_source_grant() {
    let store = store();
    let pool = pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let preparation = Preparation::with_grant(Control::none(), 2 << 20, grant.clone()).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            pool: Some(pool.clone()),
            ..opts()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 1);
    assert!(
        Arc::ptr_eq(result.test_context().grant().unwrap(), &grant),
        "pool metadata must not replace the actual source grant"
    );
}
#[test]
fn same_pool_foreign_grant_cannot_relabel_the_actual_source_parent() {
    let store = store();
    let pool = pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let foreign = Arc::new(pool.empty_grant());
    let preparation = Preparation::with_grant(Control::none(), 2 << 20, grant.clone()).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let before = (preparation.inner_used(), grant.used(), foreign.used());
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            grant: Some(foreign.clone()),
            ..opts()
        },
    );
    assert!(
        matches!(result,Err(Error::Component(c)) if c.code=="foreign-grant"),
        "a real but different same-pool grant must not label the source's existing parent"
    );
    assert_eq!(
        (preparation.inner_used(), grant.used(), foreign.used()),
        before
    );
}

#[test]
fn original_source_grant_precedes_pool_fallback_and_refusal_precedes_new_allocation() {
    let store = store();
    let pool = pool::QueryPool::new(64 << 20);
    let grant = Arc::new(pool.empty_grant());
    let other = Arc::new(pool.empty_grant());
    let foreign_pool = pool::QueryPool::new(64 << 20);
    let preparation = Preparation::with_grant(Control::none(), 2 << 20, grant.clone()).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let baseline = preparation.inner_used();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            grant: Some(grant.clone()),
            pool: Some(foreign_pool.clone()),
            ..opts()
        },
    )
    .unwrap();
    assert!(Arc::ptr_eq(result.test_context().grant().unwrap(), &grant));
    drop(result);
    assert_eq!(preparation.inner_used(), baseline);
    assert_eq!(foreign_pool.granted(), 0);
    let pressure = preparation.reserve(preparation.room()).unwrap();
    let before = (preparation.inner_used(), grant.used(), other.used());
    assert!(
        matches!(query_source(view.read_source(),&store,"ASK {}",&QueryOptions {grant:Some(other.clone()),..opts()}),Err(Error::Component(c)) if c.code=="foreign-grant")
    );
    assert_eq!(
        (preparation.inner_used(), grant.used(), other.used()),
        before
    );
    drop(pressure);
    assert_eq!(preparation.inner_used(), baseline);
}

#[test]
fn parsed_checked_request_source_reuses_preparation_controls_policy_and_final_owner() {
    let store = store();
    let pool = pool::QueryPool::new(16 << 20);
    let grant = Arc::new(pool.empty_grant());
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let options = QueryOptions {
        grant: Some(grant.clone()),
        max_memory_bytes: Some(4 << 20),
        cancel: Some(cancel.clone()),
        deadline: Some(Instant::now() + std::time::Duration::from_secs(60)),
        no_cache: true,
        ..Default::default()
    };
    let root = Preparation::for_request(&options).unwrap();
    let controlled = root.checked_for_query(&options).unwrap();
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    let view = access
        .masked_controlled(&store.snapshot(), &controlled)
        .unwrap();
    let baseline = root.inner_used();
    let parsed = parse_query("SELECT ?s WHERE {GRAPH ?g {?s <urn:p> ?o}}", None, &[]).unwrap();
    let result = execute_source(view.read_source(), &store, &parsed, &options, 0.125).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result.timing.parse_ms, 0.125);
    assert!(Arc::ptr_eq(result.test_context().grant().unwrap(), &grant));
    let last = result.row(0).unwrap().term(0).unwrap().unwrap();
    assert_eq!(last.as_term().to_string(), "<urn:a>");
    let source = Arc::downgrade(view.query_input().0);
    let room = root.room();
    let pressure = root.reserve(room).unwrap();
    assert!(
        matches!(execute_source(view.read_source(), &store, &parsed, &options, 0.0), Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory)
    );
    assert_eq!(last.as_term().to_string(), "<urn:a>");
    drop(pressure);
    assert!(root.inner_used() > baseline);
    cancel.store(true, Ordering::SeqCst);
    assert!(matches!(
        execute_source(view.read_source(), &store, &parsed, &options, 0.0),
        Err(Error::Cancelled)
    ));
    drop(result);
    drop(view);
    drop(controlled);
    drop(root);
    drop(store);
    assert!(source.upgrade().is_some());
    assert!(grant.used() > 0);
    assert_eq!(last.as_term().to_string(), "<urn:a>");
    drop(last);
    assert!(source.upgrade().is_none());
    assert_eq!(grant.used(), 0);
    drop(options);
    drop(grant);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn parsed_historical_source_still_requires_actual_store_current_policy_and_parent() {
    let store = store();
    let preparation = Preparation::new(Control::none(), 8 << 20).unwrap();
    store
        .activate_memory_configuration_history(&preparation)
        .unwrap();
    let view = store
        .evidence_view(&crate::history::At::Head, &preparation)
        .unwrap();
    let parsed = parse_query("ASK {}", None, &[]).unwrap();
    let foreign = Store::in_memory(Default::default());
    let baseline = preparation.inner_used();
    assert!(execute_source(view.read_source(), &foreign, &parsed, &opts(), 0.0).is_err());
    assert!(matches!(
        execute_source(view.read_source(), &store, &parsed, &opts(), 0.0),
        Err(Error::NotPermitted(_))
    ));
    assert_eq!(preparation.inner_used(), baseline);
    let current = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::all())),
        ..opts()
    };
    let result = execute_source(view.read_source(), &store, &parsed, &current, 0.25).unwrap();
    assert!(result.boolean);
    assert_eq!(result.timing.parse_ms, 0.25);
}

#[test]
fn transferred_vocabulary_literal_frees_its_buffer_before_original_preparation_credit() {
    let _observe = Observe;
    let store = Store::in_memory(StoreOptions::default());
    store
        .load(&[crate::io::Source::from_bytes(
            format!("<urn:s> <urn:p> \"{}\" .", "x".repeat(4096)).into_bytes(),
            crate::io::RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let preparation = Preparation::new(Control::none(), 64 << 10).unwrap();
    let original = preparation.inner_used();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let view_only = preparation.inner_used();
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(result.inner.table.cols[0][0].tag(), crate::id::Tag::Vocab);
    let before = preparation.inner_used();
    let local_before = result.inner.ctx.live_bytes();
    let rows = result.rows().unwrap();
    let Some(oxrdf::Term::Literal(value)) = &rows.row(0).unwrap().values()[0] else {
        panic!("the genuine result must retain its literal");
    };
    assert_eq!(value.value().len(), 4096);
    let held = preparation.inner_used();
    assert!(held >= before + 4098);
    DEALLOCATION.with(|watch| {
        watch.set((
            value.value().as_ptr() as usize,
            preparation.test_used_counter() as *const _ as usize,
            0,
            0,
        ))
    });
    let last = rows.clone();
    drop(rows);
    assert_eq!(DEALLOCATION.with(std::cell::Cell::get).3, 0);
    assert_eq!(preparation.inner_used(), held);
    drop(last);
    let (_, _, used_at_free, events) = DEALLOCATION.with(std::cell::Cell::get);
    assert_eq!(
        events, 1,
        "actual final lexical-buffer deallocation must be observed"
    );
    assert_eq!(
        used_at_free, held,
        "physical buffer retires before its original parent credit"
    );
    assert_eq!(result.inner.ctx.live_bytes(), local_before);
    // Ordinary live contexts may keep bounded upstream reservation slack. That
    // is distinct from actual live bytes and exact native receiving credit.
    let parent_after = preparation.inner_used();
    assert!(parent_after >= before && parent_after <= held);
    DEALLOCATION.with(|watch| watch.set((0, 0, 0, 0)));
    drop(result);
    assert_eq!(preparation.inner_used(), view_only);
    drop(view);
    assert_eq!(preparation.inner_used(), original);
}
