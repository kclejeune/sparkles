use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, FallbackPolicy, QueryCursor, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

fn store(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..n)
        .map(|i| format!("<urn:s:{i}> <urn:p> {i} .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn options(rows: usize) -> CursorOptions {
    CursorOptions {
        batch_rows: rows,
        fallback: FallbackPolicy::RejectMaterialization,
        ..Default::default()
    }
}

fn open(s: &Store, q: &str, rows: usize) -> QueryCursor {
    select_cursor(s.snapshot(), q, &QueryOptions::default(), &options(rows))
        .unwrap_or_else(|e| panic!("{q}: {e}"))
}

fn all(mut c: QueryCursor) -> Vec<Vec<Option<Term>>> {
    let mut answer = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        for row in 0..batch.len() {
            answer.push(batch.row(row).unwrap());
        }
    }
    assert_eq!(c.status(), CursorStatus::Complete);
    answer
}

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut rows = rows
        .into_iter()
        .map(|r| format!("{r:?}"))
        .collect::<Vec<_>>();
    rows.sort();
    rows
}

#[test]
fn inline_and_dictionary_scalars_match_eager_across_batches() {
    let s = store(0);
    let queries = [
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?x (?x + 1 AS ?plus) (ABS(?x) AS ?abs) (isNumeric(?x) AS ?numeric) WHERE { VALUES ?x { -19 3.25 4e0 true \"0003\"^^xsd:integer \"invalid\"^^xsd:integer } }",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?x (YEAR(?x) AS ?year) WHERE { VALUES ?x { \"2030-01-02\"^^xsd:date \"2031-02-03T04:05:06Z\"^^xsd:dateTime \"invalid\"^^xsd:date } }",
    ];
    for q in queries {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for rows in [1, 3, 4096] {
            assert_eq!(bag(all(open(&s, q, rows))), expected, "{q}; batch {rows}");
        }
    }
}

#[test]
fn computed_id_reuse_respects_budgets_options_and_impure_expressions() {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..10_000)
        .map(|i| format!("<urn:s:{i}> <urn:p> {} .\n", i % 7))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "SELECT ?s ?x WHERE { ?s <urn:p> ?o BIND(FLOOR(?o / 2) AS ?x) }";
    let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
    for (memory, reuse) in [(2 << 20, true), (2 << 20, false), (256 << 10, true)] {
        let opts = QueryOptions {
            max_memory_bytes: Some(memory),
            optimizations: Some(sparkles_core::sparql::Optimizations {
                expr_cache: reuse,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
        let mut rows = Vec::new();
        while let Some(b) = c.next_batch().unwrap() {
            for i in 0..b.len() {
                rows.push(b.row(i).unwrap());
            }
        }
        assert_eq!(bag(rows), expected);
        assert!(c.stats().mem_peak_bytes <= memory);
    }
    let rows = all(open(
        &s,
        "SELECT (BNODE() AS ?fresh) WHERE { ?s <urn:p> ?o }",
        4096,
    ));
    let nodes = rows
        .iter()
        .map(|r| r[0].clone())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(nodes.len(), 10_000);
}

#[test]
fn batch_filter_reuse_preserves_term_errors_local_values_and_budget_fallback() {
    let s = Store::in_memory(StoreOptions::default());
    let terms = [
        "\"Ada Lovelace\"",
        "\"Ada\"@en",
        "\"Ada\"@fr",
        "\"Adá\"",
        "<urn:Ada>",
        "7",
        "\"Ada\"^^<urn:datatype>",
    ];
    let data = (0..10_000)
        .map(|i| format!("<urn:s:{i}> <urn:p> {} .\n", terms[i % terms.len()]))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for body in [
        "?s <urn:p> ?o FILTER(CONTAINS(?o, \"Ada\"))",
        "?s <urn:p> ?o FILTER(CONTAINS(?o, \"Ada\"@en))",
        "?s <urn:p> ?o FILTER(STRSTARTS(STR(?o), \"Ada\") && STRENDS(STR(?o), \"Ada\"))",
        "?s <urn:p> ?o FILTER(LANGMATCHES(LANG(?o), \"en\"))",
        "?s <urn:p> ?o FILTER(REGEX(STR(?o), \"Ada\"))",
        "?s <urn:p> ?original BIND(CONCAT(STR(?original), \" suffix\") AS ?o) FILTER(CONTAINS(?o, \"Ada\"))",
    ] {
        let q = format!("SELECT ?s ?o WHERE {{ {body} }}");
        let expected = bag(query(s.snapshot(), &q, &Default::default()).unwrap().rows());
        for (memory, reuse, batch) in [
            (2 << 20, true, 4096),
            (2 << 20, false, 4096),
            (256 << 10, true, 4096),
            (2 << 20, true, 1031),
        ] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: reuse,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), &q, &opts, &options(batch)).unwrap();
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for i in 0..b.len() {
                    rows.push(b.row(i).unwrap());
                }
            }
            assert_eq!(bag(rows), expected, "{q}: {memory}/{reuse}/{batch}");
            assert!(c.stats().mem_peak_bytes <= memory);
        }
    }
}

#[test]
fn buffered_key_filters_preserve_batches_pending_rows_and_controls() {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..40_001)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:p> \"{} {i}\" .\n",
                if i % 4 == 0 { "Ada" } else { "Other" }
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for needle in ["", "Ada"] {
        let q = format!("SELECT ?s ?o {{ ?s <urn:p> ?o FILTER(CONTAINS(?o, \"{needle}\")) }}");
        let expected = bag(query(s.snapshot(), &q, &Default::default()).unwrap().rows());
        for (memory, rows, bytes) in [
            (64 << 20, 4096, 1 << 20),
            (12 << 20, 4096, 1 << 20),
            (1 << 20, 4096, 64 << 10),
            (64 << 20, 1007, 16 << 10),
        ] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let cursor_options = CursorOptions {
                batch_rows: rows,
                batch_bytes: bytes,
                ..options(rows)
            };
            let mut c = select_cursor(s.snapshot(), &q, &opts, &cursor_options).unwrap();
            let mut answer = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                assert!(b.len() <= rows);
                assert!(b.len() * b.width() * 8 <= bytes);
                answer.extend((0..b.len()).map(|i| b.row(i).unwrap()));
            }
            assert_eq!(bag(answer), expected);
            assert!(c.stats().mem_peak_bytes <= memory);
        }
    }
    let q = "SELECT ?s ?o { ?s <urn:p> ?o FILTER(CONTAINS(?o, \"\")) }";
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
    let batch = c.next_batch().unwrap().unwrap();
    assert_eq!(batch.len(), 4096);
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(c.next_batch().unwrap().is_none());
    assert!(batch.term(0, 1).unwrap().is_some());

    let opts = QueryOptions {
        max_rows_produced: Some(4),
        ..Default::default()
    };
    let limited = format!("{q} LIMIT 1");
    let mut c = select_cursor(s.snapshot(), &limited, &opts, &options(4096)).unwrap();
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 1);
    assert!(c.next_batch().unwrap().is_none());
    assert!(c.stats().rows_produced <= 4);
}

#[test]
fn merge_interiors_preserve_gaps_duplicates_boundaries_and_shared_compatibility() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..12_001 {
        if i % 3 != 0 || (4000..4100).contains(&i) {
            data.push_str(&format!("<urn:s:{i:05}> <urn:l> {i} .\n"));
            if i % 4093 == 0 {
                data.push_str(&format!("<urn:s:{i:05}> <urn:l> {} .\n", i + 1));
            }
        }
        if i % 4 != 0 && !(5000..7000).contains(&i) {
            data.push_str(&format!("<urn:s:{i:05}> <urn:r> {i} .\n"));
            if i % 4091 == 0 {
                data.push_str(&format!("<urn:s:{i:05}> <urn:r> {} .\n", i + 2));
            }
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?l ?r { ?s <urn:l> ?l; <urn:r> ?r }",
        "SELECT ?s ?l ?r { ?s <urn:l> ?l OPTIONAL { ?s <urn:r> ?r } }",
        "SELECT ?s ?l ?r { ?s <urn:r> ?r OPTIONAL { ?s <urn:l> ?l } }",
        "SELECT ?s ?x { ?s <urn:l> ?x; <urn:r> ?x }",
        "SELECT ?s ?x { ?s <urn:l> ?x OPTIONAL { ?s <urn:r> ?x } }",
    ] {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for batch in [1, 7, 4096] {
            assert_eq!(bag(all(open(&s, q, batch))), expected, "{q}/{batch}");
        }
    }
}

