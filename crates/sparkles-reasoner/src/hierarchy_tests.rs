//! Behavioral preimages for item 675. Initially these exercise the independent
//! numeric budgets; the implementation attaches their actual owning parents.
use crate::budget::{Budget, BudgetFailure};
use crate::graph::Graph;
use crate::incremental::Closure;
use crate::terms::Terms;
use sparkles_core::sparql::{Ctx, pool::QueryPool};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::Arc;

const MIB: usize = 1 << 20;

#[test]
fn executor_and_reasoner_share_the_query_local_limit() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ctx = Ctx::new(store.snapshot());
    ctx.mem_limit = (8 * MIB) as u64;
    let executor = ctx.charge((5 * MIB) as u64).unwrap();
    let reasoning = Budget::with_parent(Some(8 * MIB), Some(ctx.memory_parent().unwrap()));
    let result = reasoning.try_charge(5 * MIB);
    assert!(
        result.is_err(),
        "executor5MiB plus reasoning5MiB both admitted under local8MiB; reasoning used {}",
        reasoning.used()
    );
    assert!(matches!(
        result,
        Err(BudgetFailure::Parent {
            refusal: sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Local,
                ..
            },
            ..
        })
    ));
    assert_eq!(reasoning.used(), 0);
    drop(executor);
}

#[test]
fn reasoner_parent_refusal_is_server_memory_before_allocation() {
    let pool = QueryPool::new((8 * MIB) as u64);
    let executor = pool.empty_grant();
    executor.charge((5 * MIB) as u64).unwrap();
    // Local 16 MiB permits the second 5 MiB; only aggregate Q=8 MiB can refuse.
    let reasoning = Budget::with_parent(Some(16 * MIB), Some(pool.work_parent()));
    let before = (executor.used(), executor.granted(), pool.granted());
    let result = reasoning.try_charge(5 * MIB);
    assert!(
        result.is_err(),
        "independent reasoning admitted outside Q8MiB; original grant usage/reservation {:?}",
        before
    );
    assert!(matches!(
        result,
        Err(BudgetFailure::Parent {
            refusal: sparkles_budget::ParentRefused::Limit {
                scope: sparkles_budget::Scope::Work,
                ..
            },
            ..
        })
    ));
    assert_eq!(reasoning.used(), 0);
    assert_eq!(
        (executor.used(), executor.granted(), pool.granted()),
        before
    );
    drop(executor);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn failed_closure_budget_migration_preserves_all_old_owners() {
    let store = Store::in_memory(StoreOptions::default());
    let old = Budget::unlimited();
    let mut graph = Graph::with_capacity(4);
    graph.set_budget(&old).unwrap();
    let terms = Terms::new(store.snapshot());
    terms.set_budget(&old).unwrap();
    terms
        .try_id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(128 << 10),
        )))
        .unwrap();
    let mut closure = Closure::new(graph, terms).unwrap();
    let own = closure.heap_bytes() - closure.graph.heap_bytes() - closure.terms.heap_bytes();
    let destination = Budget::new(Some(own + closure.graph.heap_bytes()));
    let before = (old.used(), destination.used());
    assert!(closure.set_budget(&destination).is_err());
    assert!(
        Arc::ptr_eq(closure.graph.budget(), &old),
        "refused migration already changed graph owner"
    );
    assert!(Arc::ptr_eq(&closure.terms.budget(), &old));
    assert_eq!((old.used(), destination.used()), before);
}

#[test]
fn initial_term_parent_refusal_leaves_no_live_credit() {
    use sparkles_budget::{MemoryDomain, ParentRefused, Scope};
    let store = Store::in_memory(StoreOptions::default());
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        |requested| {
            Err(ParentRefused::Limit {
                limit: 0,
                requested,
                scope: Scope::Work,
            })
        },
        |_| panic!("refusal must not release unowned credit"),
    );
    let budget = Budget::with_parent(None, Some(parent));
    let error: anyhow::Error = Terms::new_in(store.snapshot(), &budget)
        .err()
        .unwrap()
        .into();
    assert!(matches!(
        error.downcast_ref::<sparkles_core::Error>(),
        Some(sparkles_core::Error::BudgetExceeded(
            sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            }
        ))
    ));
    assert_eq!((budget.used(), budget.peak()), (0, 0));
}

