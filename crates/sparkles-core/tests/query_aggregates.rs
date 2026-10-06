use oxrdf::{Literal, Term};
use sparkles_core::sparql::extensions::{
    AggregateAccumulator, AggregateContext, AggregateDescriptor, AggregateFactory,
    ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarError, ScalarResult,
};
use sparkles_core::sparql::{QueryOptions, execute_query, explain, parse_query, query};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const IRI: &str = "urn:test:aggregate";

#[derive(Default)]
struct Counts {
    create: AtomicUsize,
    add: AtomicUsize,
    finish: AtomicUsize,
    drop: AtomicUsize,
}

struct Counting {
    counts: Arc<Counts>,
    count: i64,
}

impl AggregateAccumulator for Counting {
    fn add(&mut self, ctx: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        ctx.check()?;
        self.counts.add.fetch_add(1, Ordering::SeqCst);
        self.count += 1;
        Ok(())
    }

    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        self.counts.finish.fetch_add(1, Ordering::SeqCst);
        Ok(Literal::from(self.count).into())
    }
}

impl Drop for Counting {
    fn drop(&mut self) {
        self.counts.drop.fetch_add(1, Ordering::SeqCst);
    }
}

fn factory(counts: Arc<Counts>, offset: i64) -> impl AggregateFactory {
    move |_: &AggregateContext<'_>| {
        counts.create.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Counting {
            counts: counts.clone(),
            count: offset,
        }) as Box<dyn AggregateAccumulator>)
    }
}

fn options(factory: impl AggregateFactory) -> QueryOptions {
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_aggregate(AggregateDescriptor::new(IRI, 1, factory).unwrap())
        .unwrap();
    QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    }
}

fn run(store: &Store, text: &str, opts: &QueryOptions) -> Vec<Vec<Option<Term>>> {
    query(store.snapshot(), text, opts).unwrap().rows()
}

fn integer(value: i64) -> Option<Term> {
    Some(Literal::from(value).into())
}

fn source(input: &str) -> String {
    format!("SELECT (AGG <{IRI}>(?v) AS ?n) {{ {input} }}")
}

fn bounded(f: impl FnOnce() + Send + 'static) {
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)))
            .unwrap();
    });
    let outcome = receive
        .recv_timeout(std::time::Duration::from_secs(4))
        .expect("aggregate callback must fail before blocking on its own writer");
    worker.join().unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

fn aggregate_ast(query: &spargebra::Query) -> Vec<spargebra::algebra::AggregateExpression> {
    use spargebra::algebra::GraphPattern as P;
    fn visit(p: &P, out: &mut Vec<spargebra::algebra::AggregateExpression>) {
        match p {
            P::Group {
                inner, aggregates, ..
            } => {
                out.extend(aggregates.iter().map(|(_, a)| a.clone()));
                visit(inner, out);
            }
            P::Project { inner, .. } | P::Extend { inner, .. } | P::Distinct { inner } => {
                visit(inner, out)
            }
            P::Values { .. } => {}
            _ => panic!("unexpected roundtrip algebra"),
        }
    }
    let spargebra::Query::Select { pattern, .. } = query else {
        panic!("expected SELECT")
    };
    let mut out = Vec::new();
    visit(pattern, &mut out);
    out
}

