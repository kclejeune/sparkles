use oxrdf::{Literal, Term};
use sparkles_core::sparql::extensions::{
    ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarError, ScalarFunction, ScalarResult,
    Volatility,
};
use sparkles_core::sparql::{QueryOptions, explain, query};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

const IRI: &str = "urn:test:application";

fn options(callback: impl ScalarFunction) -> QueryOptions {
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(ScalarDescriptor::new(IRI, 0..=128, callback).unwrap())
        .unwrap();
    QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    }
}

fn text(s: &str) -> ScalarResult {
    Ok(Literal::from(s).into())
}

fn bounded(f: impl FnOnce() + Send + 'static) {
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        send.send(outcome).unwrap();
    });
    let outcome = receive
        .recv_timeout(std::time::Duration::from_secs(4))
        .expect("engine callback reentrancy must terminate before waiting on its own writer");
    worker.join().unwrap();
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[test]
fn registry_identity_validation_and_builtin_collisions() {
    let callback = |_: &ScalarContext<'_>, _: &[Term]| text("ok");
    assert!(ScalarDescriptor::new("relative", 0..=0, callback).is_err());
    assert!(ScalarDescriptor::new(IRI, 0..=129, callback).is_err());
    let mut builder = ExtensionRegistry::builder();
    for iri in [
        "http://www.w3.org/2001/XMLSchema#string",
        "http://www.w3.org/2005/xpath-functions#substring",
        sparkles_core::sparql::catalog::extension_aggregates()[0].as_str(),
    ] {
        assert!(
            builder
                .register_scalar(ScalarDescriptor::new(iri, 0..=0, callback).unwrap())
                .is_err(),
            "{iri}"
        );
    }
    builder
        .register_scalar(ScalarDescriptor::new(IRI, 0..=0, callback).unwrap())
        .unwrap();
    assert!(
        builder
            .register_scalar(ScalarDescriptor::new(IRI, 0..=0, callback).unwrap())
            .is_err()
    );
    let first = builder.build();
    assert_ne!(
        first.identity(),
        ExtensionRegistry::builder().build().identity()
    );
    assert_eq!(first.identity(), first.clone().identity());
}

#[test]
fn independent_registries_and_prepared_runs_keep_callbacks_isolated() {
    let store = Arc::new(Store::in_memory(StoreOptions::default()));
    let parsed = Arc::new(
        sparkles_core::sparql::parse_query(&format!("SELECT (<{IRI}>() AS ?x) {{}}"), None, &[])
            .unwrap(),
    );
    let workers = ["first", "second"].map(|expected| {
        let store = store.clone();
        let parsed = parsed.clone();
        std::thread::spawn(move || {
            let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| text(expected));
            for _ in 0..8 {
                let result =
                    sparkles_core::sparql::execute_query(store.snapshot(), &parsed, &opts, 0.0)
                        .unwrap();
                assert_eq!(result.rows()[0][0], Some(Literal::from(expected).into()));
            }
        })
    });
    for worker in workers {
        worker.join().unwrap();
    }
}

struct Lifetime(Arc<AtomicUsize>);
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl ScalarFunction for Lifetime {
    fn call(&self, _: &ScalarContext<'_>, _: &[Term]) -> ScalarResult {
        text("kept")
    }
}

