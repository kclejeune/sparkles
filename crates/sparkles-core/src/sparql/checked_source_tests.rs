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
fn validation_query_context_admits_physical_ids_and_retains_their_credit() {
    use crate::sparql::ctx::{DEFAULT_GRAPH_IRI, UNION_GRAPH_IRI};
    use crate::validation::DataGraph;
    let store = store();
    let snap = store.snapshot();
    let empty = DataGraph::new(
        snap.clone(),
        Some(DEFAULT_GRAPH_IRI),
        &[],
        &[DEFAULT_GRAPH_IRI.into()],
    )
    .unwrap();
    let all = DataGraph::new(snap, Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
    let empty_ctx = validation_query_context(&empty, opts()).unwrap();
    assert_eq!(empty_ctx.dataset.default.as_deref(), Some(&[][..]));
    let empty_bytes = empty_ctx.source_setup.as_ref().unwrap().bytes();
    let empty_used = empty_ctx.source_preparation.as_ref().unwrap().inner_used();
    drop(empty_ctx);
    let work = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let refusal = validation_query_context(
        &all,
        QueryOptions {
            max_memory_bytes: Some(empty_used),
            work: Some(work.clone()),
            ..opts()
        },
    );
    assert!(
        matches!(
            refusal,
            Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            }))
        ),
        "the same cap fits fixed setup alone"
    );
    assert_eq!(work.load(Ordering::Relaxed), 0);

    let ctx = validation_query_context(&all, opts()).unwrap();
    let ids = ctx.dataset.default.as_ref().unwrap();
    assert_eq!(ids.as_slice(), all.sel.ids(&all.snap).unwrap());
    assert_eq!(ids.first(), Some(&Id::DEFAULT_GRAPH));
    assert!(ctx.dataset.named.is_none());
    assert!(
        ctx.source_setup.as_ref().unwrap().bytes()
            >= empty_bytes + (ids.capacity() * std::mem::size_of::<Id>()) as u64
    );
    let preparation = ctx.source_preparation.as_ref().unwrap().clone();
    let shared = ctx.into_shared();
    let last = shared.clone();
    let used = preparation.inner_used();
    drop(shared);
    assert_eq!(preparation.inner_used(), used);
    drop(last);
    assert!(preparation.inner_used() < used);

    let query_controls = || QueryOptions {
        max_rows_produced: Some(1),
        work: Some(work.clone()),
        ..opts()
    };
    validation_query_context(&all, query_controls())
        .unwrap()
        .produced(1)
        .unwrap();
    let second = validation_query_context(&all, query_controls()).unwrap();
    assert_eq!(second.rows_produced(), 1);
    assert!(matches!(
        second.produced(1),
        Err(Error::BudgetExceeded(crate::error::Budget {
            kind: crate::error::BudgetKind::RowsProduced,
            ..
        }))
    ));
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
    let context = Arc::downgrade(result.test_context());
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
fn snapshot_prepared_query_retains_controls_policy_and_output() {
    let store = store();
    let snap = store.snapshot();
    let pool = pool::QueryPool::new(16 << 20);
    let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    let opts = QueryOptions {
        graphs: Some(Arc::new(access)),
        union_default_graph: Some(true),
        base_iri: Some("https://example.com/".into()),
        prefixes: vec![("ex".into(), "urn:".into())],
        pool: Some(pool.clone()),
        cancellation: Some(crate::task::Cancellation::from_flags([cancelled.clone()])),
        max_memory_bytes: Some(8 << 20),
        ..opts()
    };
    let query = "SELECT * WHERE { ?z ex:p ?o . BIND(<relative> AS ?a) }";
    let result = query_snapshot_prepared(snap.clone(), query, opts.clone()).unwrap();
    assert_eq!(result.variables(), ["z", "o", "a"]);
    assert_eq!(result.len(), 1);
    let term = result
        .term(snap.lookup_iri("urn:a").unwrap())
        .unwrap()
        .unwrap();
    let values = result.rows().unwrap();
    assert_eq!(
        values.row(0).unwrap().values()[2]
            .as_ref()
            .unwrap()
            .to_string(),
        "<https://example.com/relative>"
    );
    drop(values);
    cancelled.store(true, Ordering::Relaxed);
    assert!(matches!(result.rows(), Err(Error::Cancelled)));
    drop(result);
    assert!(pool.granted() > 0);
    assert_eq!(term.to_string(), "<urn:a>");
    drop(term);
    assert_eq!(pool.granted(), 0);
    assert!(matches!(
        query_snapshot_prepared(snap.clone(), "invalid text", opts),
        Err(Error::Cancelled)
    ));
    let timeout = QueryOptions {
        timeout: Some(std::time::Duration::ZERO),
        ..Default::default()
    };
    assert!(matches!(
        query_snapshot_prepared(snap, "invalid text", timeout),
        Err(Error::Timeout)
    ));
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

/// A graph result over a prepared source keeps its query's admitted high point on the
/// source's parent until the result drops (X14 stage 4), as an unnested graph result
/// keeps it on its pool. Construction frees its duplicate set before the result returns,
/// so a nested account that shrank on release would hand that excess back early.
#[test]
fn source_graph_results_keep_their_admitted_high_point_until_dropped() {
    let store = Store::in_memory(StoreOptions::default());
    let object = "x".repeat(100_000);
    let mut body = String::new();
    for i in 0..300 {
        body.push_str(&format!("<urn:s{i}> <urn:p> \"{object}\" .\n"));
    }
    update::update(
        &store,
        &format!("INSERT DATA {{ {body} }}"),
        &Default::default(),
    )
    .unwrap();
    // DESCRIBE's decoded-output estimate charges about 960 MB here.
    let preparation = Preparation::new(Control::none(), 2 << 30).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let source_bytes = preparation.inner_used();
    for q in [
        "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }",
        "CONSTRUCT { GRAPH <urn:g> { ?s <urn:q> ?o . ?s <urn:q> ?o } } WHERE { ?s <urn:p> ?o }",
        "DESCRIBE ?s WHERE { ?s <urn:p> ?o }",
    ] {
        let result = query_source(view.read_source(), &store, q, &opts()).unwrap();
        assert_eq!(result.len(), 300, "{q}");
        let peak = result.mem_peak_bytes;
        assert!(peak >= 30_000_000, "{q}");
        assert!(
            preparation.inner_used() - source_bytes >= peak,
            "{q}: {} retained below the admitted high point {peak}",
            preparation.inner_used() - source_bytes
        );
        let kept = result.clone();
        drop(result);
        assert!(preparation.inner_used() - source_bytes >= peak, "{q}");
        drop(kept);
        assert_eq!(preparation.inner_used(), source_bytes, "{q}");
    }
    // A SELECT result still keeps only what it retains.
    let result = query_source(
        view.read_source(),
        &store,
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts(),
    )
    .unwrap();
    assert_eq!(result.len(), 300);
    assert!(preparation.inner_used() - source_bytes < 30_000_000);
    drop(result);
    assert_eq!(preparation.inner_used(), source_bytes);
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
type UnionGraphObserver = Box<dyn Fn(Id)>;
type ContextObserver = Box<dyn Fn(&Ctx)>;
thread_local! {
    static RELEASE_OBSERVER: std::cell::RefCell<Option<ReleaseObserver>> = const { std::cell::RefCell::new(None) };
    static UNION_OBSERVER: std::cell::RefCell<Option<UnionObserver>> = const { std::cell::RefCell::new(None) };
    static UNION_GRAPH_OBSERVER: std::cell::RefCell<Option<UnionGraphObserver>> =
        const { std::cell::RefCell::new(None) };
    static CONTEXT_OBSERVER: std::cell::RefCell<Option<ContextObserver>> = const { std::cell::RefCell::new(None) };
}
// The crate test allocator records the watched allocation, the original
// live-counter address, the observed value and the event count.
use crate::test_alloc::DEALLOCATION;
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
pub(super) fn observe_union_graph(graph: Id) {
    UNION_GRAPH_OBSERVER.with(|observer| {
        if let Some(observer) = observer.borrow().as_ref() {
            observer(graph);
        }
    });
}
struct Observe;
impl Drop for Observe {
    fn drop(&mut self) {
        RELEASE_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        UNION_OBSERVER.with(|observer| *observer.borrow_mut() = None);
        UNION_GRAPH_OBSERVER.with(|observer| *observer.borrow_mut() = None);
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
    assert_redacted(result.plan());
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
            evaluated_under: None,
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
fn snapshot_prepared_query_cancels_inside_cold_union_restriction() {
    let _observe = Observe;
    let store = store();
    let pool = pool::QueryPool::new(16 << 20);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let triggered = cancel.clone();
    UNION_GRAPH_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |_| triggered.store(true, Ordering::Relaxed)));
    });
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::none(),
    );
    assert!(matches!(
        query_snapshot_prepared(
            store.snapshot(),
            "SELECT ?x WHERE { BIND(1 AS ?x) }",
            QueryOptions {
                graphs: Some(Arc::new(access)),
                union_default_graph: Some(true),
                cancel: Some(cancel.clone()),
                pool: Some(pool.clone()),
                ..opts()
            },
        ),
        Err(Error::Cancelled)
    ));
    assert!(cancel.load(Ordering::Relaxed));
    assert_eq!(pool.granted(), 0);
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
fn snapshot_selector_uses_physical_default_graphs_and_has_no_named_graphs() {
    use crate::validation::DataGraph;
    let store = store();
    store
        .load(&[crate::io::Source::from_bytes(
            format!(
                "_:blank {{ <urn:blank> <urn:p> 4 . <urn:s> <urn:p> 1 }}
                 <{}> {{ <urn:defaultAlias> <urn:p> 5 }}
                 <{}> {{ <urn:unionAlias> <urn:p> 6 }}",
                ctx::DEFAULT_GRAPH_IRI,
                ctx::UNION_GRAPH_IRI
            )
            .into_bytes(),
            crate::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let snapshot = store.snapshot();
    let merged = DataGraph::new(
        snapshot.clone(),
        Some(ctx::UNION_GRAPH_IRI),
        &[],
        &["urn:hidden".into()],
    )
    .unwrap();
    let empty = DataGraph::new(
        snapshot.clone(),
        None,
        &[],
        &[ctx::DEFAULT_GRAPH_IRI.into()],
    )
    .unwrap();
    let options = QueryOptions {
        default_graph_uris: vec!["urn:hidden".into()],
        ..opts()
    };
    let root = output::SnapshotOutputRoot::new(snapshot, &options).unwrap();
    let run = |query: &str, data: &DataGraph| {
        let parsed = parse_query(query, None, &[]).unwrap();
        root.execute_selector(&parsed, &options, data).unwrap()
    };

    let query = "SELECT ?s FROM <urn:hidden> WHERE { ?s <urn:p> ?o } ORDER BY ?s";
    let result = run(query, &merged);
    let subjects: Vec<_> = result
        .test_application_rows()
        .iter()
        .map(|row| row[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(
        subjects,
        [
            "<urn:a>",
            "<urn:blank>",
            "<urn:defaultAlias>",
            "<urn:s>",
            "<urn:unionAlias>"
        ]
    );
    assert!(run(query, &empty).is_empty());
    assert_eq!(
        run(
            "SELECT ?s FROM <urn:hidden> WHERE { BIND(<urn:s> AS ?s) }",
            &empty
        )
        .len(),
        1
    );
    assert_eq!(
        run("SELECT ?s WHERE { VALUES ?s { <urn:s> } }", &empty).len(),
        1
    );

    for graph in [
        "?g",
        "<urn:allowed>",
        "<urn:x-arq:DefaultGraph>",
        "<urn:x-arq:DefaultGraphNode>",
        "<urn:x-arq:UnionGraph>",
    ] {
        for pattern in [
            format!("VALUES ?s {{ <urn:s> }} GRAPH {graph} {{}}"),
            format!("GRAPH {graph} {{ ?s <urn:p>+ ?o }}"),
            format!("VALUES ?s {{ <urn:s> }} FILTER(EXISTS {{ GRAPH {graph} {{}} }})"),
        ] {
            let query = format!("SELECT ?s FROM NAMED <urn:allowed> WHERE {{ {pattern} }}");
            assert!(run(&query, &merged).is_empty(), "{query}");
        }
    }
    assert!(
        run(
            "SELECT ?p (COUNT(?s) AS ?count) WHERE {
                GRAPH <urn:x-arq:UnionGraph> { ?s ?p ?o }
             } GROUP BY ?p",
            &merged,
        )
        .is_empty()
    );
}

#[test]
fn snapshot_selector_retains_the_roots_snapshot_policy_and_controls() {
    use crate::validation::DataGraph;
    let store = store();
    let snapshot = store.snapshot();
    let all = DataGraph::new(snapshot.clone(), Some(ctx::UNION_GRAPH_IRI), &[], &[]).unwrap();
    let foreign = DataGraph::new(Arc::new((*snapshot).clone()), None, &[], &[]).unwrap();
    let pool = pool::QueryPool::new(16 << 20);
    let grant = Arc::new(pool.empty_grant());
    let cancel = Arc::new(AtomicBool::new(false));
    let deadline = Instant::now() + std::time::Duration::from_secs(60);
    let options = QueryOptions {
        graphs: Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
            Graphs::none(),
        ))),
        grant: Some(grant.clone()),
        max_memory_bytes: Some(4 << 20),
        cancel: Some(cancel.clone()),
        deadline: Some(deadline),
        ..opts()
    };
    let root = output::SnapshotOutputRoot::new(snapshot, &options).unwrap();
    let baseline = grant.used();
    let parsed = parse_query("SELECT ?s WHERE { ?s <urn:p> ?o }", None, &[]).unwrap();
    assert!(root.execute_selector(&parsed, &opts(), &foreign).is_err());
    assert_eq!(grant.used(), baseline);

    let result = root.execute_selector(&parsed, &opts(), &all).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(
        result.row(0).unwrap().term(0).unwrap().unwrap().to_string(),
        "<urn:a>"
    );
    let ctx = result.test_context();
    assert!(Arc::ptr_eq(ctx.grant().unwrap(), &grant));
    let preparation = ctx.source_preparation.as_ref().unwrap().clone();
    assert_eq!(preparation.control().deadline, Some(deadline));
    drop(result);
    assert_eq!(grant.used(), baseline);

    let pressure = preparation.reserve(preparation.room()).unwrap();
    assert!(matches!(
        root.execute_selector(&parsed, &opts(), &all),
        Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
    ));
    drop(pressure);
    assert_eq!(grant.used(), baseline);
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(
        root.execute_selector(&parsed, &opts(), &all),
        Err(Error::Cancelled)
    ));
    assert_eq!(grant.used(), baseline);

    let deadline = Instant::now() + Duration::from_secs(1);
    let timed_root = output::SnapshotOutputRoot::new(
        all.snap.clone(),
        &QueryOptions {
            deadline: Some(deadline),
            ..opts()
        },
    )
    .unwrap();
    let result = timed_root.execute_selector(&parsed, &opts(), &all).unwrap();
    std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    assert!(matches!(result.test_context().check(), Err(Error::Timeout)));
    assert!(matches!(
        timed_root.execute_selector(&parsed, &opts(), &all),
        Err(Error::Timeout)
    ));
}