#[test]
fn arbitrary_iri_parser_roundtrip_and_registry_independent_prepared_runs() {
    let store = Arc::new(Store::in_memory(StoreOptions::default()));
    let text = source("VALUES ?v {1 2 2}");
    let parsed = Arc::new(parse_query(&text, None, &[]).unwrap());
    assert!(parsed.to_sse().contains(IRI));
    let formatted = parsed.to_string();
    let roundtrip = parse_query(&formatted, None, &[]).unwrap();
    assert_eq!(aggregate_ast(&parsed), aggregate_ast(&roundtrip));
    assert!(formatted.contains(&format!("AGG <{IRI}>(?v)")));
    assert!(roundtrip.to_sse().contains(&format!("(<{IRI}> ?v)")));
    assert!(roundtrip.to_sse().contains("(group "));
    let counts = Arc::new(Counts::default());
    let opts = options(factory(counts, 0));
    assert_eq!(
        execute_query(store.snapshot(), &roundtrip, &opts, 0.0)
            .unwrap()
            .rows(),
        vec![vec![integer(3)]]
    );
    let distinct = parse_query(&text.replace("(?v)", "(DISTINCT ?v)"), None, &[]).unwrap();
    let displayed = distinct.to_string();
    assert!(displayed.contains(&format!("AGG <{IRI}>(DISTINCT ?v)")));
    let distinct_roundtrip = parse_query(&displayed, None, &[]).unwrap();
    assert_eq!(aggregate_ast(&distinct), aggregate_ast(&distinct_roundtrip));
    assert!(
        distinct_roundtrip
            .to_sse()
            .contains(&format!("(<{IRI}> distinct ?v)"))
    );
    assert_eq!(
        execute_query(store.snapshot(), &distinct_roundtrip, &opts, 0.0)
            .unwrap()
            .rows(),
        vec![vec![integer(2)]]
    );
    let workers = [0, 10].map(|offset| {
        let store = store.clone();
        let parsed = parsed.clone();
        std::thread::spawn(move || {
            let counts = Arc::new(Counts::default());
            let opts = options(factory(counts.clone(), offset));
            for _ in 0..4 {
                let result = execute_query(store.snapshot(), &parsed, &opts, 0.0).unwrap();
                assert_eq!(result.rows(), vec![vec![integer(offset + 3)]]);
            }
            assert_eq!(counts.create.load(Ordering::SeqCst), 4);
            assert_eq!(counts.drop.load(Ordering::SeqCst), 4);
        })
    });
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(run(&store, &text, &Default::default()), vec![vec![None]]);
}

#[test]
fn groups_distinct_rdf_identity_and_empty_group_lifecycles() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let opts = options(factory(counts.clone(), 0));
    let text = format!(
        "SELECT ?g (AGG <{IRI}>(?v) AS ?all) (AGG <{IRI}>(DISTINCT ?v) AS ?d) {{
        VALUES (?g ?v) {{(1 1) (1 1) (1 1.0) (2 2)}} }} GROUP BY ?g ORDER BY ?g"
    );
    assert_eq!(
        run(&store, &text, &opts),
        vec![
            vec![integer(1), integer(3), integer(2)],
            vec![integer(2), integer(1), integer(1)]
        ]
    );
    assert_eq!(counts.create.load(Ordering::SeqCst), 4);
    assert_eq!(counts.add.load(Ordering::SeqCst), 7);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 4);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 4);
    assert_eq!(
        run(&store, &source("VALUES ?v {}"), &opts),
        vec![vec![integer(0)]]
    );
    assert_eq!(counts.create.load(Ordering::SeqCst), 5);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 5);
    assert!(
        run(
            &store,
            &format!("SELECT ?g (AGG <{IRI}>(?v) AS ?n) {{VALUES (?g ?v) {{}}}} GROUP BY ?g"),
            &opts
        )
        .is_empty()
    );
    assert_eq!(counts.create.load(Ordering::SeqCst), 5);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 5);
}

#[test]
fn argument_errors_make_group_unbound_but_other_groups_continue() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let opts = options(factory(counts.clone(), 0));
    let text = format!(
        "SELECT ?g (AGG <{IRI}>(?v) AS ?n) {{VALUES (?g ?v) {{(1 1) (1 UNDEF) (2 2)}}}} GROUP BY ?g ORDER BY ?g"
    );
    assert_eq!(
        run(&store, &text, &opts),
        vec![vec![integer(1), None], vec![integer(2), integer(1)]]
    );
    assert_eq!(counts.create.load(Ordering::SeqCst), 2);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 2);
    assert_eq!(
        run(
            &store,
            &format!("SELECT (AGG <{IRI}>(1/0) AS ?n) {{VALUES ?v {{1 2}}}}"),
            &opts
        ),
        vec![vec![None]]
    );
}