#[test]
fn term_payloads_drop_before_parent_credit() {
    use sparkles_budget::MemoryDomain;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    let store = Store::in_memory(StoreOptions::default());
    let snap = Arc::new((*store.snapshot()).clone());
    let weak = Arc::downgrade(&snap);
    let retiring = Arc::new(AtomicBool::new(false));
    let check = retiring.clone();
    let owned = Arc::new(AtomicU64::new(0));
    let take = owned.clone();
    let give = owned.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            if check.load(Ordering::SeqCst) {
                assert!(
                    weak.upgrade().is_none(),
                    "term owner returned credit before payload fields dropped"
                );
            }
            give.fetch_sub(n, Ordering::SeqCst);
        },
    );
    let budget = Budget::with_parent(None, Some(parent));
    let terms = Terms::new_in(snap, &budget).unwrap();
    terms
        .try_id_for(&oxrdf::Term::Literal(oxrdf::Literal::new_simple_literal(
            "x".repeat(2 << 20),
        )))
        .unwrap();
    retiring.store(true, Ordering::SeqCst);
    drop(terms);
    assert_eq!(budget.used(), 0);
    drop(budget);
    assert_eq!(owned.load(Ordering::SeqCst), 0);
}

/// A key or skolem buffer that moves into the term table is charged once: the
/// caller's temporary credit becomes part of the permanent charge, so interning
/// leaves no transient peak of a second key above what it keeps.
#[test]
fn interned_buffers_are_charged_once() {
    let store = Store::in_memory(StoreOptions::default());
    let terms = Terms::new(store.snapshot());
    let budget = Budget::unlimited();
    terms.set_budget(&budget).unwrap();
    // A first small key takes the map's and table's first growth.
    terms.try_id_for_key(b"\"warm").unwrap();
    const KEY: usize = 1 << 16;
    let mut key = vec![b'x'; KEY];
    key[0] = b'"';
    let before = budget.used();
    let id = terms.try_id_for_key(&key).unwrap();
    let kept = budget.used() - before;
    assert!(
        kept >= 2 * KEY,
        "the map key and the table copy are kept: {kept}"
    );
    assert!(
        budget.peak() < budget.used() + KEY / 2,
        "the moved key was charged twice: peak {} used {}",
        budget.peak(),
        budget.used()
    );
    // Found again, the lookup releases its temporary and keeps nothing.
    let used = budget.used();
    assert_eq!(terms.try_id_for_key(&key).unwrap(), id);
    assert_eq!(budget.used(), used);

    let args = vec![7u64; KEY / 8];
    let used = budget.used();
    terms.try_skolem(&args).unwrap();
    assert!(budget.used() - used >= KEY);
    assert!(
        budget.peak() < budget.used() + KEY / 2,
        "the moved skolem key was charged twice: peak {} used {}",
        budget.peak(),
        budget.used()
    );
}

fn load_turtle(store: &Store, ttl: &str) {
    use sparkles_core::io::{RdfFormat, Source};
    store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
}

fn is_server_memory_anyhow(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<sparkles_core::Error>(),
        Some(sparkles_core::Error::BudgetExceeded(
            sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            }
        ))
    )
}