#[test]
fn result_owns_registry_until_closed() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let store = Store::in_memory(StoreOptions::default());
    let opts = options(Lifetime(dropped.clone()));
    let result = query(
        store.snapshot(),
        &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
        &opts,
    )
    .unwrap();
    drop(opts);
    assert_eq!(dropped.load(Ordering::SeqCst), 0);
    assert_eq!(result.rows()[0][0], Some(Literal::from("kept").into()));
    drop(result);
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn expression_errors_arity_unbound_and_short_circuit() {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut builder = ExtensionRegistry::builder();
    builder
        .register_scalar(
            ScalarDescriptor::new(IRI, 1..=1, move |_: &ScalarContext<'_>, _: &[Term]| {
                count.fetch_add(1, Ordering::SeqCst);
                Err(ScalarError::Expression)
            })
            .unwrap(),
        )
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(builder.build()),
        ..Default::default()
    };
    let store = Store::in_memory(StoreOptions::default());
    let result = query(
        store.snapshot(),
        &format!(
            "SELECT ?a ?b ?c ?d ?e ?f ?g ?h {{
      BIND(<{IRI}>() AS ?a) BIND(<{IRI}>(?missing) AS ?b)
      BIND(IF(true, 7, <{IRI}>(1)) AS ?c)
      BIND(COALESCE(8, <{IRI}>(1)) AS ?d)
      BIND(false && <{IRI}>(1) AS ?e) BIND(true || <{IRI}>(1) AS ?f)
      BIND(<{IRI}>(1) AS ?g) BIND(<urn:unknown>(1) AS ?h)
    }}"
        ),
        &opts,
    )
    .unwrap();
    let rows = result.rows();
    let row = &rows[0];
    assert!(row[0].is_none() && row[1].is_none() && row[6].is_none() && row[7].is_none());
    assert_eq!(row[2], Some(Literal::from(7).into()));
    assert_eq!(row[3], Some(Literal::from(8).into()));
    assert_eq!(row[4], Some(Literal::from(false).into()));
    assert_eq!(row[5], Some(Literal::from(true).into()));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let result = query(
        store.snapshot(),
        &format!("SELECT ?x {{VALUES ?x {{1 2}} FILTER(<{IRI}>(?x))}}"),
        &opts,
    )
    .unwrap();
    assert!(result.is_empty());
}

#[test]
fn callback_terms_preserve_rdf12_and_cannot_forge_blank_nodes() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(&store, "INSERT DATA { _:stored <urn:p> \"bonjour\"@fr . <urn:s> <urn:t> <<( _:stored <urn:p> \"x\" )>> }", &Default::default()).unwrap();
    let opts = options(|_: &ScalarContext<'_>, args: &[Term]| Ok(args[0].clone()));
    let result = query(
        store.snapshot(),
        &format!("SELECT ?x ?y {{?s ?p ?x BIND(<{IRI}>(?x) AS ?y)}}"),
        &opts,
    )
    .unwrap();
    for row in result.rows() {
        assert_eq!(row[0], row[1]);
    }
    let result = query(
        store.snapshot(),
        &format!("SELECT ?s ?y {{?s <urn:p> ?x BIND(<{IRI}>(?s) AS ?y)}}"),
        &opts,
    )
    .unwrap();
    let stored = result.rows()[0][0].clone().unwrap();
    assert_eq!(Some(stored.clone()), result.rows()[0][1]);
    let forged = stored.clone();
    let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| Ok(forged.clone()));
    let result = query(
        store.snapshot(),
        &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
        &opts,
    )
    .unwrap();
    assert_ne!(result.rows()[0][0], Some(stored));
    assert!(matches!(&result.rows()[0][0], Some(Term::BlankNode(_))));
}