#[test]
fn having_subqueries_optional_and_unused_registry_preserve_results() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let opts = options(factory(counts.clone(), 0));
    let text = format!(
        "SELECT ?g (AGG <{IRI}>(?v) AS ?n) {{VALUES (?g ?v) {{(1 1) (1 2) (2 3)}}}} GROUP BY ?g HAVING(AGG <{IRI}>(?v)>1) ORDER BY ?g"
    );
    assert_eq!(
        run(&store, &text, &opts),
        vec![vec![integer(1), integer(2)]]
    );
    let text = format!(
        "SELECT ?n {{VALUES ?outer {{1}} OPTIONAL {{SELECT (AGG <{IRI}>(?v) AS ?n) {{VALUES ?v {{1 2 3}}}}}}}}"
    );
    assert_eq!(run(&store, &text, &opts), vec![vec![integer(3)]]);
    let before = counts.create.load(Ordering::SeqCst);
    let ordinary = "SELECT (SUM(?v) AS ?sum) {VALUES ?v {1 2 3}}";
    assert_eq!(
        run(&store, ordinary, &opts),
        run(&store, ordinary, &Default::default())
    );
    fn normalize(v: &mut serde_json::Value) {
        // Each independent parse mints internal aggregate variable names.
        if let serde_json::Value::Object(map) = v {
            map.remove("description");
            map.remove("columns");
            if let Some(serde_json::Value::Array(children)) = map.get_mut("children") {
                children.iter_mut().for_each(normalize);
            }
        }
    }
    let mut registered =
        serde_json::to_value(explain(store.snapshot(), ordinary, &opts).unwrap().1).unwrap();
    let mut ordinary_plan = serde_json::to_value(
        explain(store.snapshot(), ordinary, &Default::default())
            .unwrap()
            .1,
    )
    .unwrap();
    normalize(&mut registered);
    normalize(&mut ordinary_plan);
    assert_eq!(registered, ordinary_plan);
    assert_eq!(counts.create.load(Ordering::SeqCst), before);
}

#[test]
fn registry_rejects_unsupported_arity_builtin_and_cross_category_collisions() {
    let count = Arc::new(Counts::default());
    assert!(AggregateDescriptor::new(IRI, 0, factory(count.clone(), 0)).is_err());
    assert!(AggregateDescriptor::new(IRI, 2, factory(count.clone(), 0)).is_err());
    assert!(AggregateDescriptor::new("relative", 1, factory(count.clone(), 0)).is_err());
    for iri in [
        "http://www.w3.org/2001/XMLSchema#string",
        "http://www.w3.org/2005/xpath-functions#substring",
        sparkles_core::sparql::catalog::extension_aggregates()[0].as_str(),
    ] {
        assert!(
            ExtensionRegistry::builder()
                .register_aggregate(
                    AggregateDescriptor::new(iri, 1, factory(count.clone(), 0)).unwrap()
                )
                .is_err()
        );
    }
    for aggregate_first in [true, false] {
        let mut registry = ExtensionRegistry::builder();
        let scalar = ScalarDescriptor::new(IRI, 1..=1, |_: &ScalarContext<'_>, args: &[Term]| {
            Ok(args[0].clone())
        })
        .unwrap();
        let aggregate = AggregateDescriptor::new(IRI, 1, factory(count.clone(), 0)).unwrap();
        if aggregate_first {
            registry.register_aggregate(aggregate).unwrap();
            assert!(registry.register_scalar(scalar).is_err());
        } else {
            registry.register_scalar(scalar).unwrap();
            assert!(registry.register_aggregate(aggregate).is_err());
        }
    }
    let mut registry = ExtensionRegistry::builder();
    let mut malformed = AggregateDescriptor::new(IRI, 1, factory(count, 0)).unwrap();
    malformed.arity = 2;
    assert!(registry.register_aggregate(malformed).is_err());
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Create,
    Add,
    Finish,
    Drop,
}

struct Fault {
    stage: Stage,
    panic: bool,
    drops: Arc<AtomicUsize>,
}

impl Fault {
    fn fail(&self, stage: Stage) -> Result<(), ScalarError> {
        if self.stage == stage {
            if self.panic {
                panic!("untrusted aggregate panic payload");
            }
            Err(ScalarError::Execution("bounded failure".repeat(1024)))
        } else {
            Ok(())
        }
    }
}

impl AggregateAccumulator for Fault {
    fn add(&mut self, _: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        self.fail(Stage::Add)
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        self.fail(Stage::Finish)?;
        Ok(Literal::from(1).into())
    }
}

impl Drop for Fault {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
        if self.stage == Stage::Drop {
            panic!("untrusted aggregate destructor panic payload");
        }
    }
}

fn faulty(stage: Stage, panic: bool, drops: Arc<AtomicUsize>) -> QueryOptions {
    options(move |_: &AggregateContext<'_>| {
        let accumulator = Fault {
            stage,
            panic,
            drops: drops.clone(),
        };
        accumulator.fail(Stage::Create)?;
        Ok(Box::new(accumulator) as Box<dyn AggregateAccumulator>)
    })
}