#[test]
fn topk_partial_date_order_preserves_eager_results_across_chunks() {
    let s = store(0);
    let values = [
        "\"2020-01-02T00:00:00+14:00\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
        "\"2020-01-01T15:00:00\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
        "\"2020-01-01T12:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
    ];
    for [a, b, c] in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let q = format!(
            "SELECT ?v {{VALUES ?v {{ {} {} {} {} }} }} ORDER BY ?v LIMIT 1",
            values[a], values[a], values[b], values[c]
        );
        let expected = query(s.snapshot(), &q, &Default::default()).unwrap().rows();
        let cursor_options = CursorOptions {
            batch_bytes: 16,
            fallback: FallbackPolicy::AllowMaterialization,
            ..options(7)
        };
        let actual =
            all(select_cursor(s.snapshot(), &q, &Default::default(), &cursor_options).unwrap());
        assert_eq!(actual, expected, "{q}");
    }
}

#[test]
fn batch_filter_does_not_cache_application_callbacks() {
    use sparkles_core::sparql::extensions::{
        ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarError,
    };
    use std::sync::atomic::AtomicUsize;
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..10_000)
        .map(|i| format!("<urn:s:{i}> <urn:p> \"same\" .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:counted",
                1..=1,
                move |_: &ScalarContext<'_>, _: &[Term]| -> Result<Term, ScalarError> {
                    counted.fetch_add(1, Ordering::Relaxed);
                    Ok(oxrdf::Literal::from(true).into())
                },
            )
            .unwrap(),
        )
        .unwrap();
    let opts = QueryOptions {
        extensions: Some(registry.build()),
        ..Default::default()
    };
    let q = "SELECT ?s WHERE { ?s <urn:p> ?o FILTER(<urn:counted>(?o)) }";
    assert_eq!(
        all(select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap()).len(),
        10_000
    );
    assert_eq!(calls.load(Ordering::Relaxed), 10_000);
}

#[test]
fn key_filters_borrow_large_delta_terms_without_decoding_them() {
    use sparkles_core::id::{Id, Tag};
    let s = Store::in_memory(StoreOptions::default());
    let mut tx = s.write();
    let p = tx
        .intern(&oxrdf::NamedNode::new_unchecked("urn:p").into())
        .unwrap();
    let object = tx
        .intern(
            &oxrdf::Literal::new_simple_literal(format!("needle{}", "x".repeat(256 << 10))).into(),
        )
        .unwrap();
    assert_eq!(object.tag(), Tag::Delta);
    for i in 0..1024 {
        let subject = tx
            .intern(&oxrdf::NamedNode::new_unchecked(format!("urn:s:{i}")).into())
            .unwrap();
        tx.insert([subject, p, object, Id::DEFAULT_GRAPH]).unwrap();
    }
    tx.commit().unwrap();
    let opts = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER(CONTAINS(?o, \"needle\")) }",
        &opts,
        &options(4096),
    )
    .unwrap();
    let mut rows = 0;
    while let Some(b) = c.next_batch().unwrap() {
        rows += b.len();
    }
    assert_eq!(rows, 1024);
    assert!(c.stats().mem_peak_bytes <= 1 << 20);
    let mut terms = select_cursor(
        s.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    )
    .unwrap();
    let batch = terms.next_batch().unwrap().unwrap();
    assert!(
        matches!(batch.term(0, 0), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
}

#[test]
fn scans_emit_before_exhaustion_and_limit_does_not_replay() {
    let s = store(10_000);
    let mut c = open(&s, "SELECT ?s ?o WHERE { ?s <urn:p> ?o }", 7);
    assert!(!c.plan().has_materialization());
    assert_eq!(c.next_batch().unwrap().unwrap().len(), 7);
    assert_eq!(c.status(), CursorStatus::Open);
    assert!(c.stats().rows_produced < 100);
    let mut limited = open(&s, "SELECT ?s WHERE { ?s <urn:p> ?o } LIMIT 1", 4096);
    assert_eq!(limited.next_batch().unwrap().unwrap().len(), 1);
    assert_eq!(limited.status(), CursorStatus::Complete);
    assert!(limited.stats().rows_produced < 10);
    assert!(limited.next_batch().unwrap().is_none());
}

#[test]
fn bags_match_for_filters_bind_offsets_range_and_union() {
    let s = store(97);
    let queries = [
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o }",
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o FILTER(?o >= 12 && ?o < 85) }",
        "SELECT ?s ?label WHERE { ?s <urn:p> ?o BIND(CONCAT(STR(?s), STR(?o)) AS ?label) }",
        "SELECT ?s ?x WHERE { ?s <urn:p> ?o BIND(1 / 0 AS ?x) FILTER(?o != 12) }",
        "SELECT ?s WHERE { ?s <urn:p> ?o } OFFSET 15 LIMIT 31",
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER(?o > 100000) }",
        "SELECT ?x WHERE { { VALUES ?x { 1 1 UNDEF 2 } } UNION { VALUES ?x { 1 3 } } }",
        "SELECT ?x ?y WHERE { { VALUES ?x { 1 2 } } UNION { VALUES ?y { 3 4 } } }",
        "SELECT ?x WHERE { VALUES ?x { } }",
    ];
    for q in queries {
        let eager = query(s.snapshot(), q, &Default::default()).unwrap();
        for rows in [1, 3, 17] {
            assert_eq!(
                bag(eager.rows()),
                bag(all(open(&s, q, rows))),
                "{q}, batch {rows}"
            );
        }
    }
}

#[test]
fn graph_union_dedup_and_repeated_variables_survive_boundaries() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        br#"
      <urn:g:a> { <urn:a> <urn:p> <urn:a> . <urn:b> <urn:p> <urn:b> . }
      <urn:g:b> { <urn:a> <urn:p> <urn:a> . <urn:b> <urn:p> <urn:c> . }
    "#
        .to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    sparkles_core::sparql::update::update(&s, "DELETE DATA { GRAPH <urn:g:a> { <urn:b> <urn:p> <urn:b> } }; INSERT DATA { GRAPH <urn:g:b> { <urn:z> <urn:p> <urn:z> } }", &Default::default()).unwrap();
    for q in [
        "SELECT ?s FROM <urn:g:a> FROM <urn:g:b> WHERE { ?s <urn:p> ?o }",
        "SELECT ?s FROM <urn:g:a> FROM <urn:g:b> WHERE { ?s <urn:p> ?s }",
        "SELECT ?s WHERE { GRAPH <urn:g:b> { ?s <urn:p> ?s } }",
    ] {
        assert_eq!(
            bag(query(s.snapshot(), q, &Default::default()).unwrap().rows()),
            bag(all(open(&s, q, 1)))
        );
    }
    // GRAPH ?g currently plans a join to eligible graph names, so it exercises
    // the explicitly allowed join fallback until the join cursor phase lands.
    let q = "SELECT ?g ?s WHERE { GRAPH ?g { ?s <urn:p> ?s } }";
    let cursor = select_cursor(
        s.snapshot(),
        q,
        &Default::default(),
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(cursor.plan().has_materialization());
    assert_eq!(
        bag(all(cursor)),
        bag(query(s.snapshot(), q, &Default::default()).unwrap().rows())
    );
}

#[test]
fn zero_column_duplicates_and_first_appearance_order() {
    let s = store(0);
    let q = "SELECT * WHERE { VALUES (?z ?a) { (1 2) (1 2) } }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    let c = open(&s, q, 1);
    assert_eq!(c.variables(), eager.vars);
    assert_eq!(all(c), eager.rows());
    assert_eq!(
        all(open(&s, "SELECT * WHERE { {} UNION {} }", 1)),
        vec![vec![], vec![]]
    );
}

#[test]
fn initial_bindings_are_restored_and_snapshot_is_pinned() {
    let s = store(5);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let opts = QueryOptions {
        initial_bindings: vec![("o".into(), oxrdf::Literal::from(2).into())],
        ..Default::default()
    };
    let expected = query(s.snapshot(), q, &opts).unwrap().rows();
    let c = select_cursor(s.snapshot(), q, &opts, &options(1)).unwrap();
    sparkles_core::sparql::update::update(&s, "CLEAR ALL", &Default::default()).unwrap();
    assert_eq!(all(c), expected);
}