#[test]
fn unused_registry_preserves_plans_and_does_not_invoke_callbacks() {
    let store = Store::in_memory(StoreOptions::default());
    let opts = options(|_: &ScalarContext<'_>, _: &[Term]| panic!("unused registry invoked"));
    let anonymous = regex::Regex::new("[0-9a-f]{16,32}").unwrap();
    for source in [
        "SELECT ?x {VALUES ?x {1 2} FILTER(?x > 1)}",
        "ASK {VALUES ?x {1}}",
        "SELECT (COUNT(*) AS ?n) {?s ?p ?o}",
    ] {
        let plain = explain(store.snapshot(), source, &Default::default()).unwrap();
        let registered = explain(store.snapshot(), source, &opts).unwrap();
        assert_eq!(
            anonymous.replace_all(&plain.0, "anonymous"),
            anonymous.replace_all(&registered.0, "anonymous")
        );
        assert_eq!(
            anonymous.replace_all(&serde_json::to_string(&plain.1).unwrap(), "anonymous"),
            anonymous.replace_all(&serde_json::to_string(&registered.1).unwrap(), "anonymous")
        );
        assert_eq!(
            query(store.snapshot(), source, &Default::default())
                .unwrap()
                .rows(),
            query(store.snapshot(), source, &opts).unwrap().rows()
        );
    }
}

#[test]
fn callbacks_are_never_sampled_cached_or_reused_per_distinct_value() {
    let store = Store::in_memory(StoreOptions::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Literal::from(true).into())
    });
    let values = "1 ".repeat(2048);
    let source = format!("SELECT ?x {{VALUES ?x {{{values}}} FILTER(<{IRI}>(?x))}}");
    explain(store.snapshot(), &source, &opts).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    for _ in 0..2 {
        assert_eq!(query(store.snapshot(), &source, &opts).unwrap().len(), 2048);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 4096);
    let source =
        format!("SELECT ?x {{VALUES ?x {{1 1 1}} FILTER EXISTS {{ FILTER(<{IRI}>(?x)) }} }}");
    assert_eq!(query(store.snapshot(), &source, &opts).unwrap().len(), 3);
    assert_eq!(calls.load(Ordering::SeqCst), 4099);
}

struct BadCardinality;
impl ScalarFunction for BadCardinality {
    fn call(&self, _: &ScalarContext<'_>, _: &[Term]) -> ScalarResult {
        text("unused")
    }
    fn call_batch(&self, _: &ScalarContext<'_>, _: &[Vec<Term>]) -> Vec<ScalarResult> {
        Vec::new()
    }
}

#[test]
fn protocol_panics_and_execution_errors_abort_all_result_shapes() {
    let store = Store::in_memory(StoreOptions::default());
    for opts in [
        options(BadCardinality),
        options(|_: &ScalarContext<'_>, _: &[Term]| panic!("private panic content")),
        options(|_: &ScalarContext<'_>, _: &[Term]| Err(ScalarError::Execution("failure".into()))),
    ] {
        for source in [
            format!("SELECT ?x {{BIND(COALESCE(<{IRI}>(), 1) AS ?x)}}"),
            format!("ASK {{FILTER(<{IRI}>())}}"),
            format!("CONSTRUCT {{<urn:s> <urn:p> ?x}} {{BIND(<{IRI}>() AS ?x)}}"),
            format!("SELECT ?x {{VALUES ?x {{1}} FILTER(<{IRI}>())}}"),
        ] {
            assert!(query(store.snapshot(), &source, &opts).is_err(), "{source}");
        }
        let source = format!(
            "INSERT DATA {{<urn:s> <urn:before> 1}}; INSERT {{<urn:s> <urn:after> ?x}} WHERE {{BIND(COALESCE(<{IRI}>(), 1) AS ?x)}}"
        );
        assert!(sparkles_core::sparql::update::update(&store, &source, &opts).is_err());
        assert!(
            query(store.snapshot(), "SELECT * {?s ?p ?o}", &Default::default())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn cancellation_and_suppressed_budget_failure_are_sticky() {
    let store = Store::in_memory(StoreOptions::default());
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = cancelled.clone();
    let mut opts = options(move |ctx: &ScalarContext<'_>, _: &[Term]| {
        cancel.store(true, Ordering::SeqCst);
        let _ = ctx.check();
        text("ignored")
    });
    opts.cancel = Some(cancelled);
    assert!(matches!(
        query(
            store.snapshot(),
            &format!("ASK {{FILTER(<{IRI}>())}}"),
            &opts
        ),
        Err(sparkles_core::Error::Cancelled)
    ));
    let mut opts = options(|ctx: &ScalarContext<'_>, _: &[Term]| {
        let _ = ctx.charge(4096);
        text("ignored")
    });
    opts.max_memory_bytes = Some(1024);
    assert!(matches!(
        query(
            store.snapshot(),
            &format!("SELECT ?x {{BIND(COALESCE(<{IRI}>(), 1) AS ?x)}}"),
            &opts
        ),
        Err(sparkles_core::Error::BudgetExceeded(_))
    ));
}

#[test]
fn same_family_callback_operations_fail_under_a_transaction_and_recover() {
    for operation in 0..7 {
        bounded(move || {
            let store = Arc::new(Store::in_memory(StoreOptions::default()));
            let captured = store.clone();
            let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
                // Catching an infallible API's private refusal cannot clear the fatal flag.
                let _ =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match operation {
                        0 => {
                            captured.head_commit();
                        }
                        1 => {
                            captured.snapshot();
                        }
                        2 => {
                            let _ = captured.write();
                        }
                        3 => {
                            let _ =
                                captured.clone_to_memory(&Default::default(), Default::default());
                        }
                        4 => {
                            let _ = captured.create_branch("child", &Default::default());
                        }
                        5 => {
                            let _ = captured
                                .materialized_backup_capture("captured", &Default::default());
                        }
                        _ => {
                            let _ = captured.preview_merge("main", "main", &Default::default());
                        }
                    }));
                text("suppressed")
            });
            let transaction = store.write();
            let result = query(
                Arc::new(transaction.view()),
                &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
                &opts,
            );
            assert!(result.is_err(), "operation {operation}");
            drop(transaction);
            sparkles_core::sparql::update::update(
                &store,
                "INSERT DATA {<urn:s> <urn:p> 1}",
                &Default::default(),
            )
            .unwrap();
        });
    }
}

