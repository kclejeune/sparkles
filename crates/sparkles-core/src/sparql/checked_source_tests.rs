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
            writeln!(
                input,
                "<urn:n:0> <urn:s:{step}> <urn:m:{step}> <urn:g:{graph}> ."
            )
            .unwrap();
            writeln!(
                input,
                "<urn:n:{step}> <urn:v> \"{step}\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g:{graph}> ."
            )
            .unwrap();
        }
        // enough values that a range read beats a filtered scan
        for k in 0..400 {
            writeln!(
                input,
                "<urn:w:{k}> <urn:v> \"{k}\"^^<http://www.w3.org/2001/XMLSchema#integer> <urn:g:{graph}> ."
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

/// A star of `width` patterns on `?x0` in each graph, which the program plans in
/// rounds from a width of 11.
fn checked_star_query(width: usize) -> String {
    use std::fmt::Write;

    let mut text = String::from("SELECT * WHERE { GRAPH ?g {");
    for step in 1..=width {
        write!(text, " ?x0 <urn:s:{step}> ?y{step} .").unwrap();
    }
    for step in 1..=width {
        write!(text, " FILTER(?y{step} != <urn:n:absent>)").unwrap();
    }
    text.push_str(" BIND(?x0 AS ?first) } }");
    text
}

/// Two chains of `width` patterns in each graph that share no variable, which the
/// program joins below a width of 16 and the greedy plan leaves disconnected above it,
/// with a filter over both.
fn checked_apart_query(width: usize) -> String {
    use std::fmt::Write;

    let mut text = String::from("SELECT * WHERE { GRAPH ?g {");
    for step in 1..=width {
        write!(text, " ?x{} <urn:c:{step}> ?x{step} .", step - 1).unwrap();
        write!(text, " ?z{} <urn:c:{step}> ?z{step} .", step - 1).unwrap();
    }
    text.push_str(" FILTER(?x0 != ?z1) BIND(?x0 AS ?first) } }");
    text
}

/// The planner storage of each join-order stage that a checked GRAPH plan holds
/// beyond the credit it acquired, for a connected chain of `width` patterns.
fn checked_join_order_excess(width: usize) -> [i64; crate::test_alloc::STAGES] {
    checked_join_order_excess_of(width, &checked_chain_query(width))
}

/// [`checked_join_order_excess`] for the query `text` over the chain fixture of `width`.
fn checked_join_order_excess_of(width: usize, text: &str) -> [i64; crate::test_alloc::STAGES] {
    checked_join_order_excess_kinds(width, text).0
}

/// The merge, hash, index and other joins of `node` and its children, added to `out`.
fn checked_join_kinds(node: &plan::Node, out: &mut [usize; 4]) {
    match &node.kind {
        plan::Kind::Join { algo, .. } => {
            out[match algo {
                plan::JoinAlgo::Merge => 0,
                plan::JoinAlgo::Hash => 1,
                _ => 3,
            }] += 1
        }
        plan::Kind::IndexJoin(_) => out[2] += 1,
        _ => {}
    }
    for child in &node.children {
        checked_join_kinds(child, out);
    }
}

/// [`checked_join_order_excess_of`] with the merge, hash, index and other joins of the
/// branches' plans.
fn checked_join_order_excess_kinds(
    width: usize,
    text: &str,
) -> ([i64; crate::test_alloc::STAGES], [usize; 4]) {
    use plan::{ActiveGraph, Planner};

    let store = checked_chain_fixture(width);
    let data = physical_default(&store);
    let ctx = validation_query_context(&data, opts()).unwrap();
    let prep = ctx.source_preparation.as_ref().unwrap().clone();
    let Query::Select { pattern, .. } = parse_query(text, None, &[]).unwrap() else {
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
    let group = checked_graph_group(plan.as_ref().unwrap());
    assert_eq!(group.inputs.len(), 2);
    let mut kinds = [0; 4];
    for input in &group.inputs {
        checked_join_kinds(input, &mut kinds);
    }
    (excess, kinds)
}

#[test]
fn checked_graph_r12_admits_built_filters() {
    // A negated property set is a scan under a FILTER that tests the set's terms, a list
    // as long as the set, which the plan keeps. The BIND makes the graph's group one that
    // is planned per graph. The set's terms are not in the store, so planning interns
    // them in the context's vocabulary, which the context's own account admits. They are
    // interned before the observed plan, so that nothing else the context caches is.
    for terms in [4, 400] {
        let set = (0..terms)
            .map(|step| format!("<urn:n:p{step}>"))
            .collect::<Vec<_>>()
            .join("|");
        let excess = checked_interned_plan_excess(
            &format!("SELECT * WHERE {{ GRAPH ?g {{ ?x0 !({set}) ?x1 . BIND(?x0 AS ?first) }} }}"),
            terms,
            |plan| {
                checked_graph_group(plan);
            },
        );
        println!("negated set of {terms}: {excess:?}");
        assert!(excess[6] <= 512, "filters: {} unadmitted bytes", excess[6]);
    }
}

/// The unadmitted bytes per stage of planning `text` on a checked source, after the
/// context interned the IRIs `urn:n:p0` to `urn:n:p{terms - 1}`, which the store does
/// not hold. `check` reads the plan.
fn checked_interned_plan_excess(
    text: &str,
    terms: usize,
    check: impl FnOnce(&plan::Node),
) -> [i64; crate::test_alloc::STAGES] {
    use plan::{ActiveGraph, Planner};
    let store = checked_chain_fixture(4);
    let data = physical_default(&store);
    let ctx = validation_query_context(&data, opts()).unwrap();
    let prep = ctx.source_preparation.as_ref().unwrap().clone();
    let Query::Select { pattern, .. } = parse_query(text, None, &[]).unwrap() else {
        unreachable!()
    };
    for step in 0..terms {
        ctx.intern_term(&Term::NamedNode(oxrdf::NamedNode::new_unchecked(format!(
            "urn:n:p{step}"
        ))));
    }
    let mut planner = Planner::new(&ctx);
    let mut plan = None;
    let excess = crate::test_alloc::record_unadmitted(&prep, || {
        plan = Some(
            planner
                .plan(&pattern, &ActiveGraph::Default, Vec::new())
                .unwrap(),
        );
    });
    check(plan.as_ref().unwrap());
    excess
}

#[test]
#[ignore = "roadmap item 1236"]
fn checked_graph_r12_admits_simple_graph_groups() {
    // Without the BIND, the graph's group does not mention ?g and is planned on the
    // ordinary route, whose FILTER list scales with the set.
    let set = (0..400)
        .map(|step| format!("<urn:n:p{step}>"))
        .collect::<Vec<_>>()
        .join("|");
    let excess = checked_interned_plan_excess(
        &format!("SELECT * WHERE {{ GRAPH ?g {{ ?x0 !({set}) ?x1 }} }}"),
        400,
        |_| {},
    );
    println!("simple negated set of 400: {excess:?}");
    assert!(excess.iter().all(|&bytes| bytes <= 512), "{excess:?}");
}

#[test]
#[ignore = "roadmap item 1238"]
fn checked_graph_r12_admits_optional_joins() {
    // An OPTIONAL in a graph's branch builds its join after the payload is admitted.
    use crate::sparql::ctx::DEFAULT_GRAPH_IRI;
    use crate::sparql::plan::{ActiveGraph, Planner};
    use crate::validation::DataGraph;
    use std::fmt::Write;
    let store = Store::in_memory(StoreOptions {
        bulk_threshold: u64::MAX,
        ..Default::default()
    });
    let mut input = String::new();
    for i in 0..4 {
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
    let ctx = validation_query_context(&data, opts()).unwrap();
    let Query::Select { pattern, .. } = parse_query(
        "SELECT ?g WHERE { GRAPH ?g { OPTIONAL { <urn:x> <urn:p> ?o } } }",
        None,
        &[],
    )
    .unwrap() else {
        unreachable!()
    };
    plan::BRANCH_SHORTFALL.with(|most| most.set(0));
    drop(
        Planner::new(&ctx)
            .plan(&pattern, &ActiveGraph::Default, Vec::new())
            .unwrap(),
    );
    let short = plan::BRANCH_SHORTFALL.with(std::cell::Cell::get);
    println!("optional: branch short {short}");
    assert_eq!(short, 0);
}

#[test]
#[ignore = "known red, see the lane notes"]
fn checked_graph_r12_admits_join_order_tables() {
    // Stage 0 indexes the group's variables, filters and join models. Stage 2 builds the program's tables over every subset of the inputs, which grow
    // with 2^n, stage 3 runs the program over them and stage 4 rebuilds its plan. The
    // greedy plan alone handles widths past 16, and a star of 12 is planned in rounds.
    // The copies of inputs that filters are placed on, whose size follows the input,
    // are admitted in stage 3. Filter placement itself is stage 6, and the join
    // estimates from characteristic sets or probes are stage 8. The last cases have a
    // sub-select input of 2 and of 12 patterns with a filter on its variables.
    let subtree = |k: usize| {
        let mut text = String::from("SELECT * WHERE { GRAPH ?g { { SELECT ?x0 ?x1 WHERE {");
        for step in 1..=k {
            text.push_str(&format!(" ?x{} <urn:c:{step}> ?x{step} .", step - 1));
        }
        text.push_str(" } } ?x0 <urn:s:1> ?y1 . ?x0 <urn:s:2> ?y2 .");
        text.push_str(" FILTER(?x1 != <urn:n:absent>) BIND(?x0 AS ?first) } }");
        text
    };
    // Two sub-selects that each bind every variable of a chain of `k`, so that each
    // input of the group holds `k + 1` join variables in one input.
    let opaque = |k: usize| {
        let mut chain = String::new();
        for step in 1..=k {
            chain.push_str(&format!(" ?x{} <urn:c:{step}> ?x{step} .", step - 1));
        }
        format!(
            "SELECT * WHERE {{ GRAPH ?g {{ {{ SELECT * WHERE {{{chain} }} }} \
             {{ SELECT * WHERE {{{chain} }} }} ?x0 <urn:s:1> ?y1 . \
             BIND(?x0 AS ?first) }} }}"
        )
    };
    let mut cases: Vec<(usize, String)> = [4, 8, 12]
        .into_iter()
        .map(|width| (width, checked_chain_query(width)))
        .collect();
    cases.push((40, checked_chain_query(40)));
    cases.push((12, checked_star_query(12)));
    cases.push((40, checked_star_query(40)));
    cases.push((30, opaque(2)));
    cases.push((60, opaque(60)));
    // Two sub-selects that share only long-named variables they may leave unbound, so
    // that they meet in a hash join with many long keys to describe.
    let hash_keys = |k: usize| {
        let key = |step: usize| format!("k{step}_{}", "a".repeat(200));
        let side = |s: &str| {
            let mut text = format!("{{ SELECT * WHERE {{ ?{s} <urn:s:1> ?{s}m .");
            for step in 2..=k {
                text.push_str(&format!(
                    " OPTIONAL {{ ?{s} <urn:s:{step}> ?{} }}",
                    key(step)
                ));
            }
            text.push_str(" } }");
            text
        };
        format!(
            "SELECT * WHERE {{ GRAPH ?g {{ {} {} BIND(?p AS ?first) }} }}",
            side("p"),
            side("q")
        )
    };
    cases.push((12, hash_keys(9)));
    let apart = checked_apart_query;
    cases.push((12, subtree(2)));
    cases.push((12, subtree(12)));
    cases.push((6, apart(6)));
    cases.push((20, apart(20)));
    // Filters over a wide input and with many conjuncts, a numeric range on a scan's
    // sorted column, which moves into the scan, and a string prefix of a scan's sorted
    // column, which sets the rows the filter reads.
    let wide_filter = {
        let mut text = opaque(60);
        let test = (0..60)
            .map(|step| format!("?x{step} != ?x{}", step + 1))
            .collect::<Vec<_>>()
            .join(" || ");
        text.insert_str(text.len() - 4, &format!(" FILTER({test})"));
        text
    };
    cases.push((60, wide_filter));
    let conjuncts = {
        let mut text = checked_chain_query(12);
        let test = (0..48)
            .map(|step| format!("?x{} != <urn:n:a{step}>", step % 13))
            .collect::<Vec<_>>()
            .join(" && ");
        text.insert_str(text.len() - 4, &format!(" FILTER({test})"));
        text
    };
    cases.push((12, conjuncts));
    let range = |width: usize| {
        let mut text = checked_chain_query(width);
        let at = text.len() - 4;
        text.insert_str(
            at,
            " ?x1 <urn:v> ?v . FILTER(?v > 2 && ?v < 9 && ?v != 5 && ?v >= -1) \
             ?w <urn:v> ?u . FILTER(?u > 2 && ?u < 9)",
        );
        text
    };
    cases.push((12, range(12)));
    cases.push((20, range(20)));
    let prefix = |width: usize| {
        let mut text = checked_star_query(width);
        let at = text.len() - 4;
        text.insert_str(at, " FILTER(STRSTARTS(STR(?y1), \"urn:m:1\"))");
        text
    };
    cases.push((12, prefix(12)));
    cases.push((40, prefix(40)));
    // A range of 40 conjuncts on one scan's sorted column, a prefix with 24 more key
    // tests of the same column, and ten filters on a BIND's variable, placed together
    // after the BIND.
    let many_range = {
        let mut text = checked_chain_query(12);
        let test = (0..40)
            .map(|step| format!("?v > -{step}"))
            .collect::<Vec<_>>()
            .join(" && ");
        text.insert_str(text.len() - 4, &format!(" ?x1 <urn:v> ?v . FILTER({test})"));
        text
    };
    cases.push((12, many_range));
    // The tests are over a pattern of its own, apart from the star, so that its scan is
    // filtered alone and its key ranges are planned.
    let many_tests = {
        let mut text = checked_star_query(12);
        text.insert_str(text.len() - 4, " ?w <urn:s:1> ?t .");
        let mut test = String::from("STRSTARTS(STR(?t), \"urn:m:1\")");
        for step in 0..24 {
            test.push_str(&format!(" && CONTAINS(STR(?t), \"urn:m:{step:0>40}\")"));
        }
        text.insert_str(text.len() - 4, &format!(" FILTER({test})"));
        text
    };
    cases.push((12, many_tests));
    let bound_filters = {
        let mut text = subtree(2);
        for step in 0..10 {
            text.insert_str(
                text.len() - 4,
                &format!(" FILTER(?first != <urn:n:b{step}>)"),
            );
        }
        text
    };
    cases.push((12, bound_filters));
    let caps = [
        (0, "group", 512),
        (1, "greedy", 1024),
        (2, "program tables", 512),
        (3, "program run", 512),
        (4, "program build", 256),
        (5, "greedy build", 256),
        (6, "filters", 512),
        (7, "joins", 512),
    ];
    let mut failures = Vec::new();
    let mut run = Vec::new();
    let mut kinds = [0; 4];
    for (width, text) in &cases {
        plan::REACHED.with(|r| r.set((0, 0)));
        plan::BOUND_TERMS.with(|t| t.set([0; 3]));
        let (excess, of) = checked_join_order_excess_kinds(*width, text);
        // The range and key-test cases reach the code their caps check, and the terms of
        // the bound for that code set the bound.
        let reached = plan::REACHED.with(std::cell::Cell::get);
        let terms = plan::BOUND_TERMS.with(std::cell::Cell::get);
        if text.contains("?v > -39") && (reached.0 < 40 || terms[0] == 0) {
            failures.push(format!(
                "no range scan of 40 conjuncts bounded: {reached:?} {terms:?}"
            ));
        }
        if text.contains("CONTAINS(STR(?t)") && (reached.1 < 25 || terms[1] == 0) {
            failures.push(format!(
                "no scan set by 25 key tests bounded: {reached:?} {terms:?}"
            ));
        }
        println!(
            "{}: {excess:?} merge/hash/index/other joins {of:?} reached {reached:?} terms {terms:?}",
            &text[..text.len().min(160)]
        );
        if text.contains("?k2_") {
            assert!(of[1] > 0, "the sub-selects meet in a hash join");
        }
        for (all, one) in kinds.iter_mut().zip(of) {
            *all += one;
        }
        run.push(excess[3]);
        for (stage, name, cap) in caps {
            if excess[stage] > cap {
                failures.push(format!(
                    "{name}: {} unadmitted bytes for {}",
                    excess[stage],
                    &text[..text.len().min(160)]
                ));
            }
        }
    }
    println!("merge/hash/index/other joins in all: {kinds:?}");
    assert!(
        kinds[0] > 0 && kinds[1] > 0,
        "merge and hash joins are built"
    );
    // Index joins, forced since probing these small patterns never beats reading them,
    // including probes into filtered patterns and wide inputs.
    struct Unforce;
    impl Drop for Unforce {
        fn drop(&mut self) {
            crate::sparql::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(false));
        }
    }
    let _unforce = Unforce;
    crate::sparql::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(true));
    // A chain of one-letter variables whose filters test each variable against a long
    // list of itself, which are large expressions with short descriptions, so that the
    // copies of the probes' filters outweigh their text.
    let lists = |width: usize| {
        let name = |step: usize| char::from(b'h' + step as u8);
        let mut text = String::from("SELECT * WHERE { GRAPH ?g {");
        for step in 1..=width {
            text.push_str(&format!(
                " ?{} <urn:c:{step}> ?{} .",
                name(step - 1),
                name(step)
            ));
        }
        for step in 0..=width {
            let v = name(step);
            let list = vec![format!("?{v}"); 100].join(", ");
            text.push_str(&format!(" FILTER(?{v} IN ({list}))"));
        }
        text.push_str(" BIND(?h AS ?first) } }");
        text
    };
    let mut index = 0;
    for (width, text) in [
        (12, checked_chain_query(12)),
        (40, checked_chain_query(40)),
        (12, checked_star_query(12)),
        (60, opaque(60)),
        (20, apart(20)),
        (12, lists(12)),
    ] {
        let (excess, of) = checked_join_order_excess_kinds(width, &text);
        let text = &text[..text.len().min(160)];
        println!("forced {text}: {excess:?} merge/hash/index/other joins {of:?}");
        index += of[2];
        if excess[7] > 512 {
            failures.push(format!(
                "forced joins: {} unadmitted bytes for {text}",
                excess[7]
            ));
        }
    }
    assert!(index > 0, "index joins are built");
    // Long variable names and filters with long constants over the probed patterns, so
    // that the join descriptions and the copies of the probes' filters are large.
    let heavy = {
        let name = |step: usize| format!("v{step}_{}", "a".repeat(200));
        let mut text = String::from("SELECT * WHERE { GRAPH ?g {");
        for step in 1..=12 {
            text.push_str(&format!(
                " ?{} <urn:c:{step}> ?{} .",
                name(step - 1),
                name(step)
            ));
        }
        for step in 0..=12 {
            let absent: Vec<String> = (0..40)
                .map(|i| format!("<urn:n:absent-{i}-{}>", "b".repeat(100)))
                .collect();
            text.push_str(&format!(
                " FILTER(?{} NOT IN ({}))",
                name(step),
                absent.join(", ")
            ));
        }
        text.push_str(&format!(" BIND(?{} AS ?first) }} }}", name(0)));
        text
    };
    for force in [true, false] {
        crate::sparql::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(force));
        let (excess, of) = checked_join_order_excess_kinds(12, &heavy);
        println!("long names, forced {force}: {excess:?} merge/hash/index/other joins {of:?}");
        for (stage, name, cap) in caps {
            if excess[stage] > cap {
                failures.push(format!(
                    "long names, forced {force}: {} unadmitted bytes in {name}",
                    excess[stage]
                ));
            }
        }
    }
    // The copy of the larger sub-select is admitted as the smaller one is.
    let (small, large) = (run[run.len() - 4], run[run.len() - 3]);
    if large > small + 256 {
        failures.push(format!(
            "input copies: {small} then {large} unadmitted bytes"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The outcome, produced rows and phase events of a checked GRAPH query whose progress
/// callback acts at event `at` of `phase`: it cancels when `cancel`, fills the budget
/// when `fill`, and otherwise only records. Also returns the program's growth target at
/// that event, and the allocations of `size` bytes before that event and in all.
#[allow(clippy::type_complexity)]
fn checked_join_order_at_phase(
    store: &Store,
    text: &str,
    phase: &'static str,
    at: usize,
    cancel: bool,
    fill: bool,
    size: usize,
) -> (Result<QueryResult>, u64, usize, usize, usize, usize) {
    use std::sync::atomic::{AtomicU64, AtomicUsize};

    let cancelled = Arc::new(AtomicBool::new(false));
    let events = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(AtomicUsize::new(usize::MAX));
    let growth = Arc::new(AtomicUsize::new(0));
    let account = Arc::new(parking_lot::Mutex::new(
        None::<(Preparation, Option<crate::access::Reservation>)>,
    ));
    let mut control = Control::with_cancel(cancelled.clone());
    control.progress = crate::task::Progress::new({
        let (events, seen, growth, account) = (
            events.clone(),
            seen.clone(),
            growth.clone(),
            account.clone(),
        );
        move |_, name| {
            if name != phase || events.fetch_add(1, Ordering::Relaxed) != at {
                return;
            }
            seen.store(
                crate::test_alloc::admitted_allocations_seen(),
                Ordering::Relaxed,
            );
            growth.store(
                crate::sparql::joinorder::DP_GROWTH.with(std::cell::Cell::get),
                Ordering::Relaxed,
            );
            if cancel {
                cancelled.store(true, Ordering::Relaxed);
            } else if fill {
                let mut account = account.lock();
                let (prep, filler) = account.as_mut().expect("set before query");
                *filler = Some(prep.reserve(prep.room()).unwrap());
            }
        }
    });
    let prep = Preparation::new(control, 64 << 20).unwrap();
    let view = GraphAccess::all()
        .masked_controlled(&store.snapshot(), &prep)
        .unwrap();
    let before = prep.inner_used();
    *account.lock() = Some((prep.clone(), None));
    let rows = Arc::new(AtomicU64::new(0));
    let mut outcome = None;
    let (allocated, ..) = crate::test_alloc::record_admitted_allocations(&prep, size, || {
        outcome = Some(query_prepared_source(
            view.read_source(),
            text,
            &QueryOptions {
                work: Some(rows.clone()),
                ..opts()
            },
        ));
    });
    drop(account.lock().take());
    let outcome = outcome.unwrap();
    if outcome.is_err() {
        assert_eq!(prep.inner_used(), before, "{phase}: credit returns");
    }
    (
        outcome,
        rows.load(Ordering::Relaxed),
        events.load(Ordering::Relaxed),
        growth.load(Ordering::Relaxed),
        seen.load(Ordering::Relaxed),
        allocated,
    )
}

#[test]
fn checked_graph_r12_refuses_or_cancels_join_order_admissions() {
    use crate::sparql::joinorder::{
        BUILD_PHASE, FILTER_VARS_PHASE, GREEDY_BUILD_PHASE, GREEDY_GROW_PHASE, GREEDY_LEAF_PHASE,
        GREEDY_PHASE, GREEDY_UNIT_PHASE, GROUP_PHASE, GROW_PHASE, JOIN_BUILD_PHASE, LEAF_PHASE,
        MODELS_PHASE, PARTS_PHASE, PREPARE_PHASE, ROUND_PHASE, SCRATCH_PHASE,
    };
    use crate::sparql::plan::FILTER_PHASE;

    // A chain of 12 is planned by the program in one round over 12 inputs, and a star
    // of 12 in several rounds.
    let width = 12;
    let store = checked_chain_fixture(width);
    let chain = checked_chain_query(width).replacen("SELECT *", "SELECT ?g ?x0", 1);
    let star = checked_star_query(width).replacen("SELECT *", "SELECT ?g ?x0", 1);
    // The largest table each admission allocates first: the subsets' join variables
    // (one u128 per subset), and the round's inputs per subset (one u32 per subset).
    // A size of 0 stands for the first storage the admission covers, which a recording
    // run learns, and `usize::MAX` for an admission of a copy, whose allocations are
    // not told apart. Growth events 0 to 2 make room before the inputs are entered, and
    // later ones as subsets are planned. An event of `usize::MAX` stands for the last.
    // The greedy plan alone orders a chain of 20 and a star of 40, and the program's
    // bound is the greedy plan of the chain of 12.
    let wide = checked_chain_fixture(40);
    let chain20 = checked_chain_query(20).replacen("SELECT *", "SELECT ?g ?x0", 1);
    let star40 = checked_star_query(40).replacen("SELECT *", "SELECT ?g ?x0", 1);
    let apart20 = checked_apart_query(20).replacen("SELECT *", "SELECT ?g ?x0", 1);
    let n = 1usize << width;
    for (store, text, phase, at, size) in [
        (&store, &chain, GREEDY_PHASE, 0, 0),
        (&store, &chain, GREEDY_UNIT_PHASE, 0, usize::MAX),
        (&store, &chain, GREEDY_UNIT_PHASE, usize::MAX, usize::MAX),
        (&store, &chain, GREEDY_LEAF_PHASE, 0, usize::MAX),
        (&store, &chain, GREEDY_LEAF_PHASE, usize::MAX, usize::MAX),
        (&store, &chain, GREEDY_GROW_PHASE, 0, 0),
        (&wide, &chain20, GREEDY_PHASE, 0, 0),
        (&wide, &chain20, GREEDY_UNIT_PHASE, usize::MAX, usize::MAX),
        (&wide, &chain20, GREEDY_BUILD_PHASE, 0, 0),
        (&wide, &chain20, GREEDY_BUILD_PHASE, 1, 0),
        (&wide, &star40, GREEDY_GROW_PHASE, 0, 0),
        (&wide, &star40, GREEDY_GROW_PHASE, usize::MAX, 0),
        (&wide, &star40, GREEDY_BUILD_PHASE, 0, 0),
        (&wide, &chain20, JOIN_BUILD_PHASE, 0, usize::MAX),
        (&wide, &chain20, JOIN_BUILD_PHASE, usize::MAX, usize::MAX),
        (&wide, &star40, JOIN_BUILD_PHASE, usize::MAX, usize::MAX),
        // the last join of the greedy plan joins its disconnected parts
        (&wide, &apart20, JOIN_BUILD_PHASE, usize::MAX, usize::MAX),
        (&wide, &chain20, FILTER_PHASE, 0, usize::MAX),
        (&wide, &chain20, FILTER_PHASE, usize::MAX, usize::MAX),
        (&wide, &apart20, FILTER_PHASE, usize::MAX, usize::MAX),
    ]
    .into_iter()
    .chain(
        [
            (&chain, GROUP_PHASE, 0, 0),
            (&chain, GROUP_PHASE, usize::MAX, 0),
            (&chain, MODELS_PHASE, 0, 0),
            (&star, MODELS_PHASE, 1, 0),
            (&chain, PREPARE_PHASE, 0, 16 * n),
            (&chain, ROUND_PHASE, 0, 4 * n),
            (&chain, GROW_PHASE, 1, 0),
            (&chain, GROW_PHASE, 3, 0),
            (&chain, GROW_PHASE, usize::MAX, 0),
            (&chain, FILTER_VARS_PHASE, 0, 0),
            (&chain, FILTER_VARS_PHASE, 1, 0),
            (&chain, PARTS_PHASE, 0, 0),
            (&chain, SCRATCH_PHASE, 0, 0),
            (&chain, BUILD_PHASE, 0, 0),
            (&chain, LEAF_PHASE, 0, usize::MAX),
            (&chain, LEAF_PHASE, usize::MAX, usize::MAX),
            (&chain, JOIN_BUILD_PHASE, 0, usize::MAX),
            (&chain, JOIN_BUILD_PHASE, usize::MAX, usize::MAX),
            (&chain, FILTER_PHASE, 0, usize::MAX),
            (&chain, FILTER_PHASE, usize::MAX, usize::MAX),
            (&star, ROUND_PHASE, 1, 0),
            (&star, SCRATCH_PHASE, 1, 0),
            // the switch to rounds, then the first merged part, whose entries are too few
            // bytes to tell apart
            (&star, PARTS_PHASE, 1, 0),
            (&star, PARTS_PHASE, 2, usize::MAX),
            (&star, GROW_PHASE, usize::MAX, 0),
        ]
        .into_iter()
        .map(|(text, phase, at, size)| (&store, text, phase, at, size)),
    ) {
        let at = if at == usize::MAX {
            let (_, _, events, ..) =
                checked_join_order_at_phase(store, text, phase, 0, false, false, 1);
            assert!(events > 1, "{phase}: {events} events");
            events - 1
        } else {
            at
        };
        let (outcome, _, events, growth, ..) =
            checked_join_order_at_phase(store, text, phase, at, false, false, 1);
        assert!(outcome.is_ok(), "{phase}: the recording run answers");
        assert!(events > at, "{phase}: {events} events");
        let size = if size == 0 { growth } else { size };
        assert!(size > 0, "{phase}: a growth target");
        if size != usize::MAX {
            // the storage is allocated after the event when nothing refuses it
            let (_, _, _, _, seen, allocated) =
                checked_join_order_at_phase(store, text, phase, at, false, false, size);
            assert!(
                allocated > seen,
                "{phase} {at}: {size} bytes allocated after"
            );
        }
        if text == &star && phase == ROUND_PHASE {
            // both graphs plan the star in several rounds, so event 1 is the first
            // graph's second round
            assert!(events >= 4, "{events} rounds");
        }
        for cancel in [false, true] {
            let (outcome, rows, events, _, seen, allocated) =
                checked_join_order_at_phase(store, text, phase, at, cancel, !cancel, size);
            if cancel {
                assert!(matches!(outcome, Err(Error::Cancelled)), "{phase} {at}");
            } else {
                assert!(
                    matches!(
                        outcome,
                        Err(Error::BudgetExceeded(crate::error::Budget {
                            kind: crate::error::BudgetKind::Memory,
                            ..
                        }))
                    ),
                    "{phase} {at}: a refused admission fails the query"
                );
            }
            assert_eq!(rows, 0, "{phase} {at}");
            assert_eq!(events, at + 1, "{phase} {at}: no later admission");
            assert_eq!(
                allocated, seen,
                "{phase} {at}: the refused table was allocated after the refusal"
            );
        }
    }
}

/// A chain of `width` patterns in the default graph, and a star of `width` patterns on
/// `<urn:n:0>`.
fn default_chain_fixture(width: usize) -> Store {
    use std::fmt::Write;

    let store = Store::in_memory(StoreOptions::default());
    let mut input = String::new();
    for step in 1..=width {
        writeln!(
            input,
            "<urn:n:{}> <urn:c:{step}> <urn:n:{step}> .",
            step - 1
        )
        .unwrap();
        writeln!(input, "<urn:n:0> <urn:s:{step}> <urn:m:{step}> .").unwrap();
        writeln!(
            input,
            "<urn:n:{step}> <urn:v> \"{step}\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
        )
        .unwrap();
    }
    // enough values that a range read beats a filtered scan
    for k in 0..400 {
        writeln!(
            input,
            "<urn:w:{k}> <urn:v> \"{k}\"^^<http://www.w3.org/2001/XMLSchema#integer> ."
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

#[test]
fn checked_graph_r12_join_order_matches_ordinary_plan_text() {
    use plan::Planner;

    use crate::sparql::joinorder::{DP_ROUNDS, GREEDY_RELEASED};
    use std::fmt::Write;

    // The program plans chains up to 16 in one round, a star of 12 in several, and the
    // greedy plan wider groups. Both routes plan the same group of the same graph, so
    // only admission differs.
    // The last cases add a numeric range that moves into a scan and a string prefix
    // that sets the rows a filtered scan reads.
    let range = " ?x1 <urn:v> ?v . FILTER(?v > 2 && ?v < 9 && ?v != 5) \
        ?w <urn:v> ?u . FILTER(?u > 2 && ?u < 9)";
    let prefix = " FILTER(STRSTARTS(STR(?y1), \"urn:m:1\"))";
    for (width, star, apart, extra) in [
        (4, false, false, ""),
        (12, false, false, ""),
        (12, true, false, ""),
        (20, false, false, ""),
        (40, true, false, ""),
        (6, false, true, ""),
        (20, false, true, ""),
        (12, false, false, range),
        (20, false, false, range),
        (12, true, false, prefix),
        (40, true, false, prefix),
    ] {
        let store = default_chain_fixture(width);
        let data = physical_default(&store);
        let ctx = validation_query_context(&data, opts()).unwrap();
        let prep = ctx.source_preparation.as_ref().unwrap().clone();
        let mut text = if star {
            let mut text = String::from("SELECT * WHERE {");
            for step in 1..=width {
                write!(text, " ?x0 <urn:s:{step}> ?y{step} .").unwrap();
            }
            for step in 1..=width {
                write!(text, " FILTER(?y{step} != <urn:n:absent>)").unwrap();
            }
            text.push_str(" BIND(?x0 AS ?first) }");
            text
        } else if apart {
            checked_apart_query(width)
                .replacen("GRAPH ?g {", "", 1)
                .replacen("} }", "}", 1)
        } else {
            checked_chain_query(width)
                .replacen("GRAPH ?g {", "", 1)
                .replacen("} }", "}", 1)
        };
        text.insert_str(text.len() - 2, extra);
        // the program plans a star of 16 or fewer in rounds, and the greedy plan a wider one
        let rounds_expected = star && width <= 16;
        let width = if star { width + 100 } else { width };
        let Query::Select { pattern, .. } = parse_query(&text, None, &[]).unwrap() else {
            unreachable!()
        };
        // A fresh planner per route, since a planner numbers the patterns it plans.
        let ordinary = Planner::new(&ctx)
            .plan(&pattern, &plan::ActiveGraph::Default, Vec::new())
            .unwrap();
        let before = prep.inner_used();
        let live_before = ctx.live_bytes();
        let rounds = DP_ROUNDS.with(std::cell::Cell::get);
        let checked = Planner::new(&ctx)
            .plan_admitted_for_test(&pattern, &prep)
            .unwrap();
        let rounds = DP_ROUNDS.with(std::cell::Cell::get) - rounds;
        println!("width {width}: {rounds} rounds");
        let (released, kept, storage) = GREEDY_RELEASED.with(|r| r.replace((0, 0, 0)));
        println!("width {width}: greedy released {released}, kept {kept} for {storage}");
        // The greedy ordering keeps credit for the plans it keeps and releases its heap
        // and working lists when it ends.
        assert!(
            released > 0,
            "width {width}: the ordering released its scratch"
        );
        assert!(kept >= storage, "width {width}: {kept} kept for {storage}");
        assert!(
            kept <= storage + storage / 4 + 256,
            "width {width}: {kept} kept for {storage}"
        );
        if rounds_expected {
            assert!(rounds > 1, "the star is planned in rounds");
        }
        // For the chains apart, the checked plan takes the query context's first growth
        // step, when it decodes a scan's constant term for the plan text (in its scan
        // options), and the step stays as the context's admitted high point. Its live
        // bytes are unchanged, and shrinking it returns the credit.
        if apart {
            assert_eq!(ctx.live_bytes(), live_before, "width {width}: live bytes");
            assert!(
                prep.inner_used() <= before + (64 << 10),
                "width {width}: one growth step"
            );
            ctx.shrink_retained(0).unwrap();
        }
        assert_eq!(prep.inner_used(), before, "width {width}: credit returns");
        let (a, b) = (
            format!("{:?}", exec::describe(&ctx, &ordinary)),
            format!("{:?}", exec::describe(&ctx, &checked)),
        );
        assert!(a.contains("Join"), "width {width}: {a}");
        assert_eq!(a, b, "width {width}");
        fn ranged(n: &plan::Node) -> bool {
            matches!(n.kind, plan::Kind::RangeScan(..)) || n.children.iter().any(ranged)
        }
        assert_eq!(
            ranged(&checked),
            extra == range,
            "width {width}: range scan in {b}"
        );
    }
}

#[test]
fn checked_graph_r12_refuses_join_order_tables_without_fallback() {
    use crate::sparql::joinorder::DP_REFUSALS;

    let width = 12;
    let store = checked_chain_fixture(width);
    // The routes may order the columns of `SELECT *` differently, so project them.
    let text = format!(
        "{} ORDER BY ?g",
        checked_chain_query(width).replacen("SELECT *", "SELECT ?g ?x0 ?x12", 1)
    );
    let ordinary = query(store.snapshot(), &text, &opts())
        .unwrap()
        .test_application_rows();
    let mut refused = 0;
    let mut answered = 0;
    let mut memory = 64 << 10;
    while memory <= 4 << 20 {
        let prep = Preparation::new(Control::default(), memory).unwrap();
        let view = GraphAccess::all()
            .masked_controlled(&store.snapshot(), &prep)
            .unwrap();
        let before = prep.inner_used();
        let refusals = DP_REFUSALS.with(std::cell::Cell::get);
        let outcome = query_prepared_source(view.read_source(), &text, &opts());
        let tables_refused = DP_REFUSALS.with(std::cell::Cell::get) > refusals;
        match outcome {
            Ok(rows) => {
                assert!(
                    !tables_refused,
                    "{memory}: a refused program must fail the query"
                );
                assert_eq!(rows.test_application_rows(), ordinary, "{memory}");
                answered += 1;
            }
            Err(Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            })) => refused += usize::from(tables_refused),
            Err(error) => panic!("{memory}: {error}"),
        }
        assert_eq!(prep.inner_used(), before, "{memory}: credit returns");
        memory += memory / 8;
    }
    println!("{refused} budgets refused the program's tables, {answered} answered");
    assert!(refused > 0, "no budget refused the program's tables");
    assert!(answered > 0, "no budget answered");
}

#[test]
fn checked_graph_r12_admits_join_order_storage() {
    const STAGES: [&str; 9] = [
        "group",
        "greedy",
        "program tables",
        "program run",
        "program build",
        "greedy build",
        "filtered plans",
        "join plans",
        "join estimates",
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

/// The unadmitted-growth observer's maxima while `f` runs against a Preparation of its own.
fn observer_excess(f: impl FnOnce(&Preparation)) -> [i64; crate::test_alloc::STAGES] {
    let prep = Preparation::new(Control::none(), 1 << 20).unwrap();
    crate::test_alloc::record_unadmitted(&prep, || f(&prep))
}

/// `n` bytes of heap storage in one allocation.
fn observer_bytes(n: usize) -> Vec<u8> {
    std::hint::black_box(vec![1u8; n])
}

#[test]
fn checked_graph_r12_observer_nested_surplus() {
    use crate::test_alloc::Stage;
    // An inner stage keeps 1000 bytes of credit for 100 bytes of storage. Its surplus
    // must not cover 800 bytes the outer stage allocates without credit.
    let excess = observer_excess(|prep| {
        let _outer = Stage::enter(1);
        let held = {
            let _inner = Stage::enter(2);
            (prep.reserve(1000).unwrap(), observer_bytes(100))
        };
        let omitted = observer_bytes(800);
        std::hint::black_box((&held, &omitted));
    });
    assert!(excess[1] >= 800, "outer omission hidden: {excess:?}");
    assert!(excess[2] <= 0, "inner stage is covered: {excess:?}");
}

#[test]
fn checked_graph_r12_observer_nested_retirement() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // The outer stage retires the inner storage and, as a named settlement, the inner
    // credit. That is no deficit, and it leaves no cushion for a later omission.
    for omitted in [0, 800] {
        let excess = observer_excess(|prep| {
            let _outer = Stage::enter(1);
            let (held, storage) = {
                let _inner = Stage::enter(2);
                (prep.reserve(1000).unwrap(), observer_bytes(100))
            };
            drop(storage);
            drop(Settled::new(Origin::handed_up(), Some(held)));
            let omitted = (omitted > 0).then(|| observer_bytes(omitted));
            std::hint::black_box(&omitted);
        });
        if omitted == 0 {
            assert!(excess[1] <= 0, "retirement reads as a deficit: {excess:?}");
        } else {
            assert!(excess[1] >= 800, "retirement left a cushion: {excess:?}");
        }
    }
    // An inner stage with retained storage it never admitted reads it itself, and the
    // outer stage's own omission is still seen.
    let excess = observer_excess(|_| {
        let _outer = Stage::enter(1);
        let kept = {
            let _inner = Stage::enter(2);
            observer_bytes(300)
        };
        let omitted = observer_bytes(800);
        std::hint::black_box((&kept, &omitted));
    });
    assert!(excess[2] >= 300 && excess[1] >= 800, "{excess:?}");
}

#[test]
fn checked_graph_r12_observer_settles_nested_frames() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // A settlement inside a nested frame hands the parent no deficit, for credit acquired
    // before the stages and for credit a child stage handed up.
    let excess = observer_excess(|prep| {
        let origin = Origin::now();
        let input = prep.reserve(1000).unwrap();
        let storage = observer_bytes(1000);
        let _outer = Stage::enter(1);
        {
            let _inner = Stage::enter(2);
            drop(storage);
            drop(Settled::new(origin, Some(input)));
        }
    });
    assert!(excess[1] <= 0 && excess[2] <= 0, "input: {excess:?}");
    let excess = observer_excess(|prep| {
        let _outer = Stage::enter(1);
        {
            let _middle = Stage::enter(2);
            let (held, storage) = {
                let _inner = Stage::enter(3);
                (prep.reserve(1000).unwrap(), observer_bytes(100))
            };
            drop(storage);
            drop(Settled::new(Origin::handed_up(), Some(held)));
        }
    });
    assert!(
        excess[1] <= 0 && excess[2] <= 0 && excess[3] <= 0,
        "child: {excess:?}"
    );
}

#[test]
fn checked_graph_r12_observer_retires_by_origin() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // The parent acquires 4096 bytes of credit for 4096 bytes of storage. Wherever the
    // storage and the credit go away, the parent then sees exactly the Y bytes it
    // allocates without credit, and the child sees nothing.
    for omitted in [0usize, 3072] {
        for case in 0..4 {
            let excess = observer_excess(|prep| {
                let _parent = Stage::enter(1);
                let origin = Origin::now();
                let held = prep.reserve(4096).unwrap();
                let mut storage = Some(observer_bytes(4096));
                let mut held = Some(held);
                if case == 1 {
                    drop(storage.take());
                }
                {
                    let _child = Stage::enter(2);
                    if case == 0 || case == 2 {
                        drop(storage.take());
                    }
                    if case < 2 {
                        drop(Settled::new(origin, held.take()));
                    }
                }
                drop(storage.take());
                drop(held.take());
                let omitted = (omitted > 0).then(|| observer_bytes(omitted));
                std::hint::black_box(&omitted);
            });
            assert_eq!(
                (excess[1], excess[2]),
                (omitted as i64, 0),
                "case {case}, omitted {omitted}"
            );
        }
    }
}

#[test]
fn checked_graph_r12_observer_retires_two_levels_down() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // The grandparent's credit and storage go away two stages below it. No frame reads
    // a deficit, and an omission in any frame afterwards is seen exactly there.
    for at in [0, 1, 2, 3] {
        let excess = observer_excess(|prep| {
            let _grand = Stage::enter(1);
            let origin = Origin::now();
            let held = prep.reserve(2048).unwrap();
            let storage = observer_bytes(2048);
            // Options, not a list, so that keeping them allocates nothing more.
            let (mut low, mut mid, mut top) = (None, None, None);
            {
                let _middle = Stage::enter(2);
                {
                    let _inner = Stage::enter(3);
                    drop(storage);
                    drop(Settled::new(origin, Some(held)));
                    if at == 3 {
                        top = Some(observer_bytes(700));
                    }
                }
                if at == 2 {
                    mid = Some(observer_bytes(700));
                }
            }
            if at == 1 {
                low = Some(observer_bytes(700));
            }
            std::hint::black_box((&low, &mid, &top));
        });
        let mut expected = [0i64; crate::test_alloc::STAGES];
        if at > 0 {
            expected[at] = 700;
        }
        assert_eq!(excess, expected, "omission at {at}");
    }
}

#[test]
fn checked_graph_r12_observer_retires_imported_credit_in_a_later_child() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // An earlier child hands its credit and storage up; a later child retires both.
    // The imported credit is consumed, so it covers no later omission.
    for omitted in [0usize, 900] {
        let excess = observer_excess(|prep| {
            let _parent = Stage::enter(1);
            let (origin, held, storage) = {
                let _earlier = Stage::enter(2);
                let origin = Origin::now();
                (origin, prep.reserve(1500).unwrap(), observer_bytes(1500))
            };
            {
                let _later = Stage::enter(3);
                drop(storage);
                drop(Settled::new(origin, Some(held)));
            }
            let omitted = (omitted > 0).then(|| observer_bytes(omitted));
            std::hint::black_box(&omitted);
        });
        assert_eq!(
            (excess[1], excess[2], excess[3]),
            (omitted as i64, 0, 0),
            "omitted {omitted}"
        );
    }
}

#[test]
#[should_panic(expected = "unadmitted-growth observer failure")]
fn checked_graph_r12_observer_retirement_consumes_imported_credit() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // Once a later child retires the credit an earlier child handed up, the parent has
    // no child credit left, so settling more as handed up fails the observation.
    observer_excess(|prep| {
        let _parent = Stage::enter(1);
        let (origin, held, storage) = {
            let _earlier = Stage::enter(2);
            let origin = Origin::now();
            (origin, prep.reserve(1500).unwrap(), observer_bytes(1500))
        };
        {
            let _later = Stage::enter(3);
            drop(storage);
            drop(Settled::new(origin, Some(held)));
        }
        let own = prep.reserve(1500).unwrap();
        drop(Settled::new(Origin::handed_up(), Some(own)));
    });
}

#[test]
fn checked_graph_r12_observer_child_settlement_leaves_no_cushion() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // A child keeps 1000 bytes of credit for 100 bytes of storage and hands both up, one
    // or two stages. Its ancestor frees the storage and then settles the credit: no
    // deficit, and an omission afterwards is seen in full.
    for levels in [1, 2] {
        for omitted in [0usize, 800] {
            let excess = observer_excess(|prep| {
                let _outer = Stage::enter(1);
                let (held, storage) = {
                    let _middle = (levels == 2).then(|| Stage::enter(2));
                    let _inner = Stage::enter(3);
                    (prep.reserve(1000).unwrap(), observer_bytes(100))
                };
                drop(storage);
                drop(Settled::new(Origin::handed_up(), Some(held)));
                let omitted = (omitted > 0).then(|| observer_bytes(omitted));
                std::hint::black_box(&omitted);
            });
            assert_eq!(
                (excess[1], excess[2], excess[3]),
                (omitted as i64, 0, 0),
                "levels {levels}, omitted {omitted}"
            );
        }
    }
}

#[test]
fn checked_graph_r12_observer_surplus_witness_is_the_omission() {
    use crate::test_alloc::{Event, Stage};
    // The outer stage's maximum is set by its own uncredited allocation, not by the
    // teardown after it.
    let prep = Preparation::new(Control::none(), 1 << 20).unwrap();
    let (max, _) = crate::test_alloc::record_unadmitted_events(&prep, || {
        // The inner credit outlives the outer stage, so that its release is no event of
        // the outer stage.
        let held;
        {
            let _outer = Stage::enter(1);
            held = {
                let _inner = Stage::enter(2);
                (prep.reserve(1000).unwrap(), observer_bytes(100))
            };
            let omitted = observer_bytes(800);
            std::hint::black_box(&omitted);
        }
        drop(held);
    });
    assert_eq!((max[1].excess, max[1].event), (800, Event::Alloc));
    assert!(max[2].excess <= 0);
}

#[test]
fn checked_graph_r12_observer_foreign_frees() {
    use crate::test_alloc::{Origin, Settled, Stage};
    // Storage allocated before the stage and freed in it is no cushion.
    let excess = observer_excess(|_| {
        let early = observer_bytes(4096);
        let _stage = Stage::enter(1);
        drop(early);
        let omitted = observer_bytes(3072);
        std::hint::black_box(&omitted);
    });
    assert!(excess[1] >= 3072, "pre-stage free: {excess:?}");
    // Retiring pre-stage storage with its credit, settled, is no deficit, and an
    // unrelated omission after it is still seen.
    for omitted in [0, 3072] {
        let excess = observer_excess(|prep| {
            let origin = Origin::now();
            let held = prep.reserve(4096).unwrap();
            let early = observer_bytes(4096);
            let _stage = Stage::enter(1);
            drop(early);
            drop(Settled::new(origin, Some(held)));
            let omitted = (omitted > 0).then(|| observer_bytes(omitted));
            std::hint::black_box(&omitted);
        });
        if omitted == 0 {
            assert!(excess[1] <= 0, "settled retirement: {excess:?}");
        } else {
            assert!(excess[1] >= 3072, "after retirement: {excess:?}");
        }
    }
    // A child stage's storage freed after it returns is no cushion for the parent.
    let excess = observer_excess(|prep| {
        let _outer = Stage::enter(1);
        let (held, storage) = {
            let _inner = Stage::enter(2);
            (prep.reserve(1000).unwrap(), observer_bytes(1000))
        };
        drop(storage);
        let omitted = observer_bytes(1000);
        std::hint::black_box((&held, &omitted));
    });
    assert!(excess[1] >= 1000, "child free: {excess:?}");
    // Storage of an earlier entry of the same stage number is foreign to a later one.
    let excess = observer_excess(|prep| {
        let (held, storage) = {
            let _first = Stage::enter(1);
            (prep.reserve(1000).unwrap(), observer_bytes(1000))
        };
        let _second = Stage::enter(1);
        drop(storage);
        let omitted = observer_bytes(1000);
        std::hint::black_box((&held, &omitted));
    });
    assert!(excess[1] >= 1000, "repeated stage: {excess:?}");
    // Storage of an earlier observation, and storage allocated while nothing observed.
    let prep = Preparation::new(Control::none(), 1 << 20).unwrap();
    let mut earlier = None;
    crate::test_alloc::record_unadmitted(&prep, || {
        let _stage = Stage::enter(1);
        earlier = Some((prep.reserve(1000).unwrap(), observer_bytes(1000)));
    });
    let (held, storage) = earlier.take().unwrap();
    let unobserved = observer_bytes(1000);
    let excess = crate::test_alloc::record_unadmitted(&prep, || {
        let _stage = Stage::enter(1);
        drop(storage);
        drop(unobserved);
        let omitted = observer_bytes(1500);
        std::hint::black_box(&omitted);
    });
    drop(held);
    assert!(excess[1] >= 1500, "earlier observation: {excess:?}");
}

#[test]
fn checked_graph_r12_observer_reallocations() {
    use crate::test_alloc::Stage;
    // Growth without credit.
    let excess = observer_excess(|prep| {
        let _stage = Stage::enter(1);
        let held = prep.reserve(1000).unwrap();
        let mut grown = Vec::<u8>::with_capacity(1000);
        grown.reserve_exact(3000);
        std::hint::black_box((&held, &grown));
    });
    assert!(excess[1] >= 2000, "growth: {excess:?}");
    // Shrinking before releasing the credit above the new size.
    let excess = observer_excess(|prep| {
        let _stage = Stage::enter(1);
        let mut held = prep.reserve(3000).unwrap();
        let mut shrunk = Vec::<u8>::with_capacity(3000);
        shrunk.shrink_to(1000);
        assert_eq!(shrunk.capacity(), 1000);
        held.release_to(1000);
        std::hint::black_box((&held, &shrunk));
    });
    assert!(excess[1] <= 0, "shrink: {excess:?}");
    // Growth of storage allocated before the stage, without credit.
    let excess = observer_excess(|_| {
        let mut grown = Vec::<u8>::with_capacity(1000);
        let _stage = Stage::enter(1);
        grown.reserve_exact(3000);
        assert_eq!(grown.capacity(), 3000);
        std::hint::black_box(&grown);
    });
    // the whole new storage, not only its growth
    assert!(excess[1] >= 3000, "foreign growth: {excess:?}");
    // A failed reallocation changes nothing: the storage is still the stage's own.
    let excess = observer_excess(|prep| {
        let _stage = Stage::enter(1);
        let held = prep.reserve(1000).unwrap();
        let mut kept = Vec::<u8>::with_capacity(1000);
        assert!(kept.try_reserve_exact(isize::MAX as usize / 2).is_err());
        assert_eq!(kept.capacity(), 1000);
        drop(kept);
        drop(held);
    });
    assert!(excess[1] <= 0, "failed reallocation: {excess:?}");
    // ... and keeps the storage counted: with it and its credit kept, 800 bytes without
    // credit are seen.
    let excess = observer_excess(|prep| {
        let _stage = Stage::enter(1);
        let held = prep.reserve(1000).unwrap();
        let mut kept = Vec::<u8>::with_capacity(1000);
        assert!(kept.try_reserve_exact(isize::MAX as usize / 2).is_err());
        let omitted = observer_bytes(800);
        std::hint::black_box((&held, &kept, &omitted));
        drop(omitted);
        drop(kept);
        drop(held);
    });
    assert!(excess[1] >= 800, "after a failed reallocation: {excess:?}");
}

#[test]
fn checked_graph_r12_observer_detects_release_without_allocation() {
    use crate::test_alloc::Stage;
    // Credit released while its storage lives is a deficit, even when it is acquired
    // again before the stage ends, and when the stage ends with it released.
    for again in [true, false] {
        let excess = observer_excess(|prep| {
            let storage;
            let held_again;
            {
                let _stage = Stage::enter(1);
                let held = prep.reserve(1024).unwrap();
                storage = observer_bytes(1024);
                drop(held);
                held_again = again.then(|| prep.reserve(1024).unwrap());
            }
            std::hint::black_box((&storage, &held_again));
        });
        assert!(excess[1] >= 1024, "acquired again {again}: {excess:?}");
    }
}

/// A GRAPH ?g whose pattern also binds ?g, as the first key column of its scan, under a
/// filter over a list of 101 variables, so that the index candidate of each graph's
/// binding probes on ?g and copies a large filter.
fn checked_graph_binding_query() -> String {
    let list = vec!["?x"; 101].join(", ");
    format!("SELECT * WHERE {{ GRAPH ?g {{ ?g <urn:c:1> ?x . FILTER(?g IN ({list})) }} }}")
}

/// Sets whether index joins are forced, and clears it again on drop.
struct ForceIndexJoin;
impl ForceIndexJoin {
    fn set(force: bool) -> Self {
        crate::sparql::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(force));
        Self
    }
}
impl Drop for ForceIndexJoin {
    fn drop(&mut self) {
        crate::sparql::indexjoin::FORCE_INDEX_JOIN.with(|f| f.set(false));
    }
}