#[test]
fn materialize_and_cache_kept_return_work_pool_credit_to_zero() {
    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:B rdfs:subClassOf ex:C .
         ex:x a ex:A .",
    );

    // 1. Unpooled Cache::kept (server default when C = 0): finishing a run returns
    // pool.granted() to exact 0, and incremental updates take_for cleanly and return to 0.
    let pool = QueryPool::new((8 * MIB) as u64);
    let cache = crate::Cache::new(10_000);
    let opts = crate::ReasonOptions {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let r1 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &opts,
    )
    .unwrap();
    assert!(cache.kept().is_some());
    assert_eq!(pool.granted(), 0, "kept closure must not pin work_parent");

    // Second incremental run takes the kept closure and returns to 0 again.
    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:y a ex:A .");
    let opts2 = crate::ReasonOptions {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let r2 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: r1.receipt.map(|r| r.commit.seq),
            cache: Some(&cache),
        },
        &opts2,
    )
    .unwrap();
    assert_eq!(r2.method, crate::Method::Incremental);
    assert_eq!(pool.granted(), 0);
    cache.clear();
    assert_eq!(pool.granted(), 0);

    // 2. Cache with retained_parent on a pool with retained capacity C > 0:
    // work_reserved returns to 0, retained_reserved > 0 until cache.clear().
    let rpool = QueryPool::with_retained_capacity((8 * MIB) as u64, (4 * MIB) as u64).unwrap();
    let rcache = crate::Cache::new(10_000).with_retained_parent(Some(rpool.retained_parent()));
    let ropts = crate::ReasonOptions {
        memory_parent: Some(rpool.work_parent()),
        ..Default::default()
    };
    crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&rcache),
        },
        &ropts,
    )
    .unwrap();
    let stats = rpool.domain_stats();
    assert_eq!(stats.work_reserved, 0);
    assert!(stats.retained_reserved > 0);
    rcache.clear();
    let after = rpool.domain_stats();
    assert_eq!((after.work_reserved, after.retained_reserved), (0, 0));

    // 3. When retained capacity C is full, materialization still succeeds, closure is not kept,
    // and both work_reserved and retained_reserved are 0.
    let small_ret = QueryPool::with_retained_capacity((8 * MIB) as u64, 64).unwrap();
    let small_cache =
        crate::Cache::new(10_000).with_retained_parent(Some(small_ret.retained_parent()));
    let sopts = crate::ReasonOptions {
        memory_parent: Some(small_ret.work_parent()),
        ..Default::default()
    };
    crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&small_cache),
        },
        &sopts,
    )
    .unwrap();
    assert!(small_cache.kept().is_none());
    let stats = small_ret.domain_stats();
    assert_eq!((stats.work_reserved, stats.retained_reserved), (0, 0));

    // 4. Tight work pool refuses materialization with ServerMemory and returns credit to 0.
    let tight = QueryPool::new(1024);
    let topts = crate::ReasonOptions {
        max_memory: Some(16 * MIB),
        memory_parent: Some(tight.work_parent()),
        ..Default::default()
    };
    let err = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental::default(),
        &topts,
    )
    .unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(tight.granted(), 0);
}

#[test]
fn shacl_af_materialize_respects_parent_pool_and_returns_credit() {
    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix sh: <http://www.w3.org/ns/shacl#> .
         ex:x a ex:Person .
         ex:PersonShape a sh:NodeShape ;
             sh:targetClass ex:Person ;
             sh:rule [
                 a sh:TripleRule ;
                 sh:subject sh:this ;
                 sh:predicate ex:tagged ;
                 sh:object true
             ] .",
    );
    let profile = crate::Profile::ShaclAf(crate::shacl_af::ShaclAf::default());

    let pool = QueryPool::new((8 * MIB) as u64);
    let opts = crate::ReasonOptions {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let report = crate::materialize(&store, &profile, &opts).unwrap();
    assert!(report.inferred >= 1);
    assert_eq!(pool.granted(), 0);

    let tight = QueryPool::new(512);
    let topts = crate::ReasonOptions {
        max_memory: Some(16 * MIB),
        memory_parent: Some(tight.work_parent()),
        ..Default::default()
    };
    let err = crate::materialize(&store, &profile, &topts).unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(tight.granted(), 0);
}

