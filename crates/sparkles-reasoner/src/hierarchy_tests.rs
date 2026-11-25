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

#[test]
fn cached_diagnostic_summary_releases_credit_outside_cache_lock() {
    use sparkles_budget::MemoryDomain;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

    let store = Store::in_memory(StoreOptions::default());
    // 256 KiB literal: term_controlled charges key.len() * 8 + 256 (> 2 MiB),
    // exceeding PARENT_SLACK + PARENT_STEP so Budget::trim() fires on drop.
    let big = "z".repeat(256 * 1024);
    load_turtle(&store, &format!("<urn:item> <urn:value> \"{big}\" ."));
    let cache = Arc::new(crate::Cache::default());
    let profile = crate::Profile::Rules(
        "[type: (?x <urn:value> ?v) -> (?v <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]"
            .into(),
    );
    crate::materialize_incremental(
        &store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions::default(),
    )
    .unwrap();
    assert!(cache.kept().is_some());

    let owned = Arc::new(AtomicU64::new(0));
    let releases = Arc::new(AtomicUsize::new(0));
    let cleared = Arc::new(AtomicBool::new(false));
    let take = owned.clone();
    let give = owned.clone();
    let rel_count = releases.clone();
    let cache_cb = cache.clone();
    let did_clear = cleared.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            give.fetch_sub(n, Ordering::SeqCst);
            rel_count.fetch_add(1, Ordering::SeqCst);
            // Must not deadlock: cache.kept mutex is unlocked during decode & release.
            let _ = cache_cb.kept();
            if !did_clear.swap(true, Ordering::SeqCst) {
                cache_cb.clear();
            }
        },
    );

    let controls = crate::diagnostics::DiagnosticControls {
        memory_parent: Some(parent),
        ..Default::default()
    };
    let typings =
        crate::literal_typings_controlled(&store, store.snapshot().commit, Some(&cache), &controls)
            .unwrap()
            .expect("cached summary");
    assert_eq!(typings.facts.len(), 1);
    assert!(
        releases.load(Ordering::SeqCst) >= 1,
        "expected Budget::trim() release callback to fire during decode"
    );
    assert!(
        cleared.load(Ordering::SeqCst) && cache.kept().is_none(),
        "release callback must be able to call cache.clear() without deadlock"
    );
    assert!(
        owned.load(Ordering::SeqCst) > 0,
        "returned LiteralTypings must retain credit until dropped"
    );
    drop(typings);
    assert_eq!(owned.load(Ordering::SeqCst), 0);
}