#[test]
fn checked_graph_r12_admits_graph_binding_index_candidate() {
    // Forced, the index candidate is offered and built. Not forced, the offer copies the
    // filter whether or not it is then refused. Both are admitted before they allocate,
    // and the branch keeps no more than was admitted while it was built.
    for force in [true, false] {
        let _force = ForceIndexJoin::set(force);
        plan::GRAPH_INDEX_OFFERS.with(|offers| offers.set(0));
        plan::BRANCH_SHORTFALL.with(|most| most.set(0));
        crate::sparql::indexjoin::PROBE_COPIES.with(|copies| copies.set(0));
        let (excess, of) = checked_join_order_excess_kinds(4, &checked_graph_binding_query());
        let offers = plan::GRAPH_INDEX_OFFERS.with(std::cell::Cell::get);
        let copies = crate::sparql::indexjoin::PROBE_COPIES.with(std::cell::Cell::get);
        let short = plan::BRANCH_SHORTFALL.with(std::cell::Cell::get);
        println!(
            "graph binding, forced {force}: {excess:?} merge/hash/index/other joins {of:?}, \
             {offers} index candidates, {copies} probe filter copies, branch short {short}"
        );
        assert_eq!(
            short, 0,
            "forced {force}: the branch keeps unadmitted bytes"
        );
        assert!(
            copies > 0,
            "forced {force}: the offer copies the probe's filter"
        );
        if force {
            assert!(
                offers > 0,
                "the binding's index candidate is built when forced"
            );
        }
        assert!(
            excess[7] <= 512,
            "forced {force}: {} unadmitted bytes in the graph binding's candidates",
            excess[7]
        );
    }
}