#[test]
fn batches_decode_local_terms_after_close_and_store_drop() {
    let s = store(5);
    let mut c = open(
        &s,
        "SELECT ?x WHERE { ?s <urn:p> ?o BIND(CONCAT(STR(?s), \"!\") AS ?x) }",
        1,
    );
    let b = c.next_batch().unwrap().unwrap();
    c.close();
    c.close();
    assert_eq!(c.status(), CursorStatus::Stopped);
    drop(c);
    drop(s);
    let value = b.term(0, 0).unwrap().unwrap();
    assert!(matches!(value, Term::Literal(l) if l.value().ends_with('!')));
}

#[test]
fn close_does_not_cancel_a_shared_flag_and_cursor_can_move_threads() {
    fn send<T: Send>() {}
    send::<QueryCursor>();
    let s = store(10);
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(2),
    )
    .unwrap();
    std::thread::spawn(move || {
        let mut c = c;
        c.next_batch().unwrap();
        c.close();
    })
    .join()
    .unwrap();
    assert!(!cancel.load(Ordering::Relaxed));
}

#[test]
fn cancellation_is_terminal_and_collection_cannot_claim_partial_success() {
    let s = store(10);
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    )
    .unwrap();
    drop(c.next_batch().unwrap());
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(c.next_batch().unwrap().is_none());
    assert!(c.stats().error.is_some());
    assert!(c.collect().is_err());
    let mut partial = open(&s, "SELECT ?s WHERE { ?s <urn:p> ?o }", 1);
    partial.next_batch().unwrap();
    assert!(partial.collect().is_err());
}

#[test]
fn deadlines_include_consumer_wait() {
    let s = store(10);
    let opts = QueryOptions {
        timeout: Some(Duration::from_millis(100)),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    )
    .unwrap();
    c.next_batch().unwrap();
    std::thread::sleep(Duration::from_millis(110));
    assert!(matches!(c.next_batch(), Err(Error::Timeout)));
}

#[test]
fn row_limits_are_cumulative_and_work_does_not_depend_on_batch_size() {
    let s = store(10);
    let q = "SELECT ?s WHERE { ?s <urn:p> ?o }";
    let opts = QueryOptions {
        max_rows: Some(4),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts, &options(1)).unwrap();
    for _ in 0..4 {
        c.next_batch().unwrap();
    }
    assert!(matches!(c.next_batch(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Rows));
    let mut work = Vec::new();
    for cap in [1, 2, 7] {
        let mut c = open(&s, q, cap);
        while c.next_batch().unwrap().is_some() {}
        work.push(c.stats().rows_produced);
    }
    assert!(work.iter().all(|n| *n == work[0]), "{work:?}");
}

#[test]
fn retained_batches_spend_the_shared_memory_budget() {
    let s = store(1000);
    let opts = QueryOptions {
        max_memory_bytes: Some(16 << 10),
        ..Default::default()
    };
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let mut c = select_cursor(s.snapshot(), q, &opts, &options(8)).unwrap();
    let mut held = Vec::new();
    loop {
        match c.next_batch() {
            Ok(Some(b)) => held.push(b),
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory);
                break;
            }
            _ => panic!("retained batches should exhaust the budget"),
        }
    }
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(held.len() > 2);
    drop(held);
    let mut c = select_cursor(s.snapshot(), q, &opts, &options(8)).unwrap();
    while c.next_batch().unwrap().is_some() {}
    assert_eq!(c.status(), CursorStatus::Complete);
}

#[test]
fn generated_vocabulary_is_budgeted_even_with_single_row_batches() {
    let s = store(500);
    let opts = QueryOptions {
        max_memory_bytes: Some(16 << 10),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?x WHERE { ?s <urn:p> ?o BIND(CONCAT(STR(?s), \"-generated\") AS ?x) }",
        &opts,
        &options(1),
    )
    .unwrap();
    let mut n = 0;
    loop {
        match c.next_batch() {
            Ok(Some(_)) => n += 1,
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory);
                break;
            }
            _ => panic!("unique terms should exhaust the budget"),
        }
    }
    assert!(n > 0 && n < 500);
}

#[test]
fn large_term_decode_fails_without_truncating_the_literal() {
    let s = Store::in_memory(StoreOptions::default());
    let data = format!("<urn:s> <urn:p> \"{}\" .", "x".repeat(32 << 10));
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NTriples,
        None,
    )])
    .unwrap();
    let opts = QueryOptions {
        max_memory_bytes: Some(16 << 10),
        ..Default::default()
    };
    let c = select_cursor(
        s.snapshot(),
        "SELECT ?o WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    );
    // Planning may decode another key in the same front-coded block. Its scratch
    // must fail before reconstruction too, even if the requested key is small.
    match c {
        Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
        Ok(mut c) => {
            let b = c.next_batch().unwrap().unwrap();
            assert!(
                matches!(b.term(0, 0), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
            );
        }
        Err(error) => panic!("unexpected error: {error}"),
    }
}

#[test]
fn charged_regex_key_filters_cover_planning_and_count_fallbacks() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let data = (0..5000)
        .map(|i| format!("<urn:s:{i}> <urn:p> \"Ada {i}\" .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    for body in [
        "REGEX(?o, \"Ada\")",
        "REGEX(STR(?o), \"^Ada\")",
        "REGEX(?o, \"ada\", \"i\")",
    ] {
        for prefix in ["SELECT ?s ?o", "SELECT (COUNT(*) AS ?count)"] {
            let q = format!("{prefix} WHERE {{ ?s <urn:p> ?o FILTER({body}) }}");
            let expected = bag(query(s.snapshot(), &q, &Default::default()).unwrap().rows());
            for memory in [256 << 10, 16 << 20] {
                let opts = QueryOptions {
                    max_memory_bytes: Some(memory),
                    ..Default::default()
                };
                let mut c =
                    select_cursor(s.snapshot(), &q, &opts, &CursorOptions::default()).unwrap();
                let mut rows = Vec::new();
                while let Some(b) = c
                    .next_batch()
                    .unwrap_or_else(|e| panic!("{q}; {memory}: {e}"))
                {
                    for i in 0..b.len() {
                        rows.push(b.row(i).unwrap());
                    }
                }
                assert_eq!(bag(rows), expected, "{q}; {memory}");
                assert!(c.stats().mem_peak_bytes <= memory);
            }
        }
    }
}

#[test]
fn query_owned_regex_matches_eager_flags_captures_and_empty_matches() {
    let s = store(0);
    let queries = [
        r#"SELECT ?x (REGEX(?x, "adá", "i") AS ?hit) WHERE { VALUES ?x { "Adá" "ada" "αβ" "" } }"#,
        r#"SELECT ?x (REGEX(?x, ".*", "q") AS ?hit) WHERE { VALUES ?x { "a.*b" "ab" "" } }"#,
        r#"SELECT ?x (REPLACE(?x, "(a)(b)", "$2$1") AS ?out) WHERE { VALUES ?x { "abab" "βabα" "zzz" "" } }"#,
        r#"SELECT ?x (REPLACE(?x, "a*", "x") AS ?out) WHERE { VALUES ?x { "ab" "β" "" } }"#,
        r#"SELECT ?x (REPLACE(?x, "\\b", "x") AS ?out) WHERE { VALUES ?x { "ab cd" "β α" "" } }"#,
        r#"SELECT ?x (REGEX(?x, "[", "") AS ?bad) (REGEX(?x, "a", "!") AS ?flag) WHERE { VALUES ?x { "a" "b" } }"#,
    ];
    for q in queries {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for memory in [256 << 10, 8 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let c = select_cursor(s.snapshot(), q, &opts, &options(2)).unwrap();
            assert_eq!(bag(all(c)), expected, "{q}; {memory}");
        }
    }
    let opts = QueryOptions {
        max_memory_bytes: Some(32 << 10),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        r#"SELECT (REGEX("α", "\\w{1000}") AS ?x) WHERE {}"#,
        &opts,
        &options(1),
    )
    .unwrap();
    assert!(
        matches!(c.next_batch(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(c.next_batch().unwrap().is_none());
}

#[test]
fn charged_exists_state_matches_eager_with_budget_decline_and_partial_keys() {
    let s = store(5000);
    let data = (0..2500)
        .map(|i| format!("<urn:s:{}> <urn:q> {} .\n", i * 2, i % 5))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let queries = [
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER NOT EXISTS { ?s <urn:q> ?x } }",
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER EXISTS { ?s <urn:q> ?x } }",
        "SELECT ?s ?x WHERE { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?x } FILTER EXISTS { ?s <urn:q> ?x } }",
        "SELECT ?s WHERE { ?s <urn:p> ?o FILTER EXISTS { ?s <urn:q> ?x FILTER(?o > 7) } }",
    ];
    for q in queries {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for memory in [256 << 10, 8 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &opts, &CursorOptions::default()).unwrap();
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for row in 0..b.len() {
                    rows.push(b.row(row).unwrap());
                }
            }
            assert_eq!(bag(rows), expected, "{q}; {memory}");
            assert!(c.stats().mem_peak_bytes <= memory);
        }
    }
}