#[test]
fn cold_incremental_rules_memory_refusal_does_not_poison_cache() {
    use sparkles_budget::{MemoryDomain, ParentRefused, Scope};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         ex:a ex:edge ex:b .",
    );
    // Unique relation program so its digest is cold in INCREMENTAL_RULES.
    let profile = crate::Profile::Rules(
        "[r1: (?x <http://ex.org/edge> ?y) -> rel_cold_675_poison(?x, ?y)]
         [r2: rel_cold_675_poison(?x, ?y) -> (?x <http://ex.org/reach> ?y)]"
            .into(),
    );
    let cache = crate::Cache::default();
    let pool = QueryPool::new((16 * MIB) as u64);
    let r1 = crate::materialize_incremental(
        &store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(pool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let c1 = r1.receipt.expect("commit receipt").commit.seq;
    assert_eq!(pool.granted(), 0);

    // Trigger incremental update with a parent that lets Terms::new_in succeed on the
    // first reservation chunk, then refuses subsequent growth inside RunRules::new.
    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:b ex:edge ex:c .");
    let domain = MemoryDomain::new();
    let reserves = Arc::new(AtomicUsize::new(0));
    let owned = Arc::new(AtomicU64::new(0));
    let res_n = reserves.clone();
    let take = owned.clone();
    let give = owned.clone();
    // Allow only 1 byte past Terms' fixed initial reservation, refusing lowering/relation_part.
    let parent = domain.parent(
        move |n| {
            let call = res_n.fetch_add(1, Ordering::SeqCst);
            if call >= 1 {
                return Err(ParentRefused::Limit {
                    limit: 64 * 1024,
                    requested: n,
                    scope: Scope::Work,
                });
            }
            take.fetch_add(n, Ordering::SeqCst);
            Ok(())
        },
        move |n| {
            give.fetch_sub(n, Ordering::SeqCst);
        },
    );
    let err = crate::materialize_incremental(
        &store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            max_memory: Some(16 * MIB),
            memory_parent: Some(parent),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(owned.load(Ordering::SeqCst), 0);

    // Retry the exact same digest with ample memory: must remain incrementally eligible.
    let r2 = crate::materialize_incremental(
        &store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(pool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        r2.method,
        crate::Method::Incremental,
        "transient memory refusal must not poison incremental rule capability"
    );
    assert_eq!(pool.granted(), 0);
}

#[test]
fn owl_ql_tbox_hit_combined_budget_and_rewritten_guard_stay_charged() {
    use sparkles_core::sparql::reasoning::{ReasoningProvider, SessionRequest};

    let store = Store::in_memory(StoreOptions::default());
    let mut onto = String::from(
        "@prefix ex: <http://ex.org/> .
         @prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A a owl:Class .
         ex:p a owl:ObjectProperty .
         ex:A rdfs:subClassOf [ a owl:Restriction ; owl:onProperty ex:p ; owl:someValuesFrom owl:Thing ] .\n",
    );
    for i in 0..80 {
        onto.push_str(&format!(
            "ex:SubClassLongName_{i:03}_{} a owl:Class ; rdfs:subClassOf ex:A .\n",
            "x".repeat(400)
        ));
    }
    load_turtle(&store, &onto);

    let pool = QueryPool::new((64 * MIB) as u64);
    let ql = crate::query::QueryReasoner::new(
        crate::Profile::OwlQl,
        &crate::Extras::default(),
        crate::Inputs::default(),
    )
    .unwrap()
    .with_pool(pool.clone());

    // 1. Warm the TBOXES cache (no_cache: false), then verify cache hit still admits budget.
    crate::qlrewrite::clear();
    let warm_req = SessionRequest {
        snap: store.snapshot(),
        readable: None,
        stop: Arc::new(|| Ok(())),
        max_memory: Some((16 * MIB) as u64),
        memory_parent: Some(pool.work_parent()),
        no_cache: false,
    };
    let tbox = ql.owl_ql_tbox(&warm_req).unwrap().expect("ql tbox");
    assert_eq!(pool.granted(), 0);
    drop(tbox);

    let tight_hit = QueryPool::new(1024);
    let hit_err = ql
        .owl_ql_tbox(&SessionRequest {
            snap: store.snapshot(),
            readable: None,
            stop: Arc::new(|| Ok(())),
            max_memory: Some((16 * MIB) as u64),
            memory_parent: Some(tight_hit.work_parent()),
            no_cache: false,
        })
        .unwrap_err();
    assert!(
        matches!(
            hit_err,
            sparkles_core::Error::BudgetExceeded(sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            })
        ),
        "warmed TBox cache hit must still be admitted: {hit_err:?}"
    );
    assert_eq!(tight_hit.granted(), 0);

    // 2. Rewritten with non-zero copied algebra retains pool credit until Rewritten drops.
    let long = "v".repeat(64 * 1024);
    let parsed = spargebra::SparqlParser::new()
        .parse_query(&format!(
            "PREFIX ex: <http://ex.org/> ASK {{ ?{long} ex:p ?o . ex:a ex:p ?o }}"
        ))
        .unwrap();
    let rw_req = SessionRequest {
        snap: store.snapshot(),
        readable: None,
        stop: Arc::new(|| Ok(())),
        max_memory: Some((16 * MIB) as u64),
        memory_parent: Some(pool.work_parent()),
        no_cache: false,
    };
    let rewritten = ql
        .rewrite(&parsed, &[], &rw_req)
        .unwrap()
        .expect("rewritten");
    let live_copies = pool.granted();
    assert!(
        live_copies > 0,
        "Rewritten must keep copies_held charged while alive"
    );
    drop(rewritten);
    assert_eq!(pool.granted(), 0);

    // 3. Cold TBox + rewrite combined live bytes exceeding budget must refuse.
    // TBox (~70 KiB) fits alone and copies_held (~65 KiB) fits alone under 110 KiB,
    // while both live simultaneously exceed 110 KiB.
    crate::qlrewrite::clear();
    let combined_cap = 110 * 1024u64;
    let combined_pool = QueryPool::new(combined_cap);
    let combined_err = ql
        .rewrite(
            &parsed,
            &[],
            &SessionRequest {
                snap: store.snapshot(),
                readable: None,
                stop: Arc::new(|| Ok(())),
                max_memory: Some((16 * MIB) as u64),
                memory_parent: Some(combined_pool.work_parent()),
                no_cache: true,
            },
        )
        .unwrap_err();
    assert!(
        matches!(
            combined_err,
            sparkles_core::Error::BudgetExceeded(sparkles_core::Budget {
                kind: sparkles_core::BudgetKind::ServerMemory,
                ..
            })
        ),
        "combined cold TBox + rewrite must refuse under tight pool: {combined_err:?}"
    );
    assert_eq!(combined_pool.granted(), 0);
}

#[test]
fn retained_cache_replacement_avoids_double_peak_and_clears_stale() {
    let store = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:B rdfs:subClassOf ex:C .\n",
    );
    for i in 0..500 {
        ttl.push_str(&format!("ex:x{i} a ex:A .\n"));
    }
    load_turtle(&store, &ttl);

    // Measure single closure retained reservation.
    let probe_pool =
        QueryPool::with_retained_capacity((16 * MIB) as u64, (8 * MIB) as u64).unwrap();
    let probe_cache =
        crate::Cache::new(100_000).with_retained_parent(Some(probe_pool.retained_parent()));
    crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&probe_cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(probe_pool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let one_retained = probe_pool.domain_stats().retained_reserved;
    assert!(one_retained > 0);
    probe_cache.clear();

    // Retained capacity fits 1 closure (+ 1 step margin) but strictly less than 2 closures.
    let cap = one_retained + (one_retained / 2).max(64 * 1024);
    assert!(cap < one_retained * 2);
    let rpool = QueryPool::with_retained_capacity((16 * MIB) as u64, cap).unwrap();
    let rcache = crate::Cache::new(100_000).with_retained_parent(Some(rpool.retained_parent()));

    let r1 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&rcache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(rpool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let c1 = r1.receipt.unwrap().commit.seq;
    assert_eq!(rcache.kept().unwrap().1, c1);

    // Second consecutive full materialization (since: None) replaces the kept closure
    // without hitting a 2x retained-pool peak.
    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:extra a ex:A .");
    let r2 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&rcache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(rpool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let c2 = r2.receipt.unwrap().commit.seq;
    assert_ne!(c1, c2);
    assert_eq!(
        rcache.kept().map(|(_, seq)| seq),
        Some(c2),
        "second materialization must replace stale kept closure within 1x retained capacity"
    );
    let stats = rpool.domain_stats();
    assert_eq!(stats.work_reserved, 0);
    assert!(stats.retained_reserved > 0 && stats.retained_reserved <= cap);
    rcache.clear();
    let after = rpool.domain_stats();
    assert_eq!((after.work_reserved, after.retained_reserved), (0, 0));
}

#[test]
fn take_for_uses_single_reservation_and_preserves_generalized_on_refusal() {
    use sparkles_budget::MemoryDomain;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    // Part A: Large kept closure (> PARENT_SLACK + PARENT_STEP = 1.06 MiB) taken via
    // take_for must never release its prepared reservation back to 0 and re-reserve.
    let store = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from(
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:B rdfs:subClassOf ex:C .
         ex:C rdfs:subClassOf ex:D .\n",
    );
    for i in 0..4_500 {
        ttl.push_str(&format!("ex:n{i} a ex:A .\n"));
    }
    load_turtle(&store, &ttl);
    let cache = crate::Cache::new(100_000);
    let r1 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions::default(),
    )
    .unwrap();
    let c1 = r1.receipt.unwrap().commit.seq;
    assert!(cache.kept().is_some());

    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:n_extra a ex:A .");
    let owned = Arc::new(AtomicU64::new(0));
    let saw_large = Arc::new(AtomicBool::new(false));
    let bounced_to_zero = Arc::new(AtomicBool::new(false));
    let take = owned.clone();
    let give = owned.clone();
    let large_take = saw_large.clone();
    let large_give = saw_large.clone();
    let bounced_take = bounced_to_zero.clone();
    let dropped_to_zero = Arc::new(AtomicBool::new(false));
    let dz_take = dropped_to_zero.clone();
    let dz_give = dropped_to_zero.clone();
    let domain = MemoryDomain::new();
    let parent = domain.parent(
        move |n| {
            let next = take.fetch_add(n, Ordering::SeqCst) + n;
            if next > (1 << 20) + (64 << 10) {
                large_take.store(true, Ordering::SeqCst);
            }
            if dz_take.load(Ordering::SeqCst) {
                bounced_take.store(true, Ordering::SeqCst);
            }
            Ok(())
        },
        move |n| {
            let prev = give.fetch_sub(n, Ordering::SeqCst);
            if prev == n && large_give.load(Ordering::SeqCst) {
                dz_give.store(true, Ordering::SeqCst);
            }
        },
    );
    let r2 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(parent),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r2.method, crate::Method::Incremental);
    assert!(
        saw_large.load(Ordering::SeqCst),
        "closure should exceed slack threshold"
    );
    assert!(
        !bounced_to_zero.load(Ordering::SeqCst),
        "take_for must not drop a probe reservation to 0 and re-reserve"
    );
    assert_eq!(owned.load(Ordering::SeqCst), 0);

    // Part B: Admission refusal with nonempty generalized facts leaves the kept closure
    // in Cache intact instead of losing it inside `previous`.
    let gstore = Store::in_memory(StoreOptions::default());
    let mut gttl = String::new();
    for i in 0..400 {
        gttl.push_str(&format!("<urn:item{i}> <urn:val> \"lit{i}\" .\n"));
    }
    load_turtle(&gstore, &gttl);
    let gprofile = crate::Profile::Rules(
        "[g: (?x <urn:val> ?v) -> (?v <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]"
            .into(),
    );
    let gcache = crate::Cache::new(100_000);
    let gr1 = crate::materialize_incremental(
        &gstore,
        &gprofile,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&gcache),
        },
        &crate::ReasonOptions::default(),
    )
    .unwrap();
    let gc1 = gr1.receipt.unwrap().commit.seq;
    assert!(gcache.kept().is_some());

    load_turtle(&gstore, "<urn:item_new> <urn:val> \"lit_new\" .");
    // Size the tight pool so incremental_rules succeeds but the full kept heap
    // (closure + generalized facts) is refused during take_for.
    let tight = QueryPool::new(80 * 1024);
    let err = crate::materialize_incremental(
        &gstore,
        &gprofile,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(gc1),
            cache: Some(&gcache),
        },
        &crate::ReasonOptions {
            max_memory: Some(16 * MIB),
            memory_parent: Some(tight.work_parent()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(
        gcache.kept().map(|(_, seq)| seq),
        Some(gc1),
        "kept closure with generalized facts must remain in cache after admission refusal"
    );
    assert_eq!(tight.granted(), 0);
}

#[test]
fn failed_take_for_does_not_restore_after_interleaved_clear() {
    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:x a ex:A .",
    );
    let rpool = QueryPool::with_retained_capacity((16 * MIB) as u64, (4 * MIB) as u64).unwrap();
    let cache =
        Arc::new(crate::Cache::new(10_000).with_retained_parent(Some(rpool.retained_parent())));
    let r1 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(rpool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let c1 = r1.receipt.unwrap().commit.seq;
    // Run one incremental step with ample memory so INCREMENTAL_RULES is warm for RdfsSimple.
    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:y0 a ex:A .");
    let r2 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(rpool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let c1 = r2.receipt.unwrap().commit.seq;
    assert!(cache.kept().is_some());
    assert!(rpool.domain_stats().retained_reserved > 0);

    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:y a ex:A .");
    // Interleave cache.clear() after take_for detaches the entry and before transfer fails.
    let hook_fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook_flag = hook_fired.clone();
    let cache_clear = cache.clone();
    crate::maintain::on_take_detached(move || {
        hook_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        cache_clear.clear();
    });

    // Reserve nearly all of rpool's work capacity so transfer_budget refuses on Scope::Work,
    // while leaving room if `incremental_rules` runs cold before `take_for`.
    let blocker = rpool.empty_grant();
    blocker.charge((16 * MIB - 128) as u64).unwrap();
    let err = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            max_memory: Some(16 * MIB),
            memory_parent: Some(rpool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        hook_fired.load(std::sync::atomic::Ordering::SeqCst),
        "test hook must fire between Cache::take_for detach and transfer_budget"
    );
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert!(
        cache.kept().is_none(),
        "failed take_for must not restore an entry after an interleaved cache.clear()"
    );
    assert_eq!(
        rpool.domain_stats().retained_reserved,
        0,
        "interleaved clear must drop retained credit to 0"
    );
    drop(blocker);
    assert_eq!(rpool.granted(), 0);
}

#[test]
fn literal_typings_retains_credit_across_cached_file_and_analysis() {
    let mut ttl = String::new();
    for i in 0..40 {
        ttl.push_str(&format!(
            "<urn:item{i}> <urn:val> \"not-an-int-{i}-{}\" .\n",
            "x".repeat(256)
        ));
    }
    let profile = crate::Profile::Rules(
        "[t: (?x <urn:val> ?v) -> (?v <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <http://www.w3.org/2001/XMLSchema#integer>)]"
            .into(),
    );

    // 1. AnalysisContext::take_literal_typings (in-memory analysis store) retains pool
    //    credit even after AnalysisContext and WitnessMap drop.
    let mem_store = Store::in_memory(StoreOptions::default());
    load_turtle(&mem_store, &ttl);
    let pool = QueryPool::new((16 * MIB) as u64);
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let witnesses = crate::analysis::WitnessMap::with_memory_parent(
        1,
        Some(16 * MIB),
        Some(pool.work_parent()),
        cancel,
        None,
    );
    let ctx = crate::analysis::AnalysisContext::new(witnesses.clone(), Some(16 * MIB), "p".into())
        .unwrap();
    let mem_report = crate::materialize_incremental(
        &mem_store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental::default(),
        &crate::ReasonOptions {
            memory_parent: Some(pool.work_parent()),
            analysis: Some(ctx.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    let mem_commit = mem_report.receipt.unwrap().commit.seq;
    let analysis_typings = ctx
        .take_literal_typings(mem_commit)
        .expect("analysis typings");
    drop(ctx);
    drop(witnesses);
    assert_eq!(analysis_typings.facts.len(), 40);
    assert!(
        pool.granted() > 0,
        "AnalysisContext::take_literal_typings must retain pool credit"
    );
    drop(analysis_typings);
    assert_eq!(pool.granted(), 0);

    // 2. Cached & file-backed literal_typings_controlled (persistent store) retain pool
    //    credit until dropped, and file-backed refuses under a tight pool near capacity.
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), StoreOptions::default()).unwrap();
    load_turtle(&store, &ttl);
    let cache = crate::Cache::default();
    let report = crate::materialize_incremental(
        &store,
        &profile,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(pool.work_parent()),
            ..Default::default()
        },
    )
    .unwrap();
    let commit = report.receipt.unwrap().commit.seq;
    assert_eq!(pool.granted(), 0);

    let controls = crate::diagnostics::DiagnosticControls {
        memory_parent: Some(pool.work_parent()),
        ..Default::default()
    };
    let cached_typings = crate::literal_typings_controlled(&store, commit, Some(&cache), &controls)
        .unwrap()
        .expect("cached typings");
    assert_eq!(cached_typings.facts.len(), 40);
    assert!(
        pool.granted() > 0,
        "cached literal_typings_controlled must retain pool credit"
    );
    drop(cached_typings);
    assert_eq!(pool.granted(), 0);

    let file_typings = crate::literal_typings_controlled(&store, commit, None, &controls)
        .unwrap()
        .expect("file-backed typings");
    assert_eq!(file_typings.facts.len(), 40);
    assert!(
        pool.granted() > 0,
        "file-backed literal_typings_controlled must retain pool credit"
    );
    drop(file_typings);
    assert_eq!(pool.granted(), 0);

    let tight = QueryPool::new(40 * 1024);
    let tight_controls = crate::diagnostics::DiagnosticControls {
        max_memory: Some((16 * MIB) as u64),
        memory_parent: Some(tight.work_parent()),
        ..Default::default()
    };
    let err = crate::literal_typings_controlled(&store, commit, None, &tight_controls).unwrap_err();
    assert!(is_server_memory_anyhow(&err), "unexpected error: {err:#}");
    assert_eq!(tight.granted(), 0);
}

#[test]
fn cross_domain_transfer_refused_with_foreign_domain() {
    use sparkles_budget::ParentRefused;

    let store = Store::in_memory(StoreOptions::default());
    load_turtle(
        &store,
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         ex:A rdfs:subClassOf ex:B .
         ex:B rdfs:subClassOf ex:C .
         ex:x a ex:A .",
    );

    let pool_a = QueryPool::with_retained_capacity((16 * MIB) as u64, (4 * MIB) as u64).unwrap();
    let pool_b = QueryPool::new((16 * MIB) as u64);
    let cache = crate::Cache::new(10_000).with_retained_parent(Some(pool_a.retained_parent()));
    let r1 = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: None,
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(pool_a.work_parent()),
            witnesses: Some(4),
            ..Default::default()
        },
    )
    .unwrap();
    let c1 = r1.receipt.unwrap().commit.seq;
    let retained_before = pool_a.domain_stats().retained_reserved;
    assert!(retained_before > 0);

    // Attempting an incremental run backed by a distinct QueryPool domain refuses with
    // ForeignDomain, preserves the kept closure in pool_a, and leaves pool_b at 0.
    load_turtle(&store, "@prefix ex: <http://ex.org/> . ex:y a ex:A .");
    let err = crate::materialize_incremental(
        &store,
        &crate::Profile::RdfsSimple,
        &crate::Extras::default(),
        crate::Incremental {
            since: Some(c1),
            cache: Some(&cache),
        },
        &crate::ReasonOptions {
            memory_parent: Some(pool_b.work_parent()),
            witnesses: Some(4),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            err.downcast_ref::<ParentRefused>(),
            Some(ParentRefused::ForeignDomain)
        ),
        "expected ForeignDomain refusal, got: {err:#}"
    );
    assert_eq!(cache.kept().map(|(_, seq)| seq), Some(c1));
    assert_eq!(pool_a.domain_stats().retained_reserved, retained_before);
    assert_eq!(pool_b.granted(), 0);

    // Direct Closure::set_budget & Witnesses::set_budget cross-domain refusal checks.
    {
        let b_a = Budget::with_parent(None, Some(pool_a.work_parent()));
        let b_b = Budget::with_parent(None, Some(pool_b.work_parent()));
        let snap = store.snapshot();
        let base = Arc::new(
            crate::facts::Base::new(snap.clone(), &[sparkles_core::id::Id::DEFAULT_GRAPH]).unwrap(),
        );
        let g = Graph::with_base_in(base, &b_a).unwrap();
        let t = Terms::new_in(snap, &b_a).unwrap();
        let mut closure = Closure::new(g, t).unwrap();
        let err = closure.set_budget(&b_b).unwrap_err();
        assert!(
            matches!(
                err,
                BudgetFailure::Parent {
                    refusal: ParentRefused::ForeignDomain,
                    ..
                }
            ),
            "Closure::set_budget must refuse ForeignDomain: {err:?}"
        );
        assert_eq!(pool_b.granted(), 0);

        let rules =
            crate::profile_rules(&crate::Profile::RdfsSimple, &crate::Extras::default()).unwrap();
        let mut warnings = crate::budget::WorkingWarnings::new(&b_a);
        let compiled = crate::engine::compile(&rules, &closure.terms, &mut warnings).unwrap();
        let limits = crate::engine::Limits {
            max_iterations: 100,
            max_inferred: 10_000,
            cancel: None,
            deadline: None,
            progress: None,
            budget: b_a.clone(),
            closures: false,
            magic: None,
            sampled_planning: false,
        };
        let _ = crate::engine::run(&mut closure.graph, &compiled, &closure.terms, &limits, None)
            .unwrap();
        let mut witnesses =
            crate::witness::Witnesses::build(&mut closure, &compiled, 4, &limits).unwrap();
        assert!(witnesses.heap_bytes() > 0);
        let err = witnesses.set_budget(&b_b).unwrap_err();
        assert!(
            matches!(
                err,
                BudgetFailure::Parent {
                    refusal: ParentRefused::ForeignDomain,
                    ..
                }
            ),
            "Witnesses::set_budget must refuse ForeignDomain: {err:?}"
        );
        assert_eq!(pool_b.granted(), 0);
    }
    cache.clear();
    assert_eq!(pool_a.domain_stats().work_reserved, 0);
    assert_eq!(pool_a.domain_stats().retained_reserved, 0);
}