#[test]
fn snapshot_selector_drops_partial_graph_ids_on_refusal() {
    let _observe = Observe;
    let store = store();
    store
        .load(&[crate::io::Source::from_bytes(
            b"<urn:g1> { <urn:a> <urn:p> 1 }
              <urn:g2> { <urn:a> <urn:p> 1 }
              <urn:g3> { <urn:a> <urn:p> 1 }"
                .to_vec(),
            crate::io::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let control = Control::none();
    let preparation = Preparation::new(control.clone(), 4 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &preparation)
        .unwrap();
    let (mut ctx, _) = make_source_ctx(view.read_source(), &opts()).unwrap();
    let baseline = preparation.inner_used();
    let initial_bytes = (4 * std::mem::size_of::<Id>()) as u64;
    let pressure = preparation
        .reserve(preparation.room() - initial_bytes)
        .unwrap();
    assert!(matches!(
        resolve_selector_graphs(&mut ctx, &GraphSel::All),
        Err(Error::BudgetExceeded(b)) if b.kind == crate::BudgetKind::Memory
    ));
    assert_eq!(preparation.room(), initial_bytes);
    drop(pressure);
    assert_eq!(preparation.inner_used(), baseline);

    // Trigger cancellation when the first allocated buffer grows and returns
    // its credit. A partially filled replacement still has to be retired.
    RELEASE_OBSERVER.with(|observer| {
        *observer.borrow_mut() = Some(Box::new(move |_, bytes| {
            if bytes == initial_bytes {
                control.cancel.cancel();
            }
        }));
    });
    assert!(matches!(
        resolve_selector_graphs(&mut ctx, &GraphSel::All),
        Err(Error::Cancelled)
    ));
    assert_eq!(preparation.inner_used(), baseline);
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

#[test]
fn controlled_default_plan_keeps_graph_filter_memory_admitted() {
    use crate::sparql::ctx::UNION_GRAPH_IRI;
    use crate::sparql::plan::{ActiveGraph, Planner};
    use crate::validation::DataGraph;
    use std::fmt::Write;

    const GRAPHS: usize = 512;
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..GRAPHS {
        writeln!(
            data,
            "<urn:s:{i}> <urn:p> <urn:o> <urn:g:{i}> .
             <urn:s:{i}> <urn:q> <urn:o> <urn:g:{i}> .
             <urn:s:{i}> <urn:r> <urn:o> <urn:g:{i}> ."
        )
        .unwrap();
    }
    store
        .load(&[crate::io::Source::from_bytes(
            data.into_bytes(),
            crate::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let snap = store.snapshot();
    let data = DataGraph::new(snap, Some(UNION_GRAPH_IRI), &[], &[]).unwrap();
    let ctx = validation_query_context(&data, opts()).unwrap();
    let preparation = ctx.source_preparation.as_ref().unwrap().clone();
    let before = preparation.inner_used();
    let Query::Select { pattern, .. } = parse_query(
        "SELECT ?s WHERE { ?s <urn:p> ?o . ?s <urn:q> ?x . ?s <urn:r> ?y }",
        None,
        &[],
    )
    .unwrap() else {
        unreachable!()
    };
    let ids_bytes = (GRAPHS * std::mem::size_of::<Id>()) as u64;
    let filler = preparation
        .reserve(preparation.room() - (ids_bytes - 1))
        .unwrap();
    let refused = Planner::new(&ctx).plan(&pattern, &ActiveGraph::Default, Vec::new());
    assert!(matches!(
        refused,
        Err(Error::BudgetExceeded(crate::error::Budget {
            kind: crate::error::BudgetKind::Memory,
            ..
        }))
    ));
    drop(filler);
    assert_eq!(preparation.inner_used(), before);
    let mut planner = Planner::new(&ctx);
    let plan = planner
        .plan(&pattern, &ActiveGraph::Default, Vec::new())
        .unwrap();
    let owner = plan.clone();
    let after_clone = preparation.inner_used();
    drop(plan);
    drop(planner);
    drop(ctx);
    let retained = preparation.inner_used();
    assert!(
        after_clone >= before + ids_bytes && retained >= ids_bytes,
        "controlled default graph filters have no retained admission: before {before}, cloned {after_clone}, retained {retained}"
    );
    drop(owner);
    assert!(preparation.inner_used() < retained);
}

#[test]
fn checked_non_simple_graph_charges_branches_after_graph_names_fit() {
    use crate::sparql::ctx::DEFAULT_GRAPH_IRI;
    use crate::sparql::plan::{ActiveGraph, Planner};
    use crate::validation::DataGraph;
    use std::fmt::Write;

    let store = Store::in_memory(StoreOptions {
        bulk_threshold: u64::MAX,
        ..Default::default()
    });
    let mut input = String::new();
    for i in 0..16 {
        writeln!(input, "<urn:x> <urn:p> <urn:o> <urn:g:{i}> .").unwrap();
    }
    store
        .load(&[crate::io::Source::from_bytes(
            input.into_bytes(),
            crate::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    let data = DataGraph::new(store.snapshot(), Some(DEFAULT_GRAPH_IRI), &[], &[]).unwrap();
    // A twin context that plans nothing gives the credit a dropped context leaves behind.
    let twin = validation_query_context(&data, opts()).unwrap();
    let twin_preparation = twin.source_preparation.clone().unwrap();
    drop(twin);
    let dropped = twin_preparation.inner_used();
    let ctx = validation_query_context(&data, opts()).unwrap();
    let preparation = ctx.source_preparation.clone().unwrap();
    let filler = preparation.reserve(preparation.room() - 8192).unwrap();
    let plan_query = |query: &str, refused: bool| {
        let Query::Select { pattern, .. } = parse_query(query, None, &[]).unwrap() else {
            unreachable!()
        };
        let plan = Planner::new(&ctx).plan(&pattern, &ActiveGraph::Default, Vec::new());
        if refused {
            assert!(
                matches!(
                    plan,
                    Err(Error::BudgetExceeded(crate::error::Budget {
                        kind: crate::error::BudgetKind::Memory,
                        ..
                    }))
                ),
                "{query}"
            );
        } else {
            drop(plan.unwrap());
        }
    };
    ctx.shrink_retained(0).unwrap();
    let before = preparation.inner_used();
    let live = ctx.live_bytes();
    let refused_query = "SELECT ?g WHERE { GRAPH ?g { OPTIONAL { <urn:x> <urn:p> ?o } } }";
    for (query, refused) in [
        ("SELECT ?g WHERE { GRAPH ?g {} }", false),
        (refused_query, true),
    ] {
        plan_query(query, refused);
        assert_eq!(
            ctx.live_bytes(),
            live,
            "planning {query} keeps no live bytes"
        );
        // The context's memory account keeps its admitted high point, here the plan
        // text's decoded constants, until the context lowers it or drops. Branch
        // credit is not part of it, and a second plan of the same query adds nothing.
        let held = preparation.inner_used();
        assert!(
            held - before <= ctx.mem_peak(),
            "planning {query} holds {} bytes above its account's high point {}",
            held - before,
            ctx.mem_peak()
        );
        plan_query(query, refused);
        assert_eq!(
            preparation.inner_used(),
            held,
            "planning {query} again must hold no more credit"
        );
        ctx.shrink_retained(0).unwrap();
        assert_eq!(
            preparation.inner_used(),
            before,
            "planning {query} must return its credit"
        );
    }
    // Without the shrink, dropping the context returns the account's high point.
    plan_query(refused_query, true);
    drop(filler);
    drop(ctx);
    assert_eq!(preparation.inner_used(), dropped);
}

fn checked_graph_fixture(count: usize) -> Store {
    use std::fmt::Write;

    let store = Store::in_memory(StoreOptions::default());
    let mut input = String::new();
    for graph in 0..count {
        let predicate = if graph == 0 { "urn:p" } else { "urn:q" };
        writeln!(
            input,
            "<urn:s> <{predicate}> <urn:g:{graph}> <urn:g:{graph}> ."
        )
        .unwrap();
    }
    store
        .load(&[crate::io::Source::from_bytes(
            input.into_bytes(),
            crate::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store
}

fn physical_default(store: &Store) -> crate::validation::DataGraph {
    crate::validation::DataGraph::new(store.snapshot(), Some(ctx::DEFAULT_GRAPH_IRI), &[], &[])
        .unwrap()
}

fn checked_exists_query(width: usize, term_bytes: usize) -> String {
    use std::fmt::Write;

    let mut text = String::from("SELECT ?g ?hit WHERE { GRAPH ?g { BIND(EXISTS {");
    let literal = "L".repeat(term_bytes);
    for i in 0..width {
        write!(text, " <urn:missing> <urn:p:{i}> \"{literal}\" .").unwrap();
    }
    text.push_str(" } AS ?hit) } } LIMIT 1");
    text
}

fn checked_graph_group(node: &plan::Node) -> &plan::PreparedGraphBranches {
    match &node.kind {
        plan::Kind::GraphBranches(group) => group,
        _ => node
            .children
            .iter()
            .find_map(find_checked_graph_group)
            .expect("non-simple GRAPH must retain its prepared branches"),
    }
}

fn find_checked_graph_group(node: &plan::Node) -> Option<&plan::PreparedGraphBranches> {
    match &node.kind {
        plan::Kind::GraphBranches(group) => Some(group),
        _ => node.children.iter().find_map(find_checked_graph_group),
    }
}

fn branch_exists(node: &plan::Node) -> Option<&expr::ExistsSpec> {
    match &node.kind {
        plan::Kind::Extend(_, expr::Expr::Exists(spec)) => Some(spec),
        _ => node.children.iter().find_map(branch_exists),
    }
}

#[test]
fn checked_graph_r12_shares_admitted_exists_syntax() {
    use plan::{ActiveGraph, Planner};
    use spargebra::algebra::GraphPattern;

    let mut failures = Vec::new();
    for graphs in [1, 2, 8] {
        for (width, term_bytes) in [(8, 4097), (32, 1025)] {
            let store = checked_graph_fixture(graphs);
            let data = physical_default(&store);
            let ctx = validation_query_context(&data, opts()).unwrap();
            let prep = ctx.source_preparation.as_ref().unwrap().clone();
            let before = prep.inner_used();
            let Query::Select { pattern, .. } =
                parse_query(&checked_exists_query(width, term_bytes), None, &[]).unwrap()
            else {
                unreachable!()
            };
            let mut planner = Planner::new(&ctx);
            let plan = planner
                .plan(&pattern, &ActiveGraph::Default, Vec::new())
                .unwrap();
            let branches = &checked_graph_group(&plan).inputs;
            assert_eq!(branches.len(), graphs);
            let mut patterns = Vec::new();
            let mut specs = Vec::new();
            for branch in branches {
                let spec = branch_exists(branch).expect("compiled EXISTS");
                let GraphPattern::Bgp { patterns: triples } = spec.pattern() else {
                    panic!("expected a flat EXISTS BGP")
                };
                assert_eq!(triples.len(), width);
                patterns.push(triples.as_ptr());
                specs.push(spec as *const _);
            }
            assert!(specs.windows(2).all(|pair| pair[0] != pair[1]));
            let admitted = prep.inner_used() - before;
            let owner = plan.clone();
            drop(plan);
            drop(planner);
            drop(ctx);
            let retained = prep.inner_used();
            drop(owner);
            let released = retained - prep.inner_used();
            let shared = patterns.windows(2).all(|pair| pair[0] == pair[1]);
            if !shared
                || admitted < (width * term_bytes) as u64
                || released < (width * term_bytes) as u64
            {
                failures.push(format!(
                    "{graphs} graphs, {width} triples, literal {term_bytes} bytes: shared backing {shared}, admitted {admitted}, released {released}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn checked_graph_r12_refuses_before_copying_exists() {
    use plan::{ActiveGraph, Planner};

    let store = checked_graph_fixture(2);
    let data = physical_default(&store);
    let ctx = validation_query_context(&data, opts()).unwrap();
    let prep = ctx.source_preparation.as_ref().unwrap();
    let empty = prep.inner_used();
    let filler = prep.reserve(prep.room() - 8192).unwrap();
    let before = prep.inner_used();
    let Query::Select { pattern, .. } =
        parse_query(&checked_exists_query(2, 65521), None, &[]).unwrap()
    else {
        unreachable!()
    };
    let mut result = None;
    let largest = crate::test_alloc::record_largest(|| {
        result = Some(Planner::new(&ctx).plan(&pattern, &ActiveGraph::Default, Vec::new()));
    });
    let result = result.unwrap();
    let refused = matches!(
        result,
        Err(Error::BudgetExceeded(crate::error::Budget {
            kind: crate::error::BudgetKind::Memory,
            ..
        }))
    );
    drop(result);
    assert_eq!(ctx.rows_produced(), 0);
    assert_eq!(prep.inner_used(), before);
    drop(filler);
    assert_eq!(prep.inner_used(), empty);
    assert!(
        refused && largest <= 8192,
        "a branch allocated {largest} bytes with only 8192 bytes available, refused {refused}"
    );
}

#[test]
fn checked_graph_r12_admits_simultaneous_keyed_values_copies() {
    use plan::{ActiveGraph, Planner};

    const ROWS: usize = 4096;
    const COLUMN_BYTES: usize = ROWS * std::mem::size_of::<Id>();
    const ROOM: u64 = 116 * 1024;

    let store = checked_graph_fixture(4);
    let data = physical_default(&store);
    let ctx = validation_query_context(&data, opts()).unwrap();
    let prep = ctx.source_preparation.as_ref().unwrap();
    let empty = prep.inner_used();
    let filler = prep.reserve(prep.room() - ROOM).unwrap();
    let before = prep.inner_used();
    let values = "<urn:g:0> ".repeat(ROWS);
    let text = format!("SELECT ?g {{ GRAPH ?g {{ VALUES ?g {{ {values} }} }} }}");
    let Query::Select { pattern, .. } = parse_query(&text, None, &[]).unwrap() else {
        unreachable!()
    };
    assert!(plan::merge_offered(ROWS as f64, false, 1.0, false));
    let mut result = None;
    let (copies, smallest, largest) =
        crate::test_alloc::record_admitted_allocations(prep, COLUMN_BYTES, || {
            result = Some(Planner::new(&ctx).plan(&pattern, &ActiveGraph::Default, Vec::new()))
        });
    let result = result.unwrap();
    let refused = matches!(
        result,
        Err(Error::BudgetExceeded(crate::error::Budget {
            kind: crate::error::BudgetKind::Memory,
            ..
        }))
    );
    drop(result);
    assert_eq!(ctx.rows_produced(), 0);
    assert_eq!(prep.inner_used(), before);
    drop(filler);
    assert_eq!(prep.inner_used(), empty);
    assert!(
        refused
            && copies >= 4
            && smallest >= before + COLUMN_BYTES as u64
            && largest >= before + (3 * COLUMN_BYTES) as u64,
        "{copies} keyed candidate column copies, admitted {} to {} bytes above baseline, refused {refused}",
        smallest.saturating_sub(before),
        largest.saturating_sub(before),
    );
}

const GRAPH_R12_BIND: &str = "SELECT ?g ?value { GRAPH ?g { ?s <urn:p> ?o . BIND(?o AS ?value) } }";

fn graph_r12_open_cursor(
    store: &Store,
    query: &str,
    work_limit: Option<u64>,
    fallback: FallbackPolicy,
) -> Result<QueryCursor> {
    let policy = Arc::new(GraphAccess::all());
    let preparation = Preparation::new(Control::none(), 32 << 20).unwrap();
    let view = store.prepare_read(policy.clone(), &preparation).unwrap();
    select_source_cursor(
        view.read_source(),
        store,
        query,
        &QueryOptions {
            graphs: Some(policy),
            max_rows_produced: work_limit,
            ..opts()
        },
        &CursorOptions {
            batch_rows: 1,
            fallback,
            ..Default::default()
        },
    )
}

#[test]
fn checked_graph_r12_preserves_eager_singleton_rows_and_diagnostics() {
    let store = checked_graph_fixture(1);
    let ordinary = query(store.snapshot(), GRAPH_R12_BIND, &opts()).unwrap();
    let checked = query_snapshot_prepared(store.snapshot(), GRAPH_R12_BIND, opts()).unwrap();
    assert_eq!(
        checked.test_application_rows(),
        ordinary.test_application_rows()
    );
    fn shape(plan: &PlanInfo) -> (String, Vec<String>, Vec<String>) {
        let mut operators = Vec::new();
        fn walk(plan: &PlanInfo, operators: &mut Vec<String>) {
            operators.push(plan.operator.clone());
            for child in &plan.children {
                walk(child, operators);
            }
        }
        walk(plan, &mut operators);
        (plan.operator.clone(), plan.sorted_on.clone(), operators)
    }
    let exact_limit = QueryOptions {
        max_rows_produced: Some(ordinary.rows_produced),
        ..opts()
    };
    let bounded = query_snapshot_prepared(store.snapshot(), GRAPH_R12_BIND, exact_limit);
    assert_eq!(
        (
            checked.rows_produced,
            shape(checked.plan()),
            bounded.is_ok()
        ),
        (ordinary.rows_produced, shape(ordinary.plan()), true)
    );
}

#[test]
fn checked_graph_r12_preserves_cursor_singleton_rows_and_diagnostics() {
    fn operators(plan: &CursorPlan, out: &mut Vec<String>) {
        out.push(plan.operator.operator.clone());
        for child in &plan.children {
            operators(child, out);
        }
    }

    let store = checked_graph_fixture(1);
    for fallback in [
        FallbackPolicy::RejectMaterialization,
        FallbackPolicy::AllowMaterialization,
    ] {
        let options = CursorOptions {
            batch_rows: 1,
            fallback,
            ..Default::default()
        };
        let mut ordinary =
            select_cursor(store.snapshot(), GRAPH_R12_BIND, &opts(), &options).unwrap();
        let mut expected = Vec::new();
        operators(ordinary.plan(), &mut expected);
        let mut ordinary_rows = 0;
        while let Some(batch) = ordinary.next_batch().unwrap() {
            ordinary_rows += batch.len();
        }
        let mut checked = graph_r12_open_cursor(&store, GRAPH_R12_BIND, None, fallback).unwrap();
        let mut actual = Vec::new();
        operators(checked.plan(), &mut actual);
        let mut checked_rows = 0;
        while let Some(batch) = checked.next_batch().unwrap() {
            checked_rows += batch.len();
        }
        let allowed = ordinary.stats().rows_produced;
        let mut tight =
            graph_r12_open_cursor(&store, GRAPH_R12_BIND, Some(allowed), fallback).unwrap();
        let bounded = (|| -> Result<()> {
            while tight.next_batch()?.is_some() {}
            Ok(())
        })();
        assert_eq!(
            (
                checked_rows,
                checked.stats().rows_produced,
                actual,
                bounded.is_ok()
            ),
            (ordinary_rows, allowed, expected, true),
            "{fallback:?}"
        );
    }
}

#[test]
fn checked_graph_r12_keeps_exists_evaluation_distinct_by_graph() {
    let store = checked_graph_fixture(2);
    let query_text = "SELECT ?g ?hit {
        GRAPH ?g {
            VALUES ?s { <urn:s> <urn:s> }
            BIND(EXISTS { ?s <urn:p> ?g } AS ?hit)
        }
    } ORDER BY ?g ?hit";
    let ordinary = query(store.snapshot(), query_text, &opts()).unwrap();
    let checked = query_snapshot_prepared(store.snapshot(), query_text, opts()).unwrap();
    let rows = ordinary.test_application_rows();
    assert_eq!(rows.len(), 4);
    let hit = |i: usize| rows[i][1].as_ref().unwrap().to_string();
    assert_eq!(hit(0), hit(1));
    assert_eq!(hit(2), hit(3));
    assert_ne!(hit(0), hit(2));
    assert_eq!(checked.test_application_rows(), rows);
}

#[test]
fn checked_graph_r12_cancels_and_releases_partial_syntax_and_candidates() {
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    let store = checked_graph_fixture(2);
    let values = "<urn:g:0> ".repeat(64);
    let join = format!("SELECT ?g {{ GRAPH ?g {{ VALUES ?g {{ {values} }} }} }}");
    for (query, phase, at) in [
        (
            checked_exists_query(2, 16_513),
            "copying checked graph text",
            2,
        ),
        (join, "copying graph join candidate", 2),
    ] {
        let flag = Arc::new(AtomicBool::new(false));
        let stop = flag.clone();
        let checkpoints = Arc::new(AtomicUsize::new(0));
        let hits = checkpoints.clone();
        let mut control = Control::with_cancel(flag);
        control.progress = crate::task::Progress::new(move |_, message| {
            if message == phase && hits.fetch_add(1, Ordering::Relaxed) + 1 == at {
                stop.store(true, Ordering::Relaxed);
            }
        });
        let preparation = Preparation::new(control, 2 << 20).unwrap();
        let view = GraphAccess::all()
            .masked_controlled(&store.snapshot(), &preparation)
            .unwrap();
        let before = preparation.inner_used();
        let work = Arc::new(AtomicU64::new(0));
        let result = query_prepared_source(
            view.read_source(),
            &query,
            &QueryOptions {
                work: Some(work.clone()),
                ..opts()
            },
        );
        assert!(matches!(result, Err(Error::Cancelled)), "{phase}");
        assert_eq!(checkpoints.load(Ordering::Relaxed), at);
        assert_eq!(work.load(Ordering::Relaxed), 0);
        assert_eq!(preparation.inner_used(), before);
    }
}

#[test]
fn checked_graph_r12_keeps_ordered_branch_diagnostics_for_limit() {
    fn eager_group(plan: &PlanInfo) -> Option<&PlanInfo> {
        if plan.operator == "GraphBranches" {
            Some(plan)
        } else {
            plan.children.iter().find_map(eager_group)
        }
    }

    fn cursor_group(plan: &CursorPlan) -> Option<&CursorPlan> {
        if plan.operator.operator == "GraphBranches" {
            Some(plan)
        } else {
            plan.children.iter().find_map(cursor_group)
        }
    }

    let store = checked_graph_fixture(2);
    let all = "SELECT ?g ?value WHERE { GRAPH ?g { VALUES ?value { 1 } } }";
    let limited = format!("{all} LIMIT 1");
    let eager = query_snapshot_prepared(store.snapshot(), all, opts()).unwrap();
    let children = &eager_group(eager.plan()).unwrap().children;
    assert_eq!(children.len(), 2);
    assert!(children.iter().all(|branch| branch.actual_rows == 1));
    let eager = query_snapshot_prepared(store.snapshot(), &limited, opts()).unwrap();
    let children = &eager_group(eager.plan()).unwrap().children;
    assert!(children[0].actual_rows >= 0);
    assert_eq!(children[1].actual_rows, -1);

    for fallback in [
        FallbackPolicy::RejectMaterialization,
        FallbackPolicy::AllowMaterialization,
    ] {
        let mut cursor = graph_r12_open_cursor(&store, &limited, None, fallback).unwrap();
        let children = &cursor_group(cursor.plan()).unwrap().children;
        assert_eq!(children.len(), 2);
        assert!(
            children
                .iter()
                .all(|branch| branch.operator.actual_rows == -1)
        );
        assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 1);
        assert!(cursor.next_batch().unwrap().is_none());
        let children = &cursor_group(cursor.plan()).unwrap().children;
        assert!(children[0].operator.actual_rows >= 0);
        assert_eq!(children[1].operator.actual_rows, -1);
        assert!(!children[1].complete);

        let mut cursor = graph_r12_open_cursor(&store, all, None, fallback).unwrap();
        while cursor.next_batch().unwrap().is_some() {}
        let children = &cursor_group(cursor.plan()).unwrap().children;
        assert_eq!(children.len(), 2);
        assert!(
            children
                .iter()
                .all(|branch| branch.operator.actual_rows == 1)
        );
        assert!(children.iter().all(|branch| branch.complete));
    }
}

#[test]
fn checked_graph_r12_matches_ordinary_group_answers() {
    let store = checked_graph_fixture(2);
    for group in [
        "OPTIONAL { ?s <urn:p> ?value }",
        "?s <urn:p> ?o . BIND(?o AS ?value)",
        "BIND(EXISTS { ?s <urn:p> ?g } AS ?value)",
        "BIND(NOT EXISTS { ?s <urn:p> ?g } AS ?value)",
        "VALUES ?s { <urn:s> } FILTER EXISTS { ?s <urn:p> ?g } BIND(?s AS ?value)",
        "{ BIND(1 AS ?value) } UNION { BIND(2 AS ?value) }",
        "{ SELECT (COUNT(?o) AS ?value) WHERE { ?s <urn:p> ?o } }",
        "VALUES (?g ?value) { (<urn:g:0> 1) (<urn:g:1> 2) (UNDEF 3) }",
        "GRAPH ?nested { OPTIONAL { ?s <urn:p> ?value } }",
    ] {
        let query_text =
            format!("SELECT ?g ?value WHERE {{ GRAPH ?g {{ {group} }} }} ORDER BY ?g ?value");
        for bound in [false, true] {
            let options = if bound {
                QueryOptions {
                    initial_bindings: vec![(
                        "g".into(),
                        Term::NamedNode(oxrdf::NamedNode::new_unchecked("urn:g:0")),
                    )],
                    ..opts()
                }
            } else {
                opts()
            };
            let ordinary = query(store.snapshot(), &query_text, &options).unwrap();
            let prepared = query_snapshot_prepared(store.snapshot(), &query_text, options).unwrap();
            assert_eq!(
                ordinary.test_application_rows(),
                prepared.test_application_rows(),
                "{group}, initial graph {bound}",
            );
        }
    }
}

#[test]
fn checked_graph_r12_refuses_or_cancels_before_growing_group_items() {
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    let store = checked_graph_fixture(2);
    let text = "SELECT ?g { GRAPH ?g { OPTIONAL {
        ?s <urn:p:1> ?value . ?s <urn:p:2> ?value .
        ?s <urn:p:3> ?value . ?s <urn:p:4> ?value .
        ?s <urn:p:5> ?value . ?s <urn:p:6> ?value .
        ?s <urn:p:7> ?value .
    } } }";
    for cancel in [false, true] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let grows = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(AtomicUsize::new(usize::MAX));
        let account = Arc::new(parking_lot::Mutex::new(
            None::<(Preparation, Option<crate::access::Reservation>)>,
        ));
        let mut control = Control::with_cancel(cancelled.clone());
        control.progress = crate::task::Progress::new({
            let (grows, seen, account) = (grows.clone(), seen.clone(), account.clone());
            move |_, phase| {
                if phase != "growing checked graph planning buffer"
                    || grows.fetch_add(1, Ordering::Relaxed) != 1
                {
                    return;
                }
                seen.store(
                    crate::test_alloc::admitted_allocations_seen(),
                    Ordering::Relaxed,
                );
                if cancel {
                    cancelled.store(true, Ordering::Relaxed);
                } else {
                    let mut account = account.lock();
                    let (prep, filler) = account.as_mut().expect("set before query");
                    *filler = Some(prep.reserve(prep.room()).unwrap());
                }
            }
        });
        let prep = Preparation::new(control, 2 << 20).unwrap();
        let view = GraphAccess::all()
            .masked_controlled(&store.snapshot(), &prep)
            .unwrap();
        let before = prep.inner_used();
        *account.lock() = Some((prep.clone(), None));
        let rows = Arc::new(AtomicU64::new(0));
        let mut outcome = None;
        let (allocated, ..) = crate::test_alloc::record_admitted_allocations(
            &prep,
            plan::graph_item_slots_bytes(8),
            || {
                outcome = Some(query_prepared_source(
                    view.read_source(),
                    text,
                    &QueryOptions {
                        work: Some(rows.clone()),
                        ..opts()
                    },
                ));
            },
        );
        drop(account.lock().take());
        let outcome = outcome.unwrap();
        if cancel {
            assert!(matches!(outcome, Err(Error::Cancelled)));
        } else {
            assert!(matches!(
                outcome,
                Err(Error::BudgetExceeded(crate::error::Budget {
                    kind: crate::error::BudgetKind::Memory,
                    ..
                }))
            ));
        }
        assert_eq!(grows.load(Ordering::Relaxed), 2);
        assert_eq!(
            allocated,
            seen.load(Ordering::Relaxed),
            "the replacement item vector allocated after refusal"
        );
        assert_eq!(rows.load(Ordering::Relaxed), 0);
        assert_eq!(prep.inner_used(), before);
    }
}

fn checked_chain_fixture(width: usize) -> Store {
    use std::fmt::Write;

    let store = Store::in_memory(StoreOptions::default());
    let mut input = String::new();
    for graph in 0..2 {
        for step in 1..=width {
            writeln!(
                input,
                "<urn:n:{}> <urn:c:{step}> <urn:n:{step}> <urn:g:{graph}> .",
                step - 1
            )
            .unwrap();
        }
    }
    store
        .load(&[crate::io::Source::from_bytes(
            input.into_bytes(),
            crate::io::RdfFormat::NQuads,
            None,
        )])
        .unwrap();
    store
}

fn checked_chain_query(width: usize) -> String {
    use std::fmt::Write;

    let mut text = String::from("SELECT * WHERE { GRAPH ?g {");
    for step in 1..=width {
        write!(text, " ?x{} <urn:c:{step}> ?x{step} .", step - 1).unwrap();
    }
    for step in 0..=width {
        write!(text, " FILTER(?x{step} != <urn:n:absent>)").unwrap();
    }
    // The BIND makes the group non-simple, so each graph gets its own branch.
    text.push_str(" BIND(?x0 AS ?first) } }");
    text
}

/// The planner storage of each join-order stage that a checked GRAPH plan holds
/// beyond the credit it acquired, for a connected chain of `width` patterns.
fn checked_join_order_excess(width: usize) -> [i64; crate::test_alloc::STAGES] {
    use plan::{ActiveGraph, Planner};

    let store = checked_chain_fixture(width);
    let data = physical_default(&store);
    let ctx = validation_query_context(&data, opts()).unwrap();
    let prep = ctx.source_preparation.as_ref().unwrap().clone();
    let Query::Select { pattern, .. } =
        parse_query(&checked_chain_query(width), None, &[]).unwrap()
    else {
        unreachable!()
    };
    let mut planner = Planner::new(&ctx);
    let mut plan = None;
    let excess = crate::test_alloc::record_unadmitted(&prep, || {
        plan = Some(
            planner
                .plan(&pattern, &ActiveGraph::Default, Vec::new())
                .unwrap(),
        );
    });
    assert_eq!(checked_graph_group(plan.as_ref().unwrap()).inputs.len(), 2);
    excess
}

#[test]
#[ignore = "fail-first witness for join-order admission, which is pending"]
fn checked_graph_r12_admits_join_order_storage() {
    const STAGES: [&str; 8] = [
        "group",
        "greedy",
        "program tables",
        "program run",
        "program build",
        "greedy build",
        "filtered plans",
        "join plans",
    ];
    // The exhaustive program plans up to 16 inputs, and greedy alone plans more.
    let mut failures = Vec::new();
    for (small, large) in [(4, 12), (20, 40)] {
        let (a, b) = (
            checked_join_order_excess(small),
            checked_join_order_excess(large),
        );
        println!("widths {small}/{large}: {a:?} / {b:?}");
        for (stage, name) in STAGES.iter().enumerate() {
            if b[stage] > a[stage].max(0) + 4096 {
                failures.push(format!(
                    "{name}: {} unadmitted bytes at width {small}, {} at width {large}",
                    a[stage], b[stage]
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn checked_graph_r12_join_order_matches_ordinary_answers() {
    for width in [4, 12, 20] {
        let store = checked_chain_fixture(width);
        let text = format!(
            "{} ORDER BY ?g",
            checked_chain_query(width).replacen("SELECT *", "SELECT ?g ?x0", 1)
        );
        let ordinary = query(store.snapshot(), &text, &opts()).unwrap();
        let prepared = query_snapshot_prepared(store.snapshot(), &text, opts()).unwrap();
        let rows = ordinary.test_application_rows();
        assert_eq!(rows.len(), 2, "width {width}");
        assert_eq!(rows, prepared.test_application_rows(), "width {width}");
    }
}