#[test]
fn demand_session_and_ql_rewrite_respect_parent_pool_and_return_credit() {
    use sparkles_core::sparql::reasoning::{Demand, DemandTerm, ReasoningProvider, SessionRequest};

    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:x a ex:A .",
    );

    // Demand session via QueryReasoner::with_pool
    let pool = QueryPool::new((64 * MIB) as u64);
    let reasoner = crate::query::QueryReasoner::new(
        crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap()
    .with_pool(pool.clone());
    let mut session = reasoner
        .start(SessionRequest {
            snap: store.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: None,
            memory_parent: None,
            no_cache: true,
        })
        .unwrap();
    let demand = Demand {
        s: DemandTerm::Var,
        p: DemandTerm::Const(oxrdf::Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned())),
        o: DemandTerm::Const(oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            "http://ex.org/B",
        ))),
    };
    session.prepare(&[demand], false).unwrap();
    let derived = session.derive(vec![(0, Vec::new())]).unwrap();
    assert!(!derived.facts.is_empty());
    drop(session);
    assert_eq!(pool.granted(), 0);

    // Tight pool refusal on demand session -> ServerMemory + exact 0 credit return
    let tight = QueryPool::new(1024);
    let tight_reasoner = crate::query::QueryReasoner::new(
        crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap()
    .with_pool(tight.clone());
    let err = tight_reasoner
        .start(SessionRequest {
            snap: store.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: Some((16 * MIB) as u64),
            memory_parent: None,
            no_cache: true,
        })
        .err()
        .expect("tight pool must refuse demand session");
    assert!(
        matches!(
            err,
            sparkles_core::Error::BudgetExceeded(sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            })
        ),
        "unexpected error: {err:?}"
    );
    assert_eq!(tight.granted(), 0);

    // OWL 2 QL TBox + rewrite_query_with_parent
    crate::qlrewrite::clear();
    let ql = crate::query::QueryReasoner::new(
        crate::Profile::OwlQl,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap()
    .with_pool(pool.clone());
    assert!(ql.rewrites());
    let parsed = spargebra::SparqlParser::new()
        .parse_query("SELECT ?x WHERE { ?x a <http://ex.org/B> }")
        .unwrap();
    let req = SessionRequest {
        snap: store.snapshot(),
        readable: None,
        stop: Arc::new(|| Ok(())),
        max_memory: None,
        memory_parent: None,
        no_cache: true,
    };
    let rewritten = ql.rewrite(&parsed, &[], &req).unwrap();
    assert!(rewritten.is_some());
    assert_eq!(pool.granted(), 0);

    // Tight pool refuses OWL 2 QL TBox read with ServerMemory and returns credit to 0.
    let err = ql
        .owl_ql_tbox(&SessionRequest {
            snap: store.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: Some((16 * MIB) as u64),
            memory_parent: Some(tight.work_parent()),
            no_cache: true,
        })
        .unwrap_err();
    assert!(
        matches!(
            err,
            sparkles_core::Error::BudgetExceeded(sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            })
        ),
        "unexpected error: {err:?}"
    );
    assert_eq!(tight.granted(), 0);
}