#[test]
fn lifecycle_errors_and_panics_are_bounded_fatal_and_release_accumulators() {
    let store = Store::in_memory(StoreOptions::default());
    for stage in [Stage::Create, Stage::Add, Stage::Finish, Stage::Drop] {
        for panic in [false, true] {
            let drops = Arc::new(AtomicUsize::new(0));
            let opts = faulty(stage, panic, drops.clone());
            let error = query(store.snapshot(), &source("VALUES ?v {1}"), &opts)
                .err()
                .unwrap();
            let text = error.to_string();
            assert!(!text.contains("untrusted aggregate"));
            assert!(text.len() < 650);
            assert_eq!(drops.load(Ordering::SeqCst), 1);
            assert!(run(&store, "SELECT * {?s ?p ?o}", &Default::default()).is_empty());
        }
    }
}

struct Suppress {
    cancel: Option<Arc<AtomicBool>>,
    drops: Arc<AtomicUsize>,
}

impl AggregateAccumulator for Suppress {
    fn add(&mut self, ctx: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::SeqCst);
            let _ = ctx.check();
        } else {
            let _ = ctx.retain(4096);
        }
        Ok(())
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        Ok(Literal::from(1).into())
    }
}

impl Drop for Suppress {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn suppressed_cancellation_and_retained_budget_errors_remain_fatal() {
    let store = Store::in_memory(StoreOptions::default());
    for cancelling in [true, false] {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelling.then_some(cancelled.clone());
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = drops.clone();
        let mut opts = options(move |_: &AggregateContext<'_>| {
            Ok(Box::new(Suppress {
                cancel: cancel.clone(),
                drops: observed.clone(),
            }) as Box<dyn AggregateAccumulator>)
        });
        opts.cancel = Some(cancelled);
        opts.max_memory_bytes = Some(1024);
        let error = query(store.snapshot(), &source("VALUES ?v {1 2}"), &opts)
            .err()
            .unwrap();
        if cancelling {
            assert!(matches!(error, sparkles_core::Error::Cancelled));
        } else {
            assert!(matches!(error, sparkles_core::Error::BudgetExceeded(_)));
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn caught_fatal_update_failure_cannot_commit_prior_mutations() {
    for stage in [Stage::Create, Stage::Add, Stage::Finish, Stage::Drop] {
        let store = Store::in_memory(StoreOptions::default());
        let opts = faulty(stage, false, Default::default());
        let mut transaction = store.write();
        let text = format!(
            "INSERT DATA {{<urn:s> <urn:before> 1}}; INSERT {{<urn:s> <urn:after> ?n}} WHERE {{SELECT (AGG <{IRI}>(?v) AS ?n) {{VALUES ?v {{1}}}}}}"
        );
        assert!(sparkles_core::sparql::update::update_in(&mut transaction, &text, &opts).is_err());
        assert!(transaction.commit().is_err());
        assert!(run(&store, "SELECT * {?s ?p ?o}", &Default::default()).is_empty());
        sparkles_core::sparql::update::update(
            &store,
            "INSERT DATA {<urn:s> <urn:p> 1}",
            &Default::default(),
        )
        .unwrap();
    }
}

struct Reenter {
    store: Arc<Store>,
    stage: Stage,
}

impl Reenter {
    fn attempt(&self, stage: Stage) {
        if self.stage == stage {
            let _ =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.store.snapshot()));
        }
    }
}

impl AggregateAccumulator for Reenter {
    fn add(&mut self, _: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        self.attempt(Stage::Add);
        Ok(())
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        self.attempt(Stage::Finish);
        Ok(Literal::from(1).into())
    }
}

impl Drop for Reenter {
    fn drop(&mut self) {
        self.attempt(Stage::Drop);
    }
}

#[test]
fn every_lifecycle_thread_blocks_caught_family_reentrancy_before_writer_wait() {
    for stage in [Stage::Create, Stage::Add, Stage::Finish, Stage::Drop] {
        bounded(move || {
            let store = Arc::new(Store::in_memory(StoreOptions::default()));
            let captured = store.clone();
            let opts = options(move |_: &AggregateContext<'_>| {
                let accumulator = Reenter {
                    store: captured.clone(),
                    stage,
                };
                accumulator.attempt(Stage::Create);
                Ok(Box::new(accumulator) as Box<dyn AggregateAccumulator>)
            });
            let transaction = store.write();
            assert!(
                query(
                    Arc::new(transaction.view()),
                    &source("VALUES ?v {1}"),
                    &opts
                )
                .is_err()
            );
            assert!(transaction.commit().is_err());
            sparkles_core::sparql::update::update(
                &store,
                "INSERT DATA {<urn:s> <urn:p> 1}",
                &Default::default(),
            )
            .unwrap();
        });
    }
}

