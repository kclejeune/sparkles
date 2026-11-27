//! Integration tests for Track 1 of Item 675: reasoner budgets wired into `QueryPool`.
#![cfg(feature = "reasoning")]

use oxrdf::{NamedNode, Term};
use sparkles::io::RdfFormat;
use sparkles::reasoning::ReasonRequest;
use sparkles::reasoning::analysis::AnalysisOptions;
use sparkles::reasoning::auto::AutoReasoner;
use sparkles::reasoning::diff::DiffRequest;
use sparkles::reasoning::explain::{ExplainMode, ExplainRequest, ExplainSource};
use sparkles::reasoning::justify::{JustifyRequest, JustifyTarget};
use sparkles::reasoning::reasoner::Profile;
use sparkles::reasoning::reasoner::diagnostics::DiagnoseOptions;
use sparkles::reasoning::reasoner::query::QueryReasoner;
use sparkles::sparql::QueryOptions;
use sparkles::sparql::pool::QueryPool;
use sparkles::{Budget, BudgetKind, Dataset, DatasetOptions, Error};
use std::sync::Arc;

const MIB: u64 = 1 << 20;

fn pooled_dataset(capacity: u64) -> (tempfile::TempDir, Arc<QueryPool>, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let pool = QueryPool::new(capacity);
    let opts = DatasetOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let ds = Dataset::open_with(dir.path().join("db"), opts).unwrap();
    ds.load_str(
        "@prefix ex: <http://ex.org/> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         @prefix owl: <http://www.w3.org/2002/07/owl#> .
         ex:A rdfs:subClassOf ex:B .
         ex:B rdfs:subClassOf ex:C .
         ex:x a ex:A .",
        RdfFormat::Turtle,
    )
    .unwrap();
    (dir, pool, ds)
}

fn ex(local: &str) -> Term {
    Term::NamedNode(NamedNode::new_unchecked(format!("http://ex.org/{local}")))
}

fn rdf_type() -> Term {
    Term::NamedNode(oxrdf::vocab::rdf::TYPE.into_owned())
}

fn is_server_memory(err: &Error) -> bool {
    matches!(
        err,
        Error::BudgetExceeded(Budget {
            kind: BudgetKind::ServerMemory,
            ..
        })
    )
}

#[test]
fn facade_reasoning_operations_return_exact_pool_credit_to_zero() {
    let (_dir, pool, ds) = pooled_dataset(128 * MIB);

    // 1. Full materialization + Cache::kept
    let req = ReasonRequest {
        profile: Profile::RdfsSimple,
        incremental: true,
        ..Default::default()
    };
    let out1 = ds.reasoning().run(&req).unwrap();
    assert!(out1.report.inferred > 0);
    assert_eq!(pool.granted(), 0, "full materialization must return credit");

    // 2. Incremental materialization reusing Cache::kept
    ds.load_str(
        "@prefix ex: <http://ex.org/> . ex:y a ex:A .",
        RdfFormat::Turtle,
    )
    .unwrap();
    let out2 = ds.reasoning().run(&req).unwrap();
    assert_eq!(
        out2.report.method,
        sparkles::reasoning::reasoner::Method::Incremental
    );
    assert_eq!(
        pool.granted(),
        0,
        "incremental materialization must return credit"
    );

    // 3. Query-time reasoning (demand & auto)
    let qr = ds.query_reasoner(Some(Profile::RdfsSimple)).unwrap();
    let qopts = QueryOptions {
        pool: Some(pool.clone()),
        reasoning: Some(qr.provider()),
        ..Default::default()
    };
    let rows = ds
        .query_with("SELECT ?s WHERE { ?s a <http://ex.org/C> }", &qopts)
        .unwrap();
    assert_eq!(rows.rows().unwrap().len(), 2);
    drop(rows);
    assert_eq!(pool.granted(), 0, "demand query must return credit");

    // auto (reads_all fast path on fresh materialization)
    let auto = Arc::new(AutoReasoner::new(qr, ds.clone(), false));
    let auto_opts = QueryOptions {
        pool: Some(pool.clone()),
        reasoning: Some(auto),
        default_graph_extra: vec![sparkles::reasoning::INFERRED_GRAPH.to_string()],
        ..Default::default()
    };
    let rows = ds
        .query_with("SELECT ?s WHERE { ?s a <http://ex.org/C> }", &auto_opts)
        .unwrap();
    assert_eq!(rows.rows().unwrap().len(), 2);
    drop(rows);
    assert_eq!(pool.granted(), 0, "auto query must return credit");

    // 4. OWL 2 QL query rewriting
    let ql = ds.query_reasoner(Some(Profile::OwlQl)).unwrap();
    let ql_opts = QueryOptions {
        pool: Some(pool.clone()),
        reasoning: Some(ql.provider()),
        no_cache: true,
        ..Default::default()
    };
    let rows = ds
        .query_with("SELECT ?s WHERE { ?s a <http://ex.org/C> }", &ql_opts)
        .unwrap();
    assert_eq!(rows.rows().unwrap().len(), 2);
    drop(rows);
    assert_eq!(pool.granted(), 0, "owl-ql rewrite query must return credit");

    // 5. Explain (materialized, demand, why-not)
    let mut mat_req = ExplainRequest::new([ex("x"), rdf_type(), ex("C")]);
    mat_req.source = Some(ExplainSource::Materialized);
    let mat_exp = ds.reasoning().explain(&mat_req).unwrap();
    assert!(matches!(
        mat_exp.explained,
        sparkles::explain::Explained::Found { .. }
    ));
    assert_eq!(pool.granted(), 0, "materialized explain must return credit");

    let mut dem_req = ExplainRequest::new([ex("x"), rdf_type(), ex("C")]);
    dem_req.source = Some(ExplainSource::Demand);
    let dem_exp = ds.reasoning().explain(&dem_req).unwrap();
    assert!(matches!(
        dem_exp.explained,
        sparkles::explain::Explained::Found { .. }
    ));
    assert_eq!(pool.granted(), 0, "demand explain must return credit");

    let mut wn_req = ExplainRequest::new([ex("missing"), rdf_type(), ex("C")]);
    wn_req.source = Some(ExplainSource::Demand);
    wn_req.mode = ExplainMode::WhyNot;
    let wn_exp = ds.reasoning().explain(&wn_req).unwrap();
    assert!(wn_exp.why_not.is_some());
    assert_eq!(pool.granted(), 0, "why-not explain must return credit");

    // 6. Entailment diff, justify, diagnostics
    let diff = ds
        .reasoning()
        .diff_admitted(&DiffRequest::new("1", "head"))
        .unwrap();
    assert!(diff.added > 0);
    assert!(
        pool.granted() > 0,
        "the live diff report must keep its credit"
    );
    drop(diff);
    assert_eq!(pool.granted(), 0, "dropping the diff must return credit");

    let _just = ds
        .reasoning()
        .justify(&JustifyRequest::new(JustifyTarget::AllClasses))
        .unwrap();
    assert_eq!(pool.granted(), 0, "justify must return credit");

    let _diag = ds
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            inferences: true,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pool.granted(), 0, "diagnostics must return credit");
}

