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
    let diff = ds.reasoning().diff(&DiffRequest::new("1", "head")).unwrap();
    assert!(diff.added > 0);
    assert_eq!(pool.granted(), 0, "entailment diff must return credit");

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