struct Last {
    value: Option<Term>,
    returned: Option<Term>,
}

impl AggregateAccumulator for Last {
    fn add(&mut self, context: &AggregateContext<'_>, argument: &Term) -> Result<(), ScalarError> {
        context.retain(argument.to_string().len() as u64 + 64)?;
        self.value = Some(argument.clone());
        Ok(())
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        self.returned
            .clone()
            .or_else(|| self.value.clone())
            .ok_or(ScalarError::Expression)
    }
}

#[test]
fn retained_terms_preserve_received_blank_nodes_but_cannot_forge_stored_identity() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA {_:s <urn:p> 1}",
        &Default::default(),
    )
    .unwrap();
    let blank = run(&store, "SELECT ?s {?s <urn:p> 1}", &Default::default())[0][0]
        .clone()
        .unwrap();
    let opts = options(|_: &AggregateContext<'_>| {
        Ok(Box::new(Last {
            value: None,
            returned: None,
        }) as Box<dyn AggregateAccumulator>)
    });
    assert_eq!(
        run(
            &store,
            &format!("SELECT (AGG <{IRI}>(?s) AS ?last) {{?s <urn:p> 1}}"),
            &opts
        ),
        vec![vec![Some(blank.clone())]]
    );
    let opts = options(move |_: &AggregateContext<'_>| {
        Ok(Box::new(Last {
            value: None,
            returned: Some(blank.clone()),
        }) as Box<dyn AggregateAccumulator>)
    });
    let text = format!(
        "SELECT ?last ?found {{ {{SELECT (AGG <{IRI}>(?v) AS ?last) {{VALUES ?v {{1}}}}}} OPTIONAL {{?last <urn:p> ?found}} }}"
    );
    let rows = run(&store, &text, &opts);
    assert!(matches!(rows[0][0], Some(Term::BlankNode(_))));
    assert!(rows[0][1].is_none());
}

struct Domain(Stage, Arc<AtomicUsize>);
impl AggregateAccumulator for Domain {
    fn add(&mut self, _: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        if self.0 == Stage::Add {
            Err(ScalarError::Expression)
        } else {
            Ok(())
        }
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        if self.0 == Stage::Finish {
            Err(ScalarError::Expression)
        } else {
            Ok(Literal::from(1).into())
        }
    }
}
impl Drop for Domain {
    fn drop(&mut self) {
        self.1.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn lifecycle_expression_errors_are_unbound_without_poisoning_transactions() {
    for stage in [Stage::Create, Stage::Add, Stage::Finish] {
        let store = Store::in_memory(StoreOptions::default());
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = drops.clone();
        let opts = options(move |_: &AggregateContext<'_>| {
            if stage == Stage::Create {
                return Err(ScalarError::Expression);
            }
            Ok(Box::new(Domain(stage, observed.clone())) as Box<dyn AggregateAccumulator>)
        });
        let transaction = store.write();
        assert_eq!(
            query(
                Arc::new(transaction.view()),
                &source("VALUES ?v {1}"),
                &opts
            )
            .unwrap()
            .rows(),
            vec![vec![None]]
        );
        transaction.commit().unwrap();
        assert_eq!(
            drops.load(Ordering::SeqCst),
            usize::from(stage != Stage::Create)
        );
    }
}

struct Singleton(Arc<AtomicUsize>);
impl AggregateAccumulator for Singleton {
    fn add(&mut self, _: &AggregateContext<'_>, _: &Term) -> Result<(), ScalarError> {
        panic!("batch override must be dispatched")
    }
    fn add_batch(&mut self, _: &AggregateContext<'_>, args: &[Term]) -> Result<(), ScalarError> {
        assert_eq!(args.len(), 1);
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn finish(&mut self, _: &AggregateContext<'_>) -> ScalarResult {
        Ok(Literal::from(1).into())
    }
}

#[test]
fn batch_capability_is_honest_and_scalar_argument_calls_keep_multiplicity() {
    let store = Store::in_memory(StoreOptions::default());
    let batches = Arc::new(AtomicUsize::new(0));
    let observed = batches.clone();
    let scalar_calls = Arc::new(AtomicUsize::new(0));
    let observed_scalar = scalar_calls.clone();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_aggregate(
            AggregateDescriptor::new(IRI, 1, move |_: &AggregateContext<'_>| {
                Ok(Box::new(Singleton(observed.clone())) as Box<dyn AggregateAccumulator>)
            })
            .unwrap(),
        )
        .unwrap();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:test:scalar-argument",
                1..=1,
                move |_: &ScalarContext<'_>, args: &[Term]| {
                    observed_scalar.fetch_add(1, Ordering::SeqCst);
                    Ok(args[0].clone())
                },
            )
            .unwrap(),
        )
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let values = std::iter::repeat_n("1", 128).collect::<Vec<_>>().join(" ");
    let text = format!(
        "SELECT (AGG <{IRI}>(<urn:test:scalar-argument>(?v)) AS ?n) {{VALUES ?v {{{values}}}}}"
    );
    for _ in 0..2 {
        assert_eq!(run(&store, &text, &opts), vec![vec![integer(1)]]);
    }
    assert_eq!(batches.load(Ordering::SeqCst), 256);
    assert_eq!(scalar_calls.load(Ordering::SeqCst), 256);
    let text = text.replace(&format!("AGG <{IRI}>("), &format!("AGG <{IRI}>(DISTINCT "));
    run(&store, &text, &opts);
    assert_eq!(batches.load(Ordering::SeqCst), 257);
    assert_eq!(scalar_calls.load(Ordering::SeqCst), 384);
}