#[test]
fn held_other_family_is_inherited_by_callback_workers() {
    bounded(|| {
        let a = Store::in_memory(StoreOptions::default());
        let b = Arc::new(Store::in_memory(StoreOptions::default()));
        let captured = b.clone();
        let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
            let _ = captured.try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            );
            text("suppressed")
        });
        let held = b.write();
        // Large enough to execute the scalar expression on rayon callback workers.
        let source = format!(
            "SELECT ?x {{VALUES ?n {{{}}} BIND(<{IRI}>() AS ?x)}}",
            "1 ".repeat(20_000)
        );
        assert!(query(a.snapshot(), &source, &opts).is_err());
        drop(held);
        let _recovered = b.write();
    });
}

#[test]
fn unrelated_unheld_family_query_remains_safe() {
    bounded(|| {
        let a = Store::in_memory(StoreOptions::default());
        let b = Arc::new(Store::in_memory(StoreOptions::default()));
        let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
            assert!(
                query(b.snapshot(), "ASK {}", &Default::default())
                    .unwrap()
                    .boolean
            );
            text("safe")
        });
        assert_eq!(
            query(
                a.snapshot(),
                &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
                &opts
            )
            .unwrap()
            .rows()[0][0],
            Some(Literal::from("safe").into())
        );
    });
}

#[test]
fn stable_context_and_explain_metadata() {
    let store = Store::in_memory(StoreOptions::default());
    let mut descriptor =
        ScalarDescriptor::new(IRI, 0..=0, |ctx: &ScalarContext<'_>, _: &[Term]| {
            text(&ctx.timestamp().to_string())
        })
        .unwrap();
    descriptor.volatility = Volatility::Stable;
    descriptor.description = Some("application secret not serialized".into());
    let mut builder = ExtensionRegistry::builder();
    builder.register_scalar(descriptor).unwrap();
    let opts = QueryOptions {
        extensions: Some(builder.build()),
        ..Default::default()
    };
    let source = format!("SELECT (<{IRI}>() AS ?a) (<{IRI}>() AS ?b) {{}}");
    let result = query(store.snapshot(), &source, &opts).unwrap();
    assert_eq!(result.rows()[0][0], result.rows()[0][1]);
    let info =
        serde_json::to_string(&explain(store.snapshot(), &source, &opts).unwrap().1).unwrap();
    assert!(info.contains("scalar Stable") && info.contains("singleton batch"));
    assert!(!info.contains("application secret"));
}