#[test]
fn concurrent_query_and_reasoner_overshoot_refused_with_server_memory() {
    // Pool holds 4 MiB total; a concurrent query reserves all but 1 KiB while each
    // reasoner operation has a generous 16 MiB local cap. Combined overshoot must be
    // refused with BudgetKind::ServerMemory (HTTP 503) and leave the concurrent grant intact.
    let (_dir, pool, ds) = pooled_dataset(4 * MIB);
    // First materialize with full pool so explain/diff also have recorded state.
    ds.reasoning()
        .run(&ReasonRequest {
            profile: Profile::RdfsSimple,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(pool.granted(), 0);

    let concurrent = pool.empty_grant();
    concurrent.charge(4 * MIB - 1024).unwrap();
    let held_before = (concurrent.used(), concurrent.granted(), pool.granted());

    // 1. Materialization refuses with ServerMemory
    let err = ds
        .reasoning()
        .run(&ReasonRequest {
            profile: Profile::RdfsSimple,
            incremental: false,
            max_memory: Some((16 * MIB) as usize),
            ..Default::default()
        })
        .unwrap_err();
    assert!(is_server_memory(&err), "run error: {err:?}");
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    // 2. Demand query refuses with ServerMemory
    let qr: QueryReasoner = ds.query_reasoner(Some(Profile::RdfsSimple)).unwrap();
    let err = ds
        .query_with(
            "SELECT ?s WHERE { ?s a <http://ex.org/C> }",
            &QueryOptions {
                pool: Some(pool.clone()),
                max_memory_bytes: Some(16 * MIB),
                reasoning: Some(qr.provider()),
                no_cache: true,
                ..Default::default()
            },
        )
        .err()
        .expect("demand query must refuse when pool is full");
    assert!(is_server_memory(&err), "demand query error: {err:?}");
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    // 3. Materialized explain refuses with ServerMemory
    let mut exp_req = ExplainRequest::new([ex("x"), rdf_type(), ex("C")]);
    exp_req.source = Some(ExplainSource::Materialized);
    exp_req.max_memory = Some((16 * MIB) as usize);
    let err = ds.reasoning().explain(&exp_req).unwrap_err();
    assert!(is_server_memory(&err), "explain error: {err:?}");
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    // 4. Entailment diff (runs on temporary in-memory `copied` Dataset whose own pool is None)
    // MUST still charge parent `dataset.state().pool` and refuse with ServerMemory!
    let mut diff_req = DiffRequest::new("1", "head");
    diff_req.analysis = AnalysisOptions {
        max_memory: Some((16 * MIB) as usize),
        ..Default::default()
    };
    let err = ds.reasoning().diff(&diff_req).unwrap_err();
    assert!(is_server_memory(&err), "diff error: {err:?}");
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    // 5. Diagnostics and justified diagnostics with `grant: None` MUST still pool
    // their SPARQL check queries and refuse with ServerMemory under contention.
    let err = ds
        .reasoning()
        .diagnostics(&DiagnoseOptions {
            inferences: true,
            ..Default::default()
        })
        .unwrap_err();
    assert!(is_server_memory(&err), "diagnostics error: {err:?}");
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    let err = ds
        .reasoning()
        .diagnostics_justified(
            &DiagnoseOptions {
                inferences: true,
                ..Default::default()
            },
            &AnalysisOptions::default(),
            4,
        )
        .unwrap_err();
    assert!(
        is_server_memory(&err),
        "diagnostics_justified error: {err:?}"
    );
    assert_eq!(
        (concurrent.used(), concurrent.granted(), pool.granted()),
        held_before
    );

    // 6. Dropping the concurrent grant returns pool credit to exact 0.
    drop(concurrent);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn query_reasoning_and_view_share_one_capacity() {
    use sparkles::incr::views::ViewDef;
    use sparkles::task::{Control, Progress};
    use std::time::Duration;

    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    let wait = Duration::from_secs(10);
    ds.views()
        .put("types", ViewDef::table("SELECT ?s ?t WHERE { ?s a ?t }"))
        .unwrap();
    use sparkles::incr::views::ViewState;
    assert_eq!(
        ds.views().wait_built("types", wait).unwrap().state,
        ViewState::Current
    );
    assert!(ds.views().settle_checked(wait));
    let view_bytes = pool.granted();
    assert!(view_bytes > 0, "the maintained view must hold credit");

    let grant = Arc::new(pool.empty_grant());
    let result = ds
        .query_with(
            "CONSTRUCT { ?s a ?t } WHERE { ?s a ?t }",
            &QueryOptions {
                grant: Some(grant.clone()),
                max_memory_bytes: Some(32 * MIB),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.len(), 1);
    let query_bytes = grant.granted();
    assert!(query_bytes > 0);
    let siblings = view_bytes + query_bytes;
    assert_eq!(pool.granted(), siblings);

    let mut ctl = Control::none();
    let cancel = ctl.cancel.clone();
    let probe_pool = pool.clone();
    let probe_grant = grant.clone();
    ctl.progress = Progress::new(move |_, stage| {
        if stage != "reasoning" || cancel.is_cancelled() {
            return;
        }
        let admitted = probe_pool.granted();
        assert!(admitted > siblings, "the active reasoner must hold credit");
        let used = probe_grant.used();
        let slack = query_bytes - used;
        let more = probe_pool.capacity() - admitted + slack + 1;
        let err = probe_grant.charge(more).unwrap_err();
        assert!(is_server_memory(&err), "query growth error: {err:?}");
        assert_eq!(probe_grant.used(), used);
        assert_eq!(probe_pool.granted(), admitted);
        cancel.cancel();
    });
    let err = ds
        .reasoning()
        .run_with(
            &ReasonRequest {
                profile: Profile::RdfsSimple,
                max_memory: Some((32 * MIB) as usize),
                incremental: false,
                ..Default::default()
            },
            &ctl,
        )
        .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err:?}");
    drop(ctl);
    assert_eq!(pool.granted(), siblings);

    drop(result);
    drop(grant);
    assert_eq!(pool.granted(), view_bytes);
    assert!(ds.views().delete("types").unwrap());
    assert!(ds.views().settle_checked(wait));
    assert_eq!(pool.granted(), 0);
}

#[test]
fn owl_ql_status_uses_the_request_pool_and_releases_its_credit() {
    use sparkles::reasoning::QuerySetting;

    let (_dir, dataset_pool, ds) = pooled_dataset(16 * MIB);
    ds.set_query_reasoning(Some(QuerySetting {
        profile: Some("owl-ql".into()),
        ..Default::default()
    }))
    .unwrap();

    let request_pool = QueryPool::new(4 * MIB);
    let opts = QueryOptions {
        pool: Some(request_pool.clone()),
        ..Default::default()
    };
    let full = request_pool
        .work_parent()
        .reserve(request_pool.capacity())
        .unwrap();
    let err = ds.owl_ql_status(&opts).unwrap_err();
    assert!(is_server_memory(&err), "OWL 2 QL status error: {err:?}");
    assert_eq!(request_pool.granted(), full.bytes());
    assert_eq!(dataset_pool.granted(), 0);

    let cancelled = QueryOptions {
        cancel: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        ..opts.clone()
    };
    assert!(matches!(
        ds.owl_ql_status(&cancelled),
        Err(Error::Cancelled)
    ));
    assert_eq!(request_pool.granted(), full.bytes());
    drop(full);
    assert!(ds.owl_ql_status(&opts).unwrap().is_some());
    assert!(matches!(
        ds.owl_ql_status(&cancelled),
        Err(Error::Cancelled)
    ));
    assert_eq!(request_pool.granted(), 0);
    assert_eq!(dataset_pool.granted(), 0);
}

#[test]
fn public_rewritten_and_literal_typings_struct_literals_compile_and_work() {
    use sparkles::reasoning::reasoner::diagnostics::LiteralTypings;
    use sparkles::sparql::reasoning::{
        Demand, DemandInfo, Derived, ReasoningProvider, ReasoningSession, Report, Rewritten,
        SessionRequest,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    // External 2-field LiteralTypings struct literal + Eq.
    let a = LiteralTypings {
        commit: 7,
        facts: Vec::new(),
    };
    let b = LiteralTypings {
        commit: 7,
        facts: Vec::new(),
    };
    assert_eq!(a, b);

    // External 4-field Rewritten struct literal in a custom ReasoningProvider.
    struct EmptySession;
    impl ReasoningSession for EmptySession {
        fn intensional(
            &self,
            _s: Option<&oxrdf::Term>,
            _p: Option<&oxrdf::Term>,
            _o: Option<&oxrdf::Term>,
        ) -> sparkles::Result<bool> {
            Ok(false)
        }
        fn prepare(
            &mut self,
            demands: &[Demand],
            _estimates: bool,
        ) -> sparkles::Result<Vec<DemandInfo>> {
            Ok(vec![DemandInfo::default(); demands.len()])
        }
        fn derive(&mut self, _seeds: Vec<(usize, Vec<u64>)>) -> sparkles::Result<Derived> {
            Ok(Derived::default())
        }
        fn report(&self) -> Report {
            Report::default()
        }
    }

    struct DownstreamProvider {
        calls: AtomicUsize,
    }
    impl ReasoningProvider for DownstreamProvider {
        fn label(&self) -> String {
            "downstream-custom".into()
        }
        fn rewrite(
            &self,
            query: &spargebra::Query,
            _bound: &[(String, oxrdf::Term)],
            _req: &SessionRequest,
        ) -> sparkles::Result<Option<Rewritten>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Rewritten {
                query: query.clone(),
                derive: Vec::new(),
                nodes: Vec::new(),
                warnings: Vec::new(),
            }))
        }
        fn start(&self, _req: SessionRequest) -> sparkles::Result<Box<dyn ReasoningSession>> {
            Ok(Box::new(EmptySession))
        }
    }

    let ds = Dataset::memory();
    let provider = Arc::new(DownstreamProvider {
        calls: AtomicUsize::new(0),
    });
    let pool = QueryPool::new(16 * MIB);
    let opts = sparkles::sparql::QueryOptions {
        pool: Some(pool.clone()),
        reasoning: Some(provider.clone()),
        ..Default::default()
    };
    sparkles::sparql::query(ds.store().snapshot(), "SELECT * WHERE { ?s ?p ?o }", &opts).unwrap();
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn only_unguarded_payloads_implement_clone() {
    use sparkles::reasoning::reasoner::diagnostics::{GuardedLiteralTypings, LiteralTypings};
    use sparkles::sparql::reasoning::{GuardedRewritten, Rewritten};
    use std::marker::PhantomData;

    // Method resolution takes the inherent method for a Clone payload and
    // borrows the probe again to find the fallback for other payloads.
    trait WithoutClone {
        fn payload_is_clone(&self) -> bool {
            false
        }
    }
    impl<T> WithoutClone for &T {}
    struct Probe<T>(PhantomData<T>);
    impl<T: Clone> Probe<T> {
        fn payload_is_clone(&self) -> bool {
            true
        }
    }

    let guarded_query = &Probe::<GuardedRewritten>(PhantomData);
    let guarded_typings = &Probe::<GuardedLiteralTypings>(PhantomData);
    let query = &Probe::<Rewritten>(PhantomData);
    let typings = &Probe::<LiteralTypings>(PhantomData);
    assert_eq!(
        (
            guarded_query.payload_is_clone(),
            guarded_typings.payload_is_clone()
        ),
        (false, false),
    );
    assert_eq!(
        (query.payload_is_clone(), typings.payload_is_clone()),
        (true, true)
    );
}

#[test]
fn materialization_unwind_joins_writer_monitor_and_refunds_credit() {
    use sparkles::task::{Control, Progress};
    struct Interrupted;

    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    let observed = pool.clone();
    let control = Control {
        progress: Progress::new(move |_, stage| {
            if stage == "reasoning" {
                assert!(observed.granted() > 0);
                std::panic::panic_any(Interrupted);
            }
        }),
        ..Control::none()
    };
    let before = ds.store().head_commit().seq;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        ds.reasoning().run_with(
            &ReasonRequest {
                profile: Profile::RdfsSimple,
                yield_to_writers: true,
                incremental: false,
                ..Default::default()
            },
            &control,
        )
    }));
    assert!(outcome.unwrap_err().is::<Interrupted>());
    assert_eq!(ds.store().head_commit().seq, before);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn cold_rl_inspector_refuses_a_full_pool_before_materialization() {
    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    let sibling = pool.work_parent().reserve(pool.capacity()).unwrap();
    let before = ds.store().head_commit().seq;
    let error = ds
        .reasoning()
        .run(&ReasonRequest {
            profile: Profile::OwlRl,
            incremental: false,
            ..Default::default()
        })
        .unwrap_err();
    assert!(is_server_memory(&error), "{error:?}");
    assert_eq!(ds.store().head_commit().seq, before);
    assert_eq!(pool.granted(), sibling.bytes());
    drop(sibling);
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "ontology")]
#[test]
fn classifier_and_rl_dl_query_respect_pool_without_explicit_grant() {
    use sparkles::handles::ontology::{
        OntologyRequest,
        classify::ClassifyRequest,
        dl_query::{DlQueryRequest, DlReasoning},
    };
    use sparkles::owl::Class;
    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    let req = OntologyRequest::default();
    let classify = ClassifyRequest {
        assertions: true,
        ..Default::default()
    };
    let sibling = pool.work_parent().reserve(pool.capacity()).unwrap();
    let error = ds
        .ontology()
        .classify(&req, &classify, &Default::default())
        .err()
        .expect("classification must see the dataset pool");
    assert!(is_server_memory(&error), "{error:?}");

    let mut dl = DlQueryRequest::new(Class::new("http://ex.org/A").into());
    dl.reasoning = DlReasoning::Rl;
    let error = ds
        .ontology()
        .dl_query(&req, &dl, &Default::default())
        .err()
        .expect("RL private copy must see the dataset pool");
    assert!(is_server_memory(&error), "{error:?}");
    assert_eq!(pool.granted(), sibling.bytes());
    drop(sibling);
    assert_eq!(pool.granted(), 0);

    let partial_room = pool.work_parent().reserve(pool.capacity() - 4096).unwrap();
    let error = ds
        .ontology()
        .open(&req, &Default::default())
        .err()
        .expect("cold decoding must fail even after graph discovery fits");
    assert!(is_server_memory(&error), "{error:?}");
    assert_eq!(pool.granted(), partial_room.bytes());
    drop(partial_room);

    assert!(
        !ds.ontology()
            .open(&req, &Default::default())
            .unwrap()
            .cached
    );
    assert!(
        ds.ontology()
            .open(&req, &Default::default())
            .unwrap()
            .cached
    );
    let open_credit = pool.granted();
    assert!(open_credit > 0);

    let grant = Arc::new(pool.empty_grant());
    grant.charge(128).unwrap();
    let answer = ds
        .ontology()
        .classify(
            &req,
            &classify,
            &QueryOptions {
                grant: Some(grant.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        grant.used() > 128,
        "the live one-shot classification owns caller credit"
    );
    drop(answer);
    assert_eq!(grant.used(), 128);
    drop(grant);
    assert_eq!(pool.granted(), open_credit);

    let kept = ds
        .ontology()
        .classify(&req, &Default::default(), &Default::default())
        .unwrap();
    assert!(pool.granted() > 0);
    drop(ds);
    assert!(
        pool.granted() > 0,
        "the published taxonomy outlives its dataset"
    );
    drop(kept);
    assert_eq!(pool.granted(), 0);
}

#[test]
fn kept_materialization_owns_its_configured_retained_branch() {
    use sparkles::store::{Store, StoreOptions};

    let pool = QueryPool::with_retained_capacity(16 * MIB, 4 * MIB).unwrap();
    let ds = Dataset::from_store_with(
        Store::in_memory(StoreOptions::default()),
        DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    ds.update(
        "PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA { <urn:A> rdfs:subClassOf <urn:B> . <urn:s> a <urn:A> }",
    )
    .unwrap();
    let report = ds
        .reasoning()
        .run(&ReasonRequest {
            profile: Profile::RdfsSimple,
            ..Default::default()
        })
        .unwrap();
    assert!(report.report.inferred > 0);
    assert_eq!(pool.granted(), 0);
    assert!(pool.domain_stats().retained_reserved > 0);
    drop(ds);
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn cold_rdfs_explanation_respects_the_dataset_pool() {
    use sparkles::reasoning::rdfs::NewSchema;

    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    ds.reasoning()
        .rdfs()
        .set(NewSchema::Graph("default".into()))
        .unwrap();
    let mut req = ExplainRequest::new([ex("x"), rdf_type(), ex("C")]);
    req.source = Some(ExplainSource::RdfsOnRead);
    let sibling = pool.work_parent().reserve(pool.capacity()).unwrap();
    let err = ds.reasoning().explain(&req).unwrap_err();
    assert!(is_server_memory(&err), "{err:?}");
    assert_eq!(pool.granted(), sibling.bytes());
    drop(sibling);
    ds.reasoning().explain(&req).unwrap();
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "ontology")]
#[test]
fn classification_retention_refusal_leaves_a_request_owned_answer() {
    use sparkles::handles::ontology::OntologyRequest;
    use sparkles::store::{Store, StoreOptions};

    let data = "INSERT DATA { <urn:A> <http://www.w3.org/2000/01/rdf-schema#subClassOf> <urn:B> }";
    let pool = QueryPool::with_retained_capacity(16 * MIB, 4 * MIB).unwrap();
    let ds = Dataset::from_store_with(
        Store::in_memory(StoreOptions::default()),
        DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    ds.update(data).unwrap();
    let request = OntologyRequest::default();
    let retained = pool.retained_parent().reserve(4 * MIB).unwrap();
    let answer = ds
        .ontology()
        .classify(&request, &Default::default(), &Default::default())
        .unwrap();
    assert!(answer.classification.complete);
    assert!(pool.granted() > 0);
    drop(answer);
    assert_eq!(pool.granted(), 0);
    drop(retained);

    let unpooled = Dataset::memory();
    unpooled.update(data).unwrap();
    for use_grant in [true, false] {
        let grant = Arc::new(pool.empty_grant());
        let opts = QueryOptions {
            grant: use_grant.then(|| grant.clone()),
            pool: (!use_grant).then(|| pool.clone()),
            ..Default::default()
        };
        let answer = unpooled
            .ontology()
            .classify(&request, &Default::default(), &opts)
            .unwrap();
        assert!(pool.granted() > 0);
        assert!(!answer.reused);
        drop(answer);
        assert_eq!(grant.used(), 0);
        drop(opts);
        drop(grant);
        assert_eq!(pool.granted(), 0);
    }
    let grant = Arc::new(pool.empty_grant());
    let opened = unpooled
        .ontology()
        .open(
            &request,
            &QueryOptions {
                grant: Some(grant.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let ontology: Arc<sparkles::handles::ontology::SharedOntology> = opened.ontology.clone();
    let _: &sparkles::owl::read::Ontology = ontology.as_ref().as_ref();
    drop(opened);
    assert!(
        grant.used() > 0,
        "the shared ontology owns its decoding credit"
    );
    drop(ontology);
    assert_eq!(grant.used(), 0);
    let error = unpooled
        .ontology()
        .classify(
            &request,
            &Default::default(),
            &QueryOptions {
                max_memory_bytes: Some(1),
                grant: Some(grant),
                ..Default::default()
            },
        )
        .err()
        .expect("cold opening obeys the request limit");
    assert!(matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory));
}

#[test]
fn fetching_resolved_imports_polls_and_admits_discovery_without_load() {
    use sparkles::reasoning::reasoner::{ImportMode, Inputs, fetch_imports};
    let (_dir, pool, ds) = pooled_dataset(16 * MIB);
    ds.update(
        "INSERT DATA { <urn:source> <http://www.w3.org/2002/07/owl#imports> <urn:import> .
         GRAPH <urn:import> { <urn:a> <urn:p> <urn:b> } }",
    )
    .unwrap();
    let inputs = Inputs {
        imports: ImportMode::Fetch,
        ..Default::default()
    };
    let before = ds.head_commit();
    let sibling = pool.work_parent().reserve(pool.capacity()).unwrap();
    let opts = QueryOptions {
        pool: Some(pool.clone()),
        ..Default::default()
    };
    let error = fetch_imports(ds.store(), &inputs, &[], &opts).unwrap_err();
    assert!(
        error.downcast_ref::<Error>().is_some_and(is_server_memory),
        "{error:#}"
    );
    drop(sibling);
    let late = QueryOptions {
        deadline: Some(std::time::Instant::now()),
        ..opts.clone()
    };
    let error = fetch_imports(ds.store(), &inputs, &[], &late).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<Error>(),
        Some(Error::Timeout)
    ));
    assert!(
        fetch_imports(ds.store(), &inputs, &[], &opts)
            .unwrap()
            .fetched
            .is_empty()
    );
    assert_eq!(ds.head_commit().seq, before.seq);
    assert_eq!(pool.granted(), 0);
}

#[cfg(feature = "ontology")]
#[test]
fn shared_expression_growth_refuses_after_the_cold_ontology_fits() {
    use sparkles::handles::ontology::OntologyRequest;
    let ds = Dataset::memory();
    let mut turtle = String::from(
        "@prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         <urn:A> rdfs:subClassOf _:shared . _:shared owl:intersectionOf (",
    );
    for i in 0..16 {
        turtle.push_str(&format!("<urn:{}:{i}> ", "x".repeat(2048)));
    }
    turtle.push_str(") .");
    ds.load_str(&turtle, RdfFormat::Turtle).unwrap();
    let opts = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(MIB),
        ..Default::default()
    };
    let request = OntologyRequest::default();
    let handle = ds.ontology();
    assert!(!handle.open(&request, &opts).unwrap().ontology.is_empty());
    let additions = (0..64)
        .map(|i| format!("<urn:C{i}> <http://www.w3.org/2000/01/rdf-schema#subClassOf> _:shared ."))
        .collect::<String>();
    // Reload the shared node in a single document so it has the same RDF identity.
    ds.update("CLEAR DEFAULT").unwrap();
    ds.load_str(&(turtle + &additions), RdfFormat::Turtle)
        .unwrap();
    assert!(
        matches!(handle.open(&request, &opts), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
    assert!(
        handle
            .open(
                &request,
                &QueryOptions {
                    max_memory_bytes: Some(8 * MIB),
                    ..opts
                }
            )
            .unwrap()
            .ontology
            .len()
            >= 64
    );
}

#[cfg(feature = "ontology")]
#[test]
fn shared_expression_parser_scratch_returns_before_classification_and_cache_admission() {
    use sparkles::handles::ontology::OntologyRequest;

    let mut ttl = String::from(
        "@prefix owl: <http://www.w3.org/2002/07/owl#> .
         @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .
         _:shared owl:intersectionOf (",
    );
    for i in 0..8 {
        ttl.push_str(&format!("<urn:{}:{i}> ", "x".repeat(4096)));
    }
    ttl.push_str(") .");
    for i in 0..12 {
        ttl.push_str(&format!("<urn:C{i}> rdfs:subClassOf _:shared ."));
    }

    let pool = QueryPool::with_retained_capacity(128 * MIB, MIB).unwrap();
    let ds = Dataset::from_store_with(
        sparkles::store::Store::in_memory(Default::default()),
        DatasetOptions {
            pool: Some(pool.clone()),
            ..Default::default()
        },
    );
    ds.load_str(&ttl, RdfFormat::Turtle).unwrap();
    let request = OntologyRequest::default();
    let handle = ds.ontology();
    let grant = Arc::new(pool.empty_grant());
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        no_cache: true,
        ..Default::default()
    };
    let opened = handle.open(&request, &opts).unwrap();
    let opening_peak = grant.peak();
    assert!(opening_peak > 2 * grant.used());
    assert!(grant.used() >= opened.ontology.retained_bytes() as u64);
    drop(opened);
    assert_eq!(grant.used(), 0);

    let opened = handle.open(&request, &Default::default()).unwrap();
    assert!(handle.open(&request, &Default::default()).unwrap().cached);
    assert!(pool.domain_stats().retained_reserved < opening_peak);
    drop(opened);

    let probe = Arc::new(pool.empty_grant());
    let answer = handle
        .classify(
            &request,
            &Default::default(),
            &QueryOptions {
                grant: Some(probe.clone()),
                ..opts.clone()
            },
        )
        .unwrap();
    let cap = opening_peak.max(probe.peak());
    drop(answer);
    let answer = handle
        .classify(
            &request,
            &Default::default(),
            &QueryOptions {
                max_memory_bytes: Some(cap + 8 * 1024),
                ..opts
            },
        )
        .unwrap();
    assert!(answer.classification.complete_for_taken);
}

#[cfg(feature = "ontology")]
#[test]
fn terminology_only_coverage_refuses_an_assertion_after_opening_fits() {
    use sparkles::handles::ontology::OntologyRequest;
    let ds = Dataset::memory();
    ds.update(&format!(
        "INSERT DATA {{ <urn:A> a <http://www.w3.org/2002/07/owl#Class> .
         <urn:s> <urn:assertion> \"{}\" }}",
        "a".repeat(64 << 10)
    ))
    .unwrap();
    let request = OntologyRequest::default();
    let handle = ds.ontology();
    let opts = QueryOptions {
        no_cache: true,
        max_memory_bytes: Some(128 << 10),
        ..Default::default()
    };
    assert_eq!(
        handle.open(&request, &opts).unwrap().ontology.triples.len(),
        1
    );
    let error = handle
        .classify(&request, &Default::default(), &opts)
        .err()
        .unwrap();
    assert!(matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory));
    let answer = handle
        .classify(
            &request,
            &Default::default(),
            &QueryOptions {
                max_memory_bytes: Some(8 * MIB),
                ..opts
            },
        )
        .unwrap();
    assert!(!answer.complete());
    assert!(!answer.excluded.is_empty() || answer.classification.unmapped.count > 0);
}

#[cfg(feature = "ontology")]
#[test]
fn detached_classification_and_exclusions_keep_their_caller_grant() {
    use sparkles::handles::ontology::{OntologyRequest, classify::ClassifyRequest};
    let ds = Dataset::memory();
    ds.update(
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>
        PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
        INSERT DATA { <urn:A> rdfs:subClassOf [ owl:unionOf ( <urn:B> <urn:C> ) ] . }",
    )
    .unwrap();
    let pool = QueryPool::new(16 * MIB);
    let grant = Arc::new(pool.empty_grant());
    for keep_classification in [true, false] {
        let answer = ds
            .ontology()
            .classify(
                &OntologyRequest::default(),
                &ClassifyRequest {
                    el_only: true,
                    ..Default::default()
                },
                &QueryOptions {
                    grant: Some(grant.clone()),
                    no_cache: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let classification: Arc<sparkles::owl::classify::Classification> =
            answer.classification.clone();
        let excluded: Arc<sparkles::handles::ontology::classify::ClassifiedExclusions> =
            answer.excluded.clone();
        let _: &Vec<sparkles::owl::classify::Excluded> = excluded.as_ref().as_ref();
        assert!(!excluded.is_empty());
        drop(answer);
        assert!(grant.used() > 0);
        if keep_classification {
            drop(excluded);
            assert!(grant.used() > 0);
            drop(classification);
        } else {
            drop(classification);
            assert!(grant.used() > 0);
            drop(excluded);
        }
        assert_eq!(grant.used(), 0);
    }
}

#[test]
fn invalid_import_warning_text_is_admitted_and_owned_after_fetch() {
    use sparkles::reasoning::reasoner::inputs::{ImportMode, Inputs, fetch_imports_admitted};
    let ds = Dataset::memory();
    let pool = QueryPool::new(16 * MIB);
    let grant = Arc::new(pool.empty_grant());
    let refresh: Vec<_> = (0..4).map(|i| format!("urn:import:{i}")).collect();
    let mut inputs = Inputs {
        imports: ImportMode::Fetch,
        ..Default::default()
    };
    for iri in &refresh {
        inputs
            .location_mapping
            .names
            .insert(iri.clone(), "x".repeat(32 << 10));
    }
    let opts = QueryOptions {
        grant: Some(grant.clone()),
        max_memory_bytes: Some(160 << 10),
        ..Default::default()
    };
    let error = fetch_imports_admitted(ds.store(), &inputs, &refresh, &opts).unwrap_err();
    assert!(
        format!("{error:#}").contains("keeping import warnings"),
        "{error:#}"
    );
    assert_eq!(grant.used(), 0);
    let (report, credit) = fetch_imports_admitted(
        ds.store(),
        &inputs,
        &refresh,
        &QueryOptions {
            max_memory_bytes: Some(MIB),
            ..opts
        },
    )
    .unwrap();
    assert_eq!(report.warnings.len(), refresh.len());
    assert!(grant.used() >= report.warnings.iter().map(|s| s.len() as u64).sum());
    drop(report);
    drop(credit);
    assert_eq!(grant.used(), 0);
}

fn import_warning_request(ds: &Dataset) -> ReasonRequest {
    use sparkles::reasoning::reasoner::{ImportMode, Inputs};
    ds.update("INSERT DATA { <urn:o> <http://www.w3.org/2002/07/owl#imports> <urn:missing> }")
        .unwrap();
    let mut inputs = Inputs {
        imports: ImportMode::Fetch,
        ..Default::default()
    };
    inputs
        .location_mapping
        .names
        .insert("urn:missing".into(), "x".repeat(32 << 10));
    ReasonRequest {
        inputs,
        ..Default::default()
    }
}

fn reasoning_record_bytes(ds: &Dataset) -> u64 {
    let state = ds.state().read_reasoning();
    let record = state.as_ref().unwrap();
    [&record.fetched_imports, &record.warnings]
        .into_iter()
        .map(|strings| {
            strings.capacity() * size_of::<String>()
                + strings.iter().map(String::capacity).sum::<usize>()
        })
        .sum::<usize>() as u64
}

fn memory_with_reasoning_pool(pool: Arc<QueryPool>) -> Dataset {
    use sparkles::store::{Store, StoreOptions};
    Dataset::from_store_with(
        Store::in_memory(StoreOptions::default()),
        DatasetOptions {
            pool: Some(pool),
            closure_cache_triples: 0,
            ..Default::default()
        },
    )
}

#[test]
fn fetch_record_uses_retained_credit_and_releases_the_request_grant() {
    use sparkles::reasoning::reasoner::fetch_imports;
    use sparkles::sparql::pool::{GRANULE, INITIAL_GRANT};

    let request_pool = QueryPool::new(INITIAL_GRANT);
    let grant = Arc::new(request_pool.try_grant(INITIAL_GRANT).unwrap());
    let pool = QueryPool::with_retained_capacity(16 * MIB, MIB).unwrap();
    let ds = memory_with_reasoning_pool(pool.clone());
    let mut request = import_warning_request(&ds);
    request.load.grant = Some(grant.clone());
    let owned = fetch_imports(ds.store(), &request.inputs, &[], &request.load).unwrap();
    assert_eq!(owned.warnings.len(), 1);
    assert_eq!(grant.used(), 0);

    let result = ds.reasoning().run(&request).unwrap();
    assert!(result.record.warnings.contains(&owned.warnings[0]));
    assert_eq!(grant.used(), 0);
    drop(result);
    drop(request);
    drop(grant);
    assert_eq!(request_pool.domain_stats().work_reserved, 0);
    assert!(request_pool.try_grant(GRANULE).is_some());
    assert_eq!(pool.domain_stats().work_reserved, 0);
    assert_eq!(
        pool.domain_stats().retained_reserved,
        reasoning_record_bytes(&ds)
    );
    assert!(pool.domain_stats().retained_reserved > 32 << 10);
    ds.reasoning().clear().unwrap();
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn inherited_fetch_record_keeps_credit_until_its_last_branch_clears() {
    let pool = QueryPool::with_retained_capacity(16 * MIB, MIB).unwrap();
    let ds = memory_with_reasoning_pool(pool.clone());
    let request = import_warning_request(&ds);
    let outcome = ds.reasoning().run(&request).unwrap();
    let warnings = outcome.record.warnings;
    let retained = reasoning_record_bytes(&ds);
    assert!(retained > 32 << 10);
    assert_eq!(pool.domain_stats().work_reserved, 0);

    ds.create_branch("one", &Default::default()).unwrap();
    ds.create_branch("two", &Default::default()).unwrap();
    let one = ds.branch("one").unwrap();
    let two = ds.branch("two").unwrap();
    assert_eq!(pool.domain_stats().retained_reserved, retained);
    ds.reasoning().clear().unwrap();
    one.reasoning().clear().unwrap();

    assert_eq!(two.reasoning_record().unwrap().warnings, warnings);
    assert_eq!(pool.domain_stats().retained_reserved, retained);
    two.reasoning().clear().unwrap();
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn replaced_fetch_record_keeps_previously_fetched_names_when_warnings_change() {
    let pool = QueryPool::with_retained_capacity(16 * MIB, MIB).unwrap();
    let ds = memory_with_reasoning_pool(pool.clone());
    let mut request = import_warning_request(&ds);
    let iri = format!("urn:import:{}", "x".repeat(16 << 10));
    ds.update(&format!(
        "INSERT DATA {{ <urn:o> <http://www.w3.org/2002/07/owl#imports> <{iri}> }}"
    ))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("import.ttl");
    std::fs::write(&source, "<urn:s> <urn:p> <urn:original> .").unwrap();
    request
        .inputs
        .location_mapping
        .names
        .insert(iri.clone(), format!("file://{}", source.display()));

    let first = ds.reasoning().run(&request).unwrap();
    assert_eq!(first.record.fetched_imports, [iri.as_str()]);
    assert!(!first.record.warnings.is_empty());
    let first_retained = pool.domain_stats().retained_reserved;
    assert_eq!(first_retained, reasoning_record_bytes(&ds));
    ds.update("INSERT DATA { GRAPH <urn:missing> { <urn:s> <urn:p> <urn:present> } }")
        .unwrap();
    let next = ds.reasoning().run(&request).unwrap();
    assert_eq!(next.record.fetched_imports, first.record.fetched_imports);
    assert!(
        next.record.warnings.is_empty(),
        "{:?}",
        next.record.warnings
    );
    let next_retained = pool.domain_stats().retained_reserved;
    assert!(next_retained < first_retained);
    assert_eq!(next_retained, (iri.len() + size_of::<String>()) as u64);
    assert_eq!(pool.domain_stats().work_reserved, 0);
    ds.reasoning().clear().unwrap();
    assert_eq!(pool.domain_stats().retained_reserved, 0);
}

#[test]
fn persistent_rename_readmits_fetch_record() {
    use sparkles::catalog::{CatalogOptions, MemoryOptions};
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(
        dir.path(),
        CatalogOptions {
            dataset: DatasetOptions {
                closure_cache_triples: 0,
                ..Default::default()
            },
            memory: MemoryOptions {
                pool_bytes: Some(16 * MIB),
                ..Default::default()
            },
        },
    )
    .unwrap();
    let ds = catalog.create("before", &Default::default()).unwrap();
    let pool = ds.pool().unwrap().clone();
    let request = import_warning_request(&ds);
    let outcome = ds.reasoning().run(&request).unwrap();
    assert_eq!(
        pool.domain_stats().work_reserved,
        reasoning_record_bytes(&ds)
    );
    drop(ds);

    let renamed = catalog.rename("before", "after").unwrap();
    assert_eq!(
        renamed.reasoning_record().unwrap().warnings,
        outcome.record.warnings
    );
    assert_eq!(
        pool.domain_stats().work_reserved,
        reasoning_record_bytes(&renamed)
    );
    renamed.reasoning().clear().unwrap();
    assert_eq!(pool.domain_stats().work_reserved, 0);
}

#[test]
fn refusing_replacement_record_credit_preserves_the_previous_materialization() {
    let pool = QueryPool::with_retained_capacity(16 * MIB, 1024).unwrap();
    let ds = memory_with_reasoning_pool(pool.clone());
    let previous = ds.reasoning().run(&ReasonRequest::default()).unwrap();
    let request = import_warning_request(&ds);
    let before = ds.store().snapshot().commit;
    let error = ds.reasoning().run(&request).unwrap_err();
    assert!(is_server_memory(&error), "{error}");
    assert_eq!(ds.store().snapshot().commit, before);
    assert_eq!(
        ds.reasoning_record().unwrap().commit,
        previous.record.commit
    );
    assert_eq!(
        pool.domain_stats().retained_reserved,
        reasoning_record_bytes(&ds)
    );
    assert_eq!(pool.domain_stats().work_reserved, 0);
}

#[test]
fn materialization_refreshes_the_import_named_in_its_record() {
    use sparkles::reasoning::reasoner::{ImportMode, Inputs};
    let ds = Dataset::memory();
    ds.update("INSERT DATA { <urn:o> <http://www.w3.org/2002/07/owl#imports> <urn:import> }")
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("import.ttl");
    std::fs::write(&source, "<urn:s> <urn:p> <urn:original> .").unwrap();
    let mut inputs = Inputs {
        imports: ImportMode::Fetch,
        ..Default::default()
    };
    inputs
        .location_mapping
        .names
        .insert("urn:import".into(), format!("file://{}", source.display()));
    let mut request = ReasonRequest {
        inputs,
        ..Default::default()
    };
    let first = ds.reasoning().run(&request).unwrap();
    assert_eq!(first.record.fetched_imports, ["urn:import"]);

    std::fs::write(&source, "<urn:s> <urn:p> <urn:updated> .").unwrap();
    request.refresh_imports = true;
    let next = ds.reasoning().run(&request).unwrap();
    assert_eq!(next.record.fetched_imports, first.record.fetched_imports);
    let ask = |object| {
        ds.query(&format!(
            "ASK {{ GRAPH <urn:import> {{ <urn:s> <urn:p> <{object}> }} }}"
        ))
        .unwrap()
        .boolean
    };
    assert!(ask("urn:updated"));
    assert!(!ask("urn:original"));
}

#[cfg(feature = "ontology")]
#[test]
fn public_owl_and_entailment_structs_allow_downstream_literals_and_patterns() {
    use sparkles::handles::ontology::{
        OntologyRequest,
        dl_query::{DlQueryAnswer, DlQueryRequest},
    };
    use sparkles::owl::{Class, read::Ontology};
    use sparkles::reasoning::diff::{DiffState, EntailmentDiff};

    let ontology = Ontology {
        error_count: 1,
        ..Ontology::default()
    };
    let Ontology {
        header: _,
        header_graph: _,
        other_headers: _,
        axioms: _,
        triples: _,
        graph_of: _,
        unmapped: _,
        diagnostics: _,
        diagnostics_dropped: _,
        error_count,
        typing: _,
        mode: _,
        terminology_only: _,
        reification_nodes: _,
        ontology_nodes: _,
    } = ontology;
    assert_eq!(error_count, 1);

    let state = DiffState {
        selector: "head".into(),
        commit: 0,
    };
    let diff = EntailmentDiff {
        from: state.clone(),
        to: state,
        added: 0,
        removed: 0,
        added_triples: Vec::new(),
        removed_triples: Vec::new(),
        by_predicate: Default::default(),
        by_class: Default::default(),
        new_findings: Vec::new(),
        findings_complete: true,
        sample_truncated: false,
        warnings: Vec::new(),
    };
    let EntailmentDiff {
        from: _,
        to: _,
        added,
        removed,
        added_triples: _,
        removed_triples: _,
        by_predicate: _,
        by_class: _,
        new_findings: _,
        findings_complete: _,
        sample_truncated: _,
        warnings: _,
    } = diff;
    assert_eq!(added + removed, 0);

    let ds = Dataset::memory();
    let request = DlQueryRequest::new(Class::thing().into());
    let answer = ds
        .ontology()
        .dl_query(&OntologyRequest::default(), &request, &Default::default())
        .unwrap();
    let DlQueryAnswer {
        opened,
        expression,
        reasoning,
        satisfiable,
        consistent,
        equivalents,
        superclasses,
        direct_superclasses,
        subclasses,
        direct_subclasses,
        instances,
        incremental,
        elapsed,
    } = answer;
    let reconstructed = DlQueryAnswer {
        opened,
        expression,
        reasoning,
        satisfiable,
        consistent,
        equivalents,
        superclasses,
        direct_superclasses,
        subclasses,
        direct_subclasses,
        instances,
        incremental,
        elapsed,
    };
    assert_eq!(reconstructed.expression, request.expression);
}