#[test]
fn fatal_scalar_argument_errors_abort_ask_and_update_instead_of_becoming_unbound() {
    let store = Store::in_memory(StoreOptions::default());
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_aggregate(
            AggregateDescriptor::new(IRI, 1, factory(Default::default(), 0)).unwrap(),
        )
        .unwrap();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:test:fatal",
                1..=1,
                |_: &ScalarContext<'_>, _: &[Term]| {
                    Err(ScalarError::Execution("fatal argument".into()))
                },
            )
            .unwrap(),
        )
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let inner = format!("SELECT (AGG <{IRI}>(<urn:test:fatal>(?v)) AS ?n) {{VALUES ?v {{1}}}}");
    assert!(query(store.snapshot(), &format!("ASK {{{{ {inner} }}}}"), &opts).is_err());
    let mut transaction = store.write();
    let update = format!(
        "INSERT DATA {{<urn:s> <urn:before> 1}}; INSERT {{<urn:s> <urn:after> ?n}} WHERE {{{{ {inner} }}}}"
    );
    assert!(sparkles_core::sparql::update::update_in(&mut transaction, &update, &opts).is_err());
    assert!(transaction.commit().is_err());
    assert!(run(&store, "SELECT * {?s ?p ?o}", &Default::default()).is_empty());
}

#[test]
fn other_held_transaction_families_are_guarded_in_factory_and_destructor() {
    for stage in [Stage::Create, Stage::Drop] {
        bounded(move || {
            let a = Store::in_memory(StoreOptions::default());
            let b = Arc::new(Store::in_memory(StoreOptions::default()));
            let captured = b.clone();
            let transaction = b.write();
            let opts = options(move |_: &AggregateContext<'_>| {
                let accumulator = Reenter {
                    store: captured.clone(),
                    stage,
                };
                accumulator.attempt(Stage::Create);
                Ok(Box::new(accumulator) as Box<dyn AggregateAccumulator>)
            });
            assert!(query(a.snapshot(), &source("VALUES ?v {1}"), &opts).is_err());
            assert!(transaction.commit().is_err());
            sparkles_core::sparql::update::update(
                &b,
                "INSERT DATA {<urn:s> <urn:p> 1}",
                &Default::default(),
            )
            .unwrap();
        });
    }
}