#[test]
fn suppressed_transaction_update_failure_cannot_commit_prior_mutations() {
    let store = Store::in_memory(StoreOptions::default());
    let opts = options(BadCardinality);
    let mut transaction = store.write();
    let source = format!(
        "INSERT DATA {{<urn:s> <urn:before> 1}}; INSERT {{<urn:s> <urn:after> ?x}} WHERE {{BIND(<{IRI}>() AS ?x)}}"
    );
    assert!(sparkles_core::sparql::update::update_in(&mut transaction, &source, &opts).is_err());
    assert!(transaction.commit().is_err());
    assert!(
        query(store.snapshot(), "SELECT * {?s ?p ?o}", &Default::default())
            .unwrap()
            .is_empty()
    );
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA {<urn:s> <urn:p> 1}",
        &Default::default(),
    )
    .unwrap();
}

#[test]
fn filter_sampling_and_filter_movement_do_not_invoke_or_drop_callbacks() {
    let store = Store::in_memory(StoreOptions::default());
    let triples: String = (0..64)
        .map(|n| format!("<urn:s{n}> <urn:p> {n} . "))
        .collect();
    sparkles_core::sparql::update::update(
        &store,
        &format!("INSERT DATA {{{triples}}}"),
        &Default::default(),
    )
    .unwrap();
    for volatility in [
        Volatility::Immutable,
        Volatility::Stable,
        Volatility::Volatile,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let mut descriptor =
            ScalarDescriptor::new(IRI, 1..=1, move |_: &ScalarContext<'_>, _: &[Term]| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(Literal::from(true).into())
            })
            .unwrap();
        descriptor.volatility = volatility;
        let mut builder = ExtensionRegistry::builder();
        builder.register_scalar(descriptor).unwrap();
        let opts = QueryOptions {
            extensions: Some(builder.build()),
            ..Default::default()
        };
        let source = format!("SELECT (COUNT(*) AS ?n) {{?s <urn:p> ?x FILTER(<{IRI}>(?x))}}");
        explain(store.snapshot(), &source, &opts).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            query(store.snapshot(), &source, &opts).unwrap().rows()[0][0],
            Some(Literal::from(64).into())
        );
        assert_eq!(calls.load(Ordering::SeqCst), 64);
        calls.store(0, Ordering::SeqCst);
        // Filtering after BIND must not move ahead of the volatile call.
        let source =
            format!("SELECT ?x {{?s <urn:p> ?x BIND(<{IRI}>(?x) AS ?called) FILTER(?x = 1)}}");
        assert_eq!(query(store.snapshot(), &source, &opts).unwrap().len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 64);
        calls.store(0, Ordering::SeqCst);
        let source = format!("SELECT ?x {{?s <urn:p> ?x FILTER(false && <{IRI}>(?x))}}");
        assert!(query(store.snapshot(), &source, &opts).unwrap().is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn invalid_and_oversized_callback_terms_are_fatal() {
    let store = Store::in_memory(StoreOptions::default());
    let opts = options(|_: &ScalarContext<'_>, _: &[Term]| {
        Ok(oxrdf::NamedNode::new_unchecked("relative").into())
    });
    assert!(
        query(
            store.snapshot(),
            &format!("SELECT ?x {{BIND(COALESCE(<{IRI}>(), 1) AS ?x)}}"),
            &opts
        )
        .is_err()
    );
    let opts =
        options(|_: &ScalarContext<'_>, _: &[Term]| Ok(Literal::from("x".repeat(1 << 20)).into()));
    assert!(
        query(
            store.snapshot(),
            &format!("ASK {{FILTER(<{IRI}>())}}"),
            &opts
        )
        .is_err()
    );
    let opts = options(|_: &ScalarContext<'_>, args: &[Term]| Ok(args[0].clone()));
    let source = format!("SELECT (<{IRI}>(\"{}\") AS ?x) {{}}", "x".repeat(1 << 20));
    assert!(query(store.snapshot(), &source, &opts).is_err());
}

#[test]
fn nested_callback_workers_inherit_ancestor_families_and_fatal_state() {
    bounded(|| {
        let a = Arc::new(Store::in_memory(StoreOptions::default()));
        let b = Arc::new(Store::in_memory(StoreOptions::default()));
        let a_callback = a.clone();
        let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
            let captured = a_callback.clone();
            let nested = options(move |_: &ScalarContext<'_>, _: &[Term]| {
                let _ = captured.try_write_with(
                    sparkles_core::commit::CommitKind::Transaction,
                    Default::default(),
                );
                text("suppressed")
            });
            let source = format!(
                "SELECT ?x {{VALUES ?n {{{}}} BIND(<{IRI}>() AS ?x)}}",
                "1 ".repeat(20_000)
            );
            let _ = query(b.snapshot(), &source, &nested);
            text("also suppressed")
        });
        let transaction = a.write();
        assert!(
            query(
                Arc::new(transaction.view()),
                &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
                &opts
            )
            .is_err()
        );
        assert!(transaction.commit().is_err());
        let _recovered = a.write();
    });
}