#[test]
fn checked_graph_r12_refuses_graph_binding_index_candidate() {
    use crate::sparql::plan::GRAPH_INDEX_PHASE;
    let _force = ForceIndexJoin::set(true);
    let store = checked_chain_fixture(4);
    let text = checked_graph_binding_query();
    // The filter's list, which the candidate copies after the phase.
    let size = 101 * std::mem::size_of::<crate::sparql::expr::Expr>();
    let (outcome, rows, events, _, seen, allocated) =
        checked_join_order_at_phase(&store, &text, GRAPH_INDEX_PHASE, 0, false, false, size);
    outcome.unwrap();
    assert!(events > 0, "the phase is reached");
    assert!(
        allocated > seen,
        "the candidate copies the list after the phase"
    );
    let _ = rows;
    for (cancel, fill) in [(true, false), (false, true)] {
        let (outcome, rows, _, _, seen, allocated) =
            checked_join_order_at_phase(&store, &text, GRAPH_INDEX_PHASE, 0, cancel, fill, size);
        assert!(outcome.is_err(), "cancel {cancel}: the query stops");
        assert_eq!(rows, 0, "cancel {cancel}: no rows");
        assert_eq!(
            allocated, seen,
            "cancel {cancel}: no copy after the refusal"
        );
    }
}