#[test]
fn empty_ungrouped_failures_abort_but_empty_grouped_inputs_invoke_nothing() {
    let store = Store::in_memory(StoreOptions::default());
    for stage in [Stage::Create, Stage::Finish, Stage::Drop] {
        let drops = Arc::new(AtomicUsize::new(0));
        let opts = faulty(stage, true, drops.clone());
        assert!(query(store.snapshot(), &source("VALUES ?v {}"), &opts).is_err());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let text = format!("SELECT ?g (AGG <{IRI}>(?v) AS ?n) {{VALUES (?g ?v) {{}}}} GROUP BY ?g");
        assert!(run(&store, &text, &opts).is_empty());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn invalid_and_oversized_output_terms_are_fatal_protocol_failures() {
    let store = Store::in_memory(StoreOptions::default());
    for returned in [
        Term::from(oxrdf::NamedNode::new_unchecked("relative")),
        Literal::from("x".repeat(1 << 20)).into(),
    ] {
        let opts = options(move |_: &AggregateContext<'_>| {
            Ok(Box::new(Last {
                value: None,
                returned: Some(returned.clone()),
            }) as Box<dyn AggregateAccumulator>)
        });
        let error = query(store.snapshot(), &source("VALUES ?v {1}"), &opts)
            .err()
            .unwrap();
        assert!(error.to_string().contains("callback"));
        assert!(error.to_string().len() < 650);
    }
}

#[test]
fn built_in_aggregate_formatting_keeps_keyword_and_custom_iri_semantics() {
    let store = Store::in_memory(StoreOptions::default());
    for function in ["MEDIAN", "<http://jena.apache.org/ARQ/function#var_pop>"] {
        let text = format!("SELECT ({function}(DISTINCT ?v) AS ?n) {{VALUES ?v {{1 2 2 3}}}}");
        let parsed = parse_query(&text, None, &[]).unwrap();
        let displayed = parsed.to_string();
        if function == "MEDIAN" {
            assert!(displayed.contains("MEDIAN(DISTINCT ?v)"));
        } else {
            assert!(displayed.contains(&format!("AGG {function}(DISTINCT ?v)")));
        }
        let roundtrip = parse_query(&displayed, None, &[]).unwrap();
        assert_eq!(aggregate_ast(&parsed), aggregate_ast(&roundtrip));
        assert_eq!(
            execute_query(store.snapshot(), &parsed, &Default::default(), 0.0)
                .unwrap()
                .rows(),
            execute_query(store.snapshot(), &roundtrip, &Default::default(), 0.0)
                .unwrap()
                .rows()
        );
    }
}

#[test]
fn distinct_retained_state_is_charged_and_releases_the_accumulator() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let mut opts = options(factory(counts.clone(), 0));
    opts.max_memory_bytes = Some(4096);
    let values = (0..128)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("SELECT (AGG <{IRI}>(DISTINCT ?v) AS ?n) {{VALUES ?v {{{values}}}}}");
    assert!(matches!(
        query(store.snapshot(), &text, &opts),
        Err(sparkles_core::Error::BudgetExceeded(_))
    ));
    assert_eq!(counts.create.load(Ordering::SeqCst), 1);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 0);
    opts.max_memory_bytes = None;
    assert_eq!(run(&store, &text, &opts), vec![vec![integer(128)]]);
}

struct FactoryOwner(Arc<AtomicUsize>);
impl Drop for FactoryOwner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl AggregateFactory for FactoryOwner {
    fn create(
        &self,
        _: &AggregateContext<'_>,
    ) -> Result<Box<dyn AggregateAccumulator>, ScalarError> {
        Ok(Box::new(Counting {
            count: 0,
            counts: Default::default(),
        }))
    }
}

#[test]
fn query_result_retains_factory_registry_until_it_closes() {
    let store = Store::in_memory(StoreOptions::default());
    let dropped = Arc::new(AtomicUsize::new(0));
    let opts = options(FactoryOwner(dropped.clone()));
    let result = query(store.snapshot(), &source("VALUES ?v {1 2}"), &opts).unwrap();
    drop(opts);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert_eq!(result.rows(), vec![vec![integer(2)]]);
    drop(result);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn argument_callbacks_continue_after_domain_failure_without_finishing_the_group() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_aggregate(AggregateDescriptor::new(IRI, 1, factory(counts.clone(), 0)).unwrap())
        .unwrap();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:test:domain",
                1..=1,
                move |_: &ScalarContext<'_>, args: &[Term]| {
                    observed.fetch_add(1, Ordering::SeqCst);
                    if args[0] == Literal::from(0).into() {
                        Err(ScalarError::Expression)
                    } else {
                        Ok(args[0].clone())
                    }
                },
            )
            .unwrap(),
        )
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let text = format!("SELECT (AGG <{IRI}>(<urn:test:domain>(?v)) AS ?n) {{VALUES ?v {{0 1 2}}}}");
    assert_eq!(run(&store, &text, &opts), vec![vec![None]]);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(counts.add.load(Ordering::SeqCst), 0);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 0);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
}