#[test]
fn eager_fallback_is_visible_lazy_and_budgeted() {
    let s = store(100);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o)";
    assert!(matches!(
        select_cursor(s.snapshot(), q, &Default::default(), &options(2)),
        Err(Error::Unsupported(_))
    ));
    let opts = CursorOptions {
        batch_rows: 2,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &Default::default(), &opts).unwrap();
    assert!(c.plan().has_materialization());
    assert_eq!(c.stats().rows_produced, 0);
    let b = c.next_batch().unwrap().unwrap();
    assert_eq!(
        b.row(0).unwrap(),
        query(s.snapshot(), q, &Default::default()).unwrap().rows()[0]
    );
    assert!(c.stats().rows_produced >= 100);
    assert_eq!(
        all(select_cursor(s.snapshot(), q, &Default::default(), &opts).unwrap()),
        query(s.snapshot(), q, &Default::default()).unwrap().rows()
    );
}

#[test]
fn explicit_collection_matches_eager_and_honors_its_memory_limit() {
    let s = store(97);
    let q = "SELECT ?s ?x WHERE { ?s <urn:p> ?o BIND(?o + 1 AS ?x) }";
    assert_eq!(
        bag(open(&s, q, 3).collect().unwrap().rows()),
        bag(query(s.snapshot(), q, &Default::default()).unwrap().rows())
    );
    let opts = QueryOptions {
        max_memory_bytes: Some(8 << 10),
        ..Default::default()
    };
    let s = store(1000);
    assert!(
        matches!(select_cursor(s.snapshot(), q, &opts, &options(1)).unwrap().collect(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
}

#[test]
fn non_select_and_invalid_caps_are_rejected_at_open() {
    let s = store(0);
    assert!(
        select_cursor(
            s.snapshot(),
            "ASK {}",
            &Default::default(),
            &Default::default()
        )
        .is_err()
    );
    let opts = CursorOptions {
        batch_rows: 0,
        ..Default::default()
    };
    assert!(select_cursor(s.snapshot(), "SELECT * {}", &Default::default(), &opts).is_err());
}

#[test]
fn access_views_and_plan_redaction_match_eager() {
    use sparkles_core::access::{GraphAccess, GraphRule, Graphs};
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<urn:allowed> { <urn:a> <urn:p> 1 } <urn:hidden> { <urn:b> <urn:p> 2 }".to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    let access = GraphAccess::graphs(
        Graphs::Only(GraphRule::new(["urn:allowed"], &[])),
        Graphs::All,
    );
    let opts = QueryOptions {
        graphs: Some(Arc::new(access)),
        default_graph_uris: vec!["urn:allowed".into(), "urn:hidden".into()],
        ..Default::default()
    };
    let q = "SELECT ?s WHERE { ?s <urn:p> ?o }";
    let c = select_cursor(s.snapshot(), q, &opts, &options(1)).unwrap();
    assert_eq!(c.plan().operator.estimated_rows, -1.0);
    assert_eq!(
        bag(all(c)),
        bag(query(s.snapshot(), q, &opts).unwrap().rows())
    );
}

#[test]
fn callbacks_keep_short_circuit_and_limit_counts_and_fatal_errors() {
    use sparkles_core::sparql::extensions::{
        ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarError,
    };
    use std::sync::atomic::AtomicUsize;
    let s = store(20);
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:callback",
                1..=1,
                move |_: &ScalarContext<'_>, args: &[Term]| {
                    let n = counted.fetch_add(1, Ordering::Relaxed);
                    if n == 3 {
                        Err(ScalarError::Cancelled)
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
    let q = "SELECT (<urn:callback>(?o) AS ?x) WHERE { ?s <urn:p> ?o } LIMIT 1";
    assert_eq!(
        all(select_cursor(s.snapshot(), q, &opts, &options(10)).unwrap()).len(),
        1
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    calls.store(0, Ordering::Relaxed);
    let q = "SELECT (IF(false, <urn:callback>(?o), ?o) AS ?x) WHERE { ?s <urn:p> ?o }";
    assert_eq!(
        all(select_cursor(s.snapshot(), q, &opts, &options(2)).unwrap()).len(),
        20
    );
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let q = "SELECT (<urn:callback>(?o) AS ?x) WHERE { ?s <urn:p> ?o }";
    let mut c = select_cursor(s.snapshot(), q, &opts, &options(1)).unwrap();
    for _ in 0..3 {
        c.next_batch().unwrap();
    }
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert_eq!(c.status(), CursorStatus::Failed);
}

#[test]
fn rdf_star_now_and_query_blank_nodes_survive_batches() {
    let s = store(10);
    let q = "SELECT (TRIPLE(?s, <urn:p>, ?o) AS ?triple) (NOW() AS ?now) (BNODE(\"x\") AS ?b) WHERE { ?s <urn:p> ?o }";
    let answer = all(open(&s, q, 2));
    assert_eq!(answer.len(), 10);
    for row in &answer {
        assert!(matches!(row[0], Some(Term::Triple(_))));
        assert_eq!(row[1], answer[0][1]);
        assert!(matches!(row[2], Some(Term::BlankNode(_))));
    }
    let blanks = answer
        .iter()
        .map(|r| format!("{:?}", r[2]))
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(blanks.len(), 10);
}

#[test]
fn work_budget_overflow_and_failure_do_not_wrap_shared_counters() {
    use std::sync::atomic::AtomicU64;
    let s = store(10);
    let work = Arc::new(AtomicU64::new(u64::MAX - 1));
    let opts = QueryOptions {
        work: Some(work.clone()),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    )
    .unwrap();
    assert!(
        matches!(c.next_batch(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced)
    );
    assert_eq!(work.load(Ordering::Relaxed), u64::MAX);
    let opts = QueryOptions {
        max_rows_produced: Some(3),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(1),
    )
    .unwrap();
    c.next_batch().unwrap();
    assert!(
        matches!(c.next_batch(), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced)
    );
}

#[test]
fn previously_returned_batches_remain_readable_after_producer_budget_failure() {
    let s = store(500);
    let opts = QueryOptions {
        max_memory_bytes: Some(16 << 10),
        ..Default::default()
    };
    let mut c = select_cursor(
        s.snapshot(),
        "SELECT ?x WHERE { ?s <urn:p> ?o BIND(CONCAT(STR(?s), \"!\") AS ?x) }",
        &opts,
        &options(1),
    )
    .unwrap();
    let first = c.next_batch().unwrap().unwrap();
    while matches!(c.next_batch(), Ok(Some(_))) {}
    assert_eq!(c.status(), CursorStatus::Failed);
    drop(c);
    assert!(matches!(first.term(0, 0).unwrap(), Some(Term::Literal(_))));
}

#[test]
fn standard_writers_match_eager_and_stop_at_send_without_draining() {
    use sparkles_core::sparql::results::{
        SolutionsFormat, write_cursor_solutions, write_solutions,
    };
    let s = store(97);
    let q = "SELECT ?s ?o ?missing WHERE { ?s <urn:p> ?o }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    for fmt in [
        SolutionsFormat::Json,
        SolutionsFormat::Xml,
        SolutionsFormat::Csv,
        SolutionsFormat::Tsv,
    ] {
        let mut expected = Vec::new();
        write_solutions(&eager, fmt, &mut expected, None).unwrap();
        let mut output = Vec::new();
        let mut c = open(&s, q, 3);
        let stats = write_cursor_solutions(&mut c, fmt, &mut output, None).unwrap();
        assert_eq!(output, expected, "{fmt:?}");
        assert_eq!(stats.status, CursorStatus::Complete);
        let mut output = Vec::new();
        let mut c = open(&s, q, 4096);
        let stats = write_cursor_solutions(&mut c, fmt, &mut output, Some(1)).unwrap();
        assert_eq!(stats.status, CursorStatus::Stopped);
        assert_eq!(stats.emitted_rows, 1);
        assert!(stats.rows_produced < 10);
    }
}

#[test]
fn batch_writer_decoding_matches_eager_and_declines_under_a_tight_budget() {
    use sparkles_core::sparql::results::{
        SolutionsFormat, write_cursor_solutions, write_solutions,
    };
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let terms = [
        "\"Ada\\\"\\nLovelace\"",
        "\"adá\"@en",
        "<urn:object>",
        "42",
        "\"typed\"^^<urn:datatype>",
        "_:blank",
    ];
    let data = (0..5000)
        .map(|i| format!("<urn:s:{i}> <urn:p> {} .\n", terms[i % terms.len()]))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let q = "SELECT ?s ?o ?missing WHERE { ?s <urn:p> ?o }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    for fmt in [
        SolutionsFormat::Json,
        SolutionsFormat::Xml,
        SolutionsFormat::Csv,
        SolutionsFormat::Tsv,
        SolutionsFormat::Sparkles,
    ] {
        let mut expected = Vec::new();
        write_solutions(&eager, fmt, &mut expected, None).unwrap();
        for memory in [256 << 10, 16 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
            let mut output = Vec::new();
            let stats = write_cursor_solutions(&mut c, fmt, &mut output, None).unwrap();
            assert_eq!(stats.status, CursorStatus::Complete);
            assert!(stats.mem_peak_bytes <= memory);
            if memory > 1 << 20 {
                assert!(stats.mem_peak_bytes > 1 << 20);
            }
            if fmt == SolutionsFormat::Sparkles {
                let expected: serde_json::Value = serde_json::from_slice(&expected).unwrap();
                let actual: serde_json::Value = serde_json::from_slice(&output).unwrap();
                assert_eq!(actual["rows"], expected["rows"]);
                assert_eq!(actual["vars"], expected["vars"]);
            } else {
                assert_eq!(output, expected, "{fmt:?}, {memory}");
            }
        }
    }
}

#[test]
fn writer_failures_leave_partial_output_and_fuse_production() {
    use sparkles_core::sparql::results::{SolutionsFormat, write_cursor_solutions};
    let s = store(20);
    struct Failing {
        bytes: Vec<u8>,
    }
    impl std::io::Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.len() + bytes.len() > 200 {
                return Err(std::io::Error::other("injected writer failure"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut c = open(&s, "SELECT ?s ?o WHERE { ?s <urn:p> ?o }", 1);
    let mut writer = Failing { bytes: Vec::new() };
    assert!(write_cursor_solutions(&mut c, SolutionsFormat::Json, &mut writer, None).is_err());
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(!writer.bytes.is_empty());
    assert!(serde_json::from_slice::<serde_json::Value>(&writer.bytes).is_err());
    assert!(c.next_batch().unwrap().is_none());
}

#[test]
fn native_cursor_json_has_final_metadata_and_unknown_total_when_stopped() {
    use sparkles_core::sparql::results::{NativeJsonMetadata, write_cursor_native_json};
    let s = store(19);
    let q = "SELECT ?s ?o ?blank WHERE { ?s <urn:p> ?o BIND(BNODE() AS ?blank) }";
    for send in [None, Some(0), Some(3), Some(100)] {
        let mut c = open(&s, q, 4);
        let mut output = Vec::new();
        let stats = write_cursor_native_json(
            &mut c,
            &mut output,
            send,
            Some(NativeJsonMetadata {
                commit: 42,
                dataset_id: "dataset-id",
            }),
        )
        .unwrap();
        let j: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let n = send.unwrap_or(19).min(19);
        assert_eq!(j["queryType"], "SELECT");
        assert_eq!(j["vars"], serde_json::json!(["s", "o", "blank"]));
        assert_eq!(j["rows"].as_array().unwrap().len(), n);
        assert_eq!(j["meta"]["sentRows"], n);
        assert_eq!(j["meta"]["commit"], 42);
        assert_eq!(j["meta"]["datasetId"], "dataset-id");
        assert_eq!(
            j["meta"]["totalRows"],
            if n == 19 {
                serde_json::json!(19)
            } else {
                serde_json::Value::Null
            }
        );
        assert_eq!(
            stats.status,
            if n == 19 {
                CursorStatus::Complete
            } else {
                CursorStatus::Stopped
            }
        );
        assert!(!j["meta"]["plan"]["materializes"].as_bool().unwrap());
        if n > 0 {
            assert_eq!(j["rows"][0][2]["type"], "bnode");
        }
    }
}

#[test]
fn persistent_snapshot_survives_compaction_and_store_drop() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    s.load(&[Source::from_bytes(
        b"<urn:a> <urn:p> 1 . <urn:b> <urn:p> 2 .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let mut c = open(&s, "SELECT ?s ?o { ?s <urn:p> ?o }", 1);
    let first = c.next_batch().unwrap().unwrap();
    sparkles_core::sparql::update::update(
        &s,
        "CLEAR ALL; INSERT DATA { <urn:c> <urn:p> 3 }",
        &Default::default(),
    )
    .unwrap();
    s.compact().unwrap();
    drop(s);
    let rest = all(c);
    assert_eq!(rest.len(), 1);
    assert_eq!(
        bag(vec![first.row(0).unwrap(), rest[0].clone()]),
        bag(vec![
            vec![
                Some(oxrdf::NamedNode::new_unchecked("urn:a").into()),
                Some(oxrdf::Literal::from(1).into())
            ],
            vec![
                Some(oxrdf::NamedNode::new_unchecked("urn:b").into()),
                Some(oxrdf::Literal::from(2).into())
            ]
        ])
    );
}

#[test]
fn nested_cursor_plans_pull_serialize_and_drop_on_a_small_stack() {
    std::thread::Builder::new()
        .stack_size(256 << 10)
        .spawn(|| {
            let s = store(0);
            let mut pattern = "VALUES ?x { 1 }".to_string();
            for _ in 0..80 {
                pattern = format!("{{ {pattern} }} UNION {{ VALUES ?x {{ 2 }} }}");
            }
            let q = format!("SELECT ?x {{ {pattern} }}");
            let mut c = open(&s, &q, 1);
            assert_eq!(c.next_batch().unwrap().unwrap().len(), 1);
            c.close();
            drop(c);
            let mut c = open(&s, &q, 1);
            sparkles_core::sparql::results::write_cursor_native_json(
                &mut c,
                std::io::sink(),
                Some(1),
                None,
            )
            .unwrap();
            use sparkles_core::sparql::{ExecutionMode, query_execution};
            for mode in [ExecutionMode::Eager, ExecutionMode::Auto] {
                let result = query_execution(
                    s.snapshot(),
                    &q,
                    &Default::default(),
                    &Default::default(),
                    mode,
                )
                .unwrap();
                assert_eq!(result.stats().emitted_rows, 81);
                assert!(!result.plan_json().unwrap().is_empty());
                drop(result);
            }
            let ask = query_execution(
                s.snapshot(),
                &format!("ASK {{ {pattern} }}"),
                &Default::default(),
                &Default::default(),
                ExecutionMode::Streaming,
            )
            .unwrap();
            assert!(!ask.plan_json().unwrap().is_empty());
            drop(ask);
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn incremental_distinct_preserves_unbound_and_zero_column_rows_across_batches() {
    let s = store(0);
    for q in [
        "SELECT DISTINCT ?x ?y WHERE { VALUES (?x ?y) { (1 UNDEF) (1 2) (1 UNDEF) (2 3) (1 2) (UNDEF UNDEF) (UNDEF UNDEF) } }",
        "SELECT DISTINCT ?x WHERE { { VALUES ?x { 1 2 1 } } UNION { VALUES ?x { 2 3 1 } } }",
        "SELECT DISTINCT * WHERE { {} UNION {} UNION {} }",
        "SELECT DISTINCT ?x WHERE { VALUES ?x { 1 2 1 2 3 } } LIMIT 2",
    ] {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for cap in [1, 2, 3, 4096] {
            let cursor = open(&s, q, cap);
            assert!(!cursor.plan().has_materialization());
            assert_eq!(bag(all(cursor)), expected, "{q}, cap {cap}");
        }
    }
    let s = store(5000);
    let q = "SELECT DISTINCT ?s WHERE { ?s <urn:p> ?o }";
    let opts = QueryOptions {
        max_memory_bytes: Some(32 << 10),
        ..Default::default()
    };
    let mut cursor = select_cursor(s.snapshot(), q, &opts, &options(3)).unwrap();
    let mut produced = 0;
    loop {
        match cursor.next_batch() {
            Ok(Some(batch)) => produced += batch.len(),
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory);
                break;
            }
            Err(error) => panic!("unexpected: {error}"),
            Ok(None) => panic!("DISTINCT set should exceed its memory budget"),
        }
    }
    assert!(produced > 0);
    assert!(cursor.stats().mem_peak_bytes <= 32 << 10);
    assert_eq!(cursor.status(), CursorStatus::Failed);
    assert!(cursor.next_batch().unwrap().is_none());
}

#[test]
fn retained_order_keys_match_eager_and_fail_without_partial_success() {
    let s = store(0);
    let data = (0..5000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:p> {} ; <urn:name> \"name {}\" .\n",
                i % 7,
                i % 13
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY FLOOR(?o / 2) ?s LIMIT 10",
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o * 3) LIMIT 10",
        "SELECT ?s ?o WHERE { ?s <urn:name> ?o } ORDER BY ?o ?s LIMIT 10",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        for reuse in [true, false] {
            let opts = QueryOptions {
                max_memory_bytes: Some(16 << 20),
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: reuse,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let c = select_cursor(s.snapshot(), q, &opts, &Default::default()).unwrap();
            assert_eq!(all(c), expected, "{q}; reuse {reuse}");
        }
        let opts = QueryOptions {
            max_memory_bytes: Some(32 << 10),
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), q, &opts, &Default::default()).unwrap();
        assert!(matches!(c.next_batch(), Err(Error::BudgetExceeded(_))));
        assert_eq!(c.status(), CursorStatus::Failed);
        assert_eq!(c.stats().emitted_rows, 0);
        assert!(c.next_batch().unwrap().is_none());
    }
}

#[test]
fn dictionary_numeric_topk_preserves_exact_order_and_non_numeric_fallbacks() {
    let s = store(0);
    let data = (0..5000).map(|i| {
        let term = match i % 101 {
            0 => "\"NaN\"^^<http://www.w3.org/2001/XMLSchema#double>".to_string(),
            1 => "\"2030-01-01\"^^<http://www.w3.org/2001/XMLSchema#date>".to_string(),
            2 => "\"not numeric\"".to_string(),
            _ => format!("\"{}.123456789012345678901234567890\"^^<http://www.w3.org/2001/XMLSchema#decimal>", i % 331),
        };
        format!("<urn:s:{i}> <urn:value> {term} .\n")
    }).collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?v { ?s <urn:value> ?v FILTER(?v > 1) } ORDER BY DESC(?v) LIMIT 10",
        "SELECT ?s ?v { ?s <urn:value> ?v } ORDER BY ?v LIMIT 10",
        "SELECT ?s ?v { ?s <urn:value> ?v } ORDER BY DESC(?v) LIMIT 10",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        for reuse in [true, false] {
            let opts = QueryOptions {
                max_memory_bytes: Some(16 << 20),
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: reuse,
                    ..Default::default()
                }),
                ..Default::default()
            };
            assert_eq!(
                all(select_cursor(s.snapshot(), q, &opts, &Default::default()).unwrap()),
                expected
            );
        }
    }
}

#[test]
fn resumable_binary_joins_preserve_bags_compatibility_and_optional_filters() {
    let s = store(0);
    for q in [
        "SELECT * WHERE { VALUES (?x ?a) { (1 10) (1 11) (UNDEF 12) (2 13) } VALUES (?x ?b) { (1 20) (1 21) (UNDEF 22) (3 23) } }",
        "SELECT * WHERE { VALUES ?a { 1 1 2 } VALUES ?b { 3 4 4 } }",
        "SELECT * WHERE { VALUES (?x ?a) { (1 10) (1 11) (UNDEF 12) (2 13) } OPTIONAL { VALUES (?x ?b) { (1 20) (1 21) (UNDEF 22) (3 23) } FILTER(?b > ?a + 10) } }",
        "SELECT * WHERE { VALUES ?x { 1 2 UNDEF } OPTIONAL { VALUES ?y { 3 4 } FILTER(false) } }",
        "SELECT * WHERE { VALUES ?x { 1 2 UNDEF 1 } MINUS { VALUES ?x { 1 UNDEF } } }",
        "SELECT * WHERE { VALUES ?x { 1 2 UNDEF } MINUS { VALUES ?y { 3 4 } } }",
        "SELECT * WHERE { VALUES ?x { 1 2 } VALUES ?y {} }",
        "SELECT * WHERE { VALUES ?x {} OPTIONAL { VALUES ?y { 1 2 } } }",
    ] {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for cap in [1, 2, 3, 4096] {
            let c = open(&s, q, cap);
            assert!(!c.plan().has_materialization());
            assert_eq!(bag(all(c)), expected, "{q}; cap {cap}");
        }
    }
}

#[test]
fn incremental_groups_match_eager_for_empty_unbound_numeric_and_text_inputs() {
    let s = store(0);
    for q in [
        "SELECT ?g (COUNT(*) AS ?c) (COUNT(?x) AS ?bound) (SUM(?x) AS ?sum) (AVG(?x) AS ?avg) (MIN(?x) AS ?min) (MAX(?x) AS ?max) (SAMPLE(?x) AS ?sample) WHERE { VALUES (?g ?x) { (1 2) (1 3.5) (2 UNDEF) (1 2) (2 4) (UNDEF 8) } } GROUP BY ?g",
        "SELECT ?g (MIN(?x) AS ?min) (MAX(?x) AS ?max) WHERE { VALUES (?g ?x) { (1 \"z\") (1 \"a\") (1 \"z\") (2 \"ab\"@en) (2 UNDEF) } } GROUP BY ?g",
        "SELECT (COUNT(*) AS ?c) (SUM(?x) AS ?sum) (AVG(?x) AS ?avg) (MIN(?x) AS ?min) WHERE { VALUES ?x {} }",
        "SELECT ?g (COUNT(*) AS ?c) WHERE { VALUES (?g ?x) {} } GROUP BY ?g",
    ] {
        let expected = bag(query(s.snapshot(), q, &Default::default()).unwrap().rows());
        for cap in [1, 2, 3, 4096] {
            let c = open(&s, q, cap);
            assert!(!c.plan().has_materialization());
            assert_eq!(bag(all(c)), expected, "{q}, cap {cap}");
        }
    }
}

#[test]
fn construct_cursor_deduplicates_across_batches_and_retains_snapshot() {
    use oxrdf::GraphName;
    use sparkles_core::sparql::graph_cursor;
    let s = store(40);
    let queries = [
        "CONSTRUCT { ?s <urn:q> ?o . <urn:common> <urn:p> 1 } WHERE { ?s <urn:p> ?o }",
        "CONSTRUCT { GRAPH <urn:g> { ?s <urn:q> ?o } GRAPH <urn:x-arq:DefaultGraph> { ?s <urn:q> ?o } } WHERE { ?s <urn:p> ?o }",
        "CONSTRUCT { ?s ?p ?o } WHERE { VALUES (?s ?p ?o) { (<urn:s> <urn:p> 1) (UNDEF <urn:p> 2) (1 <urn:p> 3) (<urn:s> UNDEF 4) (<urn:s> <urn:p> 1) } }",
    ];
    for q in queries {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap();
        let mut expected = expected
            .triples
            .into_iter()
            .map(|t| t.in_graph(GraphName::DefaultGraph).to_string())
            .chain(expected.quads.into_iter().map(|q| q.to_string()))
            .collect::<Vec<_>>();
        expected.sort();
        for rows in [1, 3, 4096] {
            let mut cursor =
                graph_cursor(s.snapshot(), q, &Default::default(), &options(rows)).unwrap();
            assert!(!cursor.plan().materializes);
            assert!(cursor.plan().growing_state);
            let mut got = Vec::new();
            while let Some(batch) = cursor.next_batch().unwrap() {
                assert!(batch.len() <= rows);
                got.extend(batch.quads().iter().map(|q| q.to_string()));
            }
            got.sort();
            assert_eq!(expected, got, "{q}, cap {rows}");
            assert_eq!(cursor.status(), CursorStatus::Complete);
        }
    }
    let mut cursor =
        graph_cursor(s.snapshot(), queries[0], &Default::default(), &options(1)).unwrap();
    let batch = cursor.next_batch().unwrap().unwrap();
    cursor.close();
    drop(cursor);
    drop(s);
    assert_eq!(batch.len(), 1);
    assert!(batch.quads()[0].to_string().contains("urn:"));
}

#[test]
fn graph_template_blank_nodes_remain_fresh_and_shared_within_solutions() {
    use sparkles_core::sparql::graph_cursor;
    let s = store(0);
    let q = "CONSTRUCT { _:b <urn:x> ?x ; <urn:y> ?x } WHERE { VALUES ?x { 1 1 2 } }";
    for rows in [1, 2, 4096] {
        let mut cursor =
            graph_cursor(s.snapshot(), q, &Default::default(), &options(rows)).unwrap();
        let mut groups = std::collections::HashMap::<String, usize>::new();
        while let Some(batch) = cursor.next_batch().unwrap() {
            for quad in batch.quads() {
                *groups.entry(quad.subject.to_string()).or_default() += 1;
            }
        }
        assert_eq!(groups.len(), 3);
        assert!(groups.values().all(|&n| n == 2));
    }
}

#[test]
fn graph_send_cancel_budget_and_describe_barrier_are_explicit_and_fused() {
    use sparkles_core::sparql::{cursor::graph::write_cursor_graph, graph_cursor};
    let s = store(1000);
    let q = "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }";
    let mut c = graph_cursor(s.snapshot(), q, &Default::default(), &options(4096)).unwrap();
    let mut out = Vec::new();
    let stats = write_cursor_graph(&mut c, RdfFormat::NQuads, &mut out, Some(1), &[]).unwrap();
    assert_eq!(stats.status, CursorStatus::Stopped);
    assert_eq!(stats.emitted_rows, 1);
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    assert!(c.next_batch().unwrap().is_none());
    let opts = QueryOptions {
        max_memory_bytes: Some(32 << 10),
        ..Default::default()
    };
    let mut c = graph_cursor(s.snapshot(), q, &opts, &options(1)).unwrap();
    let mut emitted = 0;
    loop {
        match c.next_batch() {
            Ok(Some(batch)) => emitted += batch.len(),
            Err(Error::BudgetExceeded(b)) => {
                assert_eq!(b.kind, BudgetKind::Memory);
                break;
            }
            other => panic!(
                "expected a charged deduplication failure, got {}",
                other.is_ok()
            ),
        }
    }
    assert!(emitted > 0);
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(c.next_batch().unwrap().is_none());
    let q = "DESCRIBE <urn:s:0>";
    assert!(matches!(
        graph_cursor(s.snapshot(), q, &Default::default(), &options(1)),
        Err(Error::Unsupported(_))
    ));
    let expected = query(s.snapshot(), q, &Default::default()).unwrap();
    let mut c = graph_cursor(
        s.snapshot(),
        q,
        &Default::default(),
        &CursorOptions {
            batch_rows: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(c.plan().materializes);
    assert!(c.plan().full_input_before_output);
    let mut got = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        got.extend(batch.quads().iter().map(|q| q.to_string()));
    }
    assert_eq!(
        got,
        expected
            .triples
            .into_iter()
            .map(|t| t.in_graph(oxrdf::GraphName::DefaultGraph).to_string())
            .collect::<Vec<_>>()
    );
    let cancel = Arc::new(AtomicBool::new(false));
    let mut c = graph_cursor(
        s.snapshot(),
        "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }",
        &QueryOptions {
            cancel: Some(cancel.clone()),
            ..Default::default()
        },
        &options(1),
    )
    .unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert!(c.next_batch().unwrap().is_none());
}

#[test]
fn streaming_ask_stops_after_a_qualifying_solution() {
    use sparkles_core::sparql::ask_streaming;
    let s = store(10000);
    for q in [
        "ASK { ?s <urn:p> ?o }",
        "ASK { ?s <urn:absent> ?o }",
        "ASK { VALUES ?x { UNDEF } }",
        "ASK {}",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().boolean;
        let r = ask_streaming(s.snapshot(), q, &Default::default(), &options(1)).unwrap();
        assert_eq!(r.boolean, expected);
        assert!(r.rows_produced < 20);
    }
}

#[test]
fn native_graph_document_and_jena_writers_stream_and_fuse_failures() {
    use sparkles_core::sparql::{QueryExecution, graph_cursor, query_cursor, results};
    let s = store(5);
    let q = "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }";
    let mut cursor = graph_cursor(s.snapshot(), q, &Default::default(), &options(1)).unwrap();
    let mut out = Vec::new();
    let stats =
        results::write_cursor_graph_native_json(&mut cursor, &mut out, Some(2), None).unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(doc["quads"].as_array().unwrap().len(), 2);
    assert_eq!(doc["quads"][0].as_array().unwrap().len(), 4);
    assert_eq!(doc["quads"][0][3], serde_json::Value::Null);
    assert_eq!(doc["meta"]["totalRows"], serde_json::Value::Null);
    assert_eq!(stats.status, CursorStatus::Stopped);
    for format in [
        sparkles_core::jena_formats::JenaFormat::TriX,
        sparkles_core::jena_formats::JenaFormat::Thrift,
        sparkles_core::jena_formats::JenaFormat::Protobuf,
        sparkles_core::jena_formats::JenaFormat::RdfJson,
    ] {
        let mut cursor = graph_cursor(s.snapshot(), q, &Default::default(), &options(1)).unwrap();
        let mut bytes = Vec::new();
        assert_eq!(
            results::write_cursor_jena_graph(&mut cursor, format, &mut bytes, None)
                .unwrap()
                .status,
            CursorStatus::Complete
        );
        assert!(!bytes.is_empty());
        struct Broken;
        impl std::io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("broken consumer"))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut cursor = graph_cursor(s.snapshot(), q, &Default::default(), &options(1)).unwrap();
        assert!(results::write_cursor_jena_graph(&mut cursor, format, Broken, None).is_err());
        assert_eq!(cursor.status(), CursorStatus::Failed);
        assert!(cursor.next_batch().unwrap().is_none());
    }
    assert!(matches!(
        query_cursor(s.snapshot(), "ASK {}", &Default::default(), &options(1)).unwrap(),
        QueryExecution::Ask(_)
    ));
}

#[test]
fn ordered_optional_unique_and_duplicate_runs_resume_across_batch_boundaries() {
    let s = store(0);
    s.load(&[Source::from_bytes(
        "<urn:s1> <urn:a> 1 . <urn:s1> <urn:a> 2 . <urn:s1> <urn:b> 3 . <urn:s1> <urn:b> 4 . <urn:s2> <urn:a> 5 . <urn:s3> <urn:a> 6 . <urn:s3> <urn:b> 7 . <urn:s4> <urn:a> 8 .".as_bytes().to_vec(),
        RdfFormat::Turtle,
        None,
    )]).unwrap();
    for q in [
        "SELECT ?s ?a ?b { ?s <urn:a> ?a OPTIONAL { ?s <urn:b> ?b } }",
        "SELECT ?s ?a { ?s <urn:a> ?a OPTIONAL { ?s <urn:b> ?a } }",
        "SELECT (COUNT(*) AS ?n) { ?s <urn:a> ?a OPTIONAL { ?s <urn:b> ?b } }",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap();
        let mut want = (0..expected.len())
            .map(|i| {
                expected
                    .table
                    .cols
                    .iter()
                    .map(|c| expected.term(c[i]).map(|t| t.to_string()))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        want.sort();
        for cap in [1, 2, 3, 4096] {
            let mut cursor =
                select_cursor(s.snapshot(), q, &Default::default(), &options(cap)).unwrap();
            let mut got = Vec::new();
            while let Some(batch) = cursor.next_batch().unwrap() {
                got.extend((0..batch.len()).map(|i| {
                    batch
                        .row(i)
                        .unwrap()
                        .into_iter()
                        .map(|t| t.map(|t| t.to_string()))
                        .collect::<Vec<_>>()
                }));
            }
            got.sort();
            assert_eq!(got, want, "{q}, cap{cap}");
        }
    }
}

#[test]
fn automatic_execution_keeps_small_and_stateful_queries_eager() {
    use sparkles_core::sparql::{ExecutionMode, QueryExecution, query_execution};
    let s = store(10);
    for q in [
        "SELECT * {?s ?p ?o}",
        "SELECT (COUNT(*) AS ?n) {?s ?p ?o}",
        "ASK {?s ?p ?o}",
        "CONSTRUCT {?s ?p ?o} WHERE {?s ?p ?o}",
    ] {
        let execution = query_execution(
            s.snapshot(),
            q,
            &Default::default(),
            &Default::default(),
            ExecutionMode::Auto,
        )
        .unwrap();
        let QueryExecution::Eager(result) = execution else {
            panic!("small query must remain eager")
        };
        let expected = query(s.snapshot(), q, &Default::default()).unwrap();
        assert_eq!(result.result().kind, expected.kind);
        assert_eq!(result.result().len(), expected.len());
        assert_eq!(result.result().boolean, expected.boolean);
    }
}

#[test]
fn strict_automatic_execution_never_hides_materialization_in_eager_mode() {
    use sparkles_core::sparql::{ExecutionMode, QueryExecution, query_execution};
    let s = store(10);
    let strict = CursorOptions {
        fallback: FallbackPolicy::RejectMaterialization,
        ..options(2)
    };
    let execution = query_execution(
        s.snapshot(),
        "SELECT * {?s ?p ?o}",
        &Default::default(),
        &strict,
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Select(cursor) = execution else {
        panic!("strict auto must use streaming even for a small scan")
    };
    assert_eq!(all(*cursor).len(), 10);
    assert!(matches!(
        query_execution(
            s.snapshot(),
            "SELECT * {?s ?p ?o} ORDER BY ?s",
            &Default::default(),
            &strict,
            ExecutionMode::Auto,
        ),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn optional_decoded_block_cache_leaves_room_for_output_under_a_tight_budget() {
    let s = store(20000);
    let opts = QueryOptions {
        max_memory_bytes: Some(1_120_000),
        ..Default::default()
    };
    let mut cursor =
        select_cursor(s.snapshot(), "SELECT * { ?s ?p ?o }", &opts, &options(4096)).unwrap();
    let mut count = 0;
    while let Some(batch) = cursor.next_batch().unwrap() {
        count += batch.len();
    }
    assert_eq!(count, 20000);
    assert!(cursor.stats().mem_peak_bytes <= 1_120_000);
}

#[test]
fn shared_scan_batches_pin_blocks_and_charge_retained_ownership() {
    let s = store(100_000);
    let opts = QueryOptions {
        max_memory_bytes: Some(2_300_000),
        ..Default::default()
    };
    let mut cursor = select_cursor(
        s.snapshot(),
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(4096),
    )
    .unwrap();
    let mut retained = Vec::new();
    loop {
        match cursor.next_batch() {
            Ok(Some(batch)) => retained.push(batch),
            Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory => break,
            _ => panic!("retained blocks must exhaust the budget"),
        }
    }
    assert_eq!(cursor.status(), CursorStatus::Failed);
    assert!(cursor.next_batch().unwrap().is_none());
    assert!(cursor.stats().mem_peak_bytes <= 2_300_000);
    assert!(retained.iter().map(|b| b.len()).sum::<usize>() > 32_768);
    let first = retained[0].row(0).unwrap();
    drop(cursor);
    drop(s);
    assert_eq!(retained[0].row(0).unwrap(), first);
    assert!(matches!(first[0], Some(Term::Literal(_))));
    assert!(matches!(first[1], Some(Term::NamedNode(_))));
}

#[test]
fn shared_scan_projection_limit_and_offset_preserve_order_and_work_limits() {
    let s = store(40_000);
    for q in [
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o } LIMIT 5000",
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o } OFFSET 7 LIMIT 5000",
        "SELECT ?o WHERE { ?s <urn:p> ?o } LIMIT 5",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        assert_eq!(all(open(&s, q, 4096)), expected);
    }
    let opts = QueryOptions {
        max_rows: Some(1),
        ..Default::default()
    };
    let mut cursor = select_cursor(
        s.snapshot(),
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o }",
        &opts,
        &options(4096),
    )
    .unwrap();
    assert!(matches!(cursor.next_batch(), Err(Error::BudgetExceeded(_))));
    assert_eq!(cursor.status(), CursorStatus::Failed);
}

#[test]
fn dictionary_filters_keep_exact_values_with_tight_budgets() {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..3000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:p> \"0{i:06}\"^^<http://www.w3.org/2001/XMLSchema#integer> .\n"
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let queries = [
        "SELECT ?s ?v {?s <urn:p> ?v FILTER(?v > 1500)}",
        "SELECT ?s ?v {?s <urn:p> ?v FILTER(?v > 1500)} ORDER BY ?v LIMIT 7",
        "SELECT ?s ?v {?s <urn:p> ?v FILTER(?v < 200 || (?v > 1500 && ?v < 2100))}",
    ];
    for (i, q) in queries.iter().enumerate() {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        for cache in [true, false] {
            for memory in if i == 0 {
                vec![2 << 20, 8 << 20]
            } else {
                vec![8 << 20]
            } {
                let opts = QueryOptions {
                    max_memory_bytes: Some(memory),
                    optimizations: Some(sparkles_core::sparql::Optimizations {
                        expr_cache: cache,
                        ..Default::default()
                    }),
                    ..Default::default()
                };
                let mut cursor =
                    select_cursor(s.snapshot(), q, &opts, &CursorOptions::default()).unwrap();
                let mut answer = Vec::new();
                while let Some(batch) = cursor.next_batch().unwrap() {
                    answer.extend((0..batch.len()).map(|row| batch.row(row).unwrap()));
                }
                assert!(cursor.stats().mem_peak_bytes <= memory);
                if i == 1 {
                    assert_eq!(answer, expected);
                } else {
                    assert_eq!(bag(answer), bag(expected.clone()));
                }
            }
        }
    }
}

#[test]
fn parallel_dictionary_order_preserves_lexical_values_and_budget_decline() {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..20_000)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:p> \"0{i}.000\"^^<http://www.w3.org/2001/XMLSchema#decimal>; <urn:w> \"word-{i:06}-é\" .\n"
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?v {?s <urn:p> ?v} ORDER BY DESC(?v) ?s LIMIT 17",
        "SELECT ?s ?v {?s <urn:p> ?v FILTER(?v > 1)} ORDER BY DESC(?v) LIMIT 17",
        "SELECT ?s ?v {?s <urn:w> ?v} ORDER BY ?v LIMIT 17",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        for memory in [8 << 20, 32 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut cursor = select_cursor(s.snapshot(), q, &opts, &Default::default()).unwrap();
            let mut rows = Vec::new();
            while let Some(batch) = cursor.next_batch().unwrap() {
                rows.extend((0..batch.len()).map(|row| batch.row(row).unwrap()));
            }
            assert_eq!(rows, expected);
            assert!(cursor.stats().mem_peak_bytes <= memory);
        }
    }
}

#[test]
fn optional_count_merges_preserve_multiplicity_with_short_and_long_key_gaps() {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..5000 {
        data.push_str(&format!("<urn:s:{i}> <urn:a> 1, 2 .\n"));
        if i % 7 == 0 || i > 4900 {
            data.push_str(&format!("<urn:s:{i}> <urn:b> 3, 4, 5 .\n"));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT (COUNT(*) AS ?n) {?s <urn:a> ?a OPTIONAL {?s <urn:b> ?b}}",
        "SELECT (COUNT(*) AS ?n) {?s <urn:b> ?b OPTIONAL {?s <urn:a> ?a}}",
    ] {
        let expected = query(s.snapshot(), q, &Default::default()).unwrap().rows();
        for cap in [1, 7, 4096] {
            assert_eq!(all(open(&s, q, cap)), expected);
        }
    }
}