#[test]
fn branch_and_main_share_callback_family_identity() {
    bounded(|| {
        let main = Arc::new(Store::in_memory(StoreOptions::default()));
        main.create_branch("other", &Default::default()).unwrap();
        let branch = main.branch("other").unwrap().shared().unwrap();
        let captured = main.clone();
        let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| {
            let _ = captured.try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            );
            text("suppressed")
        });
        assert!(
            query(
                branch.snapshot(),
                &format!("SELECT (<{IRI}>() AS ?x) {{}}"),
                &opts
            )
            .is_err()
        );
    });
}

#[test]
fn callback_composite_literals_cannot_forge_stored_blank_nodes() {
    let store = Store::in_memory(StoreOptions::default());
    sparkles_core::sparql::update::update(
        &store,
        "INSERT DATA {_:stored <urn:p> 1}",
        &Default::default(),
    )
    .unwrap();
    let stored = query(
        store.snapshot(),
        "SELECT ?s {?s <urn:p> 1}",
        &Default::default(),
    )
    .unwrap()
    .rows()[0][0]
        .clone()
        .unwrap();
    let Term::BlankNode(node) = &stored else {
        panic!("expected blank node");
    };
    let forged = Literal::new_typed_literal(
        format!("[_:{}]", node.as_str()),
        oxrdf::NamedNode::new_unchecked(sparkles_core::sparql::cdt::LIST),
    );
    let opts = options(move |_: &ScalarContext<'_>, _: &[Term]| Ok(forged.clone().into()));
    let source = format!(
        "SELECT (<http://w3id.org/awslabs/neptune/SPARQL-CDTs/get>(<{IRI}>(), 1) AS ?x) {{}}"
    );
    let result = query(store.snapshot(), &source, &opts).unwrap();
    assert!(matches!(result.rows()[0][0], Some(Term::BlankNode(_))));
    assert_ne!(result.rows()[0][0], Some(stored));
}

#[test]
fn topk_does_not_duplicate_first_key_or_skip_later_callback_keys() {
    let store = Store::in_memory(StoreOptions::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let opts = options(move |_: &ScalarContext<'_>, args: &[Term]| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(args[0].clone())
    });
    let values: String = (0..64).map(|n| format!("{n} ")).collect();
    for keys in [format!("<{IRI}>(?x) ?x"), format!("?x <{IRI}>(?x)")] {
        let source = format!("SELECT ?x {{VALUES ?x {{{values}}}}} ORDER BY {keys} LIMIT 1");
        let result = query(store.snapshot(), &source, &opts).unwrap();
        assert_eq!(result.rows()[0][0], Some(Literal::from(0).into()));
        assert_eq!(calls.swap(0, Ordering::SeqCst), 64);
    }
}