#[test]
fn large_eligible_builtin_arguments_are_not_precomputed_for_registered_aggregates() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let mut registry = ExtensionRegistry::builder();
    let mut descriptor = AggregateDescriptor::new(IRI, 1, factory(counts.clone(), 0)).unwrap();
    descriptor.volatility = sparkles_core::sparql::extensions::Volatility::Immutable;
    descriptor.description = Some("application description is private".into());
    registry.register_aggregate(descriptor).unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let values = std::iter::repeat_n("1", 256).collect::<Vec<_>>().join(" ");
    let text = format!("SELECT (AGG <{IRI}>(STR(?v)) AS ?n) {{VALUES ?v {{{values}}}}}");
    let plan = serde_json::to_string(&explain(store.snapshot(), &text, &opts).unwrap().1).unwrap();
    assert!(plan.contains("aggregate Immutable") && plan.contains("singleton batch"));
    assert!(!plan.contains("application description"));
    assert_eq!(counts.create.load(Ordering::SeqCst), 0);
    let result = query(store.snapshot(), &text, &opts).unwrap();
    assert_eq!(result.rows(), vec![vec![integer(256)]]);
    assert_eq!(counts.add.load(Ordering::SeqCst), 256);
    assert!(!result.ctx.opt.expr_cache);
    let plan = serde_json::to_string(&result.plan).unwrap();
    assert!(!plan.contains("exprCacheHits") && !plan.contains("once per"));
}

#[test]
fn successful_factory_rejected_after_cancellation_still_has_guarded_panicking_drop() {
    bounded(|| {
        let store = Store::in_memory(StoreOptions::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let drops = Arc::new(AtomicUsize::new(0));
        let observed = drops.clone();
        let mut opts = options(move |_: &AggregateContext<'_>| {
            cancel.store(true, Ordering::SeqCst);
            Ok(Box::new(Fault {
                stage: Stage::Drop,
                panic: true,
                drops: observed.clone(),
            }) as Box<dyn AggregateAccumulator>)
        });
        opts.cancel = Some(cancelled);
        assert!(matches!(
            query(store.snapshot(), &source("VALUES ?v {1}"), &opts),
            Err(sparkles_core::Error::Cancelled)
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn domain_failed_distinct_group_does_not_retain_unused_dedup_state() {
    let store = Store::in_memory(StoreOptions::default());
    let counts = Arc::new(Counts::default());
    let mut opts = options(factory(counts.clone(), 0));
    opts.max_memory_bytes = Some(4096);
    let values = (0..128)
        .map(|n| n.to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let text = format!("SELECT (AGG <{IRI}>(DISTINCT ?v) AS ?n) {{VALUES ?v {{UNDEF {values}}}}}");
    assert_eq!(run(&store, &text, &opts), vec![vec![None]]);
    assert_eq!(counts.add.load(Ordering::SeqCst), 0);
    assert_eq!(counts.finish.load(Ordering::SeqCst), 0);
    assert_eq!(counts.drop.load(Ordering::SeqCst), 1);
}

#[test]
fn aggregate_identity_preserves_received_composite_blank_nodes() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA { _:stored <urn:p> 1 . <urn:s> <urn:list> \"[_:stored]\"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> }",
        &Default::default(),
    ).unwrap();
    let expected = run(
        &store,
        "SELECT ?v { <urn:s> <urn:list> ?v }",
        &Default::default(),
    );
    let o = options(|_: &AggregateContext<'_>| {
        Ok(Box::new(Last {
            value: None,
            returned: None,
        }) as Box<dyn AggregateAccumulator>)
    });
    assert_eq!(run(&store, &source("<urn:s> <urn:list> ?v"), &o), expected);
    let forged = expected[0][0].clone().unwrap();
    let o = options(move |_: &AggregateContext<'_>| {
        Ok(Box::new(Last {
            value: None,
            returned: Some(forged.clone()),
        }) as Box<dyn AggregateAccumulator>)
    });
    assert_ne!(run(&store, &source("VALUES ?v {1}"), &o), expected);
}