#[test]
fn explain_whynot_and_analysis_respect_parent_pool_and_return_credit() {
    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:x a ex:A .",
    );
    crate::materialize(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::ReasonOptions::default(),
    )
    .unwrap();

    let snap = store.snapshot();
    let inferred = snap.lookup_iri(crate::INFERRED_GRAPH);
    let triple = [
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/x")),
        oxrdf::Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()),
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/B")),
    ];

    let pool = QueryPool::new((64 * MIB) as u64);
    let input = crate::explain::ExplainInput {
        snap: snap.clone(),
        profile: &crate::Profile::RdfsSimple,
        extras: &crate::Extras::default(),
        inputs: &[sparkles_core::id::Id::DEFAULT_GRAPH],
        inferred,
    };
    let opts = crate::explain::ExplainOptions {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let explained = crate::explain::explain(&input, &triple, &opts).unwrap();
    assert!(matches!(explained, crate::explain::Explained::Found { .. }));
    assert_eq!(pool.granted(), 0);

    // Demand explain & why-not under parent pool
    let reasoner = crate::query::QueryReasoner::new(
        crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap();
    let d_input = crate::explain::DemandInput {
        reasoner: &reasoner,
        snap: snap.clone(),
        readable: None,
    };
    let explained = crate::explain::explain_demand(&d_input, &triple, &opts).unwrap();
    assert!(matches!(explained, crate::explain::Explained::Found { .. }));
    assert_eq!(pool.granted(), 0);

    let missing = [
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/z")),
        oxrdf::Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned()),
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked("http://ex.org/B")),
    ];
    let wn = crate::whynot::why_not(
        &d_input,
        &missing,
        &crate::whynot::WhyNotOptions {
            explain: opts,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(wn, crate::whynot::WhyNot::NotEntailed { .. }));
    assert_eq!(pool.granted(), 0);

    // Tight pool refusal on explain preserves ServerMemory (not ExplainBudgetExceeded)
    let tight = QueryPool::new(1024);
    let topts = crate::explain::ExplainOptions {
        max_memory: Some(16 * MIB),
        memory_parent: Some(tight.work_parent()),
        ..Default::default()
    };
    let err = crate::explain::explain(&input, &triple, &topts).unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert!(
        err.downcast_ref::<crate::explain::ExplainBudgetExceeded>()
            .is_none(),
        "ServerMemory must not be collapsed into ExplainBudgetExceeded"
    );
    assert_eq!(tight.granted(), 0);

    // WitnessMap + AnalysisContext inherit parent pool and return credit on drop
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let witnesses = crate::analysis::WitnessMap::with_memory_parent(
        1,
        Some(16 * MIB),
        Some(pool.work_parent()),
        cancel.clone(),
        None,
    );
    let ctx = crate::analysis::AnalysisContext::new(witnesses.clone(), Some(16 * MIB), "p".into())
        .unwrap();
    ctx.witness(vec!["a".into(), "b".into()]).unwrap();
    assert!(pool.granted() > 0);
    drop(ctx);
    drop(witnesses);
    assert_eq!(pool.granted(), 0);

    // Tight pool refuses WitnessMap/AnalysisContext with ServerMemory
    let tight_w = crate::analysis::WitnessMap::with_memory_parent(
        1,
        Some(16 * MIB),
        Some(tight.work_parent()),
        cancel,
        None,
    );
    let err = crate::analysis::AnalysisContext::new(tight_w, Some(16 * MIB), "x".repeat(4096))
        .unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(tight.granted(), 0);

    // Controlled literal typings under pool returns credit to 0
    let controls = crate::diagnostics::DiagnosticControls {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let _ = crate::literal_typings_controlled(&store, snap.commit, None, &controls).unwrap();
    assert_eq!(pool.granted(), 0);
}

#[test]
fn session_payloads_drop_before_parent_credit() {
    use sparkles_budget::MemoryDomain;
    use sparkles_core::sparql::reasoning::{ReasoningProvider, SessionRequest};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    let store = Store::in_memory(StoreOptions::default());
    let snap = Arc::new((*store.snapshot()).clone());
    let weak = Arc::downgrade(&snap);
    let retiring = Arc::new(AtomicBool::new(false));
    let check = retiring.clone();
    let owned = Arc::new(AtomicU64::new(0));
    let take = owned.clone();
    let give = owned.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            let prev = give.fetch_sub(n, Ordering::SeqCst);
            if check.load(Ordering::SeqCst) && prev == n {
                assert!(
                    weak.upgrade().is_none(),
                    "session returned final parent credit before snap/payloads dropped"
                );
            }
        },
    );

    let reasoner = crate::query::QueryReasoner::new(
        crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap();
    let session = reasoner
        .start(SessionRequest {
            snap,
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: None,
            memory_parent: Some(parent),
            no_cache: true,
        })
        .unwrap();
    assert!(owned.load(Ordering::SeqCst) > 0);
    retiring.store(true, Ordering::SeqCst);
    drop(session);
    assert_eq!(owned.load(Ordering::SeqCst), 0);
}
