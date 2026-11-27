use oxrdf::Term;
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{
    CursorOptions, CursorStatus, FallbackPolicy, QueryCursor, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use sparkles_core::{BudgetKind, Error};
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

// The test-only reference evaluator (roadmap item 512), the oracle of the seeded
// differential queries.
#[path = "support/reference/mod.rs"]
mod reference;

// Explicit application-model copies for semantic comparisons only. Retained
// ownership regressions below keep their real OwnedTerm/OwnedRow handles.
trait ApplicationQueryRows {
    fn application_rows(&self) -> Vec<Vec<Option<Term>>>;
}
impl ApplicationQueryRows for sparkles_core::sparql::QueryResult {
    fn application_rows(&self) -> Vec<Vec<Option<Term>>> {
        self.rows()
            .expect("application comparison decode")
            .iter()
            .map(|row| row.values().to_vec())
            .collect()
    }
}

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
            answer.push(batch.row(row).unwrap().values().to_vec());
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
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
    let expected = bag(query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows());
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
                rows.push(b.row(i).unwrap().values().to_vec());
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
        let expected = bag(query(s.snapshot(), &q, &Default::default())
            .unwrap()
            .application_rows());
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
                    rows.push(b.row(i).unwrap().values().to_vec());
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
        let expected = bag(query(s.snapshot(), &q, &Default::default())
            .unwrap()
            .application_rows());
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
                answer.extend((0..b.len()).map(|i| b.row(i).unwrap().values().to_vec()));
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
    let decoded = batch.term(0, 1).unwrap().unwrap();
    let key = batch.key(0, 1).unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(c.next_batch(), Err(Error::Cancelled)));
    assert_eq!(c.status(), CursorStatus::Failed);
    assert!(c.next_batch().unwrap().is_none());
    assert!(matches!(decoded.as_term(), Term::Literal(_)));
    assert_eq!(batch.key(0, 1).unwrap(), key);
    assert!(matches!(batch.term(0, 1), Err(Error::Cancelled)));

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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
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
        let expected = query(s.snapshot(), &q, &Default::default())
            .unwrap()
            .application_rows();
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
    // Delta keys are borrowed, so retained output pays its actual copied payload,
    // not the old call-only eightfold reconstruction estimate.
    let retained = (0..3)
        .map(|_| batch.term(0, 0).unwrap().unwrap())
        .collect::<Vec<_>>();
    for term in &retained {
        let Term::Literal(value) = term.as_term() else {
            panic!("stored literal");
        };
        assert_eq!(value.value().len(), (256 << 10) + 6);
        assert!(value.value().starts_with("needle"));
        assert!(
            value.value().as_bytes()[6..]
                .iter()
                .all(|&byte| byte == b'x')
        );
    }
    assert!(
        matches!(batch.term(0, 0), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory)
    );
    drop(retained);
    assert!(batch.term(0, 0).unwrap().is_some());
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
                bag(eager.application_rows()),
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
            bag(query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows()),
            bag(all(open(&s, q, 1)))
        );
    }
    // GRAPH ?g plans a merge join to eligible graph names over sorted inputs. The
    // sorts block before output but run natively, so nothing falls back to eager.
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
    assert!(!cursor.plan().has_materialization(), "{:#?}", cursor.plan());
    assert_eq!(
        bag(all(cursor)),
        bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows())
    );
}

#[test]
fn zero_column_duplicates_and_first_appearance_order() {
    let s = store(0);
    let q = "SELECT * WHERE { VALUES (?z ?a) { (1 2) (1 2) } }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    let c = open(&s, q, 1);
    assert_eq!(c.variables(), eager.variables());
    assert_eq!(all(c), eager.application_rows());
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
    let expected = query(s.snapshot(), q, &opts).unwrap().application_rows();
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
    assert!(matches!(value.as_term(), Term::Literal(l) if l.value().ends_with('!')));
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
    // The fixed budget includes metadata and scratch for each Rayon worker.
    // Keep the parallel count path without scaling that overhead with the host.
    rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap()
        .install(|| {
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
                    let expected = bag(query(s.snapshot(), &q, &Default::default())
                        .unwrap()
                        .application_rows());
                    for memory in [256 << 10, 16 << 20] {
                        let opts = QueryOptions {
                            max_memory_bytes: Some(memory),
                            ..Default::default()
                        };
                        let mut c =
                            select_cursor(s.snapshot(), &q, &opts, &CursorOptions::default())
                                .unwrap();
                        let mut rows = Vec::new();
                        while let Some(b) = c
                            .next_batch()
                            .unwrap_or_else(|e| panic!("{q}; {memory}: {e}"))
                        {
                            for i in 0..b.len() {
                                rows.push(b.row(i).unwrap().values().to_vec());
                            }
                        }
                        assert_eq!(bag(rows), expected, "{q}; {memory}");
                        assert!(c.stats().mem_peak_bytes <= memory);
                    }
                }
            }
        });
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
        for memory in [256 << 10, 8 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &opts, &CursorOptions::default()).unwrap();
            let mut rows = Vec::new();
            while let Some(b) = c.next_batch().unwrap() {
                for row in 0..b.len() {
                    rows.push(b.row(row).unwrap().values().to_vec());
                }
            }
            assert_eq!(bag(rows), expected, "{q}; {memory}");
            assert!(c.stats().mem_peak_bytes <= memory);
        }
    }
}

/// Legacy eager-only `ViewSource` (`streams_views() == false`), refused by strict
/// cursors and used to test eager fallback visibility, laziness, and input streaming.
#[derive(Debug)]
struct LegacyViews(usize);

impl sparkles_core::sparql::viewsvc::ViewSource for LegacyViews {
    fn read_view(
        &self,
        _: &sparkles_core::sparql::viewsvc::ViewRead<'_>,
    ) -> sparkles_core::Result<sparkles_core::sparql::viewsvc::ViewRows> {
        Ok(sparkles_core::sparql::viewsvc::ViewRows {
            columns: vec!["x".into()],
            rows: (0..self.0)
                .map(|i| {
                    (
                        vec![Some(Term::Literal(oxrdf::Literal::from(i.to_string())))],
                        1,
                    )
                })
                .collect(),
            commit: 0,
            evaluated_under: None,
            lease: None,
        })
    }
}

const LEGACY_VIEW_LEAF: &str = "SERVICE <urn:x-sparkles:view#read> { [] <urn:x-sparkles:view#name> \"v\" ; <urn:x-sparkles:view#bind> ?x }";

fn legacy_view_opts(n: usize) -> QueryOptions {
    QueryOptions {
        views: Some(Arc::new(LegacyViews(n))),
        ..Default::default()
    }
}

#[test]
fn eager_fallback_is_visible_lazy_and_budgeted() {
    let s = store(100);
    let data = (0..100)
        .map(|i| format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i * 7) % 100))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = &format!("SELECT ?x WHERE {{ {LEGACY_VIEW_LEAF} }}");
    let opts_q = legacy_view_opts(100);
    assert!(matches!(
        select_cursor(s.snapshot(), q, &opts_q, &options(2)),
        Err(Error::Unsupported(_))
    ));
    let opts = CursorOptions {
        batch_rows: 2,
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &opts_q, &opts).unwrap();
    assert!(c.plan().has_materialization());
    assert_eq!(c.stats().rows_produced, 0);
    let b = c.next_batch().unwrap().unwrap();
    assert_eq!(
        b.row(0).unwrap().values().to_vec(),
        query(s.snapshot(), q, &opts_q).unwrap().application_rows()[0]
    );
    assert!(c.stats().rows_produced >= 100);
    assert_eq!(
        all(select_cursor(s.snapshot(), q, &opts_q, &opts).unwrap()),
        query(s.snapshot(), q, &opts_q).unwrap().application_rows()
    );
}

#[test]
fn explicit_collection_matches_eager_and_honors_its_memory_limit() {
    let s = store(97);
    let q = "SELECT ?s ?x WHERE { ?s <urn:p> ?o BIND(?o + 1 AS ?x) }";
    assert_eq!(
        bag(open(&s, q, 3).collect().unwrap().application_rows()),
        bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows())
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
        bag(query(s.snapshot(), q, &opts).unwrap().application_rows())
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
fn previously_decoded_cells_remain_readable_after_producer_budget_failure() {
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
    let decoded = first.term(0, 0).unwrap().unwrap();
    let key = first.key(0, 0).unwrap();
    let terminal = loop {
        match c.next_batch() {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the unchanged500-row/16KiB producer must exhaust its allowance"),
            Err(error) => break error,
        }
    };
    assert!(
        matches!(&terminal, Error::BudgetExceeded(budget)
        if budget.kind == BudgetKind::Memory && budget.limit == 16 << 10 && budget.requested > budget.limit),
        "preserve the real typed local producer refusal, got {terminal:?}"
    );
    assert_eq!(c.status(), CursorStatus::Failed);
    drop(c);
    // Already decoded immutable storage needs no new allocation after failure.
    // A new decode at an exhausted cap remains fallible.
    assert!(matches!(decoded.as_term(), Term::Literal(_)));
    assert_eq!(first.key(0, 0).unwrap(), key);
    assert_eq!(first.variables(), &["x"]);
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

/// A cursor writer goes out through the eager writers' buffer, so a byte budget that
/// stops it partway leaves `sent` counting the rows of the output the writer took, as
/// the eager writer's count does, and not the rows of the batches the cursor produced.
#[test]
fn counted_cursor_writers_report_the_rows_a_byte_budget_let_through() {
    use sparkles_core::sparql::results::{
        Format, LimitedWriter, RowCount, SolutionsFormat, Source, WriteOptions, write,
    };
    let s = store(3000);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    for fmt in [SolutionsFormat::Tsv, SolutionsFormat::Csv] {
        for limit in [1u64, 20_000, 40_000] {
            let mut cut = LimitedWriter::new(Vec::new(), Some(limit), None);
            let sent = RowCount::new();
            let mut c = open(&s, q, 1024);
            let result = write(
                Source::Select(&mut c),
                WriteOptions::new(Format::Solutions(fmt), &sent),
                &mut cut,
            );
            assert!(result.is_err(), "{fmt:?} {limit}");
            let out = cut.get_ref();
            let lines = out.iter().filter(|&&b| b == b'\n').count() as u64;
            assert_eq!(sent.get(), lines.saturating_sub(1), "{fmt:?} {limit}");
            assert!(c.stats().emitted_rows >= sent.get());
            let mut eager_cut = LimitedWriter::new(Vec::new(), Some(limit), None);
            let eager_sent = RowCount::new();
            assert!(
                write(
                    Source::Result(&eager),
                    WriteOptions::new(Format::Solutions(fmt), &eager_sent),
                    &mut eager_cut,
                )
                .is_err()
            );
            // the cursor's flush after its first batch moves where the buffer's pieces
            // end, so the bytes may differ, but the rule that counts them is the same
            let eager_lines = eager_cut.get_ref().iter().filter(|&&b| b == b'\n').count() as u64;
            assert_eq!(
                eager_sent.get(),
                eager_lines.saturating_sub(1),
                "{fmt:?} {limit}"
            );
        }
        let sent = RowCount::new();
        let mut whole = Vec::new();
        let mut c = open(&s, q, 1024);
        write(
            Source::Select(&mut c),
            WriteOptions::new(Format::Solutions(fmt), &sent),
            &mut whole,
        )
        .unwrap();
        assert_eq!(sent.get(), 3000);
    }
    let mut cut = LimitedWriter::new(Vec::new(), Some(40_000), None);
    let sent = RowCount::new();
    let mut c = open(&s, q, 1024);
    assert!(
        write(
            Source::Select(&mut c),
            WriteOptions::new(Format::Solutions(SolutionsFormat::Sparkles), &sent),
            &mut cut,
        )
        .is_err()
    );
    // every cell is bound and an object, so each whole row ends with `}]`
    let rows = cut.get_ref().windows(2).filter(|w| w == b"}]").count() as u64;
    assert_eq!(sent.get(), rows);
    assert!(sent.get() > 0 && sent.get() < 3000);
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
    // The writer sees the output in the buffer's pieces of 8 KiB after the first batch's
    // flush, so the answer must outlast one piece for the failure to reach production.
    let s = store(2000);
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
                io: None,
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
        bag(vec![
            first.row(0).unwrap().values().to_vec(),
            rest[0].clone()
        ]),
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
        for cap in [1, 2, 3, 4096] {
            let cursor = open(&s, q, cap);
            assert!(!cursor.plan().has_materialization());
            assert_eq!(bag(all(cursor)), expected, "{q}, cap {cap}");
        }
    }
    let s = store(5000);
    let opts = QueryOptions {
        max_memory_bytes: Some(32 << 10),
        ..Default::default()
    };
    // Input ordered on every output column keeps only its last row, so it fits.
    let q = "SELECT DISTINCT ?s WHERE { ?s <urn:p> ?o }";
    let mut cursor = select_cursor(s.snapshot(), q, &opts, &options(3)).unwrap();
    let mut produced = 0;
    while let Some(batch) = cursor.next_batch().unwrap() {
        produced += batch.len();
    }
    assert_eq!(produced, 5000);
    assert!(cursor.stats().mem_peak_bytes <= 32 << 10);
    // A computed column is not ordered, so the set of emitted rows grows past it.
    let q = "SELECT DISTINCT ?k WHERE { ?s <urn:p> ?o BIND(?o * 2 AS ?k) }";
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
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
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
fn dictionary_inputs_match_eager_for_mixed_expressions_and_budget_decline() {
    use sparkles_core::id::Id;
    let s = store(0);
    let data = (0..5000)
        .map(|i| format!(
            "<urn:s:{i}> <urn:value> \"{}.123456789012345678\"^^<http://www.w3.org/2001/XMLSchema#decimal> ; <urn:name> \"name {i} {}\" .\n",
            i, "x".repeat(96)
        ))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let numeric = oxrdf::Literal::new_typed_literal(
        "3.123456789012345678",
        oxrdf::NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#decimal"),
    );
    assert!(sparkles_core::id::inline_id(&numeric.clone().into()).is_none());
    assert!(sparkles_core::sparql::value::Value::from_term(&numeric.clone().into()).is_numeric());
    let mut tx = s.write();
    let base_id = tx.intern(&numeric.into()).unwrap();
    assert_eq!(base_id.tag(), sparkles_core::id::Tag::Vocab);
    let subject = tx
        .intern(&oxrdf::NamedNode::new_unchecked("urn:delta").into())
        .unwrap();
    for (predicate, object) in [
        (
            "urn:value",
            oxrdf::Literal::new_typed_literal(
                "4500.987654321098765432",
                oxrdf::NamedNode::new_unchecked("http://www.w3.org/2001/XMLSchema#decimal"),
            ),
        ),
        ("urn:name", oxrdf::Literal::new_simple_literal("delta name")),
    ] {
        let p = tx
            .intern(&oxrdf::NamedNode::new_unchecked(predicate).into())
            .unwrap();
        let o = tx.intern(&object.into()).unwrap();
        if predicate == "urn:value" {
            assert_eq!(o.tag(), sparkles_core::id::Tag::Delta);
        }
        tx.insert([subject, p, o, Id::DEFAULT_GRAPH]).unwrap();
    }
    tx.commit().unwrap();
    for (q, ordered, expected_count) in [
        (
            "SELECT ?s { ?s <urn:value> ?v FILTER(?v > 4000 && ?v < 4900) }",
            false,
            901,
        ),
        (
            "SELECT ?s { ?s <urn:value> ?v BIND(?v + 0.000000000000000001 AS ?x) FILTER(?x > 4000 && ?x < 4900) }",
            false,
            901,
        ),
        (
            "SELECT ?s { ?s <urn:value> ?v ; <urn:name> ?n FILTER(STRLEN(?n) > ?v) }",
            false,
            105,
        ),
        (
            "SELECT ?s ?x { ?s <urn:value> ?v ; <urn:name> ?n BIND(?v + STRLEN(?n) AS ?x) }",
            false,
            5001,
        ),
        (
            "SELECT ?s { ?s <urn:value> ?v ; <urn:name> ?n } ORDER BY (?v + STRLEN(?n)) ?s LIMIT 10",
            true,
            10,
        ),
        (
            "SELECT ?s { ?s <urn:value> ?v ; <urn:name> ?original BIND(CONCAT(?original, \" suffix\") AS ?n) FILTER(STRLEN(?n) > ?v) }",
            false,
            112,
        ),
    ] {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        assert_eq!(expected.len(), expected_count, "{q}");

        for (memory, reuse, batch) in [
            (32 << 20, true, 8192),
            (32 << 20, true, 4096),
            (32 << 20, false, 8192),
            (32 << 20, true, 31),
        ] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: reuse,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let cursor_opts = if ordered {
                CursorOptions {
                    batch_rows: batch,
                    ..Default::default()
                }
            } else {
                options(batch)
            };
            let mut cursor = select_cursor(s.snapshot(), q, &opts, &cursor_opts).unwrap();
            let mut answer = Vec::new();
            while let Some(b) = cursor.next_batch().unwrap() {
                for row in 0..b.len() {
                    answer.push(b.row(row).unwrap().values().to_vec());
                }
            }
            if ordered {
                assert_eq!(answer, expected, "{q}; reuse {reuse}; batch {batch}");
            } else {
                assert_eq!(
                    bag(answer),
                    bag(expected.clone()),
                    "{q}; reuse {reuse}; batch {batch}"
                );
            }
            assert!(cursor.stats().mem_peak_bytes <= memory);
        }
    }
    let q = "SELECT ?s { ?s <urn:value> ?v FILTER(?v > 4000 && ?v < 4900) }";
    let expected = bag(query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows());
    let opts = QueryOptions {
        max_memory_bytes: Some(1 << 20),
        ..Default::default()
    };
    let mut cursor = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
    let mut answer = Vec::new();
    while let Some(b) = cursor.next_batch().unwrap() {
        for row in 0..b.len() {
            answer.push(b.row(row).unwrap().values().to_vec());
        }
    }
    assert_eq!(bag(answer), expected);
    assert!(cursor.stats().mem_peak_bytes <= 1 << 20);
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
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
        for cap in [1, 2, 3, 4096] {
            let c = open(&s, q, cap);
            assert!(!c.plan().has_materialization());
            assert_eq!(bag(all(c)), expected, "{q}; cap {cap}");
        }
    }
}

/// The hash join keys on the shared column that its build data leaves unbound least
/// often. Bound probes must still meet build rows that leave the key unbound, and an
/// unbound probe meets every build row, with duplicates kept.
#[test]
fn hash_join_keys_cover_unbound_build_and_probe_values_with_duplicates() {
    let s = store(0);
    let left = "VALUES (?x ?y ?a) { (1 UNDEF 10) (UNDEF 2 11) (1 2 12) (1 2 12) (UNDEF UNDEF 13) (3 UNDEF 14) (2 3 15) }";
    let right = "VALUES (?x ?y ?b) { (1 2 20) (1 2 20) (UNDEF 2 21) (1 UNDEF 22) (UNDEF UNDEF 23) (2 3 24) (2 UNDEF 25) (4 4 26) }";
    // Every build row binds ?y here except one, and ?x is unbound in two.
    let mostly =
        "VALUES (?x ?y ?b) { (UNDEF 2 20) (1 2 21) (1 2 21) (UNDEF 3 22) (2 UNDEF 23) (3 4 24) }";
    let mut queries = Vec::new();
    for (l, r) in [(left, right), (right, left), (left, mostly), (mostly, left)] {
        queries.push(format!("SELECT * WHERE {{ {l} {r} }}"));
        queries.push(format!("SELECT * WHERE {{ {l} OPTIONAL {{ {r} }} }}"));
        queries.push(format!(
            "SELECT * WHERE {{ {l} OPTIONAL {{ {r} FILTER(?b > ?a + 10) }} }}"
        ));
        queries.push(format!("SELECT * WHERE {{ {l} MINUS {{ {r} }} }}"));
    }
    // Long runs of one key, so that chains are longer than a batch.
    let many: String = (0..300).map(|i| format!("({} {}) ", i % 3, i)).collect();
    let few: String = (0..40)
        .map(|i| {
            if i % 7 == 0 {
                format!("(UNDEF {i}) ")
            } else {
                format!("({} {i}) ", i % 4)
            }
        })
        .collect();
    queries.push(format!(
        "SELECT * WHERE {{ VALUES (?x ?a) {{ {many} }} VALUES (?x ?b) {{ {few} }} }}"
    ));
    queries.push(format!(
        "SELECT * WHERE {{ VALUES (?x ?a) {{ {many} }} OPTIONAL {{ VALUES (?x ?b) {{ {few} }} }} }}"
    ));
    queries.push(format!(
        "SELECT * WHERE {{ VALUES (?x ?a) {{ {few} }} MINUS {{ VALUES (?x ?b) {{ {many} }} }} }}"
    ));
    // A condition over keys with more rows than an output batch: a probe row resumes
    // its candidates in the next chunk and keeps whether one of them passed.
    queries.push(format!(
        "SELECT * WHERE {{ VALUES (?x ?a) {{ {few} }} OPTIONAL {{ VALUES (?x ?b) {{ {many} }} FILTER(?b > 150 + ?a) }} }}"
    ));
    for q in &queries {
        let q = q.as_str();
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
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
        let expected = bag(query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows());
        for cap in [1, 2, 3, 4096] {
            let c = open(&s, q, cap);
            assert!(!c.plan().has_materialization());
            assert_eq!(bag(all(c)), expected, "{q}, cap {cap}");
        }
    }
}

#[test]
fn construct_cursor_deduplicates_across_batches_and_retains_snapshot() {
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
            .triples()
            .iter()
            .map(|t| {
                t.as_ref()
                    .in_graph(oxrdf::GraphNameRef::DefaultGraph)
                    .to_string()
            })
            .chain(expected.quads().iter().map(|q| q.to_string()))
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
    // DESCRIBE describes each batch in turn, so the strict policy admits it
    let q = "DESCRIBE <urn:s:0>";
    assert!(graph_cursor(s.snapshot(), q, &Default::default(), &options(1)).is_ok());
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
    assert!(!c.plan().materializes);
    assert!(!c.plan().full_input_before_output);
    let mut got = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        got.extend(batch.quads().iter().map(|q| q.to_string()));
    }
    assert_eq!(
        got,
        expected
            .triples()
            .iter()
            .map(|t| t
                .as_ref()
                .in_graph(oxrdf::GraphNameRef::DefaultGraph)
                .to_string())
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
        let r = ask_streaming(s.snapshot(), q, &Default::default(), &Default::default()).unwrap();
        assert_eq!(r.boolean, expected);
        assert!(r.rows_produced < 20, "{q}: {}", r.rows_produced);
    }
    // An application callback in the filter runs for the qualifying solution, not for
    // the rest of a default-sized batch, so its call count and errors do not depend on
    // the batch size.
    use sparkles_core::sparql::extensions::{ExtensionRegistry, ScalarContext, ScalarDescriptor};
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(
            ScalarDescriptor::new(
                "urn:test:count",
                1..=1,
                move |_: &ScalarContext<'_>, _: &[Term]| {
                    count.fetch_add(1, Ordering::SeqCst);
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
    let r = ask_streaming(
        s.snapshot(),
        "ASK { ?s <urn:p> ?o FILTER(<urn:test:count>(?o)) }",
        &opts,
        &Default::default(),
    )
    .unwrap();
    assert!(r.boolean);
    assert!(
        calls.load(Ordering::SeqCst) < 20,
        "{} callback calls",
        calls.load(Ordering::SeqCst)
    );
}

/// Automatic and eager execution pull every ASK through cursor operators as a
/// streaming request does (item 827). Automatic execution reports the cursor's plan, and
/// an ASK with initial bindings, which runs eagerly, still stops at its first solution.
#[test]
fn every_mode_pulls_an_ask_through_the_cursor() {
    use sparkles_core::sparql::{ExecutionMode, QueryExecution, query_execution};
    let s = store(10000);
    for (q, expected) in [
        ("ASK { ?s <urn:p> ?o }", true),
        ("ASK { ?s <urn:absent> ?o }", false),
        ("ASK { ?s <urn:p> ?o . ?t <urn:p> ?o }", true),
    ] {
        for mode in [
            ExecutionMode::Eager,
            ExecutionMode::Auto,
            ExecutionMode::Streaming,
        ] {
            let execution = query_execution(
                s.snapshot(),
                q,
                &Default::default(),
                &CursorOptions::default(),
                mode,
            )
            .unwrap();
            let plan = execution.plan_json().unwrap();
            if expected {
                // the scans stop at the first solution in every mode
                assert!(plan.contains("\"stoppedEarly\":true"), "{q}: {plan}");
            }
            let (QueryExecution::Ask(result) | QueryExecution::Eager(result)) = &execution else {
                panic!("{q}: an ASK completes")
            };
            assert_eq!(result.value(), expected, "{q} under {mode:?}");
            assert!(result.result().rows_produced < 20, "{q} under {mode:?}");
            // an absent pattern has no rows, so the planner runs it whole and
            // automatic execution keeps it eager
            if mode == ExecutionMode::Auto {
                assert_eq!(matches!(execution, QueryExecution::Ask(_)), expected, "{q}");
            }
        }
    }
    let bound = QueryOptions {
        initial_bindings: vec![(
            "s".into(),
            Term::NamedNode(oxrdf::NamedNode::new_unchecked("urn:s:1")),
        )],
        ..Default::default()
    };
    let execution = query_execution(
        s.snapshot(),
        "ASK { ?s <urn:p> ?o }",
        &bound,
        &CursorOptions::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let (QueryExecution::Ask(result) | QueryExecution::Eager(result)) = &execution else {
        panic!("an ASK completes")
    };
    assert!(result.value());
    assert!(result.result().rows_produced < 20);
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
fn completion_runs_once_after_ask_empty_select_and_graph_output() {
    use sparkles_core::sparql::graph_cursor;
    use sparkles_core::sparql::results::{
        Format, RowCount, SolutionsFormat, Source, WriteOptions, write, write_with_completion,
    };

    let s = store(1);
    let construct = "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }";
    let tsv = Format::Solutions(SolutionsFormat::Tsv);
    let ntriples = Format::Rdf(RdfFormat::NTriples);
    for (text, format) in [
        ("ASK {}", tsv),
        ("SELECT ?s WHERE { ?s <urn:missing> ?o }", tsv),
        (construct, ntriples),
    ] {
        let result = query(s.snapshot(), text, &Default::default()).unwrap();
        let expected_rows = RowCount::new();
        let mut expected = Vec::new();
        write(
            Source::Result(&result),
            WriteOptions::new(format, &expected_rows),
            &mut expected,
        )
        .unwrap();
        let mut calls = 0;
        let mut out = Vec::new();
        let sent = RowCount::new();
        let stats = write_with_completion(
            Source::Result(&result),
            WriteOptions {
                format,
                send: None,
                prefixes: &[],
                metadata: None,
                rows: &sent,
            },
            &mut out,
            |out| {
                calls += 1;
                assert_eq!(*out, expected);
                assert_eq!(sent.get(), expected_rows.get());
                Ok(())
            },
        )
        .unwrap();
        assert!(stats.is_none());
        assert_eq!(calls, 1);
    }

    let mut graph =
        graph_cursor(s.snapshot(), construct, &Default::default(), &options(1)).unwrap();
    let sent = RowCount::new();
    let error = write_with_completion(
        Source::Graph(&mut graph),
        WriteOptions::new(ntriples, &sent),
        &mut Vec::new(),
        |out| {
            assert!(out.ends_with(b" .\n"));
            assert_eq!(sent.get(), 1);
            Err(Error::Cancelled)
        },
    )
    .unwrap_err();
    assert!(matches!(error, Error::Cancelled));
    let stats = graph.stats();
    assert_eq!(stats.status, CursorStatus::Failed);
    assert_eq!(stats.error, Some(error.to_string()));
}

#[test]
fn completion_is_skipped_on_dispatch_drain_and_flush_failures() {
    use sparkles_core::sparql::results::{
        Format, LimitedWriter, RowCount, SolutionsFormat, Source, WriteOptions,
        write_with_completion,
    };
    use std::io::{self, Write};

    let s = store(1);
    let ask = query(s.snapshot(), "ASK {}", &Default::default()).unwrap();
    let sent = RowCount::new();
    let error = write_with_completion(
        Source::Result(&ask),
        WriteOptions::new(Format::ResultsThrift, &sent),
        &mut Vec::new(),
        |_| panic!("invalid format must skip completion"),
    )
    .unwrap_err();
    assert!(matches!(error, Error::Invalid(_)));

    let mut limited = LimitedWriter::new(Vec::new(), Some(0), None);
    let error = write_with_completion(
        Source::Result(&ask),
        WriteOptions::new(Format::Solutions(SolutionsFormat::Csv), &sent),
        &mut limited,
        |_| panic!("failed drain must skip completion"),
    )
    .unwrap_err();
    assert!(matches!(
        limited.classify(error),
        Error::BudgetExceeded(b) if b.kind == BudgetKind::ResultBytes
    ));

    struct BrokenFlush;
    impl Write for BrokenFlush {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    let mut cursor = open(&s, "SELECT ?o WHERE { ?s <urn:p> ?o }", 1);
    let error = write_with_completion(
        Source::Select(&mut cursor),
        WriteOptions::new(Format::Solutions(SolutionsFormat::Tsv), &sent),
        &mut BrokenFlush,
        |_| panic!("failed first-batch flush must skip completion"),
    )
    .unwrap_err();
    assert!(matches!(&error, Error::Io(e) if e.kind() == io::ErrorKind::BrokenPipe));
    assert_eq!(sent.get(), 1);
    assert_eq!(cursor.stats().error, Some(error.to_string()));
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
                let row = expected.row(i).unwrap();
                row.values()
                    .iter()
                    .map(|term| term.as_ref().map(Term::to_string))
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
                        .values()
                        .to_vec()
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
    // An ASK over an input that the planner runs whole, because it is small, stays
    // eager too (item 827).
    let q = "ASK {?s ?p ?o}";
    let execution = query_execution(
        s.snapshot(),
        q,
        &Default::default(),
        &Default::default(),
        ExecutionMode::Auto,
    )
    .unwrap();
    let QueryExecution::Eager(answer) = execution else {
        panic!("a small ASK must remain eager")
    };
    let expected = query(s.snapshot(), q, &Default::default()).unwrap();
    assert!(expected.boolean);
    assert_eq!(answer.result().boolean, expected.boolean);
}

#[test]
fn automatic_execution_keeps_the_plan_of_an_eager_failure() {
    use sparkles_core::sparql::{ExecutionMode, query_execution_with_plan, query_with_plan};
    // The default mode is automatic, and a plan it keeps eager fails as an eager query
    // does: with the plan as far as it ran, which the native error body reports.
    assert_eq!(ExecutionMode::default(), ExecutionMode::Auto);
    let s = store(10);
    let q = "SELECT * {?s ?p ?o . ?t ?q ?u}";
    let opts = QueryOptions {
        max_rows: Some(20),
        ..Default::default()
    };
    let eager = query_with_plan(s.snapshot(), q, &opts)
        .err()
        .expect("the row budget fails the cross product");
    assert!(eager.plan.is_some());
    for mode in [ExecutionMode::Eager, ExecutionMode::Auto] {
        let failed = query_execution_with_plan(s.snapshot(), q, &opts, &Default::default(), mode)
            .err()
            .expect("the row budget fails the cross product");
        assert!(matches!(failed.error, Error::BudgetExceeded(_)), "{mode:?}");
        assert_eq!(failed.plan.is_some(), eager.plan.is_some(), "{mode:?}");
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
            &format!("SELECT ?x {{ {LEGACY_VIEW_LEAF} }}"),
            &legacy_view_opts(3),
            &strict,
            ExecutionMode::Auto,
        ),
        Err(Error::Unsupported(_))
    ));
}

#[test]
fn strict_policy_admits_a_budgeted_sort_and_reports_it_as_blocking() {
    let s = store(50);
    for q in [
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o)",
        "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY ?o LIMIT 7",
    ] {
        let c = open(&s, q, 3);
        let plan = c.plan();
        assert!(!plan.has_materialization(), "{q}");
        fn blocking(plan: &sparkles_core::sparql::CursorPlan) -> bool {
            (plan.full_input_before_output && plan.reason.is_some())
                || plan.children.iter().any(blocking)
        }
        assert!(blocking(plan), "{q}");
        assert_eq!(
            all(c),
            query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows(),
            "{q}"
        );
    }
    // An ORDER key that evaluates EXISTS runs its pattern under the cursor's charges,
    // so the strict policy admits it.
    let q = "SELECT ?s WHERE { ?s <urn:p> ?o } ORDER BY (EXISTS { ?s <urn:q> ?x }) ?s";
    let c = open(&s, q, 2);
    assert!(!c.plan().has_materialization(), "{q}");
    assert_eq!(
        all(c),
        query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows()
    );
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
        max_memory_bytes: Some(1_400_000),
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
    assert!(cursor.stats().mem_peak_bytes <= 1_400_000);
    assert!(retained.iter().map(|b| b.len()).sum::<usize>() > 32_768);
    let first = retained[0].row(0).unwrap();
    drop(cursor);
    drop(s);
    assert_eq!(retained[0].row(0).unwrap().values(), first.values());
    assert!(matches!(first.values()[0], Some(Term::Literal(_))));
    assert!(matches!(first.values()[1], Some(Term::NamedNode(_))));
}

#[test]
fn shared_scan_projection_limit_and_offset_preserve_order_and_work_limits() {
    let s = store(40_000);
    for q in [
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o } LIMIT 5000",
        "SELECT ?o ?s WHERE { ?s <urn:p> ?o } OFFSET 7 LIMIT 5000",
        "SELECT ?o WHERE { ?s <urn:p> ?o } LIMIT 5",
    ] {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
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
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
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
                    answer.extend(
                        (0..batch.len()).map(|row| batch.row(row).unwrap().values().to_vec()),
                    );
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
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for memory in [8 << 20, 32 << 20] {
            let opts = QueryOptions {
                max_memory_bytes: Some(memory),
                ..Default::default()
            };
            let mut cursor = select_cursor(s.snapshot(), q, &opts, &Default::default()).unwrap();
            let mut rows = Vec::new();
            while let Some(batch) = cursor.next_batch().unwrap() {
                rows.extend((0..batch.len()).map(|row| batch.row(row).unwrap().values().to_vec()));
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
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for cap in [1, 7, 4096] {
            assert_eq!(all(open(&s, q, cap)), expected);
        }
    }
}

/// Generation collection does not wait for live cursors. On Unix the cursor keeps
/// reading its generation's open and mapped files after the directory is removed.
#[cfg(unix)]
#[test]
fn cursor_reads_its_generation_after_compactions_retire_it() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(
        dir.path(),
        StoreOptions {
            history_max_generations: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let data = (0..20_000)
        .map(|i| format!("<urn:s:{i}> <urn:p> \"value {i}\" .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let snapshot = s.snapshot();
    let generation = snapshot
        .generation
        .dir
        .clone()
        .expect("a sealed generation");
    let expected = bag(query(snapshot.clone(), q, &Default::default())
        .unwrap()
        .application_rows());
    let mut cursor = select_cursor(snapshot, q, &Default::default(), &options(7)).unwrap();
    let mut got = Vec::new();
    let first = cursor.next_batch().unwrap().unwrap();
    got.extend((0..first.len()).map(|i| first.row(i).unwrap().values().to_vec()));
    for round in 0..3 {
        sparkles_core::sparql::update::update(
            &s,
            &format!("INSERT DATA {{ <urn:new:{round}> <urn:p> {round} }}"),
            &Default::default(),
        )
        .unwrap();
        s.compact().unwrap();
    }
    assert!(
        !generation.exists(),
        "{} should be retired",
        generation.display()
    );
    while let Some(batch) = cursor.next_batch().unwrap() {
        got.extend((0..batch.len()).map(|i| batch.row(i).unwrap().values().to_vec()));
    }
    assert_eq!(bag(got), expected);
}

/// W3C pp31: a sort over UNION arms that are each sorted feeds a merge join. The
/// concatenated arms must be sorted again, or the join misses matches.
#[test]
fn sort_over_union_arms_reorders_their_concatenation() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"@prefix : <http://www.example.org/> . :a :p1 :b . :b :p4 :c . :a :p2 :d . :d :p3 :c . :a :p1 :e .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "prefix : <http://www.example.org/> select ?t where { :a (:p1|:p2)/(:p3|:p4) ?t }";
    let expected = bag(query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows());
    assert_eq!(expected.len(), 2);
    for rows in [1, 2, 4096] {
        assert_eq!(bag(all(open(&s, q, rows))), expected, "{rows}");
    }
}

/// A small seeded generator, so that a failing case can be replayed by its seed.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

const TERMS: [&str; 7] = [
    "<urn:s:0>",
    "<urn:s:1>",
    "<urn:s:2>",
    "0",
    "1",
    "2",
    "UNDEF",
];
const VARS: [&str; 4] = ["?a", "?b", "?c", "?d"];

/// How a generated query fixes the order of its solutions.
enum Order {
    None,
    /// ORDER BY on these projected variables, each `?v` or `DESC(?v)`, so equal keys may
    /// come in any order.
    Vars(Vec<String>),
    /// ORDER BY on an expression, compared as a bag.
    Expr,
}

struct Shape {
    query: String,
    /// The same query without LIMIT and OFFSET, when it has either.
    unsliced: Option<String>,
    order: Order,
    offset: bool,
    limit: bool,
}

/// A seeded query generator. Each pattern reports the variables it binds, so that BIND
/// and subquery aggregates can target a variable that is not yet in scope.
struct Gen {
    rng: Rng,
    fresh: usize,
    fresh_vars: Vec<String>,
}

impl Gen {
    fn new(seed: u64) -> Self {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        // Neighbouring seeds start with similar xorshift states.
        for _ in 0..8 {
            rng.next();
        }
        Self {
            rng,
            fresh: 0,
            fresh_vars: Vec::new(),
        }
    }

    fn pick<'a>(&mut self, xs: &[&'a str]) -> &'a str {
        xs[self.rng.below(xs.len() as u64) as usize]
    }

    fn var(&mut self) -> &'static str {
        self.pick(&VARS)
    }

    /// A variable for a new binding: an unused one of `VARS`, so it can join with
    /// the rest of the query, or a fresh one that the top level projects.
    fn target(&mut self, scope: &BTreeSet<&'static str>, avoid: &[&str]) -> String {
        let free: Vec<&str> = VARS
            .iter()
            .copied()
            .filter(|v| !scope.contains(v) && !avoid.contains(v))
            .collect();
        if !free.is_empty() && self.rng.below(3) != 0 {
            return self.pick(&free).to_string();
        }
        let v = format!("?e{}", self.fresh);
        self.fresh += 1;
        self.fresh_vars.push(v.clone());
        v
    }

    fn condition(&mut self, depth: u32) -> String {
        let (x, y) = (self.var(), self.var());
        match self.rng.below(if depth == 0 { 11 } else { 14 }) {
            0 => format!("BOUND({x})"),
            1 => format!("!BOUND({x})"),
            2 => format!("{x} = {y}"),
            3 => format!("{x} != {y}"),
            4 => format!("{x} < {}", self.rng.below(3)),
            5 => format!("{x} >= {}", self.rng.below(3)),
            6 => format!("isIRI({x})"),
            7 => format!("sameTerm({x}, {y})"),
            8 => format!("{x} IN (0, <urn:s:1>)"),
            9 => format!("COALESCE({x}, 0) <= 1"),
            10 => format!("{x} > {y}"),
            11 => format!(
                "({}) && ({})",
                self.condition(depth - 1),
                self.condition(depth - 1)
            ),
            12 => format!(
                "({}) || ({})",
                self.condition(depth - 1),
                self.condition(depth - 1)
            ),
            _ => self.exists(),
        }
    }

    /// An EXISTS or NOT EXISTS. Single triples and joins are answered from a key
    /// set on one or more variables, a FILTER inside can make an outer variable risky,
    /// and OPTIONAL and nested EXISTS patterns are evaluated per row.
    fn exists(&mut self) -> String {
        let (x, y, z) = (self.var(), self.var(), self.var());
        let p = self.pick(&["<urn:p>", "<urn:q>"]);
        let r = self.pick(&["<urn:p>", "<urn:q>"]);
        let not = ["", "NOT "][self.rng.below(2) as usize];
        let body = match self.rng.below(7) {
            0 | 1 => format!("{x} {p} {y}"),
            2 => format!("{x} {p} {y} . {y} {r} {z}"),
            3 => format!("{x} {p} {y} FILTER({z} != {y})"),
            4 => format!("{x} {p} {y} OPTIONAL {{ {y} {r} {z} }}"),
            5 => format!("{x} {p}+ {y} FILTER NOT EXISTS {{ {y} {r} {z} }}"),
            _ => format!("<urn:s:1> {p} {y} . {y} {r} {x}"),
        };
        format!("{not}EXISTS {{ {body} }}")
    }

    fn value(&mut self) -> String {
        let (x, y) = (self.var(), self.var());
        match self.rng.below(9) {
            7 => self.exists(),
            0 => x.to_string(),
            1 => format!("({x} + 1)"),
            2 => format!("COALESCE({x}, {y})"),
            3 => format!("IF(BOUND({x}), {x}, -1)"),
            4 => format!("STR({x})"),
            5 => format!("({x} * 2)"),
            6 => format!("isIRI({x})"),
            _ => format!("ABS({x})"),
        }
    }

    fn aggregate(&mut self) -> String {
        let x = self.var();
        match self.rng.below(14) {
            0 => "COUNT(*)".into(),
            1 => format!("COUNT({x})"),
            2 => format!("COUNT(DISTINCT {x})"),
            3 => format!("SUM({x})"),
            4 => format!("MIN({x})"),
            5 => format!("MAX({x})"),
            6 => format!("AVG({x})"),
            7 => format!("SUM({x} + 1)"),
            8 => format!("SUM(DISTINCT {x})"),
            9 => format!("AVG({x} * 2)"),
            10 => format!("MAX(STR({x}))"),
            11 => format!("COUNT(DISTINCT COALESCE({x}, 0))"),
            12 => "COUNT(DISTINCT *)".into(),
            _ => format!("SUM(IF({}, 1, 0))", self.exists()),
        }
    }

    fn leaf(&mut self) -> (String, BTreeSet<&'static str>) {
        if self.rng.below(2) == 0 {
            let s = [self.var(), "<urn:s:0>"][self.rng.below(2) as usize];
            let o = self.var();
            // Alternatives in a sequence plan sorted UNION arms under a merge join.
            let p = self.pick(&[
                "<urn:p>",
                "<urn:q>",
                "(<urn:q>|<urn:p>)/(<urn:q>|<urn:p>)",
                "<urn:q>/(<urn:q>|<urn:p>)",
                "<urn:q>+",
                "^<urn:q>*",
                "(<urn:q>|<urn:p>)+",
                "(<urn:q>/<urn:q>)*",
                "^(<urn:p>|<urn:q>/<urn:q>)+",
            ]);
            let scope = [s, o].into_iter().filter(|v| v.starts_with('?')).collect();
            return (format!("{s} {p} {o} ."), scope);
        }
        let width = 1 + self.rng.below(2) as usize;
        let first = self.rng.below(4) as usize;
        let vars: Vec<&'static str> = (0..width).map(|i| VARS[(first + i) % 4]).collect();
        let rows = (0..self.rng.below(6))
            .map(|_| {
                let cells: Vec<&str> = (0..width).map(|_| self.pick(&TERMS)).collect();
                format!("({})", cells.join(" "))
            })
            .collect::<Vec<_>>()
            .join(" ");
        let scope = vars.iter().copied().collect();
        (format!("VALUES ({}) {{ {rows} }}", vars.join(" ")), scope)
    }

    fn pattern(&mut self, depth: u32) -> (String, BTreeSet<&'static str>) {
        if depth == 0 || self.rng.below(3) == 0 {
            return self.leaf();
        }
        match self.rng.below(10) {
            9 => {
                // A LATERAL that is not a join: the right side keeps one ordered
                // solution per left row, or keeps the left row without one.
                let (left, mut scope) = self.pattern(depth - 1);
                let x = self.var();
                let others: Vec<&'static str> = VARS.iter().copied().filter(|v| *v != x).collect();
                let y = self.pick(&others);
                let p = self.pick(&["<urn:p>", "<urn:q>"]);
                let right = if self.rng.below(2) == 0 {
                    scope.insert(x);
                    format!("SELECT {x} {y} WHERE {{ {x} {p} {y} }} ORDER BY {y} LIMIT 1")
                } else {
                    format!("SELECT {y} WHERE {{ {x} {p} {y} }} ORDER BY DESC({y}) LIMIT 2")
                };
                scope.insert(y);
                (format!("{{ {left} }} LATERAL {{ {right} }}"), scope)
            }
            0..=4 => {
                let (left, mut ls) = self.pattern(depth - 1);
                let (right, rs) = self.pattern(depth - 1);
                let text = match self.rng.below(4) {
                    0 => format!("{{ {left} }} {{ {right} }}"),
                    1 if self.rng.below(3) == 0 => {
                        let condition = self.exists();
                        format!("{{ {left} }} OPTIONAL {{ {right} FILTER({condition}) }}")
                    }
                    1 => format!("{{ {left} }} OPTIONAL {{ {right} }}"),
                    2 => {
                        return (format!("{{ {left} }} MINUS {{ {right} }}"), ls);
                    }
                    _ => format!("{{ {left} }} UNION {{ {right} }}"),
                };
                ls.extend(rs);
                (text, ls)
            }
            5 => {
                let (inner, scope) = self.pattern(depth - 1);
                let condition = if self.rng.below(5) == 0 {
                    self.exists()
                } else {
                    self.condition(1)
                };
                (format!("{{ {inner} }} FILTER({condition})"), scope)
            }
            6 => {
                let (inner, mut scope) = self.pattern(depth - 1);
                let value = self.value();
                let target = self.target(&scope, &[]);
                if let Some(v) = VARS.iter().find(|v| **v == target) {
                    scope.insert(v);
                }
                (format!("{{ {inner} }} BIND({value} AS {target})"), scope)
            }
            7 => {
                // ORDER BY every projected variable makes the slice deterministic up to
                // identical rows.
                let (inner, _) = self.pattern(depth - 1);
                let width = 1 + self.rng.below(3) as usize;
                let first = self.rng.below(4) as usize;
                let vars: Vec<&'static str> = (0..width).map(|i| VARS[(first + i) % 4]).collect();
                let keys = vars
                    .iter()
                    .map(|v| {
                        if self.rng.below(2) == 0 {
                            format!("DESC({v})")
                        } else {
                            v.to_string()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let distinct = ["", "DISTINCT "][self.rng.below(2) as usize];
                let limit = self.rng.below(5);
                let offset = self.rng.below(3);
                let text = format!(
                    "{{ SELECT {distinct}{} WHERE {{ {inner} }} ORDER BY {keys} LIMIT {limit} OFFSET {offset} }}",
                    vars.join(" ")
                );
                (text, vars.into_iter().collect())
            }
            _ => {
                let (inner, scope) = self.pattern(depth - 1);
                let key = (self.rng.below(3) != 0).then(|| self.var());
                let target = self.target(&scope, key.as_slice());
                let aggregate = self.aggregate();
                let text = match key {
                    Some(k) => format!(
                        "{{ SELECT {k} ({aggregate} AS {target}) WHERE {{ {inner} }} GROUP BY {k} }}"
                    ),
                    None => {
                        format!("{{ SELECT ({aggregate} AS {target}) WHERE {{ {inner} }} }}")
                    }
                };
                let mut scope: BTreeSet<&'static str> = key.into_iter().collect();
                if let Some(v) = VARS.iter().find(|v| **v == target) {
                    scope.insert(v);
                }
                (text, scope)
            }
        }
    }

    fn query(&mut self) -> Shape {
        let (pattern, _) = self.pattern(3);
        let (head, tail, projected) = if self.rng.below(10) < 3 {
            let keys: Vec<&str> = VARS
                .iter()
                .copied()
                .filter(|_| self.rng.below(3) == 0)
                .collect();
            let aggregates = (0..1 + self.rng.below(3))
                .map(|i| format!("({} AS ?n{i})", self.aggregate()))
                .collect::<Vec<_>>();
            let mut projected: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
            projected.extend((0..aggregates.len()).map(|i| format!("?n{i}")));
            let group = if keys.is_empty() {
                String::new()
            } else {
                format!(" GROUP BY {}", keys.join(" "))
            };
            let having = if self.rng.below(4) == 0 {
                " HAVING (COUNT(*) > 1)"
            } else {
                ""
            };
            (
                format!("SELECT {} {}", keys.join(" "), aggregates.join(" ")),
                format!("{group}{having}"),
                projected,
            )
        } else {
            let mut projected: Vec<String> = VARS.iter().map(|v| v.to_string()).collect();
            projected.extend(self.fresh_vars.iter().cloned());
            let distinct = ["", "DISTINCT "][(self.rng.below(4) == 0) as usize];
            (
                format!("SELECT {distinct}{}", projected.join(" ")),
                String::new(),
                projected,
            )
        };
        let (order_text, order) = match self.rng.below(6) {
            0 | 1 => (String::new(), Order::None),
            2 if self.rng.below(3) == 0 => {
                let condition = self.exists();
                (format!(" ORDER BY ({condition})"), Order::Expr)
            }
            2 => {
                let v = self.var();
                (format!(" ORDER BY DESC(STR({v})) {v}"), Order::Expr)
            }
            _ => {
                let n = 1 + self.rng.below(projected.len().min(2) as u64) as usize;
                let first = self.rng.below(projected.len() as u64) as usize;
                let keys: Vec<String> = (0..n)
                    .map(|i| projected[(first + i) % projected.len()].clone())
                    .collect();
                let keys: Vec<String> = keys
                    .iter()
                    .map(|k| {
                        if self.rng.below(2) == 0 {
                            format!("DESC({k})")
                        } else {
                            k.clone()
                        }
                    })
                    .collect();
                (format!(" ORDER BY {}", keys.join(" ")), Order::Vars(keys))
            }
        };
        let limit = (self.rng.below(3) == 0).then(|| self.rng.below(6));
        let offset = (self.rng.below(4) == 0).then(|| self.rng.below(4));
        let base = format!("{head} WHERE {{ {pattern} }}{tail}{order_text}");
        let mut query = base.clone();
        if let Some(n) = limit {
            query.push_str(&format!(" LIMIT {n}"));
        }
        if let Some(n) = offset {
            query.push_str(&format!(" OFFSET {n}"));
        }
        Shape {
            unsliced: (limit.is_some() || offset.is_some()).then_some(base),
            query,
            order,
            offset: offset.is_some_and(|n| n > 0),
            limit: limit.is_some(),
        }
    }
}

type Rows = Vec<Vec<Option<Term>>>;

fn eager(snapshot: Arc<Snapshot>, q: &str, opts: &QueryOptions) -> (Vec<String>, Rows) {
    let r = query(snapshot, q, opts).unwrap_or_else(|e| panic!("{q}: {e}"));
    (r.variables().to_vec(), r.application_rows())
}

/// Cursor rows, reordered to the columns of `vars`.
fn streamed(
    snapshot: Arc<Snapshot>,
    q: &str,
    opts: &QueryOptions,
    rows: usize,
    vars: &[String],
) -> Rows {
    let c = select_cursor(
        snapshot,
        q,
        opts,
        &CursorOptions {
            batch_rows: rows,
            ..Default::default()
        },
    )
    .unwrap_or_else(|e| panic!("{q}: {e}"));
    let columns: Vec<usize> = vars
        .iter()
        .map(|v| {
            c.variables()
                .iter()
                .position(|x| x == v)
                .unwrap_or_else(|| panic!("{q}: cursor lacks {v}"))
        })
        .collect();
    all(c)
        .into_iter()
        .map(|row| columns.iter().map(|&i| row[i].clone()).collect())
        .collect()
}

/// Consecutive rows with equal ORDER BY keys, with the rows of each run as a bag.
fn runs(rows: &Rows, keys: &[usize]) -> Vec<(Vec<Option<Term>>, Vec<String>)> {
    let mut out: Vec<(Vec<Option<Term>>, Vec<String>)> = Vec::new();
    for row in rows {
        let key: Vec<Option<Term>> = keys.iter().map(|&k| row[k].clone()).collect();
        match out.last_mut() {
            Some((last, members)) if *last == key => members.push(format!("{row:?}")),
            _ => out.push((key, vec![format!("{row:?}")])),
        }
    }
    for (_, members) in &mut out {
        members.sort();
    }
    out
}

fn contained(got: &Rows, all: &Rows) -> bool {
    let mut counts = std::collections::HashMap::<String, usize>::new();
    for row in all {
        *counts.entry(format!("{row:?}")).or_default() += 1;
    }
    got.iter().all(|row| {
        counts
            .get_mut(&format!("{row:?}"))
            .is_some_and(|n| std::mem::replace(n, n.wrapping_sub(1)) > 0)
    })
}

fn compare(shape: &Shape, vars: &[String], expected: &Rows, got: &Rows, unsliced: Option<&Rows>) {
    let q = &shape.query;
    assert_eq!(got.len(), expected.len(), "row count: {q}");
    if let Some(all) = unsliced {
        assert!(contained(got, all), "rows outside the unsliced answer: {q}");
    }
    match &shape.order {
        Order::Vars(keys) => {
            let keys: Vec<usize> = keys
                .iter()
                .map(|k| {
                    let k = k.trim_start_matches("DESC(").trim_end_matches(')');
                    vars.iter()
                        .position(|v| format!("?{v}") == k || v == k)
                        .unwrap()
                })
                .collect();
            let (e, g) = (runs(expected, &keys), runs(got, &keys));
            assert_eq!(e.len(), g.len(), "ordered runs: {q}");
            let last = e.len().saturating_sub(1);
            for (i, (a, b)) in e.iter().zip(&g).enumerate() {
                assert_eq!(a.0, b.0, "order key of run {i}: {q}");
                assert_eq!(a.1.len(), b.1.len(), "size of run {i}: {q}");
                // A slice may cut a run of ties, and either engine may keep any of it.
                let cut = (i == 0 && shape.offset) || (i == last && shape.limit);
                if !cut {
                    assert_eq!(a.1, b.1, "rows of run {i}: {q}");
                }
            }
        }
        _ if unsliced.is_some() => {}
        _ => assert_eq!(bag(got.clone()), bag(expected.clone()), "{q}"),
    }
}

/// The quads of a snapshot, for the reference evaluator.
fn snapshot_quads(snapshot: &Snapshot) -> Vec<oxrdf::Quad> {
    let mut out = Vec::new();
    snapshot
        .for_each_quad(|q| {
            out.push(snapshot.quad_to_terms(q).expect("a stored quad decodes"));
            Ok(())
        })
        .unwrap();
    out
}

fn reference_select(
    oracle: &reference::Reference,
    q: &str,
    context: &str,
) -> (Vec<String>, Rows, reference::Notes) {
    match oracle.run_noting_choices(q, None) {
        Ok((reference::Answer::Select { variables, rows }, notes)) => (variables, rows, notes),
        other => panic!("{context}, reference: {other:?}: {q}"),
    }
}

/// Run `seeds` generated queries against one view of the data and compare the eager
/// answer and cursor answers at several batch sizes with the answer of the reference
/// evaluator over `quads`, the quads the view shows. The cursor answers are also
/// compared with the eager one, including how each breaks ties.
fn differential(
    name: &str,
    snapshot: &Arc<Snapshot>,
    opts: &QueryOptions,
    seeds: u64,
    quads: Vec<oxrdf::Quad>,
) {
    let seeds = std::env::var("SPARKLES_CURSOR_SEEDS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(seeds);
    let oracle = reference::Reference::new(quads).with_union_default(snapshot.union_default_graph);
    // SPARKLES_REFERENCE_KEEP_GOING reports every disagreement with the reference
    // before failing, for triage.
    let keep_going = std::env::var_os("SPARKLES_REFERENCE_KEEP_GOING").is_some();
    let disagreements = std::cell::Cell::new(0);
    let (mut skipped, mut unordered, mut partial, mut readings) = (0, 0, 0, 0);
    // the number of answers for each reason the comparison gives, which
    // SPARKLES_REFERENCE_SKIPS prints
    let mut reasons: std::collections::BTreeMap<String, usize> = Default::default();
    for seed in 1..=seeds {
        let shape = Gen::new(seed).query();
        let q = &shape.query;
        let context = format!("{name} seed {seed}");
        let selected = match reference::check::under_engine_readings(&oracle, q) {
            Ok((reference::check::Expected::Select(s), taken)) => {
                readings += usize::from(!taken.is_empty());
                s
            }
            other => panic!("{context}, reference: {other:?}: {q}"),
        };
        let (vars, expected) = eager(snapshot.clone(), q, opts);
        let mut check =
            |got: &Rows, what: &str| match reference::check::check_select(&selected, &vars, got) {
                Ok(reference::check::Outcome::Compared) => {}
                Ok(outcome) => {
                    let (kind, why) = match outcome {
                        reference::check::Outcome::Skipped(why) => {
                            skipped += 1;
                            ("skipped", why)
                        }
                        reference::check::Outcome::OrderUnchecked(why) => {
                            unordered += 1;
                            ("unordered", why)
                        }
                        reference::check::Outcome::Partial(why) => {
                            partial += 1;
                            ("partial", why)
                        }
                        reference::check::Outcome::Compared => unreachable!(),
                    };
                    let why = why.split(':').next().unwrap_or_default();
                    *reasons.entry(format!("{kind}, {why}")).or_default() += 1;
                }
                Err(m) => {
                    eprintln!("{name} seed {seed}, {what} against the reference: {m}: {q}");
                    disagreements.set(disagreements.get() + 1);
                    if !keep_going {
                        panic!("{name} seed {seed}, {what} against the reference: {m}: {q}");
                    }
                }
            };
        check(&expected, "eager");
        let unsliced = shape
            .unsliced
            .as_ref()
            .map(|u| eager(snapshot.clone(), u, opts).1);
        for rows in [1, 2, 3, 4096] {
            let got = streamed(snapshot.clone(), q, opts, rows, &vars);
            check(&got, &format!("cursor of {rows} rows"));
            let message = format!("{name} seed {seed}, {rows} rows");
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compare(&shape, &vars, &expected, &got, unsliced.as_ref())
            }));
            if let Err(e) = result {
                eprintln!("{message}");
                std::panic::resume_unwind(e);
            }
        }
    }
    eprintln!(
        "{name}: of {seeds} queries and their five answers each, {skipped} answers have rows \
         that the specification leaves open, {unordered} were compared without their order, \
         {partial} were compared with some cells only in part, and {readings} queries compare under the engine's reading of an open roadmap item"
    );
    if std::env::var_os("SPARKLES_REFERENCE_SKIPS").is_some() {
        for (why, n) in &reasons {
            eprintln!("{name}: {n} answers {why}");
        }
    }
    assert_eq!(
        disagreements.get(),
        0,
        "{name}: disagreements with the reference"
    );
}

/// The triples the differential queries read, drawn from a fixed seed.
fn differential_triples(seed: u64) -> Vec<String> {
    let int = |n: i32| format!("\"{n}\"^^<http://www.w3.org/2001/XMLSchema#integer>");
    let mut rng = Rng(seed);
    let mut out = Vec::new();
    for i in 0..3 {
        for j in 0..3 {
            if rng.below(2) == 0 {
                out.push(format!("<urn:s:{i}> <urn:p> {}", int(j)));
            }
            if rng.below(2) == 0 {
                out.push(format!("<urn:s:{i}> <urn:q> <urn:s:{j}>"));
            }
            if rng.below(6) == 0 {
                out.push(format!("<urn:s:{i}> <urn:p> <urn:s:{j}>"));
            }
            if rng.below(6) == 0 {
                out.push(format!("<urn:s:{i}> <urn:q> {}", int(j)));
            }
        }
    }
    out
}

fn load_triples(s: &Store, triples: &[String]) {
    let data: String = triples.iter().map(|t| format!("{t} .\n")).collect();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
}

/// How many seeds each view runs by default; `SPARKLES_CURSOR_SEEDS` overrides it.
const SEEDS: u64 = 300;

/// Seeded differential over a compacted base: generated VALUES, scans, property path
/// sequences, joins, OPTIONAL, MINUS, UNION, FILTER (with EXISTS), BIND, ordered and
/// sliced subqueries, grouped subqueries, and top-level DISTINCT, GROUP BY with
/// aggregates, ORDER BY, LIMIT and OFFSET.
#[test]
fn random_queries_agree_between_cursor_and_eager_on_a_compacted_base() {
    let s = Store::in_memory(StoreOptions::default());
    load_triples(&s, &differential_triples(0x5eed_cafe_f00d_d00d));
    s.compact().unwrap();
    let snapshot = s.snapshot();
    assert!(snapshot.delta.is_empty());
    let quads = snapshot_quads(&snapshot);
    differential("compacted", &snapshot, &Default::default(), SEEDS, quads);
}

/// The same over a base with a pending delta of inserts and deletes.
#[test]
fn random_queries_agree_between_cursor_and_eager_over_a_pending_delta() {
    let s = Store::in_memory(StoreOptions::default());
    let triples = differential_triples(0xdead_beef_1234_5678);
    let (old, new) = triples.split_at(triples.len() / 2);
    load_triples(&s, old);
    s.compact().unwrap();
    let deleted: Vec<&String> = old.iter().step_by(3).collect();
    let inserts: String = new.iter().map(|t| format!("{t} . ")).collect();
    let deletes: String = deleted.iter().map(|t| format!("{t} . ")).collect();
    sparkles_core::sparql::update::update(
        &s,
        &format!("INSERT DATA {{ {inserts} }} ; DELETE DATA {{ {deletes} }}"),
        &Default::default(),
    )
    .unwrap();
    let snapshot = s.snapshot();
    assert!(!snapshot.delta.is_empty());
    let quads = snapshot_quads(&snapshot);
    differential("delta", &snapshot, &Default::default(), SEEDS, quads);
}

/// The same over data that spans several index blocks. Copies of each triple in named
/// graphs sit between the default graph's rows in every permutation, so the scans of
/// the default graph cross block boundaries.
#[test]
fn random_queries_agree_between_cursor_and_eager_across_index_blocks() {
    let s = Store::in_memory(StoreOptions::default());
    let triples = differential_triples(0x0b10_c4ed_0000_0001);
    let copies = (sparkles_core::index::BLOCK_ROWS * 2).div_ceil(triples.len());
    let mut data = String::new();
    for (i, t) in triples.iter().enumerate() {
        data.push_str(&format!("{t} .\n"));
        // Leave some triples without copies, so that some ranges pass whole.
        if i % 4 != 3 {
            for g in 0..copies {
                data.push_str(&format!("{t} <urn:g:{g}> .\n"));
            }
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let snapshot = s.snapshot();
    assert!(snapshot.perm(sparkles_core::index::Perm::Pso).blocks.len() > 1);
    let quads = snapshot_quads(&snapshot);
    differential("blocks", &snapshot, &Default::default(), SEEDS / 3, quads);
}

/// The same through a view that hides some triples, which reads a masked snapshot, over
/// a base with a pending delta.
#[test]
fn random_queries_agree_between_cursor_and_eager_through_a_masked_view() {
    use sparkles_core::access::{
        Caller, GraphAccess, Graphs, Limits, Protection, Rule, TripleRules,
    };
    let s = Store::in_memory(StoreOptions::default());
    let mut triples = differential_triples(0x0a11_0ced_0000_0002);
    triples.push("<urn:s:1> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:Hidden>".into());
    let extra = triples.split_off(triples.len() - 4);
    load_triples(&s, &triples);
    s.compact().unwrap();
    let inserts: String = extra.iter().map(|t| format!("{t} . ")).collect();
    sparkles_core::sparql::update::update(
        &s,
        &format!("INSERT DATA {{ {inserts} <urn:s:2> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:Hidden> }}"),
        &Default::default(),
    )
    .unwrap();
    let protection = Protection {
        name: "hidden".into(),
        predicates: Some(vec!["urn:p".into()]),
        classes: Some(vec!["urn:Hidden".into()]),
        subclasses: true,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    };
    let access = GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    let opts = QueryOptions {
        graphs: Some(Arc::new(access)),
        ..Default::default()
    };
    let snapshot = s.snapshot();
    // The view must hide something, or this would repeat the delta test.
    let q = "SELECT * WHERE { ?s <urn:p> ?o }";
    assert!(
        query(snapshot.clone(), q, &opts).unwrap().len()
            < query(snapshot.clone(), q, &Default::default())
                .unwrap()
                .len()
    );
    // The protection hides the <urn:p> triples of the subjects typed <urn:Hidden>. The
    // reference reads the quads that remain, which a plain scan through the view must
    // also return.
    let all = snapshot_quads(&snapshot);
    let hidden_subjects: Vec<oxrdf::NamedOrBlankNode> = all
        .iter()
        .filter(|q| {
            q.predicate.as_str() == "http://www.w3.org/1999/02/22-rdf-syntax-ns#type"
                && q.object == Term::NamedNode(oxrdf::NamedNode::new_unchecked("urn:Hidden"))
        })
        .map(|q| q.subject.clone())
        .collect();
    let visible: Vec<oxrdf::Quad> = all
        .into_iter()
        .filter(|q| !(q.predicate.as_str() == "urn:p" && hidden_subjects.contains(&q.subject)))
        .collect();
    assert_eq!(
        query(snapshot.clone(), "SELECT * WHERE { ?s ?p ?o }", &opts)
            .unwrap()
            .len(),
        visible.len()
    );
    differential("masked", &snapshot, &opts, SEEDS, visible);
}

/// A count over a join runs the join as a cursor operator of its own and counts its
/// rows, so the strict policy admits it and no operator materializes (item 829).
#[test]
fn count_join_counts_a_streamed_join() {
    use std::sync::atomic::AtomicU64;
    let s = store(60);
    let data = (0..60)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:q> <urn:s:{}> .\n<urn:s:{i}> <urn:r> <urn:g:{}> .\n",
                (i * 7) % 60,
                i % 3
            )
        })
        .collect::<String>()
        // four pairs with `?s <urn:q> ?t` on both variables, then 60 rows that match
        // nothing
        + "<urn:s:1> <urn:w> <urn:s:7> .\n<urn:s:2> <urn:w> <urn:s:14> .\n"
        + "<urn:s:3> <urn:w> <urn:s:21> .\n<urn:s:4> <urn:w> <urn:s:28> .\n"
        + &(0..60)
            .map(|i| format!("<urn:z:{i}> <urn:w> <urn:z:{i}> .\n"))
            .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    fn has(plan: &sparkles_core::sparql::CursorPlan, op: &str) -> bool {
        plan.operator.operator == op || plan.children.iter().any(|c| has(c, op))
    }
    let pairs = |n: usize, undef: bool| {
        (0..n)
            .map(|i| match undef && i % 4 == 0 {
                true => format!("(UNDEF {i})"),
                false => format!("(<urn:s:{}> {i})", i % 2),
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let two_keys = (0..20)
        .map(|i| format!("(1 1 {i})"))
        .collect::<Vec<_>>()
        .join(" ");
    for (q, count_join) in [
        // a merge join on one key, which eager counts from its key runs
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:p> ?o FILTER(?o < 20) ?s <urn:q> ?t }".to_string(),
            true,
        ),
        // a join over a sorted union, which the cursor plans as a merge join
        (
            "SELECT (COUNT(*) AS ?n) WHERE { { ?s <urn:p> ?o } UNION { ?s <urn:q> ?o } ?s <urn:q> ?t }".to_string(),
            true,
        ),
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:p> ?o FILTER(?o > 1000) ?s <urn:q> ?t }".to_string(),
            true,
        ),
        // 1180 pairs of inputs with at most 60 rows each
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:r> ?g FILTER(?s != <urn:s:0>) ?t <urn:r> ?g }".to_string(),
            true,
        ),
        // two shared variables, 400 pairs
        (
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ VALUES (?a ?b ?i) {{ {two_keys} }} VALUES (?a ?b ?j) {{ {two_keys} }} }}"
            ),
            false,
        ),
        // a hash join on two variables that BIND leaves unsorted, whose four pairs come
        // before rows that match nothing, so the join reads input after its last pair
        // (a merge join would stop at the end of one input, which eager reads whole);
        // eager refuses it at a work limit equal to its own work, as item 1103 records,
        // and so must the streamed count once it has read that input
        (
            "SELECT (COUNT(*) AS ?n) WHERE { { ?a <urn:q> ?b BIND(?a AS ?s) BIND(?b AS ?t) } ?s <urn:w> ?t }".to_string(),
            true,
        ),
        // a merge join on two variables, which the eager kernel counts (item 1129)
        (
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:q> ?t . ?s <urn:w> ?t }".to_string(),
            true,
        ),
        // a key that some rows leave unbound
        (
            format!(
                "SELECT (COUNT(*) AS ?n) WHERE {{ VALUES (?s ?i) {{ {} }} ?s <urn:q> ?t }}",
                pairs(40, true)
            ),
            false,
        ),
    ] {
        let q = q.as_str();
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for rows in [1, 2, 3, 4096] {
            let c = open(&s, q, rows);
            assert!(!c.plan().has_materialization(), "{q}: {:?}", c.plan());
            if count_join {
                assert!(has(c.plan(), "CountJoin"), "{q}: {:?}", c.plan());
            }
            if q.contains("BIND") {
                assert!(has(c.plan(), "HashJoin"), "{q}: {:?}", c.plan());
            }
            assert_eq!(all(c), expected, "{q}; {rows}");
        }
        // The pairs of the join below the count are not rows that the eager count
        // produces, so they stay out of the work count. The eager count admits them
        // against the row limit and the work limit when it forms them, and not when it
        // counts a merge join's key runs, and the streamed count follows it, so both
        // modes give the same outcome at each limit (Sol review of item 829, findings 1
        // of rounds 1 and 2).
        let with = |rows: Option<usize>, limit: Option<u64>, work: Option<Arc<AtomicU64>>| {
            QueryOptions {
                no_cache: true,
                max_rows: rows,
                max_rows_produced: limit,
                work,
                ..Default::default()
            }
        };
        type Outcome = Result<Vec<Vec<Option<Term>>>, Option<BudgetKind>>;
        let outcome = |r: Result<Vec<Vec<Option<Term>>>, Error>| -> Outcome {
            r.map_err(|e| match e {
                Error::BudgetExceeded(b) => Some(b.kind),
                _ => None,
            })
        };
        let eager = |rows: Option<usize>, limit: Option<u64>| -> Outcome {
            outcome(query(s.snapshot(), q, &with(rows, limit, None)).map(|r| r.application_rows()))
        };
        let streamed = |rows: Option<usize>, limit: Option<u64>, batch: usize| -> (Outcome, u64) {
            let work = Arc::new(AtomicU64::new(0));
            let run = || -> Result<_, Error> {
                let opts = with(rows, limit, Some(work.clone()));
                let mut c = select_cursor(s.snapshot(), q, &opts, &options(batch))?;
                let mut answer = Vec::new();
                while let Some(b) = c.next_batch()? {
                    for row in 0..b.len() {
                        answer.push(b.row(row).unwrap().values().to_vec());
                    }
                }
                Ok(answer)
            };
            let r = outcome(run());
            (r, work.load(Ordering::Relaxed))
        };
        let produced = query(s.snapshot(), q, &with(None, None, None))
            .unwrap()
            .rows_produced;
        let count = match expected[0][0].as_ref() {
            Some(Term::Literal(l)) => l.value().parse::<u64>().unwrap(),
            other => panic!("{q}: {other:?}"),
        };
        let mut limits = vec![
            (None, None),
            (Some(120), None),
            (None, Some(produced)),
            (None, Some(produced - 1)),
            (None, Some(produced - 2)),
            (None, Some(produced - 3)),
            (None, Some(produced + count)),
        ];
        if count > 0 {
            limits.push((None, Some(produced + count - 1)));
        }
        for batch in [1, 3, 4096] {
            let (answer, work) = streamed(None, None, batch);
            assert_eq!(answer, Ok(expected.clone()), "{q}; {batch}");
            assert_eq!(work, produced, "{q}; {batch}");
            for &(rows, limit) in &limits {
                assert_eq!(
                    streamed(rows, limit, batch).0,
                    eager(rows, limit),
                    "{q}; batch {batch}, max_rows {rows:?}, max_rows_produced {limit:?} (produced {produced}, count {count})"
                );
            }
        }
    }
}

/// A count over a merge join on two variables runs its eager kernel as a whole-input
/// operator under either policy, with no join operator below it, because the streamed
/// join costs 1.8 times the eager count (item 1129). It gives the eager answer.
#[test]
fn a_count_over_a_two_variable_merge_join_runs_the_eager_kernel() {
    let s = store(60);
    let data = (0..60)
        .map(|i| {
            let o = (i * 7) % 60;
            let mut t = format!("<urn:s:{i}> <urn:q> <urn:s:{o}> .\n");
            if i % 3 == 0 {
                t += &format!("<urn:s:{i}> <urn:w> <urn:s:{o}> .\n");
            }
            t + &format!("<urn:s:{i}> <urn:w> <urn:z:{i}> .\n")
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    fn ops(plan: &sparkles_core::sparql::CursorPlan, out: &mut Vec<String>) {
        out.push(plan.operator.operator.clone());
        for child in &plan.children {
            ops(child, out);
        }
    }
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:q> ?t . ?s <urn:w> ?t }";
    let expected = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows();
    assert_eq!(
        expected,
        vec![vec![Some(Term::Literal(
            oxrdf::Literal::new_typed_literal("20", oxrdf::vocab::xsd::INTEGER)
        ))]]
    );
    for fallback in [
        FallbackPolicy::RejectMaterialization,
        FallbackPolicy::AllowMaterialization,
    ] {
        for rows in [1, 2, 3, 4096] {
            let opts = CursorOptions {
                batch_rows: rows,
                fallback,
                ..Default::default()
            };
            let c = select_cursor(s.snapshot(), q, &QueryOptions::default(), &opts).unwrap();
            let mut names = Vec::new();
            ops(c.plan(), &mut names);
            assert!(
                !c.plan().has_materialization(),
                "{fallback:?}: {:?}",
                c.plan()
            );
            assert!(
                names.iter().any(|n| n == "CountJoin"),
                "{fallback:?}: {names:?}"
            );
            assert!(
                !names
                    .iter()
                    .any(|n| n.ends_with("Join") && n != "CountJoin"),
                "{fallback:?}: {names:?}"
            );
            fn count_join(
                plan: &sparkles_core::sparql::CursorPlan,
            ) -> Option<&sparkles_core::sparql::CursorPlan> {
                match plan.operator.operator == "CountJoin" {
                    true => Some(plan),
                    false => plan.children.iter().find_map(count_join),
                }
            }
            // It holds both of its inputs.
            assert!(
                count_join(c.plan()).is_some_and(|p| p.growing_state),
                "{fallback:?}: {:?}",
                c.plan()
            );
            assert_eq!(all(c), expected, "{fallback:?}; {rows}");
        }
    }
    // With an empty left input the count reads nothing of its right input, as eager
    // does, so a work limit below the right input's rows refuses neither.
    let empty = "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:q> ?t FILTER(STRSTARTS(STR(?t), \"urn:absent\")) ?s <urn:w> ?t }";
    let limited = |limit: u64| QueryOptions {
        no_cache: true,
        max_rows_produced: Some(limit),
        ..Default::default()
    };
    let expected = query(s.snapshot(), empty, &limited(65))
        .unwrap()
        .application_rows();
    for fallback in [
        FallbackPolicy::RejectMaterialization,
        FallbackPolicy::AllowMaterialization,
    ] {
        for rows in [1, 4096] {
            let o = CursorOptions {
                batch_rows: rows,
                fallback,
                ..Default::default()
            };
            let c = select_cursor(s.snapshot(), empty, &limited(65), &o).unwrap();
            let mut names = Vec::new();
            ops(c.plan(), &mut names);
            assert!(
                names.iter().any(|n| n == "CountJoin"),
                "{fallback:?}: {names:?}"
            );
            assert!(
                !names
                    .iter()
                    .any(|n| n.ends_with("Join") && n != "CountJoin"),
                "{fallback:?}: {names:?}"
            );
            let mut c = c;
            let mut answer = Vec::new();
            while let Some(batch) = c.next_batch().unwrap() {
                for row in 0..batch.len() {
                    answer.push(batch.row(row).unwrap().values().to_vec());
                }
            }
            assert_eq!(c.status(), CursorStatus::Complete);
            assert_eq!(answer, expected, "{fallback:?}; {rows}");
            // The right input never ran, and its plan says so as the eager plan does.
            fn find<'a>(
                plan: &'a sparkles_core::sparql::CursorPlan,
                op: &str,
            ) -> Option<&'a sparkles_core::sparql::CursorPlan> {
                match plan.operator.operator == op {
                    true => Some(plan),
                    false => plan.children.iter().find_map(|c| find(c, op)),
                }
            }
            fn all_skipped(plan: &sparkles_core::sparql::CursorPlan) -> bool {
                let f = plan.operator.fidelity();
                f.skipped.as_deref() == Some("the left side produced no rows")
                    && !f.stopped_early
                    && plan.operator.actual_rows == -1
                    && plan.operator.time_ms == 0.0
                    && plan.children.iter().all(all_skipped)
            }
            let count = find(c.plan(), "CountJoin").expect("a count join");
            assert_eq!(count.children.len(), 2, "{:?}", count);
            assert!(all_skipped(&count.children[1]), "{fallback:?}: {:?}", count);
            assert!(count.children[0].operator.fidelity().skipped.is_none());
        }
    }
}

/// The skipped right input of a count over a two-variable merge join is marked as never
/// run down to the children that an eager kernel describes without operators, here the
/// scans of a count from key runs inside a subquery (Sol review of item 829, round 10).
#[test]
fn a_skipped_count_input_marks_its_described_children() {
    let s = store(60);
    let data = (0..60)
        .map(|i| {
            format!(
                "<urn:s:{i}> <urn:q> <urn:s:{}> .\n<urn:s:{i}> <urn:w> <urn:z:{i}> .\n",
                (i * 7) % 60
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    type Plan = sparkles_core::sparql::CursorPlan;
    fn find<'a>(plan: &'a Plan, op: &str) -> Option<&'a Plan> {
        match plan.operator.operator == op {
            true => Some(plan),
            false => plan.children.iter().find_map(|c| find(c, op)),
        }
    }
    fn all_skipped(plan: &Plan) -> bool {
        let f = plan.operator.fidelity();
        f.skipped.as_deref() == Some("the left side produced no rows")
            && !f.stopped_early
            && !f.incomplete
            && plan.operator.actual_rows == -1
            && plan.operator.time_ms == 0.0
            && plan.children.iter().all(all_skipped)
    }
    let q = "SELECT (COUNT(*) AS ?n) WHERE { \
        ?s <urn:q> ?t FILTER(STRSTARTS(STR(?t), \"urn:absent\")) \
        { SELECT ?s ?t WHERE { \
            { SELECT (COUNT(*) AS ?c) WHERE { ?a <urn:q> ?b . ?a <urn:w> ?d } } \
            VALUES (?s ?t) { (<urn:s:1> <urn:s:7>) } } } }";
    let expected = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows();
    for fallback in [
        FallbackPolicy::RejectMaterialization,
        FallbackPolicy::AllowMaterialization,
    ] {
        for rows in [1, 4096] {
            let o = CursorOptions {
                batch_rows: rows,
                fallback,
                ..Default::default()
            };
            let mut c = select_cursor(s.snapshot(), q, &QueryOptions::default(), &o).unwrap();
            let mut answer = Vec::new();
            while let Some(batch) = c.next_batch().unwrap() {
                for row in 0..batch.len() {
                    answer.push(batch.row(row).unwrap().values().to_vec());
                }
            }
            assert_eq!(c.status(), CursorStatus::Complete);
            assert_eq!(answer, expected, "{fallback:?}; {rows}");
            let count = find(c.plan(), "CountJoin").expect("a count join");
            assert_eq!(count.children.len(), 2, "{count:?}");
            let runs = find(&count.children[1], "CountJoinFromRuns")
                .unwrap_or_else(|| panic!("{fallback:?}: {count:?}"));
            assert!(!runs.children.is_empty(), "{runs:?}");
            assert!(all_skipped(&count.children[1]), "{fallback:?}: {count:?}");
        }
    }
}

/// An operator without a cursor implementation materializes only itself. Its inputs
/// are cursors of their own, and they stream into it.
#[test]
fn a_fallback_materializes_only_its_operator_and_streams_its_inputs() {
    let s = store(60);
    let data = (0..60)
        .map(|i| format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i * 7) % 60))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    // (whether the fallback has inputs, whether an input materializes)
    fn fallbacks(plan: &sparkles_core::sparql::CursorPlan, out: &mut Vec<(bool, bool)>) {
        if plan.materializes {
            out.push((
                !plan.children.is_empty(),
                plan.children.iter().any(|c| c.has_materialization()),
            ));
        }
        for child in &plan.children {
            fallbacks(child, out);
        }
    }
    let opts_q = legacy_view_opts(30);
    for q in [
        format!(
            "SELECT ?s ?o WHERE {{ ?s <urn:p> ?o FILTER EXISTS {{ {LEGACY_VIEW_LEAF} FILTER(?x = STR(?o)) }} }}"
        ),
        format!(
            "SELECT ?s WHERE {{ ?s <urn:q> ?t FILTER NOT EXISTS {{ {LEGACY_VIEW_LEAF} FILTER(?x = \"3\") }} }}"
        ),
        // a BIND over an EXISTS that does not stream reads its whole input, and that
        // input streams into it
        format!(
            "SELECT ?s ?e WHERE {{ ?s <urn:p> ?o FILTER(?o < 5) BIND(EXISTS {{ {LEGACY_VIEW_LEAF} }} AS ?e) }}"
        ),
    ] {
        let q = q.as_str();
        let expected = bag(query(s.snapshot(), q, &opts_q).unwrap().application_rows());
        for rows in [1, 2, 3, 4096] {
            let opts = CursorOptions {
                batch_rows: rows,
                ..Default::default()
            };
            let c = select_cursor(s.snapshot(), q, &opts_q, &opts).unwrap();
            let mut found = Vec::new();
            fallbacks(c.plan(), &mut found);
            assert!(!found.is_empty(), "{q}");
            assert!(
                found.iter().all(|(_, nested)| !nested),
                "{q}: {:?}",
                c.plan()
            );
            assert!(
                found.iter().any(|(inputs, _)| *inputs),
                "{q}: {:?}",
                c.plan()
            );
            assert_eq!(bag(all(c)), expected, "{q}; {rows}");
        }
    }
}

/// Groups with several keys, expression arguments, DISTINCT and GROUP_CONCAT keep
/// running state rather than falling back, and match eager answers, including errors,
/// unbound keys and empty input.
#[test]
fn widened_incremental_groups_match_eager() {
    let s = store(40);
    let rows = "VALUES (?k ?j ?x ?y) { (1 1 1 \"a\") (1 1 1 \"b\") (1 2 2 \"a\") (UNDEF 2 3 \"c\") (2 UNDEF \"x\" <urn:i>) (2 UNDEF 4 UNDEF) (1 1 UNDEF \"a\"@en) (UNDEF UNDEF 5 \"d\") (2 1 1 \"a\") (UNDEF 1 1 \"b\") }";
    for q in [
        format!("SELECT ?k ?j (COUNT(*) AS ?n) (SUM(?x) AS ?s) WHERE {{ {rows} }} GROUP BY ?k ?j"),
        format!("SELECT ?k (SUM(?x * 2) AS ?s) (AVG(?x + 1) AS ?a) (MIN(STR(?y)) AS ?m) WHERE {{ {rows} }} GROUP BY ?k"),
        format!("SELECT ?k (COUNT(DISTINCT ?x) AS ?c) (SUM(DISTINCT ?x) AS ?s) (COUNT(DISTINCT ?y) AS ?d) WHERE {{ {rows} }} GROUP BY ?k"),
        format!("SELECT ?k (GROUP_CONCAT(?y) AS ?g) (GROUP_CONCAT(DISTINCT STR(?y); SEPARATOR=\"|\") AS ?h) WHERE {{ {rows} }} GROUP BY ?k"),
        format!("SELECT (GROUP_CONCAT(?x; SEPARATOR=\",\") AS ?g) (SAMPLE(?x * 1) AS ?a) (MAX(?x) AS ?m) WHERE {{ {rows} }}"),
        format!("SELECT ?j (COUNT(?x / 0) AS ?e) (SUM(?x / 0) AS ?f) WHERE {{ {rows} }} GROUP BY ?j"),
        "SELECT ?k ?j (COUNT(*) AS ?n) (GROUP_CONCAT(?x) AS ?g) WHERE { VALUES (?k ?j ?x) {} } GROUP BY ?k ?j".into(),
        "SELECT (COUNT(DISTINCT ?x) AS ?n) (GROUP_CONCAT(?x) AS ?g) (AVG(?x * 2) AS ?a) WHERE { VALUES ?x {} }".into(),
        format!("SELECT ?k (COUNT(DISTINCT *) AS ?d) (COUNT(*) AS ?n) WHERE {{ {rows} {rows} }} GROUP BY ?k"),
        format!("SELECT (COUNT(DISTINCT *) AS ?d) WHERE {{ {{ {rows} }} UNION {{ {rows} }} }}"),
        "SELECT (COUNT(DISTINCT *) AS ?d) WHERE { VALUES ?x {} }".into(),
        "SELECT ?s ?o (COUNT(*) AS ?n) WHERE { ?s <urn:p> ?o } GROUP BY ?s ?o HAVING (?o > 30)".into(),
        "SELECT ?m (COUNT(?s) AS ?n) (SUM(?o) AS ?t) WHERE { ?s <urn:p> ?o BIND(?o - (?o / 5 - FLOOR(?o / 5)) * 5 AS ?r) } GROUP BY (FLOOR(?o / 10) AS ?m)".into(),
    ] {
        let expected = bag(query(s.snapshot(), &q, &Default::default()).unwrap().application_rows());
        for cap in [1, 2, 3, 4096] {
            let c = open(&s, &q, cap);
            assert!(!c.plan().has_materialization(), "{q}");
            assert_eq!(bag(all(c)), expected, "{q}; cap {cap}");
        }
    }
}

/// Counts per key, from the index statistics or from key runs, keep group state and
/// emit it across batches without falling back, also over pending changes.
#[test]
fn group_count_scans_stream_their_counts() {
    fn has(plan: &sparkles_core::sparql::CursorPlan, prefix: &str) -> bool {
        plan.operator.operator.starts_with(prefix) || plan.children.iter().any(|c| has(c, prefix))
    }
    let s = store(50);
    let data = (0..50)
        .map(|i| format!("<urn:s:{}> <urn:q> <urn:s:{i}> .\n", i % 7))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let queries = [
        "SELECT ?p (COUNT(*) AS ?c) WHERE { ?s ?p ?o } GROUP BY ?p",
        "SELECT ?s (COUNT(*) AS ?c) WHERE { ?s <urn:q> ?o } GROUP BY ?s",
        "SELECT ?o (COUNT(?s) AS ?c) WHERE { ?s <urn:q> ?o } GROUP BY ?o",
    ];
    for round in 0..2 {
        for q in queries {
            let expected = bag(query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows());
            for cap in [1, 2, 3, 4096] {
                let c = open(&s, q, cap);
                assert!(has(c.plan(), "GroupCount"), "{q}: {:?}", c.plan());
                assert!(!c.plan().has_materialization());
                assert_eq!(bag(all(c)), expected, "{q}; cap {cap}; round {round}");
            }
        }
        sparkles_core::sparql::update::update(
            &s,
            "INSERT DATA { <urn:s:1> <urn:q> <urn:new> . <urn:s:9> <urn:q> <urn:s:1> } ; DELETE DATA { <urn:s:3> <urn:q> <urn:s:3> }",
            &Default::default(),
        )
        .unwrap();
    }
}

/// Transitive paths walk one start node at a time, from their own start nodes or from
/// the rows of their input, and match eager answers under the strict policy, over
/// pending changes and in named graphs.
#[test]
fn transitive_paths_stream_one_start_at_a_time() {
    let s = store(30);
    let data = (0..40)
        .map(|i| {
            format!(
                "<urn:n:{i}> <urn:q> <urn:n:{}> .\n<urn:g:{}> {{ <urn:n:{i}> <urn:r> <urn:n:{}> }}\n",
                (i * 7 + 3) % 40,
                i % 3,
                (i + 1) % 40
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(data.into_bytes(), RdfFormat::TriG, None)])
        .unwrap();
    s.compact().unwrap();
    let queries = [
        "SELECT ?s ?o WHERE { ?s <urn:q>+ ?o }",
        "SELECT ?s ?o WHERE { ?s <urn:q>* ?o }",
        "SELECT ?s WHERE { ?s <urn:q>+ ?s }",
        "SELECT ?o WHERE { <urn:n:1> <urn:q>+ ?o }",
        "SELECT ?s WHERE { ?s ^<urn:q>* <urn:n:2> }",
        "SELECT ?g ?s ?o WHERE { GRAPH ?g { ?s <urn:r>+ ?o } }",
        "SELECT ?s ?x WHERE { ?s <urn:p> ?o FILTER(?o < 5) ?s <urn:q>* ?x }",
        "SELECT ?s ?o WHERE { ?s <urn:q>+ ?o } LIMIT 7",
    ];
    for round in 0..2 {
        for q in queries {
            let expected = bag(query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows());
            for cap in [1, 2, 5, 4096] {
                let c = open(&s, q, cap);
                assert!(!c.plan().has_materialization(), "{q}: {:?}", c.plan());
                let got = bag(all(c));
                if q.contains("LIMIT") {
                    let full = "SELECT ?s ?o WHERE { ?s <urn:q>+ ?o }";
                    let full = bag(query(s.snapshot(), full, &Default::default())
                        .unwrap()
                        .application_rows());
                    assert_eq!(got.len(), expected.len(), "{q}; cap {cap}");
                    assert!(got.iter().all(|r| full.contains(r)), "{q}; cap {cap}");
                } else {
                    assert_eq!(got, expected, "{q}; cap {cap}; round {round}");
                }
            }
        }
        sparkles_core::sparql::update::update(
            &s,
            "INSERT DATA { <urn:n:1> <urn:q> <urn:new> . <urn:new> <urn:q> <urn:n:2> . GRAPH <urn:g:0> { <urn:n:5> <urn:r> <urn:new> } } ; DELETE DATA { <urn:n:3> <urn:q> <urn:n:24> }",
            &Default::default(),
        )
        .unwrap();
    }
}

/// A count of a two-hop join over key runs reads its scans itself and counts as a
/// scalar barrier rather than an eager fallback, also over pending changes.
#[test]
fn count_joins_from_key_runs_run_without_fallback() {
    fn has(plan: &sparkles_core::sparql::CursorPlan, prefix: &str) -> bool {
        plan.operator.operator.starts_with(prefix) || plan.children.iter().any(|c| has(c, prefix))
    }
    let s = store(20);
    let data = (0..60)
        .map(|i| format!("<urn:s:{}> <urn:k> <urn:s:{}> .\n", i % 13, (i * 5) % 17))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let q = "SELECT (COUNT(*) AS ?n) WHERE { ?a <urn:k> ?b . ?b <urn:k> ?c }";
    for round in 0..2 {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for cap in [1, 4096] {
            let c = open(&s, q, cap);
            if round == 0 {
                assert!(has(c.plan(), "CountJoinFromRuns"), "{:?}", c.plan());
            }
            assert!(!c.plan().has_materialization());
            assert_eq!(all(c), expected, "cap {cap}; round {round}");
        }
        sparkles_core::sparql::update::update(
            &s,
            "INSERT DATA { <urn:s:1> <urn:k> <urn:s:2> . <urn:s:2> <urn:k> <urn:s:1> } ; DELETE DATA { <urn:s:0> <urn:k> <urn:s:0> }",
            &Default::default(),
        )
        .unwrap();
    }
}

/// Values of every ORDER BY class, including dates with and without a timezone close
/// to each other (no total order) and numbers equal across types.
fn mixed_order_store(seed: u64, n: usize) -> Store {
    let mut x = seed;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut data = String::from("@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n");
    for i in 0..n {
        let r = next();
        let small = (r >> 8) as i64 % 30 - 15;
        let v = match r % 13 {
            0 | 1 => format!("{small}"),
            2 => format!("\"{small}.50\"^^xsd:decimal"),
            3 => format!("{small}.0e0"),
            4 => format!("\"{small}.25\"^^xsd:float"),
            5 => format!("\"s{}\"", small.abs()),
            6 => format!(
                "\"l{}\"@{}",
                small.abs() % 4,
                ["en", "EN", "fr"][(r >> 20) as usize % 3]
            ),
            7 => format!("<urn:o:{}>", small.abs() % 6),
            8 => format!("_:b{}", small.abs() % 3),
            9 => format!(
                "\"2020-01-0{}T{:02}:00:00{}\"^^xsd:dateTime",
                1 + (r >> 24) % 2,
                (r >> 28) % 24,
                ["", "Z", "+14:00", "-05:00"][(r >> 33) as usize % 4]
            ),
            10 => format!("\"P{}D\"^^xsd:dayTimeDuration", small.abs()),
            11 => ["true", "false", "\"x\"^^<urn:dt>", "\"NaN\"^^xsd:double"]
                [(r >> 9) as usize % 4]
                .to_string(),
            _ => format!("\"2020-01-0{}\"^^xsd:date", 1 + (r >> 24) % 3),
        };
        let subject = i % (n / 2 + 1);
        data.push_str(&format!("<urn:s:{subject}> <urn:v> {v} .\n"));
        if r % 3 != 0 {
            data.push_str(&format!("<urn:s:{subject}> <urn:w> {} .\n", small * 7));
        }
    }
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

#[test]
fn streaming_top_k_matches_eager_in_order_at_every_batch_size() {
    let s = mixed_order_store(0x2545_f491_4f6c_dd1d, 3000);
    let queries = [
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 10",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 25",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v ?s LIMIT 40 OFFSET 7",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) DESC(?s) LIMIT 1",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ABS(?v) ?s LIMIT 30",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(STR(?v)) ?v LIMIT 12",
        "SELECT ?s ?v ?w WHERE { ?s <urn:v> ?v OPTIONAL { ?s <urn:w> ?w } } ORDER BY DESC(?w) ?v ?s LIMIT 20",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v FILTER(isNumeric(?v)) } ORDER BY DESC(?v) LIMIT 15",
        "SELECT ?v WHERE { ?s <urn:v> ?v FILTER(DATATYPE(?v) = <http://www.w3.org/2001/XMLSchema#dateTime>) } ORDER BY ?v LIMIT 5",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY (?v * 2) LIMIT 2999",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 100000",
    ];
    for q in queries {
        let expected = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for (rows, bytes) in [
            (1, 1 << 20),
            (2, 1 << 20),
            (3, 16),
            (7, 1 << 20),
            (4096, 1 << 20),
        ] {
            let c = select_cursor(
                s.snapshot(),
                q,
                &Default::default(),
                &CursorOptions {
                    batch_bytes: bytes,
                    ..options(rows)
                },
            )
            .unwrap_or_else(|e| panic!("{q}: {e}"));
            assert!(!c.plan().has_materialization(), "{q}");
            assert_eq!(all(c), expected, "{q}; batch {rows}");
        }
    }
}

#[test]
fn streaming_top_k_matches_eager_with_compacted_iris_and_offset() {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        b"<urn:a> <urn:p> <urn:b> . <urn:c> <urn:p> <urn:d> . <urn:e> <urn:p> <urn:f> .".to_vec(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s.compact().unwrap();
    let query_text = "SELECT ?row WHERE {
        VALUES (?rank ?iri ?row) {
            (2 <urn:d> 0) (2 <urn:f> 1) (2 <urn:e> 2)
            (2 <urn:c> 3) (2 <urn:e> 4) (2 <urn:b> 5)
            (2 <urn:f> 6) (1 <urn:f> 7) (2 <urn:a> 8) (2 <urn:d> 9)
        }
    } ORDER BY ?rank ?iri LIMIT 2 OFFSET 1";
    let expected = query(s.snapshot(), query_text, &Default::default())
        .unwrap()
        .application_rows();
    assert_eq!(
        expected,
        [8, 5].map(|row: i32| vec![Some(Term::Literal(row.into()))]),
    );
    for rows in [1, 2, 3, 10] {
        let cursor = select_cursor(
            s.snapshot(),
            query_text,
            &Default::default(),
            &options(rows),
        )
        .unwrap();
        assert!(!cursor.plan().has_materialization());
        assert_eq!(all(cursor), expected, "batch {rows}");
    }
}

#[test]
fn streaming_top_k_keeps_only_its_rows_within_the_memory_budget() {
    let s = store(200_000);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(?o) LIMIT 5";
    let budget = |heap: bool| QueryOptions {
        max_memory_bytes: Some(2 << 20),
        optimizations: Some(sparkles_core::sparql::Optimizations {
            topk_heap: heap,
            ordered_topk: false,
            ..sparkles_core::sparql::Optimizations::ALL
        }),
        ..Default::default()
    };
    let mut c = select_cursor(s.snapshot(), q, &budget(true), &options(4096)).unwrap();
    fn reasons(p: &sparkles_core::sparql::CursorPlan, out: &mut String) {
        out.push_str(p.reason.as_deref().unwrap_or_default());
        p.children.iter().for_each(|c| reasons(c, out));
    }
    let mut reason = String::new();
    reasons(c.plan(), &mut reason);
    assert!(reason.contains("best 5 rows"), "{reason}");
    assert!(!c.plan().has_materialization());
    let mut rows = Vec::new();
    while let Some(b) = c.next_batch().unwrap() {
        for i in 0..b.len() {
            rows.push(b.row(i).unwrap().values().to_vec());
        }
    }
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0][1], Some(oxrdf::Literal::from(199_999).into()));
    // the input's IDs alone take 3.2 MB, more than the budget
    assert!(c.stats().mem_peak_bytes <= 2 << 20);
    // collecting the whole input for a sort does not fit
    let mut c = select_cursor(s.snapshot(), q, &budget(false), &options(4096)).unwrap();
    assert!(matches!(
        c.next_batch(),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));
}

#[test]
fn a_cursor_deadline_excludes_time_its_consumer_paused() {
    // No step depends on how fast the host runs. Both cursors start with an hour of
    // credited waiting, so their first batches cannot time out. The slow cursor then
    // loses its credit after its deadline has passed, and the paused one keeps it.
    let s = store(200);
    let timeout = Duration::from_millis(100);
    let opts = QueryOptions {
        timeout: Some(timeout),
        ..Default::default()
    };
    let hour = 3_600_000_000_000;
    let open = || select_cursor(s.snapshot(), "SELECT * {?s ?p ?o}", &opts, &options(16)).unwrap();
    let (mut slow, mut paused) = (open(), open());
    // both deadlines come before `opened + timeout`
    let opened = std::time::Instant::now();
    for c in [&mut slow, &mut paused] {
        c.deadline_pause().store(hour, Ordering::Relaxed);
        assert!(c.next_batch().unwrap().is_some());
        assert_eq!(c.status(), CursorStatus::Open);
    }
    while opened.elapsed() <= timeout {
        std::thread::sleep(timeout);
    }
    // a consumer that did not report its wait has used up the deadline
    slow.deadline_pause().store(0, Ordering::Relaxed);
    assert!(matches!(slow.next_batch(), Err(Error::Timeout)));
    let mut rows = 16;
    while let Some(batch) = paused.next_batch().unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 200);
    assert_eq!(paused.status(), CursorStatus::Complete);
}

#[test]
fn work_that_copies_the_deadline_takes_the_paused_one() {
    use sparkles_core::sparql::Ctx;
    let s = store(1);
    let mut ctx = Ctx::new(s.snapshot());
    assert_eq!(ctx.current_deadline(), None);
    ctx.deadline = Some(std::time::Instant::now() - Duration::from_secs(1));
    assert!(ctx.past_deadline());
    ctx.deadline_pause
        .fetch_add(3_600_000_000_000, Ordering::Relaxed);
    assert!(!ctx.past_deadline());
    // a SERVICE call or an algorithm run that starts now gets the moved deadline
    assert!(ctx.current_deadline().unwrap() > std::time::Instant::now());
}

/// A store of `n` subjects with a name, of which every seventh contains "Ada", and an
/// integer that repeats every 100 subjects.
fn named(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..n)
        .map(|i| {
            let name = if i % 7 == 0 { "Ada" } else { "Grace" };
            format!(
                "<urn:s:{i}> <urn:name> \"{name} {i}\" .\n<urn:s:{i}> <urn:age> {} .\n",
                i % 100
            )
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

fn without(names: &str) -> QueryOptions {
    QueryOptions {
        optimizations: Some(
            sparkles_core::sparql::Optimizations::default()
                .disable(names)
                .unwrap(),
        ),
        ..Default::default()
    }
}

fn without_decode_ahead() -> QueryOptions {
    QueryOptions {
        optimizations: Some(sparkles_core::sparql::Optimizations {
            decode_ahead: false,
            ..Default::default()
        }),
        ..Default::default()
    }
}

#[test]
fn writers_decoding_ahead_match_eager_at_send_limits_across_batches() {
    use sparkles_core::sparql::results::{
        SolutionsFormat, write_cursor_solutions, write_solutions,
    };
    let s = named(9000);
    let q = "SELECT ?s ?n ?a WHERE { ?s <urn:name> ?n ; <urn:age> ?a }";
    let eager = query(s.snapshot(), q, &Default::default()).unwrap();
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    for fmt in [SolutionsFormat::Tsv, SolutionsFormat::Json] {
        for send in [None, Some(0), Some(1), Some(4096), Some(4097), Some(8999)] {
            let mut expected = Vec::new();
            write_solutions(&eager, fmt, &mut expected, send).unwrap();
            for opts in [QueryOptions::default(), without_decode_ahead()] {
                let mut c = select_cursor(s.snapshot(), q, &opts, &options(1000)).unwrap();
                let mut output = Vec::new();
                let stats = pool
                    .install(|| write_cursor_solutions(&mut c, fmt, &mut output, send))
                    .unwrap();
                assert_eq!(output, expected, "{fmt:?}, send {send:?}");
                let n = send.unwrap_or(9000).min(9000) as u64;
                assert_eq!(stats.emitted_rows, n, "{fmt:?}, send {send:?}");
            }
        }
    }
}

#[test]
fn a_failure_while_decoding_ahead_comes_after_the_rows_before_it() {
    use sparkles_core::sparql::results::{SolutionsFormat, write_cursor_solutions};
    let s = store(5000);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    for opts in [QueryOptions::default(), without_decode_ahead()] {
        let opts = QueryOptions {
            max_rows_produced: Some(2500),
            ..opts
        };
        let mut c = select_cursor(s.snapshot(), q, &opts, &options(1000)).unwrap();
        let mut output = Vec::new();
        let result = pool
            .install(|| write_cursor_solutions(&mut c, SolutionsFormat::Tsv, &mut output, None));
        assert!(result.is_err(), "the row budget stops the scan");
        assert_eq!(c.status(), CursorStatus::Failed);
        // the header and the whole batches before the failing pull
        let text = String::from_utf8(output).unwrap();
        let rows = text.lines().count() - 1;
        assert!(
            (1000..5000).contains(&rows) && rows.is_multiple_of(1000),
            "{rows} rows"
        );
    }
}

#[test]
fn long_filters_and_top_k_read_growing_inputs_with_the_answers_of_eager() {
    let s = named(150_000);
    let filtered = "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"Ada\")) }";
    let expected = bag(query(s.snapshot(), filtered, &Default::default())
        .unwrap()
        .application_rows());
    assert_eq!(expected.len(), 150_000 / 7 + 1);
    let ordered = [
        "SELECT ?s ?a WHERE { ?s <urn:age> ?a } ORDER BY DESC(ABS(?a - 50)) ?s LIMIT 10",
        "SELECT ?s ?a WHERE { ?s <urn:age> ?a } ORDER BY ?a DESC(?s) LIMIT 7",
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n } ORDER BY DESC(?n) LIMIT 5",
    ];
    for opts in [QueryOptions::default(), without("grow_input")] {
        let mut c = select_cursor(s.snapshot(), filtered, &opts, &options(4096)).unwrap();
        let mut rows = Vec::new();
        while let Some(batch) = c.next_batch().unwrap() {
            assert!(
                batch.len() <= 4096,
                "the filter keeps the caller's batch size"
            );
            for row in 0..batch.len() {
                rows.push(batch.row(row).unwrap().values().to_vec());
            }
        }
        assert_eq!(bag(rows), expected);
        for q in ordered {
            let eager = query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows();
            let c = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
            assert_eq!(all(c), eager, "{q}");
        }
    }
}

/// Without `grow_input` a top-k reads its input under the caller's byte cap, as it did
/// before inputs could grow, however many rows the caller allows per batch. A budget that
/// batches of 4,096 rows fit, and one read of all 200,000 rows does not, shows it.
#[test]
fn top_k_keeps_the_callers_byte_cap_when_its_input_does_not_grow() {
    let s = store(200_000);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o } ORDER BY DESC(ABS(?o - 7)) LIMIT 5";
    let eager = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows();
    let cursor_options = CursorOptions {
        batch_rows: 1 << 20,
        batch_bytes: 64 << 10,
        ..options(1 << 20)
    };
    let budget = QueryOptions {
        max_memory_bytes: Some(4 << 20),
        ..without("grow_input")
    };
    let mut c = select_cursor(s.snapshot(), q, &budget, &cursor_options).unwrap();
    let mut rows = Vec::new();
    while let Some(batch) = c.next_batch().unwrap() {
        for row in 0..batch.len() {
            rows.push(batch.row(row).unwrap().values().to_vec());
        }
    }
    assert_eq!(rows, eager);
}

/// A top-k over input that improves row by row, where nearly every row enters the heap and
/// the first key holds each value twice, screens each value from the answers it gave
/// before, which a row entering the heap does not change.
#[test]
fn top_k_over_improving_input_with_repeated_values_matches_eager() {
    let s = Store::in_memory(StoreOptions::default());
    let n = 60_000;
    let data = (0..n)
        .map(|i| format!("<urn:s:{i:07}> <urn:v> \"v{:07}\" .\n", i / 2))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 10",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) ?s LIMIT 7",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 10",
    ] {
        let eager = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for opts in [QueryOptions::default(), without("grow_input")] {
            let c = select_cursor(
                s.snapshot(),
                q,
                &opts,
                &CursorOptions {
                    fallback: FallbackPolicy::AllowMaterialization,
                    ..CursorOptions::default()
                },
            )
            .unwrap();
            assert_eq!(all(c), eager, "{q}");
        }
    }
}

/// With `grow_input` a filter over a long scan and a top-k read larger inputs as the scan
/// goes on, which holds more rows at once than reads of the first size.
#[test]
fn growing_inputs_hold_larger_reads() {
    let s = named(300_000);
    for q in [
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"Ada\")) }",
        "SELECT ?s ?a WHERE { ?s <urn:age> ?a } ORDER BY DESC(ABS(?a - 50)) ?s LIMIT 10",
    ] {
        let peak = |opts: &QueryOptions| {
            let mut c = select_cursor(s.snapshot(), q, opts, &options(4096)).unwrap();
            while c.next_batch().unwrap().is_some() {}
            c.stats().mem_peak_bytes
        };
        let (grown, fixed) = (peak(&QueryOptions::default()), peak(&without("grow_input")));
        assert!(grown > fixed, "{q}: grown {grown}, fixed {fixed}");
    }
}

/// A writer that decodes ahead holds two batches and their terms. Under any memory budget
/// it writes what the writer without decoding ahead writes, and fails where that one
/// fails, and a writer that fails partway, while a batch is being decoded ahead, ends the
/// cursor with the same error.
#[test]
fn decoding_ahead_under_tight_budgets_and_failing_writers() {
    use sparkles_core::sparql::results::{SolutionsFormat, write_cursor_solutions};
    struct Failing(usize);
    impl std::io::Write for Failing {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            if self.0 < b.len() {
                return Err(std::io::Error::other("the client went away"));
            }
            self.0 -= b.len();
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let s = named(20_000);
    let q = "SELECT ?s ?n ?a WHERE { ?s <urn:name> ?n ; <urn:age> ?a }";
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let write = |opts: &QueryOptions| {
        let mut c = select_cursor(s.snapshot(), q, opts, &options(1000)).unwrap();
        let mut out = Vec::new();
        pool.install(|| write_cursor_solutions(&mut c, SolutionsFormat::Tsv, &mut out, None))
            .map(|_| out)
    };
    // From 550 KiB to 900 KiB the writer that decoded ahead from its first batch failed
    // where the other wrote every row.
    for kb in (100u64..=1500).step_by(50) {
        let budget = |opts: QueryOptions| QueryOptions {
            max_memory_bytes: Some(kb << 10),
            ..opts
        };
        let serial = write(&budget(without_decode_ahead()));
        let ahead = write(&budget(QueryOptions::default()));
        match (serial, ahead) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{kb} KiB"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!(
                "{kb} KiB: in turn {:?}, ahead {:?}",
                a.map(|o| o.len()),
                b.map(|o| o.len())
            ),
        }
    }
    for after in [0, 10, 40_000, 200_000] {
        for opts in [QueryOptions::default(), without_decode_ahead()] {
            let mut c = select_cursor(s.snapshot(), q, &opts, &options(1000)).unwrap();
            let r = pool.install(|| {
                write_cursor_solutions(&mut c, SolutionsFormat::Tsv, Failing(after), None)
            });
            assert!(r.is_err(), "the writer fails after {after} bytes");
            assert_ne!(c.status(), CursorStatus::Complete);
        }
    }
}

/// Three dateTimes that `ORDER BY` does not order totally: p is before q by value, and q
/// is before a and a before p by their lexical forms, since a has no timezone and lies
/// within 14 hours of both. The top-k's screen cannot remember a rejected value of such a
/// key, because a later heap can rank it again, so every batch size gives eager's answer.
#[test]
fn top_k_over_a_cycle_of_partially_ordered_keys_matches_eager_at_every_batch_size() {
    let s = Store::in_memory(StoreOptions::default());
    let dt = |v: &str| format!("\"{v}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>");
    let (p, q, a) = (
        dt("2020-01-02T01:00:00+05:00"),
        dt("2020-01-01T23:00:00Z"),
        dt("2020-01-01T23:30:00"),
    );
    let mut data = String::new();
    for (i, v) in [&p, &q, &a, &q].into_iter().enumerate() {
        data.push_str(&format!("<urn:s:{:05}> <urn:v> {v} .\n", i + 1));
    }
    for i in 5..3001 {
        data.push_str(&format!("<urn:s:{i:05}> <urn:v> {p} .\n"));
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for q in [
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 1",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v ?s LIMIT 2",
    ] {
        let eager = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for rows in [1, 512, 1024, 4096] {
            let c = select_cursor(
                s.snapshot(),
                q,
                &QueryOptions::default(),
                &CursorOptions {
                    batch_rows: rows,
                    fallback: FallbackPolicy::AllowMaterialization,
                    ..CursorOptions::default()
                },
            )
            .unwrap();
            assert_eq!(all(c), eager, "{q}; batch {rows}");
        }
    }
}

/// Later batches can be larger than the first, here because the second half of the names
/// is 200 bytes longer, and the room that decoding ahead measured from the first batch
/// can then run out where the writer in turn just fits. That happens only within a few
/// KiB of the writer's own need, and only as a memory budget error, never with other
/// output.
#[test]
fn decoding_ahead_with_growing_batches_under_tight_budgets() {
    use sparkles_core::sparql::results::{SolutionsFormat, write_cursor_solutions};
    let s = Store::in_memory(StoreOptions::default());
    let pad = "x".repeat(200);
    let data = (0..20_000)
        .map(|i| {
            let tail = if i >= 10_000 { pad.as_str() } else { "" };
            format!("<urn:s:{i:05}> <urn:name> \"name {i}{tail}\" .\n")
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let q = "SELECT ?s ?n WHERE { ?s <urn:name> ?n }";
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let write = |opts: &QueryOptions| {
        let mut c = select_cursor(s.snapshot(), q, opts, &options(4096)).unwrap();
        let mut out = Vec::new();
        pool.install(|| write_cursor_solutions(&mut c, SolutionsFormat::Tsv, &mut out, None))
            .map(|_| out)
    };
    let mut mismatches = Vec::new();
    for kb in (1000u64..=1150).step_by(3) {
        let budget = |opts: QueryOptions| QueryOptions {
            max_memory_bytes: Some(kb << 10),
            ..opts
        };
        let serial = write(&budget(without_decode_ahead()));
        let ahead = write(&budget(QueryOptions::default()));
        match (serial, ahead) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{kb} KiB"),
            (Err(_), Err(_)) => {}
            (Ok(_), Err(Error::BudgetExceeded(b))) if b.kind == BudgetKind::Memory => {
                mismatches.push(kb)
            }
            (Ok(_), Err(e)) => panic!("{kb} KiB: decoding ahead failed otherwise: {e}"),
            (Err(e), Ok(_)) => panic!("{kb} KiB: only the writer in turn failed: {e}"),
        }
    }
    // The window moves with thread timing by a few KiB. Four of these 51 budgets failed
    // at most in repeated runs.
    assert!(
        mismatches.len() <= 10,
        "decoding ahead failed at {mismatches:?} KiB"
    );
}

/// The rows the leaf scan of a cursor plan read.
fn leaf_rows(plan: &sparkles_core::sparql::CursorPlan) -> i64 {
    match plan.children.first() {
        Some(child) => leaf_rows(child),
        None => plan.operator.actual_rows,
    }
}

/// A filter over a scan on the variable it tests reads the scan in morsels of whole
/// blocks, in parallel, and answers what eager execution answers in the same order:
/// across block and morsel boundaries, under a small LIMIT and OFFSET, with a fixed start
/// that skips whole key ranges, and when the row budget hands the rest of the scan back
/// to the scan's own batches partway.
#[test]
fn morsel_filters_over_scans_match_eager_in_order() {
    let s = named(150_000);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(4)
        .build()
        .unwrap();
    let queries = [
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"Ada\")) }",
        "SELECT ?n ?s WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"9\")) }",
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(STRSTARTS(?n, \"Ada 1\")) }",
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(REGEX(STR(?s), \"s:1[0-9]{3}$\")) }",
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"Ada\")) } LIMIT 3",
        "SELECT ?s ?n WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"Ada\")) } OFFSET 40000 LIMIT 5000",
        "SELECT ?s WHERE { ?s <urn:name> ?n FILTER(CONTAINS(?n, \"nobody\")) }",
    ];
    for q in queries {
        let eager = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        for (opts, batch) in [
            (QueryOptions::default(), 4096),
            (QueryOptions::default(), 100_000),
            (without("filter_scan_runs"), 4096),
            (without("grow_input"), 5000),
            (
                QueryOptions {
                    max_rows: Some(160_000),
                    ..Default::default()
                },
                4096,
            ),
        ] {
            let c = select_cursor(s.snapshot(), q, &opts, &options(batch)).unwrap();
            let rows = pool.install(|| all(c));
            assert_eq!(rows, eager, "{q} at {batch} rows");
        }
    }
    // A fixed start reads only the key ranges that can pass, which the scan's own
    // batches do not skip.
    let q = queries[2];
    let read = |opts: &QueryOptions| {
        let mut c = select_cursor(s.snapshot(), q, opts, &options(4096)).unwrap();
        while c.next_batch().unwrap().is_some() {}
        leaf_rows(c.plan())
    };
    assert!(read(&QueryOptions::default()) < 150_000 / 7);
    assert_eq!(read(&without("filter_scan_runs")), 150_000);
    // A small first demand keeps the scan's own batches.
    let limited = format!("{} LIMIT 2", queries[0]);
    let mut c = select_cursor(s.snapshot(), &limited, &Default::default(), &options(4096)).unwrap();
    while c.next_batch().unwrap().is_some() {}
    assert!(leaf_rows(c.plan()) < 1000, "{}", leaf_rows(c.plan()));
    // Without a row budget every row is read in morsels. A row budget without room for
    // another morsel hands the rest of the scan back to its own batches.
    let morsels = |opts: &QueryOptions| {
        let mut c = select_cursor(s.snapshot(), queries[0], opts, &options(4096)).unwrap();
        while c.next_batch().unwrap().is_some() {}
        (leaf_counter(c.plan(), "morselRows"), leaf_rows(c.plan()))
    };
    assert_eq!(morsels(&QueryOptions::default()), (150_000, 150_000));
    let (read, leaf) = morsels(&QueryOptions {
        max_rows: Some(160_000),
        ..Default::default()
    });
    assert!(
        read > 0 && read < leaf && leaf == 150_000,
        "{read} of {leaf}"
    );
}

/// A counter of the leaf scan of a cursor plan, 0 when it is absent.
fn leaf_counter(plan: &sparkles_core::sparql::CursorPlan, name: &str) -> i64 {
    match plan.children.first() {
        Some(child) => leaf_counter(child, name),
        None => plan
            .operator
            .counters
            .as_ref()
            .and_then(|c| c.get(name))
            .and_then(|v| v.as_i64())
            .unwrap_or(0),
    }
}

fn plan_has(p: &sparkles_core::sparql::PlanInfo, op: &str) -> bool {
    p.operator == op || p.children.iter().any(|c| plan_has(c, op))
}

/// A store whose `<urn:v>` values are inline decimals, in value order in the index,
/// except every `every`-th one, written with a trailing zero, which keeps it in the
/// dictionary, where ids say nothing of value order. Values repeat, so ties break by the
/// subject.
fn decimal_store(n: usize, every: usize, extra: &str) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut data = (0..n)
        .map(|i| {
            let (whole, cents) = ((i * 7919) % 5000, i % 100);
            let value = if i % every == 0 {
                format!("{whole}.{}0", cents % 10)
            } else {
                format!("{whole}.{cents:02}")
            };
            format!(
                "<urn:s:{i}> <urn:v> \"{value}\"^^<http://www.w3.org/2001/XMLSchema#decimal> .\n"
            )
        })
        .collect::<String>();
    data.push_str(extra);
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// A cursor's ordered top-k reads the pieces of its range without value order a morsel
/// at a time into a heap of its best rows, and answers what eager execution answers, in
/// the same order, ties included, without an eager fallback. A value that is not totally
/// ordered in a later morsel still sends the query to the generic plan, as it does
/// eagerly, which streams too.
#[test]
fn ordered_top_k_streams_unordered_pieces_with_the_answers_of_eager() {
    let queries = [
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 25",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 7",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v FILTER(?v > 4000) } ORDER BY DESC(?v) LIMIT 30",
        "SELECT ?v WHERE { ?s <urn:v> ?v FILTER(?v < 100) } ORDER BY DESC(?v) LIMIT 3",
    ];
    let date = "<urn:t> <urn:v> \"2020-01-01\"^^<http://www.w3.org/2001/XMLSchema#date> .\n";
    for extra in ["", date] {
        let s = decimal_store(400_000, 5, extra);
        for q in queries {
            let eager = query(s.snapshot(), q, &Default::default()).unwrap();
            assert!(
                q.contains("FILTER") || plan_has(eager.plan(), "IndexTopK"),
                "{q}: {:#?}",
                eager.plan()
            );
            let expected = eager.application_rows();
            // no eager fallback, even when the generic plan answers
            let strict = options(4096);
            let mut c = select_cursor(s.snapshot(), q, &Default::default(), &strict).unwrap();
            let mut rows = Vec::new();
            while let Some(batch) = c.next_batch().unwrap() {
                rows.extend((0..batch.len()).map(|i| batch.row(i).unwrap().values().to_vec()));
            }
            assert_eq!(rows, expected, "{q} with {extra:?}");
            // The generic plan is built, and shown, only when it answers.
            if let Some(node) = cursor_node(c.plan(), "IndexTopK") {
                if extra.is_empty() {
                    assert!(node.children.is_empty(), "{q}");
                } else if !q.contains("FILTER") {
                    assert!(!node.children.is_empty(), "{q}");
                }
            } else {
                assert!(q.contains("FILTER"), "{q}: {:#?}", c.plan());
            }
        }
    }
}

/// The first node of a cursor plan with the operator `op`.
fn cursor_node<'a>(
    plan: &'a sparkles_core::sparql::CursorPlan,
    op: &str,
) -> Option<&'a sparkles_core::sparql::CursorPlan> {
    if plan.operator.operator == op {
        return Some(plan);
    }
    plan.children.iter().find_map(|c| cursor_node(c, op))
}

/// The cursor's ordered top-k holds a morsel per worker thread and the rows they keep,
/// however many rows its pieces without value order hold. On one worker the peak is a
/// single morsel's, the same for 4 and 12 morsels of a single block. On two workers it is
/// at most two morsels' peaks, for 4 and 12 morsels alike.
///
/// The two-worker bound comes from the one-worker peaks, which do not depend on
/// scheduling. A two-worker peak is not a fixed baseline, since it depends on how far the
/// work of two morsels overlaps in time, and an idle or busy worker that overlaps them
/// less gives a lower peak.
#[test]
fn ordered_top_k_memory_does_not_grow_with_its_unordered_pieces() {
    let pools = [1, 2].map(|threads| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
    });
    let q = "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 10";
    // the peaks of one and of two workers, for each store
    let mut peaks = Vec::new();
    for n in [300_000, 900_000] {
        let s = decimal_store(n, 3, "");
        let eager = query(s.snapshot(), q, &Default::default()).unwrap();
        assert!(plan_has(eager.plan(), "IndexTopK"), "{:#?}", eager.plan());
        let peak = |pool: &rayon::ThreadPool| {
            let mut c =
                select_cursor(s.snapshot(), q, &QueryOptions::default(), &options(4096)).unwrap();
            let rows = pool.install(|| {
                let mut rows = Vec::new();
                while let Some(b) = c.next_batch().unwrap() {
                    rows.extend((0..b.len()).map(|i| b.row(i).unwrap().values().to_vec()));
                }
                rows
            });
            assert_eq!(rows, eager.application_rows());
            c.stats().mem_peak_bytes
        };
        peaks.push(pools.each_ref().map(peak));
    }
    // one worker: the same morsel peak however many morsels there are
    assert!(peaks[1][0] < peaks[0][0] * 9 / 8, "{peaks:?}");
    // two workers: at most twice one worker's peak, with a margin for per-worker state
    let bound = peaks[0][0] * 2 * 9 / 8;
    assert!(
        peaks.iter().all(|p| p[1] < bound),
        "{peaks:?} against {bound}"
    );
}

/// A store whose `<urn:p>` values in `<urn:g1>` are 400,000 inline decimals below 5,
/// which keep the ordered top-k on the index, and whose 20,000 `<urn:d:j>` subjects hold
/// the dictionary decimal `5.0` in each of `graphs` graphs. `shift` singletons of the
/// same value come first, so the pairs of copies sit at either parity against the block
/// grid.
fn union_store(graphs: usize, shift: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let dec = "<http://www.w3.org/2001/XMLSchema#decimal>";
    let mut data = String::new();
    for i in 0..400_000 {
        let (a, bc) = (i % 5, (i / 5) % 100);
        data.push_str(&format!(
            "<urn:s:{i}> <urn:p> \"{a}.{bc:02}7\"^^{dec} <urn:g1> .\n"
        ));
    }
    for j in 0..shift {
        data.push_str(&format!("<urn:a:{j}> <urn:p> \"5.0\"^^{dec} <urn:g1> .\n"));
    }
    for j in 0..20_000 {
        for g in 1..=graphs {
            data.push_str(&format!(
                "<urn:d:{j:05}> <urn:p> \"5.0\"^^{dec} <urn:g{g}> .\n"
            ));
        }
    }
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::NQuads,
        None,
    )])
    .unwrap();
    s
}

/// A default graph of several named graphs holds each triple once, however the copies of
/// a triple fall across the morsels of a cursor's ordered top-k: the morsel after a pair
/// split by a block boundary drops its copy, as one read of the whole range does.
#[test]
fn ordered_top_k_dedups_union_graph_rows_across_morsels() {
    for (graphs, shift) in [(2, 0), (2, 1), (3, 0), (3, 1), (3, 2)] {
        let s = union_store(graphs, shift);
        let from = (1..=graphs)
            .map(|g| format!("FROM <urn:g{g}> "))
            .collect::<String>();
        for q in [
            format!("SELECT ?s ?v {from}WHERE {{ ?s <urn:p> ?v }} ORDER BY DESC(?v) LIMIT 45000"),
            format!("SELECT ?s ?v {from}WHERE {{ ?s <urn:p> ?v }} ORDER BY DESC(?v) LIMIT 21000"),
        ] {
            let eager = query(s.snapshot(), &q, &Default::default()).unwrap();
            assert!(
                plan_has(eager.plan(), "IndexTopK"),
                "{q}: {:#?}",
                eager.plan()
            );
            let expected = eager.application_rows();
            assert_eq!(
                expected
                    .iter()
                    .collect::<std::collections::HashSet<_>>()
                    .len(),
                expected.len(),
                "{q}: eager repeats a row"
            );
            for threads in [1, 4] {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let c =
                    select_cursor(s.snapshot(), &q, &Default::default(), &options(4096)).unwrap();
                let rows = pool.install(|| all(c));
                assert_eq!(
                    rows.len(),
                    expected.len(),
                    "{q}: {graphs} graphs, shift {shift}, {threads} threads"
                );
                assert_eq!(rows, expected, "{q}: {graphs} graphs, shift {shift}");
            }
        }
    }
}

/// A store of `n` subjects whose `<urn:o>` values mix every kind of term a key filter
/// decides: IRIs, plain, language-tagged and typed strings with shared prefixes, accented
/// and astral characters, inline integers and decimals, and `<urn:v>` decimals that are
/// mostly inline with every third kept in the dictionary. The values come from a fixed
/// seed, and with `delta` a pending update inserts and deletes some of them.
fn mixed_store(n: usize, seed: u64, delta: bool) -> Store {
    let mut state = seed;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    let term = |r: u64| match r % 9 {
        0 => format!("<urn:x:{}>", r % 5000),
        1 => format!("\"ab{}\"@en", r % 3000),
        2 => format!("\"abc{}\"@fr", r % 3000),
        3 => format!("\"ab{}\"", r % 4000),
        4 => format!("\"\u{e9}{}\"^^<urn:dt>", r % 2000),
        5 => format!("\"\u{10FFFF}ab{}\"", r % 1000),
        6 => format!(
            "\"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>",
            r % 10_000
        ),
        7 => format!(
            "\"{}.{}\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
            r % 1000,
            1 + r % 9
        ),
        _ => format!("\"abd{}\"@en-GB", r % 3000),
    };
    let decimal = |i: usize, r: u64| {
        let (whole, digit) = (r % 2000, 1 + r % 9);
        let value = if i.is_multiple_of(3) {
            format!("{whole}.{digit}0")
        } else {
            format!("{whole}.{digit}")
        };
        format!("\"{value}\"^^<http://www.w3.org/2001/XMLSchema#decimal>")
    };
    let mut triples = Vec::new();
    for i in 0..n {
        triples.push(format!("<urn:s:{i}> <urn:o> {}", term(next())));
        triples.push(format!("<urn:s:{i}> <urn:v> {}", decimal(i, next())));
    }
    let s = Store::in_memory(StoreOptions::default());
    let (base, added) = if delta {
        triples.split_at(triples.len() * 9 / 10)
    } else {
        (&triples[..], &[][..])
    };
    load_triples(&s, base);
    s.compact().unwrap();
    if delta {
        let inserts: String = added.iter().map(|t| format!("{t} . ")).collect();
        let deletes: String = base.iter().step_by(17).map(|t| format!("{t} . ")).collect();
        sparkles_core::sparql::update::update(
            &s,
            &format!("INSERT DATA {{ {inserts} }} ; DELETE DATA {{ {deletes} }}"),
            &Default::default(),
        )
        .unwrap();
        assert!(!s.snapshot().delta.is_empty());
    }
    s
}

/// Cursor filters over scans and cursor ordered top-k answer what eager execution
/// answers, in its order, over values of every kind, at 1, 4 and 8 threads, with and
/// without a pending delta. Without a delta the filters read their scans in morsels.
#[test]
fn morsel_filters_and_ordered_top_k_agree_with_eager_on_mixed_terms() {
    let filters = [
        "FILTER(STRSTARTS(?o, \"ab\"))",
        "FILTER(STRSTARTS(?o, \"ab\"@en))",
        "FILTER(STRSTARTS(STR(?o), \"ab\"))",
        "FILTER(STRSTARTS(?o, \"\u{e9}1\"))",
        "FILTER(STRSTARTS(?o, \"\u{10FFFF}\"))",
        "FILTER(STRSTARTS(?o, \"\"))",
        "FILTER(CONTAINS(?o, \"c1\"))",
        "FILTER(REGEX(?o, \"^abc?1[0-4]\"))",
        "FILTER(REGEX(STR(?o), \"7$\"))",
        "FILTER(LANGMATCHES(LANG(?o), \"en\"))",
        "FILTER(STRSTARTS(?o, \"ab\") && !CONTAINS(?o, \"9\"))",
    ];
    let mut queries: Vec<String> = filters
        .iter()
        .map(|f| format!("SELECT ?s ?o WHERE {{ ?s <urn:o> ?o {f} }}"))
        .collect();
    queries.push(format!(
        "SELECT ?o ?s WHERE {{ ?s <urn:o> ?o {} }} OFFSET 1000 LIMIT 6000",
        filters[0]
    ));
    queries.push(format!(
        "SELECT ?s ?o WHERE {{ ?s <urn:o> ?o {} }} LIMIT 5",
        filters[6]
    ));
    for top in [
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 40",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY ?v LIMIT 9",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v FILTER(?v < 500) } ORDER BY DESC(?v) LIMIT 15",
    ] {
        queries.push(top.into());
    }
    let pools: Vec<_> = [1, 4, 8]
        .into_iter()
        .map(|n| {
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .unwrap()
        })
        .collect();
    for (seed, delta) in [(0x5eed_0001, false), (0x5eed_0002, true)] {
        let s = mixed_store(120_000, seed, delta);
        for q in &queries {
            let eager = query(s.snapshot(), q, &Default::default())
                .unwrap()
                .application_rows();
            for pool in &pools {
                for batch in [4096, 50_000] {
                    let mut c =
                        select_cursor(s.snapshot(), q, &Default::default(), &options(batch))
                            .unwrap();
                    let rows = pool.install(|| {
                        let mut rows = Vec::new();
                        while let Some(b) = c.next_batch().unwrap() {
                            rows.extend((0..b.len()).map(|i| b.row(i).unwrap().values().to_vec()));
                        }
                        rows
                    });
                    let threads = pool.current_num_threads();
                    assert_eq!(
                        rows, eager,
                        "{q}: delta {delta}, {threads} threads, {batch}"
                    );
                    // the plain key tests read morsels, as a conjunction under
                    // negation does not
                    if !delta && (q == &queries[0] || q == &queries[6]) {
                        assert!(
                            leaf_counter(c.plan(), "morselRows") > 0,
                            "{q} does not read morsels"
                        );
                    }
                }
            }
        }
    }
}

/// A cancelled or timed-out cursor over the parallel filter and the ordered top-k stops
/// with an error or a complete answer, and dropping it returns every byte it charged.
#[test]
fn parallel_cursor_operators_release_their_memory_when_stopped() {
    use sparkles_core::sparql::pool::QueryPool;
    let s = mixed_store(120_000, 0x5eed_0003, false);
    let queries = [
        "SELECT ?s ?o WHERE { ?s <urn:o> ?o FILTER(CONTAINS(?o, \"b1\")) }",
        "SELECT ?s ?v WHERE { ?s <urn:v> ?v } ORDER BY DESC(?v) LIMIT 40",
    ];
    let pool = QueryPool::new(1 << 30);
    for q in queries {
        let eager = query(s.snapshot(), q, &Default::default())
            .unwrap()
            .application_rows();
        // cancelled after its first batch
        let grant = Arc::new(pool.try_grant(0).unwrap());
        let opts = QueryOptions {
            grant: Some(grant.clone()),
            no_cache: true,
            ..Default::default()
        };
        let mut c = select_cursor(s.snapshot(), q, &opts, &options(4096)).unwrap();
        let first = c.next_batch().unwrap();
        assert!(first.is_some(), "{q}");
        drop(first);
        c.cancellation_token().store(true, Ordering::Relaxed);
        match c.next_batch() {
            Err(Error::Cancelled) | Ok(None) => {}
            Err(e) => panic!("{q}: {e}"),
            Ok(Some(_)) => panic!("{q}: a batch after cancellation"),
        }
        drop((c, opts));
        assert_eq!(grant.used(), 0, "{q}: cancelled");
        // under a deadline of a millisecond
        for (timeout, base) in [(1, true), (5, true), (1, false), (5, false)] {
            let opts = QueryOptions {
                grant: Some(grant.clone()),
                timeout: Some(Duration::from_millis(timeout)),
                no_cache: true,
                ..if base {
                    Default::default()
                } else {
                    without("filter_scan_runs")
                }
            };
            let answer = select_cursor(s.snapshot(), q, &opts, &options(4096)).and_then(|mut c| {
                let mut rows = Vec::new();
                while let Some(b) = c.next_batch()? {
                    for i in 0..b.len() {
                        rows.push(b.row(i)?.values().to_vec());
                    }
                }
                Ok(rows)
            });
            match answer {
                Ok(rows) => assert_eq!(rows, eager, "{q}"),
                Err(Error::Timeout) => {}
                Err(error) => panic!("{q}: unexpected deadline outcome: {error}"),
            }
            assert_eq!(grant.used(), 0, "{q}: {timeout} ms, {base}");
        }
        // under small memory budgets, where a morsel the budget refuses goes back to the
        // scan's own batches
        for kib in [192, 512, 2048, 8192] {
            let opts = QueryOptions {
                grant: Some(grant.clone()),
                no_cache: true,
                max_memory_bytes: Some(kib << 10),
                ..Default::default()
            };
            let answer = select_cursor(s.snapshot(), q, &opts, &options(4096)).and_then(|mut c| {
                let mut rows = Vec::new();
                while let Some(b) = c.next_batch()? {
                    for i in 0..b.len() {
                        rows.push(b.row(i)?.values().to_vec());
                    }
                }
                Ok((rows, c.stats().mem_peak_bytes))
            });
            match answer {
                Ok((rows, peak)) => {
                    assert_eq!(rows, eager, "{q} in {kib} KiB");
                    assert!(peak <= kib << 10, "{q}: {peak} in {kib} KiB");
                }
                Err(Error::BudgetExceeded(b)) => assert_eq!(b.kind, BudgetKind::Memory),
                Err(e) => panic!("{q} in {kib} KiB: {e}"),
            }
            assert_eq!(grant.used(), 0, "{q}: {kib} KiB");
        }
        drop(grant);
        assert_eq!(pool.granted(), 0, "{q}");
    }
}

#[test]
fn temporal_bind_reuse_matches_an_independent_calendar_oracle() {
    use oxrdf::{Literal, vocab::xsd};
    // YEAR reads the local calendar component. Decimal division and FLOOR round
    // negative years toward minus infinity. BIND errors retain an unbound value.
    let cases = [
        ("1999-12-31", xsd::DATE, Some(1999)),
        ("2000-01-01", xsd::DATE, Some(2000)),
        ("2000-01-01+00:00", xsd::DATE, Some(2000)),
        ("2000-01-01-00:00", xsd::DATE, Some(2000)),
        ("2000-02-29", xsd::DATE, Some(2000)),
        ("1900-02-29", xsd::DATE, None),
        ("2001-02-29", xsd::DATE, None),
        ("1600-02-29", xsd::DATE, Some(1600)),
        ("0000-01-01", xsd::DATE, Some(0)),
        ("-0001-01-01", xsd::DATE, Some(-1)),
        ("-0010-12-31", xsd::DATE, Some(-10)),
        ("-0011-01-01", xsd::DATE, Some(-11)),
        ("1999-12-31T24:00:00Z", xsd::DATE_TIME, Some(2000)),
        ("1999-12-31T23:59:59.5-14:00", xsd::DATE_TIME, Some(1999)),
        ("2000-01-01+14:01", xsd::DATE, None),
        ("02000-01-01", xsd::DATE, None),
        ("not-a-date", xsd::DATE, None),
        ("2000-01-01", xsd::STRING, None),
        ("1999-12-31-00:00", xsd::DATE, Some(1999)),
    ];
    let store = Store::in_memory(StoreOptions::default());
    let repeats = 256;
    let mut data = String::new();
    let delta_case = cases.len() - 1;
    for (i, (lex, datatype, _)) in cases.iter().enumerate().take(delta_case) {
        for row in 0..repeats {
            data += &format!(
                "<urn:case:{i}:{row}> <urn:case> {i} ; <urn:date> {} .\n",
                Literal::new_typed_literal(*lex, *datatype)
            );
        }
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let old = store.snapshot();
    let text = "SELECT ?case ?date ?year ?decade WHERE { ?s <urn:case> ?case ; <urn:date> ?date BIND(YEAR(?date) AS ?year) BIND(FLOOR(YEAR(?date) / 10) * 10 AS ?decade) }";
    let validate = |rows: Vec<Vec<Option<Term>>>, extra: usize| {
        let mut counts = vec![0; cases.len()];
        for row in rows {
            let Some(Term::Literal(case)) = &row[0] else {
                panic!("case ID must stay bound")
            };
            let i = case.value().parse::<usize>().unwrap();
            counts[i] += 1;
            let (lex, datatype, year) = cases[i];
            assert_eq!(
                row[1],
                Some(Term::Literal(Literal::new_typed_literal(lex, datatype)))
            );
            match year {
                Some(year) => {
                    let Some(Term::Literal(actual)) = &row[2] else {
                        panic!("valid calendar year became unbound: {lex}")
                    };
                    assert_eq!(actual.datatype(), xsd::INTEGER);
                    assert_eq!(actual.value().parse::<i64>().unwrap(), year);
                    let Some(Term::Literal(actual)) = &row[3] else {
                        panic!("valid decade became unbound: {lex}")
                    };
                    assert_eq!(actual.datatype(), xsd::DECIMAL);
                    assert_eq!(
                        actual.value().parse::<f64>().unwrap(),
                        (year.div_euclid(10) * 10) as f64
                    );
                }
                None => assert!(row[2].is_none() && row[3].is_none(), "{lex}"),
            }
        }
        for (i, count) in counts.into_iter().enumerate() {
            assert_eq!(count, if i == delta_case { extra } else { repeats });
        }
    };
    validate(
        query(old.clone(), text, &Default::default())
            .unwrap()
            .application_rows(),
        0,
    );
    for cache in [false, true] {
        for batch in [1, 7, 4096] {
            let opts = QueryOptions {
                max_memory_bytes: Some(2 << 20),
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: cache,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let cursor = select_cursor(old.clone(), text, &opts, &options(batch)).unwrap();
            validate(all(cursor), 0);
        }
    }
    for cache in [false, true] {
        for (rows, produced) in [(Some(32), None), (None, Some(4096))] {
            let opts = QueryOptions {
                max_rows: rows,
                max_rows_produced: produced,
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: cache,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let mut cursor = select_cursor(old.clone(), text, &opts, &options(4096)).unwrap();
            loop {
                match cursor.next_batch() {
                    Ok(Some(_)) => {}
                    Err(Error::BudgetExceeded(b)) => {
                        assert_eq!(
                            b.kind,
                            if rows.is_some() {
                                BudgetKind::Rows
                            } else {
                                BudgetKind::RowsProduced
                            }
                        );
                        break;
                    }
                    Err(e) => panic!("unexpected finite-work error: {e}"),
                    Ok(None) => panic!("finite physical work must stay bounded"),
                }
            }
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let opts = QueryOptions {
        cancel: Some(cancel.clone()),
        ..Default::default()
    };
    let mut cursor = select_cursor(old.clone(), text, &opts, &options(4096)).unwrap();
    assert_eq!(cursor.next_batch().unwrap().unwrap().len(), 4096);
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(cursor.next_batch(), Err(Error::Cancelled)));
    let delta = (0..32)
        .map(|i| {
            format!(
                "<urn:delta:{i}> <urn:case> {delta_case} ; <urn:date> \"1999-12-31-00:00\"^^<http://www.w3.org/2001/XMLSchema#date> ."
            )
        })
        .collect::<String>();
    sparkles_core::sparql::update::update(
        &store,
        &format!("INSERT DATA {{ {delta} }}"),
        &Default::default(),
    )
    .unwrap();
    let changed = store.snapshot();
    let delta_id = changed
        .lookup_term(&Term::Literal(Literal::new_typed_literal(
            "1999-12-31-00:00",
            xsd::DATE,
        )))
        .unwrap();
    assert_eq!(delta_id.tag(), sparkles_core::id::Tag::Delta);
    for (snapshot, extra) in [(old, 0), (changed, 32)] {
        let cursor = select_cursor(snapshot, text, &Default::default(), &options(4096)).unwrap();
        validate(all(cursor), extra);
    }
}

#[test]
fn arq_uuid_aliases_and_dynamic_calls_stay_volatile_across_cursor_morsels() {
    let s = Store::in_memory(StoreOptions::default());
    let data = (0..5000)
        .map(|i| format!("<urn:uuid:{i}> <urn:key> \"repeated string\" .\n"))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    for fresh in [
        "afn:uuid()",
        "afn:struuid()",
        "fn:apply(afn:uuid)",
        "fn:apply(afn:struuid)",
        "afn:eval(afn:uuid)",
        "afn:eval(afn:struuid)",
    ] {
        let text = format!(
            "PREFIX afn: <http://jena.apache.org/ARQ/function#> PREFIX fn: <http://www.w3.org/2005/xpath-functions#> SELECT ?fresh WHERE {{ ?s <urn:key> ?key BIND(CONCAT(STR(?key), STR({fresh})) AS ?fresh) }}"
        );
        for cache in [true, false] {
            let opts = QueryOptions {
                optimizations: Some(sparkles_core::sparql::Optimizations {
                    expr_cache: cache,
                    ..Default::default()
                }),
                ..Default::default()
            };
            let cursor = select_cursor(s.snapshot(), &text, &opts, &options(4096)).unwrap();
            let rows = all(cursor);
            assert_eq!(rows.len(), 5000);
            let unique = rows
                .iter()
                .map(|r| r[0].clone())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                unique.len(),
                5000,
                "native cursor {fresh}, cache={cache} must generate one UUID per row"
            );
            assert!(!unique.contains(&None));
            let rows = query(s.snapshot(), &text, &opts)
                .unwrap()
                .application_rows();
            let unique = rows
                .iter()
                .map(|r| r[0].clone())
                .collect::<std::collections::HashSet<_>>();
            assert_eq!(
                unique.len(),
                5000,
                "eager {fresh}, cache={cache} must generate one UUID per row"
            );
            assert!(!unique.contains(&None));
        }
    }
}

/// The graph writers flush once after their first batch, before they pull the second, so
/// a writer that commits on a flush starts the response then. The probe cancels the query
/// at its first flush, so the next pull fails, and the output holds exactly the first
/// batch's statements, written and counted.
#[test]
fn graph_writers_flush_after_their_first_batch() {
    use sparkles_core::jena_formats::JenaFormat;
    use sparkles_core::sparql::graph_cursor;
    use sparkles_core::sparql::results::{
        self, Format, RowCount, SolutionsFormat, Source, WriteOptions,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Probe {
        out: Vec<u8>,
        flushes: usize,
        cancel: Arc<AtomicBool>,
    }
    impl std::io::Write for Probe {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.out.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            self.cancel.store(true, Ordering::Relaxed);
            Ok(())
        }
    }
    let s = store(1000);
    let q = "CONSTRUCT { ?s <urn:q> ?o } WHERE { ?s <urn:p> ?o }";
    let formats = [
        Format::Rdf(RdfFormat::NTriples),
        Format::Rdf(RdfFormat::NQuads),
        Format::Rdf(RdfFormat::Turtle),
        Format::Jena(JenaFormat::Thrift),
        Format::Jena(JenaFormat::Protobuf),
        Format::Jena(JenaFormat::TriX),
        Format::Solutions(SolutionsFormat::Sparkles),
    ];
    for format in formats {
        let mut c = graph_cursor(s.snapshot(), q, &Default::default(), &options(10)).unwrap();
        let mut probe = Probe {
            out: Vec::new(),
            flushes: 0,
            cancel: c.cancellation_token(),
        };
        let rows = RowCount::new();
        let options = WriteOptions::new(format, &rows);
        let r = results::write(Source::Graph(&mut c), options, &mut probe);
        assert!(
            matches!(r, Err(sparkles_core::Error::Cancelled)),
            "{format:?}: {r:?}"
        );
        assert_eq!(probe.flushes, 1, "{format:?}");
        assert_eq!(rows.get(), 10, "{format:?}");
        if matches!(format, Format::Rdf(RdfFormat::NTriples | RdfFormat::NQuads)) {
            let lines = probe.out.iter().filter(|&&b| b == b'\n').count();
            assert_eq!(lines, 10, "{format:?}");
        }
    }
}

/// The cursor's SPARQL Results Thrift writer flushes once after its first batch, before it
/// pulls the second, so a writer that commits on a flush starts the response then. The
/// probe cancels the query at its first flush, and the output holds exactly the first
/// batch's rows, written and counted.
#[test]
fn the_cursor_thrift_writer_flushes_after_its_first_batch() {
    use sparkles_core::jena_formats::ThriftResults;
    use sparkles_core::sparql::results::{self, Format, RowCount, Source, WriteOptions};
    struct Probe {
        out: Vec<u8>,
        flushes: usize,
        cancel: Arc<AtomicBool>,
    }
    impl std::io::Write for Probe {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.out.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            self.cancel.store(true, Ordering::Relaxed);
            Ok(())
        }
    }
    let s = store(1000);
    let q = "SELECT ?s ?o WHERE { ?s <urn:p> ?o }";
    let eager = query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows();
    let vars = ["s".to_string(), "o".to_string()];
    let mut expected = ThriftResults::new(Vec::new(), &vars).unwrap();
    for row in &eager[..10] {
        expected.row(row).unwrap();
    }
    let mut c = open(&s, q, 10);
    let mut probe = Probe {
        out: Vec::new(),
        flushes: 0,
        cancel: c.cancellation_token(),
    };
    let rows = RowCount::new();
    let r = results::write(
        Source::Select(&mut c),
        WriteOptions::new(Format::ResultsThrift, &rows),
        &mut probe,
    );
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert_eq!(probe.flushes, 1);
    assert_eq!(rows.get(), 10);
    assert_eq!(
        &probe.out,
        expected.get_mut(),
        "the first batch's rows, whole"
    );
}

/// An OPTIONAL over two inputs sorted on their shared variable merges when the
/// `merge_left_join` switch is on and hashes when it is off, in the cursor as in eager
/// execution, and both plans name the algorithm that ran. The first query fills the
/// result cache whatever its execution time, and the switched queries bypass the cache,
/// so that their plans name the join that ran.
#[test]
fn the_cursor_optional_honours_merge_left_join_and_names_its_algorithm() {
    let s = Store::in_memory(StoreOptions {
        result_cache_min_ms: 0.0,
        ..StoreOptions::default()
    });
    let data = (0..200)
        .map(|i| {
            let mut t = format!("<urn:s:{i}> <urn:p> {i} .\n");
            if i % 3 != 0 {
                t.push_str(&format!("<urn:s:{i}> <urn:q> \"{i}\" .\n"));
            }
            t
        })
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    // The second query's condition reads both sides, and the third's is an EXISTS.
    for q in [
        "SELECT ?s ?o ?x WHERE { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?x } }",
        "SELECT ?s ?o ?x WHERE { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?x FILTER(?o > 50 && ?x != \"77\") } }",
        "SELECT ?s ?o ?x WHERE { ?s <urn:p> ?o OPTIONAL { ?s <urn:q> ?x FILTER EXISTS { ?s <urn:p> ?v FILTER(?v < 120) } } }",
    ] {
        optional_merge_names_its_algorithm(&s, q);
    }
}

fn optional_merge_names_its_algorithm(s: &Store, q: &str) {
    let expected = bag(query(s.snapshot(), q, &Default::default())
        .unwrap()
        .application_rows());
    for merge in [true, false] {
        let opts = QueryOptions {
            optimizations: Some(sparkles_core::sparql::Optimizations {
                merge_left_join: merge,
                ..sparkles_core::sparql::Optimizations::ALL
            }),
            // A cached result would name no join, since none ran.
            no_cache: true,
            ..Default::default()
        };
        let eager = query(s.snapshot(), q, &opts).unwrap();
        let eager_plan = serde_json::to_string(eager.plan()).unwrap();
        let c = select_cursor(s.snapshot(), q, &opts, &options(7)).unwrap();
        let plan = c.plan_json().unwrap();
        for (mode, plan) in [("eager", &eager_plan), ("cursor", &plan)] {
            assert_eq!(
                plan.contains("[merge on ?s]"),
                merge,
                "{mode} plan with merge_left_join {merge}: {plan}"
            );
        }
        assert_eq!(plan.contains("[hash join]"), !merge, "{plan}");
        assert_eq!(bag(all(c)), expected, "{q}: merge_left_join {merge}");
        assert_eq!(bag(eager.application_rows()), expected, "{q}");
    }
}

/// A whole-input operator counts the rows it produces against the work limit as the
/// eager kernel does: both modes count the same rows, a limit one row short of that
/// count stops both, and the count itself passes both (item 829). The forms are a path
/// search over its group, a path search and an algorithm call over a nested edge
/// pattern, and an algorithm call over its group.
#[test]
fn whole_input_operators_count_their_rows_as_eager_does() {
    use std::sync::atomic::AtomicU64;
    let s = store(30);
    let data = (0..30)
        .map(|i| format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i * 7 + 1) % 30))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let prefixes = "PREFIX path: <urn:x-sparkles:path#> PREFIX algo: <urn:x-sparkles:algo#> ";
    fn reasons(plan: &sparkles_core::sparql::CursorPlan, out: &mut Vec<String>) {
        out.extend(plan.reason.clone());
        for c in &plan.children {
            reasons(c, out);
        }
    }
    for (q, reads) in [
        (
            "SELECT ?s ?b WHERE { ?s <urn:p> ?o FILTER(?o < 5) SERVICE path:search { [] path:source ?s ; path:target ?b ; path:predicate <urn:q> ; path:limit 7 } }",
            "all of its input",
        ),
        (
            "SELECT ?b ?i WHERE { SERVICE path:search { [] path:source <urn:s:1> ; path:target ?b ; path:limit 6 ; path:edgeIndex ?i ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
            "all of its edge pattern",
        ),
        (
            "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:start ?a ; algo:end ?b ; algo:node ?n ; algo:component ?c . ?a <urn:q> ?b } }",
            "all of its edge pattern",
        ),
        (
            "SELECT ?src ?n ?d WHERE { ?src <urn:p> ?o FILTER(?o < 3) SERVICE algo:bfs { [] algo:predicate <urn:q> ; algo:source ?src ; algo:node ?n ; algo:depth ?d } }",
            "all of its input",
        ),
    ] {
        let q = format!("{prefixes}{q}");
        let opts = |limit: Option<u64>, work: Option<Arc<AtomicU64>>| QueryOptions {
            no_cache: true,
            max_rows_produced: limit,
            work,
            ..Default::default()
        };
        let eager = query(s.snapshot(), &q, &opts(None, None)).unwrap();
        let produced = eager.rows_produced;
        assert!(!eager.is_empty(), "{q}");
        let expected = bag(eager.application_rows());
        let streamed = |limit: Option<u64>, rows: usize| -> Result<(u64, Vec<String>), Error> {
            let work = Arc::new(AtomicU64::new(0));
            let mut c = select_cursor(
                s.snapshot(),
                &q,
                &opts(limit, Some(work.clone())),
                &options(rows),
            )?;
            let mut found = Vec::new();
            reasons(c.plan(), &mut found);
            assert!(
                found
                    .iter()
                    .any(|r| r.contains(reads) && r.contains("runs once")),
                "{q}: {found:?}"
            );
            let mut answer = Vec::new();
            while let Some(batch) = c.next_batch()? {
                for row in 0..batch.len() {
                    answer.push(batch.row(row).unwrap().values().to_vec());
                }
            }
            Ok((work.load(Ordering::Relaxed), bag(answer)))
        };
        for rows in [1, 3, 4096] {
            let (work, answer) = streamed(None, rows).unwrap();
            assert_eq!(answer, expected, "{q}; {rows}");
            assert_eq!(work, produced, "{q}; {rows}");
            assert!(streamed(Some(produced), rows).is_ok(), "{q}; {rows}");
            assert!(
                matches!(streamed(Some(produced - 1), rows), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
                "{q}; {rows}"
            );
        }
        assert!(
            matches!(query(s.snapshot(), &q, &opts(Some(produced - 1), None)), Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::RowsProduced),
            "{q}"
        );
    }
}

/// A whole-input operator holds its input under the memory budget, so a budget that
/// cannot hold the input refuses the query with a budget error in both modes, while
/// small batches alone fit (item 829). The forms read their group or their edge pattern.
#[test]
fn whole_input_operators_refuse_an_input_over_the_memory_budget() {
    let n = 6000;
    let s = store(n);
    let data = (0..n)
        .map(|i| format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i * 7 + 1) % n))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let prefixes = "PREFIX path: <urn:x-sparkles:path#> PREFIX algo: <urn:x-sparkles:algo#> ";
    for q in [
        "SELECT ?s ?b WHERE { ?s <urn:p> ?o SERVICE path:search { [] path:source ?s ; path:target ?b ; path:predicate <urn:q> ; path:limit 3 } }",
        "SELECT ?b WHERE { SERVICE path:search { [] path:source <urn:s:1> ; path:target ?b ; path:limit 3 ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
        "SELECT ?n WHERE { SERVICE algo:wcc { [] algo:start ?a ; algo:end ?b ; algo:node ?n . ?a <urn:q> ?b } } LIMIT 1",
    ] {
        let q = format!("{prefixes}{q}");
        let tight = QueryOptions {
            no_cache: true,
            max_memory_bytes: Some(48 << 10),
            ..Default::default()
        };
        let memory = |r: Result<(), Error>| matches!(r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory);
        assert!(
            memory(query(s.snapshot(), &q, &tight).map(drop)),
            "{q}: eager"
        );
        let run = |opts: &QueryOptions| -> Result<(), Error> {
            let mut c = select_cursor(s.snapshot(), &q, opts, &options(8))?;
            while c.next_batch()?.is_some() {}
            Ok(())
        };
        run(&QueryOptions::default()).unwrap_or_else(|e| panic!("{q}: {e}"));
        let r = run(&tight);
        let refused = matches!(&r, Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory);
        assert!(refused, "{q}: {:?}", r.err().map(|e| e.to_string()));
    }
}

/// Peak memory and time of the whole-input and bounded-leaf forms against eager runs of
/// the same queries, for the item 829 measurement. Run it alone in release mode with
/// `--ignored --nocapture`; LEAF_MEASURE_N sets the graph size (default 200000).
#[test]
#[ignore]
fn measure_whole_input_and_leaf_forms() {
    use std::time::Instant;
    let n: usize = std::env::var("LEAF_MEASURE_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(200_000);
    let s = store(n);
    let data = (0..n)
        .map(|i| format!("<urn:s:{i}> <urn:q> <urn:s:{}> .\n", (i * 7 + 1) % n))
        .collect::<String>();
    s.load(&[Source::from_bytes(
        data.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    let p = "PREFIX path: <urn:x-sparkles:path#> PREFIX algo: <urn:x-sparkles:algo#> ";
    let forms = [
        (
            "path search over its group",
            "SELECT ?s ?b WHERE { ?s <urn:p> ?o SERVICE path:search { [] path:source ?s ; path:target ?b ; path:predicate <urn:q> ; path:limit 10 } }",
        ),
        (
            "path search over an edge pattern",
            "SELECT ?b WHERE { SERVICE path:search { [] path:source <urn:s:1> ; path:target ?b ; path:limit 10 ; path:start ?x ; path:end ?y . ?x <urn:q> ?y } }",
        ),
        (
            "path search leaf",
            "SELECT ?b WHERE { SERVICE path:search { [] path:source <urn:s:1> ; path:target ?b ; path:predicate <urn:q> ; path:limit 10 } }",
        ),
        (
            "algorithm call over its group",
            "SELECT ?n ?d WHERE { ?src <urn:p> ?o FILTER(?o < 4) SERVICE algo:bfs { [] algo:predicate <urn:q> ; algo:source ?src ; algo:node ?n ; algo:depth ?d } }",
        ),
        (
            "algorithm call over an edge pattern",
            "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:start ?a ; algo:end ?b ; algo:node ?n ; algo:component ?c . ?a <urn:q> ?b } }",
        ),
        (
            "algorithm call leaf",
            "SELECT ?n ?c WHERE { SERVICE algo:wcc { [] algo:predicate <urn:q> ; algo:node ?n ; algo:component ?c } }",
        ),
        (
            "count join of two scans (key runs)",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:p> ?o . ?s <urn:q> ?t }",
        ),
        (
            "count join over a filter",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:p> ?o FILTER(?o >= 0) ?s <urn:q> ?t }",
        ),
        (
            "count join on two variables",
            "SELECT (COUNT(*) AS ?n) WHERE { ?s <urn:q> ?t . ?t <urn:q> ?s }",
        ),
    ];
    println!("| form | rows | eager peak | streamed peak | eager ms | streamed ms |");
    println!("|---|---:|---:|---:|---:|---:|");
    for (name, q) in forms {
        let q = format!("{p}{q}");
        let opts = QueryOptions {
            no_cache: true,
            ..Default::default()
        };
        let (mut eager_ms, mut streamed_ms) = (f64::MAX, f64::MAX);
        let (mut eager_peak, mut streamed_peak, mut rows) = (0, 0, 0);
        for _ in 0..5 {
            let t = Instant::now();
            let r = query(s.snapshot(), &q, &opts).unwrap();
            eager_ms = eager_ms.min(t.elapsed().as_secs_f64() * 1000.0);
            eager_peak = r.mem_peak_bytes;
            rows = r.len();
            let t = Instant::now();
            let mut c = select_cursor(s.snapshot(), &q, &opts, &options(4096)).unwrap();
            while c.next_batch().unwrap().is_some() {}
            streamed_ms = streamed_ms.min(t.elapsed().as_secs_f64() * 1000.0);
            streamed_peak = c.stats().mem_peak_bytes;
        }
        println!(
            "| {name} | {rows} | {:.1} MiB | {:.1} MiB | {eager_ms:.1} | {streamed_ms:.1} |",
            eager_peak as f64 / 1048576.0,
            streamed_peak as f64 / 1048576.0
        );
    }
}

/// The reference evaluator's answers to the cases of its first review, and the
/// comparison's verdicts on answers that the specification allows and forbids.
#[test]
fn reference_evaluator_review_cases() {
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad};
    use reference::check::{Outcome, check_select};
    let iri = |s: &str| NamedNode::new_unchecked(s);
    let quad =
        |s: oxrdf::NamedOrBlankNode, p: &str, o: Term, g: GraphName| Quad::new(s, iri(p), o, g);
    let a = || oxrdf::NamedOrBlankNode::from(iri("urn:a"));
    let shared = BlankNode::new_unchecked("rb0");
    let quads = vec![
        quad(a(), "urn:p", iri("urn:b").into(), GraphName::DefaultGraph),
        quad(a(), "urn:q", iri("urn:b").into(), GraphName::DefaultGraph),
        quad(
            shared.clone().into(),
            "urn:p",
            iri("urn:x").into(),
            iri("urn:g1").into(),
        ),
        quad(
            shared.clone().into(),
            "urn:q",
            iri("urn:y").into(),
            iri("urn:g2").into(),
        ),
    ];
    let r = reference::Reference::new(quads.clone()).with_spec_readings(true);
    let jena = reference::Reference::new(quads);
    let rows = |q: &str| reference_select(&r, q, "review").1;
    let one = |q: &str| rows(q).remove(0).remove(0);
    let int = |n: i32| Some(Term::Literal(Literal::from(n)));
    let string = |v: &str| Some(Term::Literal(Literal::new_simple_literal(v)));

    // 1: a negated property set gives a pair once, and under Jena's reading once for each
    // triple
    let nps = "SELECT ?o WHERE { <urn:a> !<urn:z> ?o }";
    assert_eq!(rows(nps).len(), 1);
    assert_eq!(reference_select(&jena, nps, "review").1.len(), 2);
    // 2: BNODE(label) is a new blank node for each solution that an aggregate reads
    assert_eq!(
        one("SELECT (COUNT(DISTINCT BNODE(\"x\")) AS ?n) WHERE { VALUES ?v { 1 2 } }"),
        int(2)
    );
    // 3: a made blank node is none of the dataset's
    assert_eq!(
        rows("SELECT ?s WHERE { GRAPH ?g { ?s <urn:p> <urn:x> } FILTER(sameTerm(?s, BNODE())) }")
            .len(),
        0
    );
    // 4: AVG of errors only is zero
    assert_eq!(
        one("SELECT (AVG(?x) AS ?a) WHERE { VALUES ?x { UNDEF } }"),
        int(0)
    );
    // 5: GROUP_CONCAT is CONCAT, which takes no IRI, and notes it
    let (_, g, notes) = reference_select(
        &r,
        "SELECT (GROUP_CONCAT(?x) AS ?g) WHERE { VALUES ?x { <urn:a> } }",
        "review",
    );
    assert_eq!(g, vec![vec![None]]);
    assert!(notes.group_concat_non_string);
    // 6: the effective boolean value of an ill-formed boolean is an error in SPARQL 1.2
    let bad = "SELECT (!\"bad\"^^<http://www.w3.org/2001/XMLSchema#boolean> AS ?x) WHERE {}";
    assert_eq!(one(bad), None);
    let r11 = reference::Reference::new(Vec::new()).with_sparql11_ebv(true);
    assert_eq!(
        reference_select(&r11, bad, "review").1[0][0],
        Some(Term::Literal(Literal::from(true)))
    );
    // 7: inf is not a double
    assert_eq!(
        one(
            "SELECT (isNumeric(\"inf\"^^<http://www.w3.org/2001/XMLSchema#double>) AS ?n) WHERE {}"
        ),
        Some(Term::Literal(Literal::from(false)))
    );
    // 8: a double casts to a string by the XPath rules
    let cast = |e: &str| {
        one(&format!(
            "SELECT (<http://www.w3.org/2001/XMLSchema#string>({e}) AS ?s) WHERE {{}}"
        ))
    };
    assert_eq!(cast("1e6"), string("1.0E6"));
    assert_eq!(cast("1.5e0"), string("1.5"));
    assert_eq!(cast("1.0e-7"), string("1.0E-7"));
    assert_eq!(cast("2.50"), string("2.5"));
    // 9: a back-reference is unsupported, not an error, and \b is no XPath escape, which
    // Jena reads as Java's word boundary
    assert!(matches!(
        r.run("SELECT (REGEX(\"aa\", \"(a)\\\\1\") AS ?r) WHERE {}", None),
        Err(reference::Error::Unsupported(_))
    ));
    let boundary = "SELECT (REGEX(\"a b\", \"a\\\\b\") AS ?r) WHERE {}";
    assert_eq!(one(boundary), None);
    assert!(matches!(
        jena.run(boundary, None),
        Err(reference::Error::Unsupported(_))
    ));
    assert_eq!(
        one("SELECT (REGEX(\"a\\rb\", \"a.b\") AS ?r) WHERE {}"),
        Some(Term::Literal(Literal::from(false)))
    );
    // 10: the default graph of two FROM clauses keeps their blank nodes apart, and Jena's
    // reading takes their union
    let from = "SELECT ?s FROM <urn:g1> FROM <urn:g2> WHERE { ?s <urn:p> ?x ; <urn:q> ?y }";
    assert!(rows(from).is_empty());
    assert_eq!(reference_select(&jena, from, "review").1.len(), 1);
    // 11: two triple terms are unordered
    use reference::value::spec_order;
    let triple = |s: &str| {
        Term::Triple(Box::new(oxrdf::Triple::new(
            iri(s),
            iri("urn:p"),
            iri("urn:o"),
        )))
    };
    assert_eq!(
        spec_order(Some(&triple("urn:a")), Some(&triple("urn:b"))),
        None
    );
    // 12: identical triple terms with NaN are the same value
    assert_eq!(
        one(
            "SELECT (<<( <urn:a> <urn:p> \"NaN\"^^<http://www.w3.org/2001/XMLSchema#double> )>> = <<( <urn:a> <urn:p> \"NaN\"^^<http://www.w3.org/2001/XMLSchema#double> )>> AS ?e) WHERE {}"
        ),
        Some(Term::Literal(Literal::from(true)))
    );

    let selected = |q: &str| r.run_selected(q, None).unwrap().unwrap();
    let vars = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // 13: a RAND column is open, and the rows still count, in a partial comparison
    let s13 = selected("SELECT ?v (RAND() AS ?r) WHERE { VALUES ?v { 1 2 } }");
    let rand = |v: i32, x: f64| vec![int(v), Some(Term::Literal(Literal::from(x)))];
    assert!(matches!(
        check_select(&s13, &vars(&["v", "r"]), &[rand(1, 0.25), rand(2, 0.5)]),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_select(&s13, &vars(&["v", "r"]), &[rand(1, 0.25)]).is_err());
    // 14: an open MIN keeps the COUNT beside it compared
    let s14 = selected("SELECT (MIN(?x) AS ?m) (COUNT(*) AS ?n) WHERE { VALUES ?x { 1 \"a\" } }");
    assert!(check_select(&s14, &vars(&["m", "n"]), &[]).is_err());
    assert!(matches!(
        check_select(&s14, &vars(&["m", "n"]), &[vec![string("a"), int(2)]]),
        Ok(Outcome::Partial(_))
    ));
    // 16: a slice must be one of an order that ORDER BY allows
    let s16 = selected(
        "SELECT ?y WHERE { VALUES (?x ?y) { (1 \"a\") (2 \"b\") (2 \"c\") (3 \"d\") } } ORDER BY ?x LIMIT 2",
    );
    let col = |v: &[&str]| v.iter().map(|x| vec![string(x)]).collect::<Vec<_>>();
    assert_eq!(
        check_select(&s16, &vars(&["y"]), &col(&["a", "c"])),
        Ok(Outcome::Compared)
    );
    assert!(check_select(&s16, &vars(&["y"]), &col(&["b", "d"])).is_err());
    assert!(check_select(&s16, &vars(&["y"]), &col(&["b", "a"])).is_err());
    let s16b = selected(
        "SELECT ?y WHERE { VALUES (?x ?y) { (1 \"a\") (2 \"b\") (2 \"c\") (3 \"d\") } } ORDER BY ?x OFFSET 1 LIMIT 1",
    );
    assert_eq!(
        check_select(&s16b, &vars(&["y"]), &col(&["c"])),
        Ok(Outcome::Compared)
    );
    assert!(check_select(&s16b, &vars(&["y"]), &col(&["a"])).is_err());
    // 17: an expression key orders the rows
    let s17 = selected("SELECT ?x WHERE { VALUES ?x { 1 2 } } ORDER BY DESC(STR(?x))");
    assert!(check_select(&s17, &vars(&["x"]), &[vec![int(1)], vec![int(2)]]).is_err());
    assert_eq!(
        check_select(&s17, &vars(&["x"]), &[vec![int(2)], vec![int(1)]]),
        Ok(Outcome::Compared)
    );
    // 18: a term of the data keeps its datatype and lexical form
    let s18 = selected(
        "SELECT ?x WHERE { VALUES ?x { \"01\"^^<http://www.w3.org/2001/XMLSchema#byte> 1 } }",
    );
    assert!(check_select(&s18, &vars(&["x"]), &[vec![int(1)], vec![int(1)]]).is_err());
    // a computed number keeps its datatype, and its lexical form is free
    let s18b = selected("SELECT ((?x + 0.0) AS ?y) WHERE { VALUES ?x { 1 } }");
    let dec = |l: &str| {
        Some(Term::Literal(Literal::new_typed_literal(
            l,
            iri("http://www.w3.org/2001/XMLSchema#decimal"),
        )))
    };
    assert_eq!(
        check_select(&s18b, &vars(&["y"]), &[vec![dec("1.0")]]),
        Ok(Outcome::Compared)
    );
    assert!(check_select(&s18b, &vars(&["y"]), &[vec![int(1)]]).is_err());
    // an open value in a FILTER leaves the rows open
    let open_rows = selected("SELECT ?v WHERE { VALUES ?v { 1 2 } FILTER(RAND() < 0.5) }");
    assert!(matches!(
        check_select(&open_rows, &vars(&["v"]), &[]),
        Ok(Outcome::Skipped(_))
    ));
}

/// The cases of the second review of the reference evaluator, each of which the
/// evaluator or the comparison got wrong before its fix.
#[test]
fn reference_evaluator_review_round_2_cases() {
    use oxrdf::{BlankNode, GraphName, Literal, NamedNode, Quad};
    use reference::check::{Expected, Outcome, check_select, under_engine_readings};
    let iri = |s: &str| NamedNode::new_unchecked(s);
    let int = |n: i64| Some(Term::Literal(Literal::from(n)));
    let string = |v: &str| Some(Term::Literal(Literal::new_simple_literal(v)));
    let boolean = |b: bool| Some(Term::Literal(Literal::from(b)));
    let vars = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let col = |v: Vec<Option<Term>>| v.into_iter().map(|x| vec![x]).collect::<Vec<_>>();
    let b = BlankNode::new_unchecked("b");
    let (a1, b2) = (
        BlankNode::new_unchecked("a1"),
        BlankNode::new_unchecked("b2"),
    );
    let quads = vec![
        Quad::new(
            b.clone(),
            iri("urn:p"),
            Literal::from(1),
            GraphName::DefaultGraph,
        ),
        Quad::new(
            b.clone(),
            iri("urn:p"),
            Literal::from(2),
            GraphName::DefaultGraph,
        ),
        Quad::new(a1.clone(), iri("urn:p"), Literal::from(1), iri("urn:g1")),
        Quad::new(b2.clone(), iri("urn:q"), Literal::from(2), iri("urn:g2")),
    ];
    let r = reference::Reference::new(quads.clone());
    let spec = reference::Reference::new(quads).with_spec_readings(true);
    let selected = |q: &str| r.run_selected(q, None).unwrap().unwrap();
    let regex = |q: &str| r.run(q, None);
    let one = |q: &str| match r.run(q, None) {
        Ok(reference::Answer::Select { mut rows, .. }) => rows.remove(0).remove(0),
        other => panic!("{other:?}: {q}"),
    };

    // 1: whether an open value is bound is open, in BOUND and in a join
    let s1 = selected("SELECT ?x WHERE { BIND(IF(RAND() < 0.5, 1, 1/0) AS ?x) FILTER(BOUND(?x)) }");
    assert!(s1.rows_open);
    let s1b = selected("SELECT ?x WHERE { BIND(IF(RAND() < 0.5, 1, 1/0) AS ?x) VALUES ?x { 2 } }");
    assert!(s1b.rows_open);
    // 2: MAX passes over the error under the specification's reading and is unbound under
    // Jena's, the engine's, which reaches STRSTARTS("Ada"@en--ltr, "A"@en); under SPARQL
    // 1.2 (item 1071) mismatched base directions are incompatible and raise a type error
    let q2 = "SELECT (IF(!BOUND(?m), STRSTARTS(\"Ada\"@en--ltr, \"A\"@en), false) AS ?x) \
              WHERE { { SELECT (MAX(?v) AS ?m) WHERE { VALUES ?v { UNDEF 1 } } } }";
    let Expected::Select(s2) = under_engine_readings(&r, q2).unwrap().0 else {
        panic!("a SELECT answer");
    };
    assert_eq!(s2.rows, vec![vec![None]]);
    // 3: an open second key keeps the order of the first, and an answer that keeps it
    // is not passed, since the open key decides the rest of the order
    let s3 = selected("SELECT ?x WHERE { VALUES ?x { 1 2 } } ORDER BY ?x RAND()");
    assert!(check_select(&s3, &vars(&["x"]), &col(vec![int(2), int(1)])).is_err());
    assert!(matches!(
        check_select(&s3, &vars(&["x"]), &col(vec![int(1), int(2)])),
        Ok(Outcome::OrderUnchecked(_))
    ));
    let s3b = selected("SELECT ?x WHERE { VALUES ?x { 1 2 } } ORDER BY ?x RAND() LIMIT 1");
    assert!(check_select(&s3b, &vars(&["x"]), &col(vec![int(2)])).is_err());
    assert!(matches!(
        check_select(&s3b, &vars(&["x"]), &col(vec![int(1)])),
        Ok(Outcome::Skipped(_))
    ));
    // 4: DISTINCT over copies with different keys leaves the order unchecked, not passed
    let s4 = selected(
        "SELECT DISTINCT ?y WHERE { VALUES (?x ?y) { (0 \"c\") (1 \"a\") (2 \"b\") (3 \"a\") (4 \"d\") } } ORDER BY ?x",
    );
    let got4 = col(["a", "c", "b", "d"].map(string).to_vec());
    assert!(matches!(
        check_select(&s4, &vars(&["y"]), &got4),
        Ok(Outcome::OrderUnchecked(_))
    ));
    // 5: the order of numbers is transitive across datatypes
    let xsd = |l: &str, d: &str| {
        Term::Literal(Literal::new_typed_literal(
            l,
            iri(&format!("http://www.w3.org/2001/XMLSchema#{d}")),
        ))
    };
    let terms = [
        xsd("16777216", "integer"),
        xsd("16777217", "decimal"),
        xsd("16777216", "float"),
        xsd("16777217", "integer"),
        xsd("0.1", "decimal"),
        xsd("0.1", "float"),
        xsd("0.1", "double"),
        xsd("1", "integer"),
        xsd("1.0", "decimal"),
        xsd("1E0", "double"),
        xsd("-0", "double"),
        xsd("0", "integer"),
        xsd("INF", "float"),
        xsd("-INF", "double"),
        xsd("NaN", "double"),
        xsd("170141183460469231731687303715884105727", "integer"),
        xsd("1.7014118346046923E38", "double"),
    ];
    use reference::value::{spec_order, total_order};
    use std::cmp::Ordering;
    for x in &terms {
        for y in &terms {
            let o = total_order(Some(x), Some(y));
            assert_eq!(o, total_order(Some(y), Some(x)).reverse(), "{x} {y}");
            if spec_order(Some(x), Some(y)) == Some(Ordering::Less) {
                assert_eq!(o, Ordering::Less, "{x} {y}");
            }
            for z in &terms {
                if o.is_le() && total_order(Some(y), Some(z)).is_le() {
                    assert!(total_order(Some(x), Some(z)).is_le(), "{x} {y} {z}");
                }
            }
        }
    }
    let s5 = selected(
        "SELECT ?x WHERE { VALUES ?x { 16777216 16777217.0 \"16777216\"^^<http://www.w3.org/2001/XMLSchema#float> } } ORDER BY ?x",
    );
    let mut got5: Vec<Vec<Option<Term>>> = s5.rows.clone();
    got5.reverse();
    assert!(check_select(&s5, &vars(&["x"]), &got5).is_err());
    assert_eq!(
        check_select(&s5, &vars(&["x"]), &s5.rows),
        Ok(Outcome::Compared)
    );
    // 6: one blank node is equal to itself, so the next key orders the rows
    let s6 = selected("SELECT ?x WHERE { ?b <urn:p> ?x } ORDER BY ?b ?x");
    assert!(check_select(&s6, &vars(&["x"]), &col(vec![int(2), int(1)])).is_err());
    // equal values of different terms tie under the text, so the next condition orders
    // them, and Jena orders them by the terms
    let ties = "SELECT ?s WHERE { VALUES (?v ?s) { (1.0 <urn:b>) (1 <urn:a>) } } ORDER BY ?v ?s";
    let ba = col(vec![Some(iri("urn:b").into()), Some(iri("urn:a").into())]);
    let spec_ties = spec.run_selected(ties, None).unwrap().unwrap();
    assert!(check_select(&spec_ties, &vars(&["s"]), &ba).is_err());
    assert_eq!(
        check_select(&selected(ties), &vars(&["s"]), &ba),
        Ok(Outcome::Compared)
    );
    // 7: COALESCE passes a term through, with its lexical form
    let s7 = selected(
        "SELECT (COALESCE(?x, 0) AS ?y) WHERE { VALUES ?x { \"01\"^^<http://www.w3.org/2001/XMLSchema#integer> } }",
    );
    assert!(!matches!(
        check_select(&s7, &vars(&["y"]), &col(vec![int(1)])),
        Ok(Outcome::Compared)
    ));
    let s7b = selected(
        "SELECT ?y WHERE { { VALUES ?y { \"01\"^^<http://www.w3.org/2001/XMLSchema#integer> } } UNION { BIND(1 + 1 AS ?y) } }",
    );
    assert!(!matches!(
        check_select(&s7b, &vars(&["y"]), &[vec![int(1)], vec![int(2)]]),
        Ok(Outcome::Compared)
    ));
    // 8: an open column passes no forbidden value as compared
    let s8 = selected("SELECT (SAMPLE(?v) AS ?s) (COUNT(*) AS ?n) WHERE { VALUES ?v { 1 2 } }");
    for wrong in [int(999), None] {
        assert!(!matches!(
            check_select(&s8, &vars(&["s", "n"]), &[vec![wrong, int(2)]]),
            Ok(Outcome::Compared)
        ));
    }
    // 9: aggregates over one solution read the same BNODE
    assert_eq!(
        one(
            "SELECT (sameTerm(?a, ?b) AS ?eq) WHERE { { SELECT (SAMPLE(BNODE(\"x\")) AS ?a) (MIN(BNODE(\"x\")) AS ?b) WHERE {} } }"
        ),
        boolean(true)
    );
    // 10: the RDF merge keeps its blank nodes apart from the named graphs
    let q10 = "SELECT (sameTerm(?s, ?t) AS ?eq) FROM <urn:g1> FROM <urn:g2> FROM NAMED <urn:g1> \
               WHERE { ?s <urn:p> 1 GRAPH <urn:g1> { ?t <urn:p> 1 } }";
    match spec.run(q10, None) {
        Ok(reference::Answer::Select { rows, .. }) => assert_eq!(rows, vec![vec![boolean(false)]]),
        other => panic!("{other:?}"),
    }
    assert_eq!(one(q10), boolean(true));
    // 11: the x flag removes whitespace before the pattern is read
    assert_eq!(
        one("SELECT (REGEX(\"aa\", \"a{ 2 }\", \"x\") AS ?r) WHERE {}"),
        boolean(true)
    );
    assert_eq!(
        one("SELECT (REGEX(\"a b\", \"a[ ]b\", \"x\") AS ?r) WHERE {}"),
        boolean(true)
    );
    // 12: the i flag with a category, and the m flag with an anchor, are unsupported
    for q in [
        "SELECT (REGEX(\"a\", \"\\\\p{Lu}\", \"i\") AS ?r) WHERE {}",
        "SELECT (REGEX(\"a\\n\", \"^$\", \"m\") AS ?r) WHERE {}",
        "SELECT (REGEX(\"\u{17f}\", \"s\", \"i\") AS ?r) WHERE {}",
        "SELECT (REGEX(\"A\", \"[^a]\", \"i\") AS ?r) WHERE {}",
    ] {
        assert!(
            matches!(regex(q), Err(reference::Error::Unsupported(_))),
            "{q}"
        );
    }
    assert_eq!(
        one("SELECT (REGEX(\"aBc\", \"b\", \"i\") AS ?r) WHERE {}"),
        boolean(true)
    );
    // the m flag with an anchor agrees with XPath on text that does not end in a newline
    assert_eq!(
        one("SELECT (REGEX(\"a\\nb\", \"^b$\", \"m\") AS ?r) WHERE {}"),
        boolean(true)
    );
    // 13: a quantifier after a quantifier is an error
    assert_eq!(one("SELECT (REGEX(\"a\", \"a**\") AS ?r) WHERE {}"), None);
    assert_eq!(
        one("SELECT (REGEX(\"a\", \"a{2}{3}\") AS ?r) WHERE {}"),
        None
    );
    assert_eq!(
        one("SELECT (REGEX(\"aa\", \"a+?\") AS ?r) WHERE {}"),
        boolean(true)
    );
}

/// The cases of the third review of the reference evaluator, each of which the
/// comparison passed as compared before its fix although the specification forbids the
/// engine's answer or allows another.
#[test]
fn reference_evaluator_review_round_3_cases() {
    use oxrdf::{BlankNode, Literal, NamedNode, Quad};
    use reference::check::{Outcome, check_select};
    let iri = |s: &str| NamedNode::new_unchecked(s);
    let int = |n: i64| Some(Term::Literal(Literal::from(n)));
    let xsd = |l: &str, d: &str| {
        Some(Term::Literal(Literal::new_typed_literal(
            l,
            iri(&format!("http://www.w3.org/2001/XMLSchema#{d}")),
        )))
    };
    let vars = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let col = |v: Vec<Option<Term>>| v.into_iter().map(|x| vec![x]).collect::<Vec<_>>();
    let r = reference::Reference::new([Quad::new(
        BlankNode::new_unchecked("b"),
        iri("urn:p"),
        Literal::from(1),
        iri("urn:g1"),
    )]);
    let selected = |q: &str| r.run_selected(q, None).unwrap().unwrap();
    let compared = |o: &Result<Outcome, String>| matches!(o, Ok(Outcome::Compared));
    let mut failed: Vec<String> = Vec::new();
    let mut check = |ok: bool, what: String| {
        if !ok {
            failed.push(what);
        }
    };

    // 1: a condition that reads an open value is dropped from the key, and the rows that
    // tie on the conditions before it may still be ordered by the ones after it
    let s1 =
        selected("SELECT ?x WHERE { { BIND(RAND() AS ?r) } VALUES ?x { 1 2 } } ORDER BY ?r ?x");
    let o1 = check_select(&s1, &vars(&["x"]), &col(vec![int(2), int(1)]));
    check(!compared(&o1), format!("1: {o1:?}"));
    let s1b = selected(
        "SELECT ?x WHERE { { BIND(RAND() AS ?r) } VALUES ?x { 1 2 } } ORDER BY ?r ?x LIMIT 1",
    );
    let o1b = check_select(&s1b, &vars(&["x"]), &col(vec![int(2)]));
    check(!compared(&o1b), format!("1 LIMIT 1: {o1b:?}"));

    // 2: the constraints between the rows that a slice leaves out still fix its rows
    let q2 = "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?x WHERE { VALUES ?x { \
              16777219 \"16777219.5\"^^xsd:double \"16777220\"^^xsd:float } } \
              ORDER BY ?x OFFSET 1 LIMIT 1";
    let s2 = selected(q2);
    let o2 = check_select(&s2, &vars(&["x"]), &col(vec![int(16777219)]));
    check(!compared(&o2), format!("2: {o2:?}"));
    let o2b = check_select(&s2, &vars(&["x"]), &col(vec![xsd("16777219.5", "double")]));
    check(compared(&o2b), format!("2 allowed: {o2b:?}"));
    // Open order still permits another spelling of the computed cell.
    let s2c = selected(
        "SELECT ?y WHERE { { VALUES ?y { \"01\"^^<http://www.w3.org/2001/XMLSchema#integer> } } \
         UNION { BIND(1 + 1 AS ?y) } } ORDER BY ?y RAND()",
    );
    let o2c = check_select(
        &s2c,
        &vars(&["y"]),
        &col(vec![xsd("01", "integer"), xsd("+2", "integer")]),
    );
    check(
        matches!(o2c, Ok(Outcome::OrderUnchecked(_))),
        format!("2 mixed: {o2c:?}"),
    );
    let o2d = check_select(&s2c, &vars(&["y"]), &col(vec![int(1), int(2)]));
    check(o2d.is_err(), format!("2 changed copied term: {o2d:?}"));

    // 3: STRDT keeps the lexical form of its argument
    let s3 = selected(
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(\"01\", xsd:integer) AS ?x) WHERE {}",
    );
    let o3 = check_select(&s3, &vars(&["x"]), &col(vec![int(1)]));
    check(o3.is_err(), format!("3: {o3:?}"));
    let o3b = check_select(&s3, &vars(&["x"]), &col(vec![xsd("01", "integer")]));
    check(compared(&o3b), format!("3 allowed: {o3b:?}"));

    // 4: GRAPH joins an open graph name of its inner pattern with the graph's name
    let s4 = selected(
        "SELECT ?g WHERE { GRAPH ?g { SELECT (IF(RAND() < 0.5, <urn:g1>, <urn:g2>) AS ?g) WHERE {} } }",
    );
    let o4 = check_select(&s4, &vars(&["g"]), &[]);
    check(!compared(&o4), format!("4: {o4:?}"));
    let o4b = check_select(&s4, &vars(&["g"]), &col(vec![Some(iri("urn:g1").into())]));
    check(o4b.is_ok(), format!("4 allowed: {o4b:?}"));
    assert!(failed.is_empty(), "{failed:#?}");
}

/// An expected cell can carry the spelling of a computed number through `STR`, `STRDT`,
/// `STRLANG` and `STRLANGDIR`. Another valid spelling gives a partial comparison, while
/// constant and copied arguments keep exact comparison.
#[test]
fn reference_evaluator_str_computed_numbers_1093() {
    use oxrdf::{BaseDirection, Literal, NamedNode};
    use reference::check::{Outcome, check_select};
    let iri = |s: &str| NamedNode::new_unchecked(s);
    let string = |v: &str| Some(Term::Literal(Literal::new_simple_literal(v)));
    let typed = |v: &str, dt: &str| Some(Term::Literal(Literal::new_typed_literal(v, iri(dt))));
    let xsd = |v: &str, name: &str| typed(v, &format!("http://www.w3.org/2001/XMLSchema#{name}"));
    let lang = |v: &str, l: &str| {
        Some(Term::Literal(
            Literal::new_language_tagged_literal(v, l).unwrap(),
        ))
    };
    let lang_dir = |v: &str, l: &str, d: BaseDirection| {
        Some(Term::Literal(
            Literal::new_directional_language_tagged_literal(v, l, d).unwrap(),
        ))
    };
    let vars = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let one = |t: Option<Term>| vec![vec![t]];
    let r = reference::Reference::new(Vec::new());
    let selected = |q: &str| r.run_selected(q, None).unwrap().unwrap();

    // 1: STR over computed numbers (inline, via variable, via aggregate, scientific, INF)
    for q in [
        "SELECT (STR(1.0 + 1.0) AS ?s) WHERE {}",
        "SELECT (STR(?x) AS ?s) WHERE { BIND(1.0 + 1.0 AS ?x) }",
        "SELECT (STR(SUM(?v)) AS ?s) WHERE { VALUES ?v { 1.0 1.0 } }",
        "SELECT (STR(IF(true, 1.0 + 1.0, \"01\")) AS ?s) WHERE {}",
        "SELECT (COALESCE(?u, STR(1.0 + 1.0)) AS ?s) WHERE { VALUES ?u { UNDEF } }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["s"]), &one(string("2"))),
            Ok(Outcome::Compared),
            "exact match: {q}"
        );
        for alt in ["2.0", "+2.00", "02"] {
            assert!(
                matches!(
                    check_select(&s, &vars(&["s"]), &one(string(alt))),
                    Ok(Outcome::Partial(_))
                ),
                "allowed lexical {alt}: {q}"
            );
        }
        for bad in [
            string("3"),
            string("2.1"),
            string("two"),
            string(" 2"),
            xsd("2.0", "decimal"),
            lang("2.0", "en"),
        ] {
            assert!(
                check_select(&s, &vars(&["s"]), &one(bad.clone())).is_err(),
                "must fail for {bad:?}: {q}"
            );
        }
    }

    // Scientific notation and special floats through STR
    let s_dbl = selected("SELECT (STR(1.0e0 + 1.0e0) AS ?s) WHERE {}");
    assert_eq!(
        check_select(&s_dbl, &vars(&["s"]), &s_dbl.rows),
        Ok(Outcome::Compared)
    );
    for alt in ["2.0E0", "2.0e+0", "2.0", "2E0"] {
        let res = check_select(&s_dbl, &vars(&["s"]), &one(string(alt)));
        assert!(
            matches!(res, Ok(Outcome::Compared | Outcome::Partial(_))),
            "double lexical {alt}: {res:?}"
        );
    }
    assert!(
        matches!(
            check_select(&s_dbl, &vars(&["s"]), &one(string("2.00E+0"))),
            Ok(Outcome::Partial(_))
        ),
        "distinct double lexical must be partial"
    );

    let s_inf = selected("SELECT (STR(1.0e0 / 0.0e0) AS ?s) WHERE {}");
    assert_eq!(
        check_select(&s_inf, &vars(&["s"]), &one(string("INF"))),
        Ok(Outcome::Compared)
    );
    assert!(matches!(
        check_select(&s_inf, &vars(&["s"]), &one(string("+INF"))),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_select(&s_inf, &vars(&["s"]), &one(string("inf"))).is_err());
    assert!(check_select(&s_inf, &vars(&["s"]), &one(string("-INF"))).is_err());

    // 16-digit computed integer past f64's 2^53 precision does not collapse via f64
    let s_big = selected("SELECT (STR(9007199254740992 + 1) AS ?s) WHERE {}");
    assert_eq!(
        check_select(&s_big, &vars(&["s"]), &one(string("9007199254740993"))),
        Ok(Outcome::Compared)
    );
    assert!(matches!(
        check_select(&s_big, &vars(&["s"]), &one(string("+9007199254740993"))),
        Ok(Outcome::Partial(_))
    ));
    assert!(
        check_select(&s_big, &vars(&["s"]), &one(string("9007199254740992"))).is_err(),
        "off-by-one 16-digit integer past 2^53 must fail"
    );

    // 2: STR over constants and purely copied variables stays exact
    for (q, exact, alt) in [
        (
            "SELECT (STR(\"01\"^^<http://www.w3.org/2001/XMLSchema#integer>) AS ?s) WHERE {}",
            "01",
            "1",
        ),
        ("SELECT (STR(1.0) AS ?s) WHERE {}", "1.0", "1"),
        (
            "SELECT (STR(?x) AS ?s) WHERE { VALUES ?x { \"01\"^^<http://www.w3.org/2001/XMLSchema#integer> } }",
            "01",
            "1",
        ),
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["s"]), &one(string(exact))),
            Ok(Outcome::Compared),
            "copied exact: {q}"
        );
        assert!(
            check_select(&s, &vars(&["s"]), &one(string(alt))).is_err(),
            "copied STR must reject {alt}: {q}"
        );
    }

    // 3: STRDT over computed numbers (xsd:decimal, xsd:integer, custom datatype, via variable)
    for q in [
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(STR(1.0 + 1.0), xsd:decimal) AS ?x) WHERE {}",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(?s, xsd:decimal) AS ?x) WHERE { BIND(STR(1.0 + 1.0) AS ?s) }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["x"]), &one(xsd("2", "decimal"))),
            Ok(Outcome::Compared),
            "STRDT exact: {q}"
        );
        assert!(
            matches!(
                check_select(&s, &vars(&["x"]), &one(xsd("2.0", "decimal"))),
                Ok(Outcome::Partial(_))
            ),
            "STRDT partial: {q}"
        );
        assert!(
            check_select(&s, &vars(&["x"]), &one(xsd("2", "integer"))).is_err(),
            "STRDT wrong datatype: {q}"
        );
        assert!(
            check_select(&s, &vars(&["x"]), &one(xsd("3.0", "decimal"))).is_err(),
            "STRDT wrong value: {q}"
        );
    }

    // STRDT with custom datatype and with xsd:integer (where "2.0"^^xsd:integer is ill-formed)
    let s_dt_custom = selected("SELECT (STRDT(STR(1.0 + 1.0), <urn:custom>) AS ?x) WHERE {}");
    assert_eq!(
        check_select(&s_dt_custom, &vars(&["x"]), &one(typed("2", "urn:custom"))),
        Ok(Outcome::Compared)
    );
    assert!(matches!(
        check_select(
            &s_dt_custom,
            &vars(&["x"]),
            &one(typed("2.0", "urn:custom"))
        ),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_select(&s_dt_custom, &vars(&["x"]), &one(typed("2.0", "urn:other"))).is_err());

    let s_dt_int = selected(
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(STR(1.0 + 1.0), xsd:integer) AS ?x) WHERE {}",
    );
    assert!(matches!(
        check_select(&s_dt_int, &vars(&["x"]), &one(xsd("2.0", "integer"))),
        Ok(Outcome::Partial(_))
    ));

    // STRDT over constant, copied variable, and STR of copied variable stays exact
    for q in [
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(\"01\", xsd:integer) AS ?x) WHERE {}",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(?s, xsd:integer) AS ?x) WHERE { VALUES ?s { \"01\" } }",
        "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT (STRDT(STR(?n), xsd:integer) AS ?x) WHERE { VALUES ?n { \"01\"^^xsd:integer } }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["x"]), &one(xsd("01", "integer"))),
            Ok(Outcome::Compared),
            "STRDT copied exact: {q}"
        );
        assert!(
            check_select(&s, &vars(&["x"]), &one(xsd("1", "integer"))).is_err(),
            "STRDT copied must reject different lexical: {q}"
        );
    }

    // 4: STRLANG over computed numbers vs constants and copied variables
    for q in [
        "SELECT (STRLANG(STR(1.0 + 1.0), \"en\") AS ?x) WHERE {}",
        "SELECT (STRLANG(?s, \"en\") AS ?x) WHERE { BIND(STR(1.0 + 1.0) AS ?s) }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["x"]), &one(lang("2", "en"))),
            Ok(Outcome::Compared),
            "STRLANG exact: {q}"
        );
        assert!(
            matches!(
                check_select(&s, &vars(&["x"]), &one(lang("2.0", "en"))),
                Ok(Outcome::Partial(_))
            ),
            "STRLANG partial: {q}"
        );
        assert!(check_select(&s, &vars(&["x"]), &one(lang("2.0", "fr"))).is_err());
        assert!(check_select(&s, &vars(&["x"]), &one(string("2.0"))).is_err());
        assert!(check_select(&s, &vars(&["x"]), &one(lang("3.0", "en"))).is_err());
    }
    for q in [
        "SELECT (STRLANG(\"01\", \"en\") AS ?x) WHERE {}",
        "SELECT (STRLANG(?s, \"en\") AS ?x) WHERE { VALUES ?s { \"01\" } }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(&s, &vars(&["x"]), &one(lang("01", "en"))),
            Ok(Outcome::Compared)
        );
        assert!(check_select(&s, &vars(&["x"]), &one(lang("1", "en"))).is_err());
    }

    // 5: STRLANGDIR over computed numbers vs constants and copied variables
    for q in [
        "SELECT (STRLANGDIR(STR(1.0 + 1.0), \"en\", \"ltr\") AS ?x) WHERE {}",
        "SELECT (STRLANGDIR(?s, \"en\", \"ltr\") AS ?x) WHERE { BIND(STR(1.0 + 1.0) AS ?s) }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("2", "en", BaseDirection::Ltr))
            ),
            Ok(Outcome::Compared),
            "STRLANGDIR exact: {q}"
        );
        assert!(
            matches!(
                check_select(
                    &s,
                    &vars(&["x"]),
                    &one(lang_dir("2.0", "en", BaseDirection::Ltr))
                ),
                Ok(Outcome::Partial(_))
            ),
            "STRLANGDIR partial: {q}"
        );
        assert!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("2.0", "en", BaseDirection::Rtl))
            )
            .is_err()
        );
        assert!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("2.0", "fr", BaseDirection::Ltr))
            )
            .is_err()
        );
        assert!(check_select(&s, &vars(&["x"]), &one(lang("2.0", "en"))).is_err());
        assert!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("3.0", "en", BaseDirection::Ltr))
            )
            .is_err()
        );
    }
    for q in [
        "SELECT (STRLANGDIR(\"01\", \"en\", \"ltr\") AS ?x) WHERE {}",
        "SELECT (STRLANGDIR(?s, \"en\", \"ltr\") AS ?x) WHERE { VALUES ?s { \"01\" } }",
    ] {
        let s = selected(q);
        assert_eq!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("01", "en", BaseDirection::Ltr))
            ),
            Ok(Outcome::Compared)
        );
        assert!(
            check_select(
                &s,
                &vars(&["x"]),
                &one(lang_dir("1", "en", BaseDirection::Ltr))
            )
            .is_err()
        );
    }

    // 6: Isolation — xsd:string cast canonical form (item 1077) and copied sibling columns
    // remain strictly exact, and weaker order outcomes stay as weak.
    let s_cast = selected("SELECT (<http://www.w3.org/2001/XMLSchema#string>(1e6) AS ?s) WHERE {}");
    assert_eq!(
        check_select(&s_cast, &vars(&["s"]), &one(string("1.0E6"))),
        Ok(Outcome::Compared)
    );
    assert!(
        check_select(&s_cast, &vars(&["s"]), &one(string("1000000"))).is_err(),
        "xsd:string(1e6) must not relax canonical form"
    );

    let s_two = selected(
        "SELECT (STR(\"01\"^^<http://www.w3.org/2001/XMLSchema#integer>) AS ?a) \
         (STR(1.0 + 1.0) AS ?b) WHERE {}",
    );
    assert!(matches!(
        check_select(
            &s_two,
            &vars(&["a", "b"]),
            &[vec![string("01"), string("2.0")]]
        ),
        Ok(Outcome::Partial(_))
    ));
    assert!(
        check_select(
            &s_two,
            &vars(&["a", "b"]),
            &[vec![string("1"), string("2.0")]]
        )
        .is_err(),
        "sibling copied column must stay exact"
    );

    let s_ord =
        selected("SELECT (STR(1.0 + 1.0) AS ?s) WHERE { VALUES ?x { 1 2 } } ORDER BY ?x RAND()");
    assert!(matches!(
        check_select(
            &s_ord,
            &vars(&["s"]),
            &[vec![string("2.0")], vec![string("2.0")]]
        ),
        Ok(Outcome::OrderUnchecked(_))
    ));
}

fn check_str_1093(query: &str, rows: &[&[&str]]) -> Result<reference::check::Outcome, String> {
    let oracle = reference::Reference::new(Vec::new());
    let query = format!("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {query}");
    let expected = oracle
        .run_selected(&query, None)
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .unwrap();
    let rows = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|text| Some(text.parse::<Term>().unwrap()))
                .collect()
        })
        .collect::<Vec<_>>();
    reference::check::check_select(&expected, &expected.variables, &rows)
}

#[test]
fn reference_str_1093_tracks_the_selected_cell() {
    use reference::check::Outcome;
    for expression in [
        r#"IF(?i = 0, "02", STR(1 + 1))"#,
        r#"IF(?i = 0, ?v, STR(1 + 1))"#,
        r#"COALESCE(?v, STR(1 + 1))"#,
    ] {
        let query = format!(
            "SELECT ?i ({expression} AS ?s) {{ VALUES (?i ?v) {{ (0 \"02\") (1 UNDEF) }} }}"
        );
        let fixed = r#""02""#;
        let computed = r#""+2""#;
        assert!(matches!(
            check_str_1093(&query, &[&["1", computed], &["0", fixed]]),
            Ok(Outcome::Partial(_))
        ));
        assert!(check_str_1093(&query, &[&["0", r#""2""#], &["1", computed]]).is_err());
    }

    // The first got row can match either branch, and the second can only match the computed one.
    let duplicates = r#"SELECT (IF(?i = 0, STR(1 + 1), "2") AS ?s) { VALUES ?i { 0 1 } }"#;
    assert!(matches!(
        check_str_1093(duplicates, &[&[r#""2""#], &[r#""+2""#]]),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_str_1093(duplicates, &[&[r#""+2""#], &[r#""+2""#]]).is_err());

    let distinct = r#"SELECT DISTINCT (IF(?i = 0, STR(1 + 1), "2") AS ?s)
        { VALUES ?i { 0 1 } }"#;
    assert_eq!(
        check_str_1093(distinct, &[&[r#""2""#]]),
        Ok(Outcome::Compared)
    );
    assert!(check_str_1093(distinct, &[&[r#""+2""#]]).is_err());
}

#[test]
fn reference_str_1093_keeps_typed_numeric_lexicals_well_formed() {
    use reference::check::Outcome;
    let direct = r#"SELECT ?i (IF(?i = 0, STR(1.0 + 1.0), 1 + 1) AS ?s)
        { VALUES ?i { 0 1 } }"#;
    assert!(matches!(
        check_str_1093(
            direct,
            &[
                &["0", r#""2.0""#],
                &["1", r#""+2"^^<http://www.w3.org/2001/XMLSchema#integer>"#]
            ]
        ),
        Ok(Outcome::Partial(_))
    ));
    assert!(
        check_str_1093(
            direct,
            &[
                &["0", r#""2.0""#],
                &["1", r#""2.0"^^<http://www.w3.org/2001/XMLSchema#integer>"#]
            ]
        )
        .is_err()
    );
}

#[test]
fn reference_str_1093_uses_the_source_numeric_type() {
    use reference::check::Outcome;
    for (expression, spelling, equivalent) in [
        ("STR(1 + 1)", r#""+02""#, true),
        ("STR(1 + 1)", r#""2.0""#, false),
        ("STR(1 + 1)", r#""2E0""#, false),
        ("STR(1.0 + 1.0)", r#""2E0""#, false),
        (
            "STR(123456789012345.1 + 0)",
            r#""123456789012345.1000000000000000001""#,
            false,
        ),
        (
            "STR(123456789012345.1 + 0)",
            r#""123456789012345.1000000000000000000""#,
            true,
        ),
        ("STR(0e0 / -1e0)", r#""-0.0E0""#, true),
        ("STR(0e0 / -1e0)", r#""0""#, false),
        (
            "STR(0e0 / -1e0)",
            r#""-1e-9999999999999999999999999999999999999999""#,
            true,
        ),
        (
            "STR(xsd:float(\"0.1\") + xsd:float(\"0.0\"))",
            r#""0.10000000009""#,
            true,
        ),
        ("STR(0.1e0 + 0e0)", r#""0.10000000000000001""#, true),
        ("STR(0e0 / 0e0)", r#""NaN""#, true),
        ("STR(0e0 / 0e0)", r#""+NaN""#, false),
        (
            "STR(1e0 / 0e0)",
            r#""1e9999999999999999999999999999999999999999""#,
            true,
        ),
        (
            "STR(1e0 / 0e0)",
            r#""-1e9999999999999999999999999999999999999999""#,
            false,
        ),
        ("STR(1e0 / 0e0)", r#""1e+""#, false),
        (
            "STRDT(STR(16777216 + 0), xsd:float)",
            r#""+16777216"^^<http://www.w3.org/2001/XMLSchema#float>"#,
            true,
        ),
        (
            "STRDT(STR(16777216 + 0), xsd:float)",
            r#""16777217"^^<http://www.w3.org/2001/XMLSchema#float>"#,
            false,
        ),
    ] {
        let query = format!("SELECT ({expression} AS ?s) {{}}");
        let result = check_str_1093(&query, &[&[spelling]]);
        if equivalent {
            assert!(
                matches!(result, Ok(Outcome::Compared | Outcome::Partial(_))),
                "{query} against {spelling}: {result:?}"
            );
        } else {
            assert!(result.is_err(), "{query} against {spelling}: {result:?}");
        }
    }
}

#[test]
fn reference_str_1093_keeps_string_operations_exact() {
    use reference::check::Outcome;
    for (query, changed) in [
        (r#"SELECT (STR(UCASE("01")) AS ?s) {}"#, r#""1""#),
        (r#"SELECT (STR(xsd:string(1e6)) AS ?s) {}"#, r#""1000000""#),
        (
            r#"SELECT (STR(?c) AS ?s) { BIND(xsd:string(1e6) AS ?c) }"#,
            r#""1000000""#,
        ),
        (
            r#"SELECT (STR(?c) AS ?s) { VALUES ?code { "007x" } BIND(SUBSTR(?code, 1, 3) AS ?c) }"#,
            r#""7""#,
        ),
    ] {
        assert!(check_str_1093(query, &[&[changed]]).is_err(), "{query}");
    }
    assert!(matches!(
        check_str_1093(
            "SELECT (STR(xsd:string(STR(1 + 0))) AS ?s) {}",
            &[&[r#""+01""#]]
        ),
        Ok(Outcome::Partial(_))
    ));
}

#[test]
fn reference_str_1093_respects_binding_scopes() {
    use reference::check::Outcome;
    for (query, changed) in [
        (
            r#"SELECT (STR(?n) AS ?s) { VALUES ?n { "01" }
            { SELECT ?keep { BIND(1 + 0 AS ?n) BIND(0 AS ?keep) } } }"#,
            r#""1""#,
        ),
        (
            r#"SELECT (STR(?n) AS ?s) { VALUES ?n { "01" }
            MINUS { BIND(1 + 0 AS ?n) } }"#,
            r#""1""#,
        ),
        (
            r#"SELECT (STR(?n) AS ?s) { VALUES ?n { "01" }
            { SELECT ?k { BIND(1 + 0 AS ?n) BIND(0 AS ?k) } GROUP BY ?k } }"#,
            r#""1""#,
        ),
    ] {
        assert!(check_str_1093(query, &[&[changed]]).is_err(), "{query}");
    }
    for query in [
        "SELECT (STR(?n) AS ?s) { { SELECT ?n { BIND(1 + 0 AS ?n) } } }",
        "SELECT (STR(MIN(?n)) AS ?s) { VALUES ?i { 1 2 } BIND(?i + 0 AS ?n) }",
    ] {
        assert!(
            matches!(
                check_str_1093(query, &[&[r#""+01""#]]),
                Ok(Outcome::Partial(_))
            ),
            "{query}"
        );
    }
}

#[test]
fn reference_str_1093_compares_order_using_observed_spellings() {
    use reference::check::Outcome;
    let query = "SELECT ?n (STR(?n + 0) AS ?s) { VALUES ?n { 2 10 } }";
    let two = &["2", r#""02""#][..];
    let ten = &["10", r#""10""#][..];
    let lexical = format!("{query} ORDER BY ?s");
    assert!(matches!(
        check_str_1093(&lexical, &[two, ten]),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_str_1093(&lexical, &[ten, two]).is_err());
    let sliced = format!("{lexical} LIMIT 1");
    assert!(matches!(
        check_str_1093(&sliced, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert!(check_str_1093(&sliced, &[&["3", r#""03""#]]).is_err());

    let hidden = format!("{query} ORDER BY STR(?n + 0)");
    assert!(matches!(
        check_str_1093(&hidden, &[two, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));
    let numeric = format!("{query} ORDER BY ?n ?s");
    assert!(matches!(
        check_str_1093(&numeric, &[two, ten]),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_str_1093(&numeric, &[ten, two]).is_err());
    assert!(check_str_1093(&format!("{numeric} LIMIT 1"), &[ten]).is_err());
    let unaffected = format!("{query} ORDER BY ?n LIMIT 1");
    assert!(matches!(
        check_str_1093(&unaffected, &[two]),
        Ok(Outcome::Partial(_))
    ));

    let raw = r#"SELECT ?n ?number { VALUES ?n { 2 10 } BIND(?n + 0 AS ?number) }
        ORDER BY STR(?number)"#;
    let raw_two = &["2", r#""02"^^<http://www.w3.org/2001/XMLSchema#integer>"#][..];
    let raw_ten = &["10", "10"][..];
    assert_eq!(
        check_str_1093(raw, &[raw_two, raw_ten]),
        Ok(Outcome::Compared)
    );
    assert!(check_str_1093(raw, &[raw_ten, raw_two]).is_err());

    let open = format!("{query} ORDER BY RAND() LIMIT 1");
    assert!(matches!(
        check_str_1093(&open, &[two]),
        Ok(Outcome::Skipped(_))
    ));
}

#[test]
fn reference_str_1093_rejects_inversions_with_ambiguous_matches() {
    use reference::check::Outcome;
    let query = r#"SELECT ?s {
        { VALUES ?s { "02" "10" } } UNION { BIND(STR(2 + 0) AS ?s) }
    } ORDER BY ?s"#;
    let two = &[r#""02""#][..];
    let ten = &[r#""10""#][..];
    assert!(matches!(
        check_str_1093(query, &[two, two, ten]),
        Ok(Outcome::Partial(_))
    ));
    assert!(check_str_1093(query, &[ten, two, two]).is_err());
}

#[test]
fn reference_str_1093_checks_slice_boundaries_with_ambiguous_matches() {
    use reference::check::Outcome;
    let two = &[r#""02""#][..];
    let first = &[r#""first""#][..];
    let priority = r#"SELECT ?s {
        { VALUES (?s ?priority) { ("first" 0) ("02" 1) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(1 AS ?priority) }
    } ORDER BY ?priority ?s"#;
    assert!(check_str_1093(&format!("{priority} LIMIT 1"), &[two]).is_err());
    assert!(matches!(
        check_str_1093(&format!("{priority} LIMIT 1"), &[first]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check_str_1093(
            &priority.replace("ORDER BY ?priority ?s", "ORDER BY ?priority LIMIT 1"),
            &[first]
        ),
        Ok(Outcome::Compared)
    );

    let lexical = r#"SELECT ?s {
        { VALUES ?s { "01" "02" } } UNION { BIND(STR(2 + 0) AS ?s) }
    }"#;
    assert!(check_str_1093(&format!("{lexical} ORDER BY ?s LIMIT 1"), &[two]).is_err());
    assert!(check_str_1093(&format!("{lexical} ORDER BY DESC(?s) OFFSET 2"), &[two]).is_err());

    let between = r#"SELECT ?s {
        { VALUES (?s ?k) { ("02" 0) ("middle" 1) ("last" 2) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(0 AS ?k) }
    } ORDER BY ?k ?s"#;
    let last = &[r#""last""#][..];
    assert!(check_str_1093(&format!("{between} LIMIT 2 OFFSET 1"), &[two, last]).is_err());

    let surplus = r#"SELECT ?s {
        { VALUES (?s ?k) { ("02" 0) ("02" 0) ("last" 1) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(0 AS ?k) }
    } ORDER BY ?k ?s LIMIT 2"#;
    assert!(check_str_1093(surplus, &[two, last]).is_err());

    let unresolved = r#"SELECT ?s {
        { VALUES ?s { "02" } } UNION { BIND(STR(2 + 0) AS ?s) }
    } ORDER BY ?s LIMIT 1"#;
    assert!(matches!(
        check_str_1093(unresolved, &[two]),
        Ok(Outcome::Skipped(_))
    ));
}

#[test]
fn reference_str_1093_tracks_failed_binds_in_order_keys() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
        BIND(IF(?n = 10, 1, IF(?s = "2", 1 / 0, 2)) AS ?key)
    }"#;
    let two = &["2", r#""002""#][..];
    let ten = &["10", r#""10""#][..];
    assert!(matches!(
        check_str_1093(&format!("{query} ORDER BY ?key"), &[ten, two]),
        Ok(Outcome::OrderUnchecked(_))
    ));
    assert!(check_str_1093(&format!("{query} ORDER BY ?n ?key"), &[ten, two]).is_err());

    let union = r#"SELECT ?n ?s {
        VALUES ?group { 0 }
        { VALUES (?n ?s ?key ?group) { (10 "10" 0 0) } }
        UNION
        { SELECT ?n ?s ?key ?group {
            BIND(2 AS ?n) BIND(STR(2 + 0) AS ?s) BIND(0 AS ?group)
            BIND(IF(?s = "2", 1 / 0, 1) AS ?key)
        } }
    } ORDER BY ?key ?n"#;
    assert!(matches!(
        check_str_1093(union, &[ten, two]),
        Ok(Outcome::OrderUnchecked(_))
    ));
}

#[test]
fn reference_str_1093_tracks_boundness_in_order_keys() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
        BIND(IF(?s = "2", 1, 1 / 0) AS ?key)
    }"#;
    let two = &["2", r#""002""#][..];
    let ten = &["10", r#""10""#][..];
    assert!(matches!(
        check_str_1093(&format!("{query} ORDER BY BOUND(?key) ?n"), &[two, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));
    assert!(check_str_1093(&format!("{query} ORDER BY BOUND(?s) ?n"), &[ten, two]).is_err());
    assert!(
        check_str_1093(
            &format!("{query} ORDER BY DESC(?n) BOUND(?key)"),
            &[two, ten]
        )
        .is_err()
    );
}

#[test]
fn reference_str_1093_tracks_aggregate_value_dependencies_in_order_keys() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ?s"#;
    let two = &["2", r#""002""#][..];
    let ten = &["10", r#""10""#][..];
    for aggregate in ["SUM(STRLEN(?s))", "AVG(STRLEN(?s))"] {
        let ordered = format!("{query} ORDER BY {aggregate}");
        assert!(
            matches!(
                check_str_1093(&ordered, &[ten, two]),
                Ok(Outcome::OrderUnchecked(_))
            ),
            "{ordered}"
        );
    }
    let ordered = format!("{query} ORDER BY GROUP_CONCAT(?s)");
    assert!(matches!(
        check_str_1093(&ordered, &[two, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));
}

#[test]
fn reference_str_1093_tracks_aggregate_presence_and_distinct_values() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES (?n ?copy) { (2 true) (2 false) (10 true) (10 false) }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ?s"#;
    let two = &["2", r#""002""#][..];
    let ten = &["10", r#""10""#][..];
    for aggregate in [
        r#"COUNT(IF(?copy || ?s != "2", 1, 1 / 0))"#,
        r#"COUNT(DISTINCT IF(?copy, "2", ?s))"#,
        "MAX(DISTINCT IF(?copy, IF(?n = 2, 1, 2), STRLEN(?s)))",
    ] {
        let ordered = format!("{query} ORDER BY {aggregate} DESC(?n)");
        assert!(
            matches!(
                check_str_1093(&ordered, &[ten, two]),
                Ok(Outcome::OrderUnchecked(_))
            ),
            "{ordered}"
        );
    }
    assert!(check_str_1093(&format!("{query} ORDER BY COUNT(?s) ?n"), &[ten, two]).is_err());
}

#[test]
fn reference_str_1093_keeps_distinct_sort_prefixes() {
    use reference::check::Outcome;
    let query = r#"SELECT DISTINCT ?n ?s {
        { VALUES (?n ?s) { (0 "first") (1 "2") } }
        UNION { BIND(1 AS ?n) BIND(STR(1 + 1) AS ?s) }
    } ORDER BY ?n ?s"#;
    let first = &["0", r#""first""#][..];
    let second = &["1", r#""2""#][..];
    assert!(check_str_1093(query, &[second, first]).is_err());
    assert!(check_str_1093(&format!("{query} LIMIT 1"), &[second]).is_err());
    assert_eq!(
        check_str_1093(query, &[first, second]),
        Ok(Outcome::Compared)
    );

    let differing_suffix = r#"SELECT DISTINCT ?n ?s {
        VALUES (?n ?s ?suffix) { (0 "first" 0) (1 "2" 0) (1 "2" 1) }
    } ORDER BY ?n ?suffix"#;
    assert!(check_str_1093(differing_suffix, &[second, first]).is_err());
    assert!(check_str_1093(&format!("{differing_suffix} LIMIT 1"), &[second]).is_err());
    assert!(matches!(
        check_str_1093(differing_suffix, &[first, second]),
        Ok(Outcome::OrderUnchecked(_))
    ));
}

#[test]
fn reference_str_1093_keeps_spelling_dependencies_of_numeric_keys() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
        BIND(STRLEN(?s) AS ?len)
        BIND(?len + 1 AS ?key)
    }"#;
    let two = &["2", r#""002""#][..];
    let ten = &["10", r#""10""#][..];
    for key in ["?len", "?key"] {
        assert!(matches!(
            check_str_1093(&format!("{query} ORDER BY {key}"), &[ten, two]),
            Ok(Outcome::OrderUnchecked(_))
        ));
        assert!(check_str_1093(&format!("{query} ORDER BY ?n {key}"), &[ten, two]).is_err());
    }
}

#[test]
fn reference_str_1093_keeps_order_after_spelling_invariant_metadata() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
    }"#;
    let two = &["2", r#""2""#][..];
    let ten = &["10", r#""10""#][..];
    for metadata in [
        "LANG(?s)",
        "DATATYPE(STR(?n + 0))",
        "LANG(UCASE(?s))",
        "DATATYPE(LCASE(UCASE(?s)))",
    ] {
        let ordered = format!("{query} ORDER BY {metadata} ?n");
        assert!(check_str_1093(&ordered, &[ten, two]).is_err());
        assert_eq!(check_str_1093(&ordered, &[two, ten]), Ok(Outcome::Compared));
    }
    let dependent = format!(r#"{query} ORDER BY LANG(STRLANG("x", IF(?s = "2", "en", "fr"))) ?n"#);
    let padded = &["2", r#""002""#][..];
    assert!(matches!(
        check_str_1093(&dependent, &[ten, padded]),
        Ok(Outcome::OrderUnchecked(_))
    ));
}

#[test]
fn reference_str_1093_r4_shared_candidate_capacity_rejects_forced_omissions() {
    use reference::check::Outcome;
    let first = &[r#""first""#][..];
    let two = &[r#""02""#][..];
    let last = &[r#""last""#][..];

    let leading = r#"SELECT ?s {
        { VALUES (?s ?k) { ("02" 0) ("last" 1) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(0 AS ?k) }
    } ORDER BY ?k ?s LIMIT 2"#;
    assert!(check_str_1093(leading, &[two, last]).is_err());
    assert!(matches!(
        check_str_1093(leading, &[two, two]),
        Ok(Outcome::Partial(_))
    ));

    let tail = r#"SELECT ?s {
        { VALUES (?s ?k) { ("first" 0) ("02" 1) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(1 AS ?k) }
    } ORDER BY ?k ?s OFFSET 1 LIMIT 2"#;
    assert!(check_str_1093(tail, &[first, two]).is_err());
    assert!(matches!(
        check_str_1093(tail, &[two, two]),
        Ok(Outcome::Partial(_))
    ));

    let between = r#"SELECT ?s {
        { VALUES (?s ?k) { ("first" 0) ("02" 1) ("last" 2) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(1 AS ?k) }
    } ORDER BY ?k ?s LIMIT 3"#;
    assert!(check_str_1093(between, &[first, two, last]).is_err());

    let spare_unresolved = r#"SELECT ?s {
        { VALUES (?s ?k) { ("02" 0) ("02" 0) ("last" 1) } }
        UNION { BIND(STR(2 + 0) AS ?s) BIND(0 AS ?k) }
    } ORDER BY ?k ?s LIMIT 2"#;
    assert!(matches!(
        check_str_1093(spare_unresolved, &[two, two]),
        Ok(Outcome::Skipped(_))
    ));
}

#[test]
fn reference_str_1093_r4_tracks_selection_aggregate_dependencies() {
    use reference::check::Outcome;
    let query = r#"SELECT ?n ?s {
        VALUES (?n ?copy) { (2 true) (2 false) (10 true) (10 false) }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ?s"#;
    let two_exact = &["2", r#""2""#][..];
    let two_padded3 = &["2", r#""002""#][..];
    let two_padded4 = &["2", r#""0002""#][..];
    let ten = &["10", r#""10""#][..];

    for aggregate in [
        "MAX(DISTINCT IF(?copy, IF(?n = 2, 2, 3), STRLEN(?s)))",
        "MAX(IF(?copy, IF(?n = 2, 2, 3), STRLEN(?s))) ?n",
    ] {
        let ordered = format!("{query} ORDER BY {aggregate}");
        assert!(
            matches!(
                check_str_1093(&ordered, &[ten, two_padded4]),
                Ok(Outcome::OrderUnchecked(_))
            ),
            "{ordered}"
        );
    }

    let min_overtaken = format!(r#"{query} ORDER BY MIN(IF(?copy, IF(?n = 2, "05", "03"), ?s))"#);
    assert!(matches!(
        check_str_1093(&min_overtaken, &[two_padded3, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));

    let min_metadata =
        format!("{query} ORDER BY DATATYPE(MIN(DISTINCT IF(?copy, 1.5, STRLEN(?s)))) ?n");
    assert!(matches!(
        check_str_1093(&min_metadata, &[two_padded3, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));

    let sample = format!(r#"{query} ORDER BY SAMPLE(IF(?copy, IF(?n = 2, "2", "10"), ?s))"#);
    assert!(matches!(
        check_str_1093(&sample, &[two_padded3, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));

    let fixed_metadata = format!("{query} ORDER BY DATATYPE(MIN(IF(?copy, 2, STRLEN(?s)))) ?n");
    assert!(check_str_1093(&fixed_metadata, &[ten, two_exact]).is_err());
    assert_eq!(
        check_str_1093(&fixed_metadata, &[two_exact, ten]),
        Ok(Outcome::Compared)
    );

    let leading_key =
        format!("{query} ORDER BY ?n MAX(DISTINCT IF(?copy, IF(?n = 2, 2, 3), STRLEN(?s)))");
    assert!(check_str_1093(&leading_key, &[ten, two_padded4]).is_err());
}

#[test]
fn reference_str_1093_r4_tracks_count_distinct_star_dependencies() {
    use reference::check::Outcome;
    let two = &["2"][..];
    let ten = &["10"][..];

    let lexical = r#"SELECT ?n {
        { VALUES (?n ?s) { (2 "02") (10 "10") } }
        UNION { VALUES ?n { 2 10 } BIND(STR(?n + 0) AS ?s) }
    } GROUP BY ?n"#;
    assert!(matches!(
        check_str_1093(
            &format!("{lexical} ORDER BY COUNT(DISTINCT *)"),
            &[two, ten]
        ),
        Ok(Outcome::OrderUnchecked(_))
    ));
    assert!(
        check_str_1093(
            &format!("{lexical} ORDER BY DATATYPE(COUNT(DISTINCT *)) ?n"),
            &[ten, two]
        )
        .is_err()
    );
    assert_eq!(
        check_str_1093(
            &format!("{lexical} ORDER BY DATATYPE(COUNT(DISTINCT *)) ?n"),
            &[two, ten]
        ),
        Ok(Outcome::Compared)
    );

    let unbound_bind = r#"SELECT ?n {
        { VALUES (?n ?k) { (2 1) (10 1) (10 2) } }
        UNION {
            SELECT ?n ?k {
                BIND(2 AS ?n)
                BIND(STR(2 + 0) AS ?s)
                BIND(IF(?s = "2", 1 / 0, 1) AS ?k)
            }
        }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) DESC(?n)"#;
    assert!(matches!(
        check_str_1093(unbound_bind, &[two, ten]),
        Ok(Outcome::OrderUnchecked(_))
    ));

    let plain_count = r#"SELECT ?n {
        { VALUES (?n ?s) { (2 "02") (10 "10") (10 "20") } }
        UNION { VALUES ?n { 2 10 } BIND(STR(?n + 0) AS ?s) }
    } GROUP BY ?n ORDER BY COUNT(*)"#;
    assert!(check_str_1093(plain_count, &[ten, two]).is_err());
    assert_eq!(
        check_str_1093(plain_count, &[two, ten]),
        Ok(Outcome::Compared)
    );

    let singleton = r#"SELECT ?n {
        VALUES ?n { 2 10 }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) ?n"#;
    assert!(check_str_1093(singleton, &[ten, two]).is_err());
    assert_eq!(
        check_str_1093(singleton, &[two, ten]),
        Ok(Outcome::Compared)
    );

    let projected_out = r#"SELECT ?n {
        SELECT ?n ?tag {
            VALUES (?n ?tag) { (2 "a") (2 "b") (10 "a") (10 "a") }
            BIND(STR(?n + 0) AS ?s)
        }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *)"#;
    assert!(check_str_1093(projected_out, &[two, ten]).is_err());
    assert_eq!(
        check_str_1093(projected_out, &[ten, two]),
        Ok(Outcome::Compared)
    );
}

#[test]
fn reference_str_1093_r5_keeps_count_distinct_star_fixed_with_discriminator() {
    use reference::check::Outcome;
    let two = &["2"][..];
    let ten = &["10"][..];

    let fixed_tag = r#"SELECT ?n WHERE {
        VALUES (?n ?tag) { (2 "a") (10 "a") (10 "b") }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(check_str_1093(fixed_tag, &[ten]).is_err());
    assert_eq!(check_str_1093(fixed_tag, &[two]), Ok(Outcome::Compared));

    let fixed_presence = r#"SELECT ?n WHERE {
        VALUES (?n ?tag) { (2 "a") (10 "a") (10 UNDEF) }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(check_str_1093(fixed_presence, &[ten]).is_err());
    assert_eq!(
        check_str_1093(fixed_presence, &[two]),
        Ok(Outcome::Compared)
    );

    let fixed_metadata = r#"SELECT ?n WHERE {
        { VALUES ?n { 2 10 } BIND(STR(?n + 0) AS ?s) }
        UNION { VALUES ?n { 10 } BIND(STRLANG(STR(?n + 0), "en") AS ?s) }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(check_str_1093(fixed_metadata, &[ten]).is_err());
    assert_eq!(
        check_str_1093(fixed_metadata, &[two]),
        Ok(Outcome::Compared)
    );

    let unbound_with_tag = r#"SELECT ?n WHERE {
        { VALUES (?n ?tag ?k) { (2 "a" 1) (10 "a" 1) } }
        UNION {
            SELECT ?n ?tag ?k {
                BIND(10 AS ?n)
                BIND("b" AS ?tag)
                BIND(STR(10 + 0) AS ?s)
                BIND(IF(?s = "10", 1 / 0, 1) AS ?k)
            }
        }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(check_str_1093(unbound_with_tag, &[ten]).is_err());
    assert_eq!(
        check_str_1093(unbound_with_tag, &[two]),
        Ok(Outcome::Compared)
    );

    let colliding_lexical = r#"SELECT ?n WHERE {
        { VALUES (?n ?tag ?s) { (2 "a" "2") (10 "a" "010") } }
        UNION { VALUES (?n ?tag) { (10 "a") } BIND(STR(?n + 0) AS ?s) }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(matches!(
        check_str_1093(colliding_lexical, &[ten]),
        Ok(Outcome::Skipped(_))
    ));

    let colliding_unbound = r#"SELECT ?n WHERE {
        { VALUES (?n ?k) { (2 1) (10 1) } }
        UNION {
            SELECT ?n ?k {
                BIND(10 AS ?n)
                BIND(STR(10 + 0) AS ?s)
                BIND(IF(?s = "10", 1 / 0, 1) AS ?k)
            }
        }
    } GROUP BY ?n ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    assert!(matches!(
        check_str_1093(colliding_unbound, &[ten]),
        Ok(Outcome::Skipped(_))
    ));
}

#[test]
fn reference_str_1093_r6_bounds_count_distinct_star_pair_scan() {
    use oxrdf::{GraphName, NamedNode, Quad};
    use reference::check::Outcome;

    let check =
        |oracle: &reference::Reference, query: &str, rows: &[&[&str]]| -> Result<Outcome, String> {
            let query = format!("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {query}");
            let expected = oracle
                .run_selected(&query, None)
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .unwrap();
            let rows = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|text| Some(text.parse::<Term>().unwrap()))
                        .collect()
                })
                .collect::<Vec<_>>();
            reference::check::check_select(&expected, &expected.variables, &rows)
        };

    let member_pred = NamedNode::new_unchecked("urn:member");
    let group_obj = NamedNode::new_unchecked("urn:group");
    let make_quads = |count: usize| -> Vec<Quad> {
        (0..count)
            .map(|i| {
                Quad::new(
                    NamedNode::new_unchecked(format!("urn:item:{i}")),
                    member_pred.clone(),
                    group_obj.clone(),
                    GraphName::DefaultGraph,
                )
            })
            .collect()
    };

    let rotating_grouped_slice = r#"SELECT ?g WHERE {
        { VALUES (?g ?a ?b ?c) { (1 <urn:only> 0 0) } }
        UNION {
            { ?a <urn:member> <urn:group> . BIND(0 AS ?b) BIND(0 AS ?c) }
            UNION {
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?a)
                BIND(1 AS ?b) BIND(1 AS ?c)
            }
            UNION {
                BIND(0 AS ?a)
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?b)
                BIND(2 AS ?c)
            }
            UNION {
                BIND(1 AS ?a) BIND(2 AS ?b)
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?c)
            }
            BIND(2 AS ?g)
        }
        BIND(STR(1 + 0) AS ?s)
    } GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"#;

    let rotating_count_only = r#"SELECT (COUNT(DISTINCT *) AS ?count) WHERE {
        { ?a <urn:member> <urn:group> . BIND(0 AS ?b) BIND(0 AS ?c) }
        UNION {
            BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?a)
            BIND(1 AS ?b) BIND(1 AS ?c)
        }
        UNION {
            BIND(0 AS ?a)
            BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?b)
            BIND(2 AS ?c)
        }
        UNION {
            BIND(1 AS ?a) BIND(2 AS ?b)
            BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?c)
        }
        BIND(STR(1 + 0) AS ?s)
    }"#;

    let one = &["1"][..];
    let two = &["2"][..];

    // Small unsplit group (13 rows -> 78 pairs <= budget) still proves fixed.
    let small = reference::Reference::new(make_quads(10));
    assert!(check(&small, rotating_grouped_slice, &[two]).is_err());
    assert_eq!(
        check(&small, rotating_grouped_slice, &[one]),
        Ok(Outcome::Compared)
    );

    // Tight work_limit bounds the pair scan conservatively without failing evaluation.
    let tight = reference::Reference::new(make_quads(60)).with_work_limit(1_000);
    assert!(matches!(
        check(&tight, rotating_grouped_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&tight, rotating_count_only, &[&["63"]]),
        Ok(Outcome::Compared)
    );

    // Single unsplit bucket exceeding the pair budget (503 rows -> 126,253 pairs > 100,000)
    // retains spelling dependency conservatively and still compares without ORDER BY.
    let over_budget = reference::Reference::new(make_quads(500));
    assert!(matches!(
        check(&over_budget, rotating_grouped_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&over_budget, rotating_count_only, &[&["503"]]),
        Ok(Outcome::Compared)
    );

    // Cross-bucket budget: 50 buckets of 73 rows each (2,628 pairs per bucket,
    // 131,400 pairs combined > 100,000) exhausts the shared budget across buckets.
    let in_bucket = NamedNode::new_unchecked("urn:inBucket");
    let is_bucket = NamedNode::new_unchecked("urn:isBucket");
    let yes = NamedNode::new_unchecked("urn:yes");
    let mut bucket_quads = Vec::with_capacity(50 * 71);
    for b in 0..50 {
        let bucket = NamedNode::new_unchecked(format!("urn:b:{b}"));
        bucket_quads.push(Quad::new(
            bucket.clone(),
            is_bucket.clone(),
            yes.clone(),
            GraphName::DefaultGraph,
        ));
        for i in 0..70 {
            bucket_quads.push(Quad::new(
                NamedNode::new_unchecked(format!("urn:item:{b}:{i}")),
                in_bucket.clone(),
                bucket.clone(),
                GraphName::DefaultGraph,
            ));
        }
    }
    let multi_bucket_query = r#"SELECT ?g WHERE {
        { VALUES (?g ?k ?a ?b ?c) { (1 <urn:only> <urn:only> 0 0) } }
        UNION {
            { ?a <urn:inBucket> ?k . BIND(0 AS ?b) BIND(0 AS ?c) }
            UNION {
                ?k <urn:isBucket> <urn:yes> .
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?a)
                BIND(1 AS ?b) BIND(1 AS ?c)
            }
            UNION {
                ?k <urn:isBucket> <urn:yes> .
                BIND(0 AS ?a)
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?b)
                BIND(2 AS ?c)
            }
            UNION {
                ?k <urn:isBucket> <urn:yes> .
                BIND(1 AS ?a) BIND(2 AS ?b)
                BIND(IF(STR(1 + 0) = "1", 0, 1) AS ?c)
            }
            BIND(2 AS ?g)
        }
        BIND(STR(1 + 0) AS ?s)
    } GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"#;
    let multi_bucket = reference::Reference::new(bucket_quads);
    assert!(matches!(
        check(&multi_bucket, multi_bucket_query, &[two]),
        Ok(Outcome::Skipped(_))
    ));

    // Large group matching the review query shape under the optimizer work limit.
    let large = reference::Reference::new(make_quads(5_000)).with_work_limit(20_000_000);
    assert_eq!(
        check(&large, rotating_count_only, &[&["5003"]]),
        Ok(Outcome::Compared)
    );
}

#[test]
fn reference_str_1093_r7_bounds_count_distinct_star_bucket_refinement() {
    use reference::check::Outcome;

    let check =
        |oracle: &reference::Reference, query: &str, rows: &[&[&str]]| -> Result<Outcome, String> {
            let query = format!("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {query}");
            let expected = oracle
                .run_selected(&query, None)
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .unwrap();
            let rows = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|text| Some(text.parse::<Term>().unwrap()))
                        .collect()
                })
                .collect::<Vec<_>>();
            reference::check::check_select(&expected, &expected.variables, &rows)
        };

    fn balanced_union(branches: &[String]) -> String {
        match branches {
            [] => unreachable!(),
            [single] => single.clone(),
            _ => {
                let mid = branches.len() / 2;
                format!(
                    "{{ {} UNION {} }}",
                    balanced_union(&branches[..mid]),
                    balanced_union(&branches[mid..])
                )
            }
        }
    }

    let peeling_union = |count: usize, prefix: &str| -> String {
        let branches: Vec<String> = (1..=count)
            .map(|i| {
                if i < count {
                    format!(
                        "{{ BIND(1 AS ?{prefix}{i}) BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?{prefix}{}) }}",
                        i + 1
                    )
                } else {
                    format!("{{ BIND(1 AS ?{prefix}{i}) }}")
                }
            })
            .collect();
        balanced_union(&branches)
    };

    let grouped_slice = |count: usize| -> String {
        let body = peeling_union(count, "v");
        format!(
            "SELECT ?g WHERE {{ \
                {{ VALUES (?g ?x) {{ (1 1) (1 2) }} }} \
                UNION {{ {body} BIND(2 AS ?g) }} \
            }} GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"
        )
    };

    let count_only = |count: usize| -> String {
        let body = peeling_union(count, "v");
        format!("SELECT (COUNT(DISTINCT *) AS ?count) WHERE {{ {body} }}")
    };

    let one = &["1"][..];
    let two = &["2"][..];

    // Small peeling chain (10 rows, 9 refinement passes, 0 pair checks) fits
    // within the proof budget and still proves COUNT(DISTINCT *) fixed.
    let default_oracle = reference::Reference::new(Vec::new());
    let small_slice = grouped_slice(10);
    assert!(check(&default_oracle, &small_slice, &[two]).is_err());
    assert_eq!(
        check(&default_oracle, &small_slice, &[one]),
        Ok(Outcome::Compared)
    );

    // Tight work_limit bounds recursive bucket refinement conservatively without
    // failing evaluation: 30 peeling rows evaluate well under work_limit(1_000)
    // but need more refinement work than the remaining proof budget allows.
    let tight = reference::Reference::new(Vec::new()).with_work_limit(1_000);
    let medium_slice = grouped_slice(30);
    assert!(matches!(
        check(&tight, &medium_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&tight, &count_only(30), &[&["30"]]),
        Ok(Outcome::Compared)
    );

    // Peeling chain exceeding the default proof budget (240 rows, 0 pair checks)
    // retains spelling dependency conservatively and still compares without ORDER BY.
    let over_budget_slice = grouped_slice(240);
    assert!(matches!(
        check(&default_oracle, &over_budget_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&default_oracle, &count_only(240), &[&["240"]]),
        Ok(Outcome::Compared)
    );

    // Shared cross-group proof budget: a single 120-row peeling group fits within
    // the default budget (~36k), while three such groups (~108k) exhaust it.
    let single_120_slice = grouped_slice(120);
    assert!(check(&default_oracle, &single_120_slice, &[two]).is_err());
    assert_eq!(
        check(&default_oracle, &single_120_slice, &[one]),
        Ok(Outcome::Compared)
    );
    let multi_group_slice = format!(
        "SELECT ?g WHERE {{ \
            {{ VALUES (?g ?x) {{ (1 1) (1 2) }} }} \
            UNION {{ {} BIND(2 AS ?g) }} \
            UNION {{ {} BIND(3 AS ?g) }} \
            UNION {{ {} BIND(4 AS ?g) }} \
        }} GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1",
        peeling_union(120, "a"),
        peeling_union(120, "b"),
        peeling_union(120, "c"),
    );
    assert!(matches!(
        check(&default_oracle, &multi_group_slice, &[one]),
        Ok(Outcome::Skipped(_))
    ));

    // Large peeling chain under the optimizer work limit completes quickly and
    // compares cleanly without ORDER BY.
    let large = reference::Reference::new(Vec::new()).with_work_limit(20_000_000);
    assert_eq!(
        check(&large, &count_only(600), &[&["600"]]),
        Ok(Outcome::Compared)
    );
}

#[test]
fn reference_str_1093_r8_projects_sparse_spelling_maps_in_peeling_chain() {
    use reference::check::Outcome;
    use std::fmt::Write as _;

    let check =
        |oracle: &reference::Reference, query: &str, rows: &[&[&str]]| -> Result<Outcome, String> {
            let query = format!("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {query}");
            let expected = oracle
                .run_selected(&query, None)
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .unwrap();
            let rows = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|text| Some(text.parse::<Term>().unwrap()))
                        .collect()
                })
                .collect::<Vec<_>>();
            reference::check::check_select(&expected, &expected.variables, &rows)
        };

    fn balanced_union(branches: &[String]) -> String {
        match branches {
            [] => unreachable!(),
            [single] => single.clone(),
            _ => {
                let mid = branches.len() / 2;
                format!(
                    "{{ {} UNION {} }}",
                    balanced_union(&branches[..mid]),
                    balanced_union(&branches[mid..])
                )
            }
        }
    }

    fn balanced_binds(vars: &[String]) -> String {
        if vars.len() <= 8 {
            let mut out = String::new();
            for v in vars {
                let _ = write!(out, " BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?{v})");
            }
            return format!("{{ {out} }}");
        }
        let mid = vars.len() / 2;
        format!(
            "{{ {} {} }}",
            balanced_binds(&vars[..mid]),
            balanced_binds(&vars[mid..])
        )
    }

    let projected_peeling_union = |count: usize, dropped_bindings: usize, prefix: &str| -> String {
        let drop_vars: Vec<String> = (0..dropped_bindings).map(|j| format!("drop{j}")).collect();
        let dropped = balanced_binds(&drop_vars);
        let branches: Vec<String> = (1..=count)
            .map(|i| {
                if i < count {
                    format!(
                        "{{ BIND(1 AS ?{prefix}{i}) BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?{prefix}{}) }}",
                        i + 1
                    )
                } else {
                    format!(
                        "{{ SELECT ?{prefix}{i} ?keep WHERE {{ {{ BIND(1 AS ?{prefix}{i}) BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?keep) }} {dropped} }} }}"
                    )
                }
            })
            .collect();
        balanced_union(&branches)
    };

    let grouped_slice = |count: usize, dropped_bindings: usize| -> String {
        let body = projected_peeling_union(count, dropped_bindings, "v");
        format!(
            "SELECT ?g WHERE {{ \
                {{ VALUES (?g ?x) {{ (1 1) (1 2) }} }} \
                UNION {{ {body} BIND(2 AS ?g) }} \
            }} GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"
        )
    };

    let count_only = |count: usize, dropped_bindings: usize| -> String {
        let body = projected_peeling_union(count, dropped_bindings, "v");
        format!("SELECT (COUNT(DISTINCT *) AS ?count) WHERE {{ {body} }}")
    };

    let one = &["1"][..];
    let two = &["2"][..];
    let default_oracle = reference::Reference::new(Vec::new());

    // Small peeling chain whose last row drops 300 of 301 spelling-dependent
    // bindings via projection still proves COUNT(DISTINCT *) fixed.
    let small_slice = grouped_slice(10, 300);
    assert!(check(&default_oracle, &small_slice, &[two]).is_err());
    assert_eq!(
        check(&default_oracle, &small_slice, &[one]),
        Ok(Outcome::Compared)
    );
    assert_eq!(
        check(&default_oracle, &count_only(10, 300), &[&["10"]]),
        Ok(Outcome::Compared)
    );

    // Multi-pass peeling chain (~36k refinement units <= 100k) where the
    // projected row survives all 119 splits with one retained spelling entry
    // out of 501 completes quickly and proves fixed.
    let medium_slice = grouped_slice(120, 500);
    assert!(check(&default_oracle, &medium_slice, &[two]).is_err());
    assert_eq!(
        check(&default_oracle, &medium_slice, &[one]),
        Ok(Outcome::Compared)
    );

    // Over-budget peeling chain with a projected trailing row falls back
    // conservatively while keeping the numeric count unchanged.
    let over_budget_slice = grouped_slice(240, 300);
    assert!(matches!(
        check(&default_oracle, &over_budget_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&default_oracle, &count_only(240, 300), &[&["240"]]),
        Ok(Outcome::Compared)
    );

    // Projection that drops every spelling-dependent binding leaves a
    // spelling-free row whose COUNT(DISTINCT *) remains fixed.
    let drop_vars: Vec<String> = (0..300).map(|j| format!("drop{j}")).collect();
    let dropped = balanced_binds(&drop_vars);
    let all_dropped_slice = format!(
        "SELECT ?g WHERE {{ \
            {{ VALUES (?g ?v) {{ (1 1) }} }} \
            UNION {{ \
                {{ SELECT ?v WHERE {{ {{ BIND(1 AS ?v) }} {dropped} }} }} \
                UNION {{ SELECT ?v WHERE {{ {{ BIND(2 AS ?v) }} {dropped} }} }} \
                BIND(2 AS ?g) \
            }} \
        }} GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"
    );
    let tight = reference::Reference::new(Vec::new()).with_work_limit(2_000);
    assert!(check(&tight, &all_dropped_slice, &[two]).is_err());
    assert_eq!(
        check(&tight, &all_dropped_slice, &[one]),
        Ok(Outcome::Compared)
    );
}

#[test]
fn reference_str_1093_r8_charges_pair_variable_visits() {
    use oxrdf::{GraphName, NamedNode, Quad};
    use reference::check::Outcome;
    use std::fmt::Write as _;

    let check =
        |oracle: &reference::Reference, query: &str, rows: &[&[&str]]| -> Result<Outcome, String> {
            let query = format!("PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> {query}");
            let expected = oracle
                .run_selected(&query, None)
                .unwrap_or_else(|e| panic!("{query}: {e}"))
                .unwrap();
            let rows = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .map(|text| Some(text.parse::<Term>().unwrap()))
                        .collect()
                })
                .collect::<Vec<_>>();
            reference::check::check_select(&expected, &expected.variables, &rows)
        };

    let member_pred = NamedNode::new_unchecked("urn:member");
    let group_obj = NamedNode::new_unchecked("urn:group");
    let make_quads = |count: usize| -> Vec<Quad> {
        (0..count)
            .map(|i| {
                Quad::new(
                    NamedNode::new_unchecked(format!("urn:item:{i}")),
                    member_pred.clone(),
                    group_obj.clone(),
                    GraphName::DefaultGraph,
                )
            })
            .collect()
    };

    fn balanced_binds(vars: &[String]) -> String {
        if vars.len() <= 8 {
            let mut out = String::new();
            for v in vars {
                let _ = write!(out, " BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?{v})");
            }
            return format!("{{ {out} }}");
        }
        let mid = vars.len() / 2;
        format!(
            "{{ {} {} }}",
            balanced_binds(&vars[..mid]),
            balanced_binds(&vars[mid..])
        )
    }

    let wide_rotating_body = |extra_vars: usize, duplicate_fixed: bool| -> String {
        let fixed_arm = if duplicate_fixed {
            "{ SELECT ?a ?b ?c WHERE { ?dummy <urn:member> <urn:group> . BIND(<urn:fixed> AS ?a) BIND(0 AS ?b) BIND(0 AS ?c) } }"
        } else {
            "{ ?a <urn:member> <urn:group> . BIND(0 AS ?b) BIND(0 AS ?c) }"
        };
        let extra_names: Vec<String> = (0..extra_vars).map(|i| format!("A{i}")).collect();
        let extra = balanced_binds(&extra_names);
        format!(
            "{fixed_arm} \
            UNION {{ {{ BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?a) BIND(1 AS ?b) BIND(1 AS ?c) }} {extra} }} \
            UNION {{ BIND(0 AS ?a) BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?b) BIND(2 AS ?c) }} \
            UNION {{ BIND(1 AS ?a) BIND(2 AS ?b) BIND(IF(STR(1 + 0) = \"1\", 0, 1) AS ?c) }}"
        )
    };

    let grouped_slice = |extra_vars: usize, duplicate_fixed: bool| -> String {
        let body = wide_rotating_body(extra_vars, duplicate_fixed);
        format!(
            "SELECT ?g WHERE {{ \
                {{ VALUES (?g ?a ?b ?c) {{ (1 <urn:only> 0 0) }} }} \
                UNION {{ {body} BIND(2 AS ?g) }} \
            }} GROUP BY ?g ORDER BY COUNT(DISTINCT *) LIMIT 1"
        )
    };

    let count_only = |extra_vars: usize, duplicate_fixed: bool| -> String {
        let body = wide_rotating_body(extra_vars, duplicate_fixed);
        format!("SELECT (COUNT(DISTINCT *) AS ?count) WHERE {{ {body} }}")
    };

    let one = &["1"][..];
    let two = &["2"][..];

    // Small wide-row group still proves COUNT(DISTINCT *) fixed within budget.
    let small = reference::Reference::new(make_quads(5));
    let small_slice = grouped_slice(10, false);
    assert!(check(&small, &small_slice, &[two]).is_err());
    assert_eq!(check(&small, &small_slice, &[one]), Ok(Outcome::Compared));

    // Tight-budget case: only 63 pairs, but repeated scans across 100 extra
    // spelling-dependent bindings on the wide row exhaust the remaining proof
    // budget, retaining the dependency without changing the count.
    let tight = reference::Reference::new(make_quads(20)).with_work_limit(2_000);
    let tight_slice = grouped_slice(100, false);
    assert!(matches!(
        check(&tight, &tight_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&tight, &count_only(100, false), &[&["23"]]),
        Ok(Outcome::Compared)
    );

    // Default proof budget (100,000): refinement (~2.3k) plus pair count (903)
    // is far below 100,000, but 302 comparisons against 400 extra bindings
    // (~121k variable visits) exhaust the default budget cleanly.
    let wide_default = reference::Reference::new(make_quads(300));
    let wide_slice = grouped_slice(400, false);
    assert!(matches!(
        check(&wide_default, &wide_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&wide_default, &count_only(400, false), &[&["303"]]),
        Ok(Outcome::Compared)
    );

    // Duplicate fixed rows also exhaust the pair variable-visit budget while
    // leaving the distinct count (4) unchanged.
    let dup_slice = grouped_slice(400, true);
    assert!(matches!(
        check(&wide_default, &dup_slice, &[two]),
        Ok(Outcome::Skipped(_))
    ));
    assert_eq!(
        check(&wide_default, &count_only(400, true), &[&["4"]]),
        Ok(Outcome::Compared)
    );
}

#[test]
fn reference_str_1093_r5_keeps_min_max_fixed_across_strict_term_classes() {
    use reference::check::Outcome;
    let two = &["2"][..];
    let ten = &["10"][..];
    let base = r#"SELECT ?n WHERE {
        VALUES (?n ?copy) { (2 true) (2 false) (10 true) (10 false) }
        BIND(STR(?n + 0) AS ?s)
    } GROUP BY ?n"#;

    let min_iri = format!("{base} ORDER BY MIN(IF(?copy, <urn:fixed>, ?s)) ?n LIMIT 1");
    assert!(check_str_1093(&min_iri, &[ten]).is_err());
    assert_eq!(check_str_1093(&min_iri, &[two]), Ok(Outcome::Compared));

    let max_triple =
        format!("{base} ORDER BY MAX(IF(?copy, <<( <urn:s> <urn:p> <urn:o> )>>, ?s)) ?n LIMIT 1");
    assert!(check_str_1093(&max_triple, &[ten]).is_err());
    assert_eq!(check_str_1093(&max_triple, &[two]), Ok(Outcome::Compared));

    let max_datatype =
        format!("{base} ORDER BY DATATYPE(MAX(IF(?copy, <urn:fixed>, ?s))) ?n LIMIT 1");
    assert!(check_str_1093(&max_datatype, &[ten]).is_err());
    assert_eq!(check_str_1093(&max_datatype, &[two]), Ok(Outcome::Compared));

    let max_literal_wins = format!("{base} ORDER BY MAX(IF(?copy, <urn:fixed>, ?s)) ?n LIMIT 1");
    assert!(matches!(
        check_str_1093(&max_literal_wins, &[ten]),
        Ok(Outcome::Skipped(_))
    ));

    let min_class_can_change = format!(
        r#"{base} ORDER BY MIN(IF(?copy, <urn:fixed>, IF(?s != "010", "a", <urn:a>))) ?n LIMIT 1"#
    );
    assert!(matches!(
        check_str_1093(&min_class_can_change, &[ten]),
        Ok(Outcome::Skipped(_))
    ));

    let min_error_can_appear = format!(
        r#"{base} ORDER BY MIN(IF(?copy, <urn:fixed>, IF(?s = "010", 1 / 0, "a"))) ?n LIMIT 1"#
    );
    assert!(matches!(
        check_str_1093(&min_error_can_appear, &[ten]),
        Ok(Outcome::Skipped(_))
    ));
}

/// The reference evaluator's own cases: the choices that it notes, the scope of the
/// solution that LATERAL substitutes, BNODE across a join and its sort order.
#[test]
fn reference_evaluator_notes_open_choices_and_keeps_scopes() {
    let s = Store::in_memory(StoreOptions::default());
    load_triples(
        &s,
        &[
            "<urn:s:0> <urn:p> <urn:a>".into(),
            "<urn:s:0> <urn:p> <urn:b>".into(),
            "<urn:a> <urn:p> <urn:c>".into(),
            "<urn:b> <urn:p> <urn:c>".into(),
        ],
    );
    let snap = s.snapshot();
    let oracle = reference::Reference::new(snapshot_quads(&snap));
    let run = |q: &str| reference_select(&oracle, q, "reference");
    let int = |n: i32| Some(Term::Literal(oxrdf::Literal::from(n)));

    // two solution numbers of BNODE do not make the sides of a join incompatible
    let (_, rows, _) =
        run("SELECT * WHERE { { BIND(BNODE(\"x\") AS ?a) } { BIND(BNODE(\"y\") AS ?b) } }");
    assert_eq!(rows.len(), 1);
    let (_, rows, _) =
        run("SELECT * WHERE { { BIND(BNODE(\"x\") AS ?a) } LATERAL { BIND(BNODE(\"y\") AS ?b) } }");
    assert_eq!(rows.len(), 1);

    // a sub-SELECT's own ?a is not the ?a that LATERAL substitutes
    let (_, rows, _) = run(
        "SELECT ?c WHERE { VALUES ?a { <urn:none> } LATERAL { SELECT ?c WHERE { ?a <urn:p> ?c } } }",
    );
    assert_eq!(rows.len(), 4);

    // MIN and MAX over an error follow the order of ORDER BY, and say so
    let (_, rows, notes) =
        run("SELECT (MIN(?x) AS ?n) (MAX(?x) AS ?m) WHERE { VALUES ?x { UNDEF 1 2 } }");
    assert_eq!(rows, vec![vec![None, int(2)]]);
    assert!(notes.min_max_error && !notes.chose);
    // the three readings of item 1064
    use reference::AggregateErrors;
    let q = "SELECT (MIN(?x) AS ?n) (MAX(?x) AS ?m) (AVG(?u) AS ?a) (GROUP_CONCAT(?x) AS ?g) \
             WHERE { VALUES ?x { UNDEF 1 2 } }";
    let reading = |r: AggregateErrors| {
        let oracle = reference::Reference::new(snapshot_quads(&snap)).with_aggregate_errors(r);
        reference_select(&oracle, q, "reference").1.remove(0)
    };
    let spec = reading(AggregateErrors::Spec);
    assert_eq!(spec[..3], [None, int(2), int(0)]);
    assert_eq!(spec[3], None, "GROUP_CONCAT over an error");
    let jena = reading(AggregateErrors::Jena);
    assert_eq!(jena[..3], [None, None, None]);
    let sparkles = reading(AggregateErrors::Sparkles);
    assert_eq!(sparkles[..3], [int(1), int(2), None]);
    let concat = "SELECT (GROUP_CONCAT(?x) AS ?g) WHERE { VALUES ?x { <urn:a> } }";
    for (r, expected) in [
        (AggregateErrors::Spec, None),
        (
            AggregateErrors::Jena,
            Some(Term::Literal(oxrdf::Literal::new_simple_literal("urn:a"))),
        ),
        (
            AggregateErrors::Sparkles,
            Some(Term::Literal(oxrdf::Literal::new_simple_literal("urn:a"))),
        ),
    ] {
        let oracle = reference::Reference::new(snapshot_quads(&snap)).with_aggregate_errors(r);
        assert_eq!(
            reference_select(&oracle, concat, "reference").1[0][0],
            expected,
            "{r:?}"
        );
    }

    // the middle of a path sequence arrives as a blank node, which COUNT(DISTINCT *)
    // cannot tell from one of a basic graph pattern
    let (_, rows, notes) = run(
        "SELECT (COUNT(DISTINCT *) AS ?n) (COUNT(*) AS ?m) WHERE { <urn:s:0> <urn:p>/<urn:p> ?d }",
    );
    assert_eq!(rows, vec![vec![int(1), int(2)]]);
    assert!(notes.distinct_over_blank_nodes);

    // a slice through keys that ORDER BY leaves unordered is a choice, and one through
    // ordered keys is not
    let (_, _, notes) = run("SELECT ?x WHERE { VALUES ?x { 1 \"a\" } } ORDER BY ?x LIMIT 1");
    assert!(notes.chose);
    let (_, rows, notes) = run(
        "SELECT ?x WHERE { VALUES ?x { 2 1 \"1\"^^<http://www.w3.org/2001/XMLSchema#decimal> } } ORDER BY ?x LIMIT 1",
    );
    assert!(notes.chose, "1 and 1.0 tie, and the cut falls between them");
    assert_eq!(rows.len(), 1);
    let (_, _, notes) = run(
        "SELECT ?x WHERE { VALUES ?x { 2 1 \"1\"^^<http://www.w3.org/2001/XMLSchema#decimal> } } ORDER BY ?x LIMIT 2",
    );
    assert!(!notes.chose, "the cut between the tie and 2 is fixed");
    let (_, rows, notes) = run("SELECT ?x WHERE { VALUES ?x { 3 1 2 } } ORDER BY DESC(?x) LIMIT 2");
    assert_eq!(rows, vec![vec![int(3)], vec![int(2)]]);
    assert!(!notes.chose);
    let (_, _, notes) = run("SELECT (SAMPLE(?x) AS ?s) WHERE { VALUES ?x { 1 2 } }");
    assert!(notes.chose);

    // the sort order is transitive across numbers, strings and other literals
    let terms: Vec<Option<Term>> = [
        "1",
        "\"2.5\"^^<http://www.w3.org/2001/XMLSchema#decimal>",
        "\"2001\"^^<http://www.w3.org/2001/XMLSchema#gYear>",
        "\"NaN\"^^<http://www.w3.org/2001/XMLSchema#double>",
        "\"a\"",
        "\"a\"@en",
        "<urn:x>",
        "true",
        "\"2001-01-01T00:00:00\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
        "\"2001-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
    ]
    .iter()
    .map(|t| {
        let (_, rows, _) = run(&format!("SELECT ?x WHERE {{ VALUES ?x {{ {t} }} }}"));
        rows[0][0].clone()
    })
    .collect();
    use reference::value::{spec_order, total_order};
    for a in &terms {
        for b in &terms {
            if let Some(o) = spec_order(a.as_ref(), b.as_ref())
                && o.is_ne()
            {
                assert_eq!(total_order(a.as_ref(), b.as_ref()), o, "{a:?} {b:?}");
            }
            for c in &terms {
                let (x, y, z) = (a.as_ref(), b.as_ref(), c.as_ref());
                if total_order(x, y).is_le() && total_order(y, z).is_le() {
                    assert!(total_order(x, z).is_le(), "{a:?} {b:?} {c:?}");
                }
            }
        }
    }
    // NaN is unordered against a number
    assert_eq!(spec_order(terms[3].as_ref(), terms[0].as_ref()), None);
}

/// A term that the memory budget refuses to decode in the middle of an answer fails the
/// output with the budget's error, in every SELECT format of both executors, after the
/// rows before it, which are written whole and counted. The long literal sorts in the
/// middle of the objects, and the query reads them in object order.
///
/// The cursor runs with batches of 1 and 64 rows, whose term caches fit the budget, so the
/// batches before the long literal are written from the cache and the batch holding it
/// falls back to decoding row by row. With 4,096 rows the one batch's cache needs 128 bytes
/// a cell before any term, 256 KiB, which the 128 KiB budget refuses, so every row is
/// decoded row by row. The unit test
/// `the_refused_decode_cases_cover_cached_and_declined_batches` checks these cases, and a
/// control without the long literal is written whole under the same budget.
#[test]
fn a_refused_decode_mid_answer_keeps_the_budget_error_in_every_writer() {
    use sparkles_core::sparql::results::{
        self, Format, RowCount, SolutionsFormat, WriteOptions, write,
    };
    let load = |long: bool| {
        let s = Store::in_memory(StoreOptions::default());
        let data = (0..1000)
            .map(|i| {
                if i == 600 && long {
                    format!(
                        "<urn:s:{i}> <urn:p> \"v{i:04}{}\" .\n",
                        "x".repeat(256 << 10)
                    )
                } else {
                    format!("<urn:s:{i}> <urn:p> \"v{i:04}\" .\n")
                }
            })
            .collect::<String>();
        s.load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();
        s
    };
    let s = load(true);
    let control = load(false);
    let q = "SELECT ?o ?s WHERE { ?s <urn:p> ?o }";
    let opts = QueryOptions {
        max_memory_bytes: Some(128 << 10),
        ..Default::default()
    };
    // the rows whose object, the last thing a row holds of its own, was written
    let complete = |out: &[u8]| -> u64 {
        out.windows(6)
            .filter(|w| w[0] == b'v' && w[1..5].iter().all(u8::is_ascii_digit) && w[5] != b'x')
            .count() as u64
    };
    let refused = |name: &str, error: &Error| {
        assert!(
            matches!(error, Error::BudgetExceeded(b) if b.kind == BudgetKind::Memory),
            "{name}: {error:?}"
        );
    };
    for batch in [1, 64, 4096] {
        let mut c = select_cursor(control.snapshot(), q, &opts, &options(batch)).unwrap();
        let rows = RowCount::new();
        write(
            results::Source::Select(&mut c),
            WriteOptions::new(Format::Solutions(SolutionsFormat::Tsv), &rows),
            &mut Vec::new(),
        )
        .unwrap();
        assert_eq!(rows.get(), 1000, "control batch {batch}");
    }
    for fmt in [
        SolutionsFormat::Json,
        SolutionsFormat::Xml,
        SolutionsFormat::Csv,
        SolutionsFormat::Tsv,
        SolutionsFormat::Sparkles,
    ] {
        let r = query(s.snapshot(), q, &opts).unwrap();
        let rows = RowCount::new();
        let mut out = Vec::new();
        let error = write(
            results::Source::Result(&r),
            WriteOptions::new(Format::Solutions(fmt), &rows),
            &mut out,
        )
        .unwrap_err();
        let name = format!("eager {fmt:?}");
        refused(&name, &error);
        assert!(
            rows.get() > 0 && rows.get() < 1000,
            "{name}: {}",
            rows.get()
        );
        assert_eq!(rows.get(), complete(&out), "{name}");
        for batch in [1, 64, 4096] {
            let mut c = select_cursor(s.snapshot(), q, &opts, &options(batch)).unwrap();
            let rows = RowCount::new();
            let mut out = Vec::new();
            let error = write(
                results::Source::Select(&mut c),
                WriteOptions::new(Format::Solutions(fmt), &rows),
                &mut out,
            )
            .unwrap_err();
            let name = format!("cursor {fmt:?} batch {batch}");
            refused(&name, &error);
            assert!(
                rows.get() > 0 && rows.get() < 1000,
                "{name}: {}",
                rows.get()
            );
            assert_eq!(rows.get(), complete(&out), "{name}");
            // a cursor still open fails with the output; the one batch of 4,096 rows
            // completed the plan before the writer failed, and keeps the output's error
            let status = if batch == 4096 {
                CursorStatus::Complete
            } else {
                CursorStatus::Failed
            };
            assert_eq!(c.status(), status, "{name}");
            assert!(c.stats().error.is_some(), "{name}");
            assert!(
                c.next_batch().unwrap().is_none(),
                "{name}: the cursor is fused"
            );
        }
    }
}

/// Regression tests for roadmap item 1077: strict XSD 1.1 float/double lexical forms,
/// canonical XPath F&O `xsd:string` casts of floats/doubles, and XPath `.` excluding `\r`
/// without the `s` flag across eager, cursor/streaming, `NumColumn`, and `KeyFilter` paths.
#[test]
fn xsd_float_double_lexical_forms_string_casts_and_xpath_regex_dot() {
    use oxrdf::Literal;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let ttl = r#"
@prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:d_inf_upper ex:num "INF"^^xsd:double .
ex:d_pinf_upper ex:num "+INF"^^xsd:double .
ex:d_ninf_upper ex:num "-INF"^^xsd:double .
ex:d_nan_upper ex:num "NaN"^^xsd:double .
ex:d_inf_lower ex:num "inf"^^xsd:double .
ex:d_ninf_lower ex:num "-inf"^^xsd:double .
ex:d_pinf_lower ex:num "+inf"^^xsd:double .
ex:d_infinity ex:num "infinity"^^xsd:double .
ex:d_nan_lower ex:num "nan"^^xsd:double .
ex:d_nnan ex:num "-NaN"^^xsd:double .
ex:d_pnan ex:num "+NaN"^^xsd:double .

ex:f_inf_upper ex:num "INF"^^xsd:float .
ex:f_pinf_upper ex:num "+INF"^^xsd:float .
ex:f_ninf_upper ex:num "-INF"^^xsd:float .
ex:f_nan_upper ex:num "NaN"^^xsd:float .
ex:f_inf_lower ex:num "inf"^^xsd:float .
ex:f_ninf_lower ex:num "-inf"^^xsd:float .
ex:f_infinity ex:num "infinity"^^xsd:float .
ex:f_nan_lower ex:num "nan"^^xsd:float .
ex:f_nnan ex:num "-NaN"^^xsd:float .

ex:c_d_1e6 ex:cast "1e6"^^xsd:double .
ex:c_d_1000000 ex:cast "1000000"^^xsd:double .
ex:c_d_n1e6 ex:cast "-1e6"^^xsd:double .
ex:c_d_1_5e6 ex:cast "1.5e6"^^xsd:double .
ex:c_d_1_23e6 ex:cast "1.23e6"^^xsd:double .
ex:c_d_1e_6 ex:cast "1e-6"^^xsd:double .
ex:c_d_n1e_6 ex:cast "-1e-6"^^xsd:double .
ex:c_d_1e_7 ex:cast "1e-7"^^xsd:double .
ex:c_d_n1e_7 ex:cast "-1e-7"^^xsd:double .
ex:c_d_999999 ex:cast "999999.0e0"^^xsd:double .
ex:c_d_1_0 ex:cast "1.0e0"^^xsd:double .
ex:c_d_1_5 ex:cast "1.5e0"^^xsd:double .
ex:c_d_0_1 ex:cast "0.1"^^xsd:double .
ex:c_d_0 ex:cast "0.0e0"^^xsd:double .
ex:c_d_n0 ex:cast "-0.0e0"^^xsd:double .
ex:c_d_inf ex:cast "INF"^^xsd:double .
ex:c_d_ninf ex:cast "-INF"^^xsd:double .
ex:c_d_nan ex:cast "NaN"^^xsd:double .

ex:c_f_1e6 ex:cast "1e6"^^xsd:float .
ex:c_f_n1e6 ex:cast "-1e6"^^xsd:float .
ex:c_f_1_5e6 ex:cast "1.5e6"^^xsd:float .
ex:c_f_1e_6 ex:cast "1e-6"^^xsd:float .
ex:c_f_above_1e_6 ex:cast "1.0000001e-6"^^xsd:float .
ex:c_f_1e_7 ex:cast "1e-7"^^xsd:float .
ex:c_f_0_1 ex:cast "0.1"^^xsd:float .
ex:c_f_0 ex:cast "0.0"^^xsd:float .
ex:c_f_n0 ex:cast "-0.0"^^xsd:float .
ex:c_f_inf ex:cast "INF"^^xsd:float .
ex:c_f_ninf ex:cast "-INF"^^xsd:float .
ex:c_f_nan ex:cast "NaN"^^xsd:float .

ex:r_cr ex:txt "a\rb" .
ex:r_lf ex:txt "a\nb" .
ex:r_crlf ex:txt "a\r\nb" .
ex:r_x ex:txt "axb" .
ex:r_dot ex:txt "a.b" .
ex:r_bs_x ex:txt "a\\xb" .
ex:r_bs_cr ex:txt "a\\\rb" .
ex:r_br_x ex:txt "a]xb" .
ex:r_br_cr ex:txt "a]\rb" .
ex:r_lines ex:txt "first\rline\nsecond" .
"#;
    store
        .load(&[Source::from_bytes(
            ttl.as_bytes().to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // Also add delta triples so both compacted base (`vocab.num`) and delta paths are exercised.
    sparkles_core::sparql::update::update(
        &store,
        r#"
        PREFIX ex: <http://ex.org/>
        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
        INSERT DATA {
            ex:delta_d_inf_lower ex:num "inf"^^xsd:double .
            ex:delta_d_inf_upper ex:num "INF"^^xsd:double .
            ex:delta_r_cr ex:txt "a\rb" .
        }
        "#,
        &Default::default(),
    )
    .unwrap();
    assert!(
        store.snapshot().generation.vocab.numeric().is_some(),
        "compacted store builds vocab.num"
    );

    let run_both = |q: &str| -> Vec<Vec<Option<Term>>> {
        let opts = QueryOptions::default();
        let eager = query(store.snapshot(), q, &opts)
            .unwrap()
            .application_rows();
        for batch in [1, 4096] {
            let streamed = all(open(&store, q, batch));
            assert_eq!(
                bag(streamed),
                bag(eager.clone()),
                "batch {batch} diverged for: {q}"
            );
        }
        eager
    };
    let iri_names = |rows: Vec<Vec<Option<Term>>>| -> Vec<String> {
        let mut names: Vec<String> = rows
            .into_iter()
            .map(|r| match &r[0] {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .strip_prefix("http://ex.org/")
                    .unwrap()
                    .to_string(),
                other => panic!("expected ex: IRI, got {other:?}"),
            })
            .collect();
        names.sort();
        names
    };
    let str_pairs = |rows: Vec<Vec<Option<Term>>>| -> Vec<(String, Option<String>)> {
        let mut pairs: Vec<(String, Option<String>)> = rows
            .into_iter()
            .map(|r| {
                let k = match &r[0] {
                    Some(Term::NamedNode(n)) => n
                        .as_str()
                        .strip_prefix("http://ex.org/")
                        .unwrap()
                        .to_string(),
                    other => panic!("expected ex: IRI, got {other:?}"),
                };
                let v = r[1].as_ref().map(|t| match t {
                    Term::Literal(l) => l.value().to_string(),
                    other => panic!("expected literal, got {other:?}"),
                });
                (k, v)
            })
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs
    };

    // 1. Strict XSD 1.1 float/double lexical forms
    let numeric = iri_names(run_both(
        "PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:num ?v FILTER(isNumeric(?v)) }",
    ));
    assert_eq!(
        numeric,
        vec![
            "d_inf_upper",
            "d_nan_upper",
            "d_ninf_upper",
            "d_pinf_upper",
            "delta_d_inf_upper",
            "f_inf_upper",
            "f_nan_upper",
            "f_ninf_upper",
            "f_pinf_upper",
        ]
    );

    let gt_zero = iri_names(run_both(
        "PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:num ?v FILTER(?v > 0) }",
    ));
    assert_eq!(
        gt_zero,
        vec![
            "d_inf_upper",
            "d_pinf_upper",
            "delta_d_inf_upper",
            "f_inf_upper",
            "f_pinf_upper",
        ]
    );

    // Arithmetic on valid vs ill-typed float/double literals.
    let arith_ok = iri_names(run_both(
        "PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:num ?v BIND(?v + 1 AS ?sum) FILTER(BOUND(?sum)) }",
    ));
    assert_eq!(
        arith_ok,
        vec![
            "d_inf_upper",
            "d_nan_upper",
            "d_ninf_upper",
            "d_pinf_upper",
            "delta_d_inf_upper",
            "f_inf_upper",
            "f_nan_upper",
            "f_ninf_upper",
            "f_pinf_upper",
        ]
    );

    // Casting an ill-typed float/double literal to xsd:string preserves its lexical form.
    let ill_typed_cast = run_both(
        "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> \
         SELECT (xsd:string(?v) AS ?out) WHERE { ex:d_inf_lower ex:num ?v }",
    );
    assert_eq!(
        ill_typed_cast,
        vec![vec![Some(Term::Literal(Literal::new_simple_literal(
            "inf"
        )))]]
    );

    // String-to-float/double casts reject non-XSD spellings and Unicode whitespace
    // (U+00A0), while accepting XML whitespace around INF/+INF/-INF/NaN.
    let cast_checks = run_both(
        r#"
        PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
        SELECT ?in (BOUND(?d) AS ?d_ok) (BOUND(?f) AS ?f_ok)
        WHERE {
            VALUES ?in { "INF" "+INF" "-INF" "NaN" " INF " "\tINF\r\n" "\u00A0INF\u00A0" "\u00A01.5\u00A0" "inf" "-inf" "+inf" "infinity" "nan" "-NaN" "+NaN" }
            BIND(xsd:double(?in) AS ?d)
            BIND(xsd:float(?in) AS ?f)
        }
        "#,
    );
    let expected_cast_ok = [
        ("INF", true),
        ("+INF", true),
        ("-INF", true),
        ("NaN", true),
        (" INF ", true),
        ("\tINF\r\n", true),
        ("\u{00A0}INF\u{00A0}", false),
        ("\u{00A0}1.5\u{00A0}", false),
        ("inf", false),
        ("-inf", false),
        ("+inf", false),
        ("infinity", false),
        ("nan", false),
        ("-NaN", false),
        ("+NaN", false),
    ];
    assert_eq!(cast_checks.len(), expected_cast_ok.len());
    let mut got_casts = std::collections::BTreeMap::new();
    for row in &cast_checks {
        let got_in = match &row[0] {
            Some(Term::Literal(l)) => l.value().to_string(),
            other => panic!("unexpected {other:?}"),
        };
        assert!(
            got_casts
                .insert(got_in.clone(), (row[1].clone(), row[2].clone()))
                .is_none(),
            "duplicate input {got_in:?}"
        );
    }
    for (input, want) in expected_cast_ok {
        let want_term = Some(Term::Literal(Literal::from(want)));
        let (d_ok, f_ok) = got_casts
            .get(input)
            .unwrap_or_else(|| panic!("missing cast check row for {input:?}"));
        assert_eq!(d_ok, &want_term, "xsd:double({input:?})");
        assert_eq!(f_ok, &want_term, "xsd:float({input:?})");
    }

    // 2. XPath F&O canonical xsd:string casts of xsd:double and xsd:float
    let casts = str_pairs(run_both(
        "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> \
         SELECT ?s (xsd:string(?v) AS ?out) WHERE { ?s ex:cast ?v }",
    ));
    let expected_casts: Vec<(&str, &str)> = vec![
        ("c_d_0", "0"),
        ("c_d_0_1", "0.1"),
        ("c_d_1000000", "1.0E6"),
        ("c_d_1_0", "1"),
        ("c_d_1_23e6", "1.23E6"),
        ("c_d_1_5", "1.5"),
        ("c_d_1_5e6", "1.5E6"),
        ("c_d_1e6", "1.0E6"),
        ("c_d_1e_6", "0.000001"),
        ("c_d_1e_7", "1.0E-7"),
        ("c_d_999999", "999999"),
        ("c_d_inf", "INF"),
        ("c_d_n0", "-0"),
        ("c_d_n1e6", "-1.0E6"),
        ("c_d_n1e_6", "-0.000001"),
        ("c_d_n1e_7", "-1.0E-7"),
        ("c_d_nan", "NaN"),
        ("c_d_ninf", "-INF"),
        ("c_f_0", "0"),
        ("c_f_0_1", "0.1"),
        ("c_f_1_5e6", "1.5E6"),
        ("c_f_1e6", "1.0E6"),
        ("c_f_1e_6", "1.0E-6"),
        ("c_f_1e_7", "1.0E-7"),
        ("c_f_above_1e_6", "0.0000010000001"),
        ("c_f_inf", "INF"),
        ("c_f_n0", "-0"),
        ("c_f_n1e6", "-1.0E6"),
        ("c_f_nan", "NaN"),
        ("c_f_ninf", "-INF"),
    ];
    let mut expected_casts: Vec<(String, Option<String>)> = expected_casts
        .into_iter()
        .map(|(k, v)| (k.to_string(), Some(v.to_string())))
        .collect();
    expected_casts.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(casts, expected_casts);

    // STR() preserves the original lexical form while xsd:string() uses XPath canonical form.
    let str_vs_cast = run_both(
        "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> \
         SELECT (STR(?v) AS ?lex) (xsd:string(?v) AS ?cast) WHERE { ex:c_d_1e6 ex:cast ?v }",
    );
    assert_eq!(
        str_vs_cast,
        vec![vec![
            Some(Term::Literal(Literal::new_simple_literal("1e6"))),
            Some(Term::Literal(Literal::new_simple_literal("1.0E6"))),
        ]]
    );

    // 3. XPath REGEX / REPLACE `.` excludes `\r` without the `s` flag (including KeyFilter pushdown)
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a.b$")) }"#
        )),
        vec!["r_dot", "r_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a.b$", "s")) }"#
        )),
        vec!["delta_r_cr", "r_cr", "r_dot", "r_lf", "r_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(STR(?v), "^a.b$")) }"#
        )),
        vec!["r_dot", "r_x"]
    );
    // Escaped `\.`, `[.]`, `[^.]`, `\\.` and `[\]].`
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a\\.b$")) }"#
        )),
        vec!["r_dot"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a[.]b$")) }"#
        )),
        vec!["r_dot"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a[^.]b$")) }"#
        )),
        vec!["delta_r_cr", "r_cr", "r_lf", "r_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a\\\\.b$")) }"#
        )),
        vec!["r_bs_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a\\\\.b$", "s")) }"#
        )),
        vec!["r_bs_cr", "r_bs_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a[\\]].b$")) }"#
        )),
        vec!["r_br_x"]
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^a[\\]].b$", "s")) }"#
        )),
        vec!["r_br_cr", "r_br_x"]
    );
    // `m` flag without `s` still excludes `\r` from `.`; `ms` includes both `\r` and `\n`.
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^first.line$", "m")) }"#
        )),
        Vec::<String>::new()
    );
    assert_eq!(
        iri_names(run_both(
            r#"PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:txt ?v FILTER(REGEX(?v, "^first.line$", "ms")) }"#
        )),
        vec!["r_lines"]
    );
    // REPLACE without `s` leaves `\r` and `\n` untouched; with `s` replaces them too;
    // with `q` treats both pattern and replacement (`$` and `\`) literally.
    let replaced = run_both(
        r#"
        SELECT
            (REPLACE("a\rb\nc", ".", "X") AS ?no_s)
            (REPLACE("a\rb\nc", ".", "X", "s") AS ?with_s)
            (REPLACE("a\rb", "a.b", "X") AS ?no_match)
            (REPLACE("a\rb", "a.b", "X", "s") AS ?match_s)
            (REPLACE("a.b", ".", "$", "q") AS ?q_dollar)
            (REPLACE("a.b", ".", "\\$1\\", "q") AS ?q_bs_dollar)
            (REGEX(" axb", "^[ ]a.b$", "x") AS ?x_space_ok)
            (REGEX(" a\rb", "^[ ]a.b$", "x") AS ?x_space_cr)
            (REGEX("hello world", "hello\\ sworld", "x") AS ?x_esc_ws)
            (REGEX("\n", "(?x)^( ?s:.)$") AS ?rx_ws_scoped_s)
            (REGEX("\r", "(?x)^(# comment\n ?-s:.)$", "s") AS ?rx_comment_scoped_no_s)
            (REGEX("x", "^(?-s:(?P<a.b>.))$", "s") AS ?named_dot_ok)
            (REGEX("\r", "^(?-s:(?P<a.b>.))$", "s") AS ?named_dot_cr)
            (REGEX("\r", "^(?s:.)$") AS ?scoped_s_on)
            (REGEX("\r", "^(?-s:.)$", "s") AS ?scoped_s_off)
        WHERE {}
        "#,
    );
    assert_eq!(
        replaced,
        vec![vec![
            Some(Term::Literal(Literal::new_simple_literal("X\rX\nX"))),
            Some(Term::Literal(Literal::new_simple_literal("XXXXX"))),
            Some(Term::Literal(Literal::new_simple_literal("a\rb"))),
            Some(Term::Literal(Literal::new_simple_literal("X"))),
            Some(Term::Literal(Literal::new_simple_literal("a$b"))),
            Some(Term::Literal(Literal::new_simple_literal(r"a\$1\b"))),
            Some(Term::Literal(Literal::from(true))),
            Some(Term::Literal(Literal::from(false))),
            Some(Term::Literal(Literal::from(true))),
            Some(Term::Literal(Literal::from(true))),
            Some(Term::Literal(Literal::from(false))),
            Some(Term::Literal(Literal::from(true))),
            Some(Term::Literal(Literal::from(false))),
            Some(Term::Literal(Literal::from(true))),
            Some(Term::Literal(Literal::from(false))),
        ]]
    );

    // Dollar-heavy literal `q` replacement stays within a tight query memory budget.
    let hay = "a".repeat(4096);
    let dollars = "$".repeat(4096);
    let q_budget =
        format!(r#"SELECT (REPLACE("{hay}", "{hay}", "{dollars}", "q") AS ?out) WHERE {{}}"#);
    let tight_opts = QueryOptions {
        max_memory_bytes: Some(8 << 20),
        ..QueryOptions::default()
    };
    let eager_q = query(store.snapshot(), &q_budget, &tight_opts)
        .unwrap()
        .application_rows();
    assert_eq!(
        eager_q,
        vec![vec![Some(Term::Literal(Literal::new_simple_literal(
            &dollars
        )))]]
    );
    let cur_q =
        all(select_cursor(store.snapshot(), &q_budget, &tight_opts, &options(4096)).unwrap());
    assert_eq!(cur_q, eager_q);
}

/// Regression test for upgrading a store whose compacted generation still carries a legacy
/// `SPKVNUM2` numeric column written under pre-1077 rules (which classified `"inf"^^xsd:double`
/// and `"-inf"^^xsd:float` as IEEE infinities): reopening ignores `SPKVNUM2` cleanly,
/// `sparkles check --full` warns rather than failing, both query modes decode from keys with
/// strict XSD 1.1 semantics, and `add_numeric_column` upgrades the file to `SPKVNUM4`.
#[test]
fn legacy_spkvnum2_column_falls_back_cleanly_on_reopen() {
    use sparkles_core::check::{CheckOptions, Status, check};
    use sparkles_core::id::Id;
    use sparkles_core::vocab::numeric::{DOUBLE, FILE, FLOAT, NOT_NUMERIC};

    let dir = tempfile::tempdir().unwrap();
    let ttl = r#"
@prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:d_inf_lower ex:v "inf"^^xsd:double .
ex:f_ninf_lower ex:v "-inf"^^xsd:float .
ex:d_pinf_upper ex:v "+INF"^^xsd:double .
ex:d_num ex:v "1.50"^^xsd:double .
"#;
    {
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        store
            .load(&[Source::from_bytes(
                ttl.as_bytes().to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.numeric().is_some());
        let gen_dir = snap.generation.dir.as_ref().unwrap().clone();
        let vocab_len = snap.generation.vocab.len();

        // Build a valid SPKVNUM2 file using the pre-1077 classification rules where
        // `"inf"^^xsd:double` and `"-inf"^^xsd:float` were stored as numeric values.
        let mut covered = 0u64;
        let mut kinds = Vec::new();
        let mut values = Vec::new();
        for id in 0..vocab_len {
            let Some(Term::Literal(lit)) = snap.term(Id::vocab(id)) else {
                break;
            };
            covered += 1;
            let dt = lit.datatype().as_str();
            let (kind, bits) = if dt == oxrdf::vocab::xsd::DOUBLE.as_str() {
                match lit.value().parse::<f64>() {
                    Ok(v) => (DOUBLE, v.to_bits()),
                    Err(_) => (NOT_NUMERIC, 0),
                }
            } else if dt == oxrdf::vocab::xsd::FLOAT.as_str() {
                match lit.value().parse::<f32>() {
                    Ok(v) => (FLOAT, v.to_bits() as u64),
                    Err(_) => (NOT_NUMERIC, 0),
                }
            } else {
                (NOT_NUMERIC, 0)
            };
            kinds.push(kind);
            if kind & 8 != 0 {
                values.push(bits);
            }
        }
        assert!(
            values.len() >= 4,
            "all four non-inlined literals are in vocab: {values:?}"
        );
        let positions = covered;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SPKVNUM2");
        bytes.extend_from_slice(&vocab_len.to_le_bytes());
        bytes.extend_from_slice(&covered.to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&positions.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes()); // 1 segment covering 0..covered
        let mut word = 0u64;
        for (i, &k) in kinds.iter().enumerate() {
            word |= (k as u64) << ((i % 16) * 4);
            if (i + 1) % 16 == 0 {
                bytes.extend_from_slice(&word.to_le_bytes());
                word = 0;
            }
        }
        if !kinds.len().is_multiple_of(16) {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        // Rank array (positions < 128 -> 1 u32 entry = 0, padded to 8 bytes).
        assert!(positions <= 128);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for bits in &values {
            bytes.extend_from_slice(&bits.to_le_bytes());
        }
        // Segment (lo=0, hi=covered, base=0).
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&covered.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());

        std::fs::write(gen_dir.join(FILE), &bytes).unwrap();
    }

    let report = check(dir.path(), &CheckOptions { quick: false }).unwrap();
    assert_eq!(
        report.status,
        Status::Warning,
        "legacy SPKVNUM2 should warn, not error: {}",
        report.to_text()
    );

    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(
        store.snapshot().generation.vocab.numeric().is_none(),
        "legacy SPKVNUM2 must not be loaded"
    );

    let verify_queries = |store: &Store| {
        let run_both = |q: &str| -> Vec<Vec<Option<Term>>> {
            let opts = QueryOptions::default();
            let eager = query(store.snapshot(), q, &opts)
                .unwrap()
                .application_rows();
            for batch in [1, 4096] {
                let streamed = all(open(store, q, batch));
                assert_eq!(
                    bag(streamed),
                    bag(eager.clone()),
                    "batch {batch} diverged for: {q}"
                );
            }
            eager
        };
        let names = |q: &str| -> Vec<String> {
            let mut out: Vec<String> = run_both(q)
                .into_iter()
                .map(|r| match &r[0] {
                    Some(Term::NamedNode(n)) => n
                        .as_str()
                        .strip_prefix("http://ex.org/")
                        .unwrap()
                        .to_string(),
                    other => panic!("expected IRI, got {other:?}"),
                })
                .collect();
            out.sort();
            out
        };
        assert_eq!(
            names(
                "PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:v ?v FILTER(isNumeric(?v)) }"
            ),
            vec!["d_num", "d_pinf_upper"]
        );
        assert_eq!(
            names("PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:v ?v FILTER(?v > 0) }"),
            vec!["d_num", "d_pinf_upper"]
        );
        assert_eq!(
            names(
                "PREFIX ex: <http://ex.org/> SELECT ?s WHERE { ?s ex:v ?v BIND(?v + 1 AS ?n) FILTER(BOUND(?n)) }"
            ),
            vec!["d_num", "d_pinf_upper"]
        );
        let mut lex_and_cast: Vec<(String, String, String)> = run_both(
            "PREFIX ex: <http://ex.org/> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> \
             SELECT ?s (STR(?v) AS ?lex) (xsd:string(?v) AS ?cast) WHERE { ?s ex:v ?v }",
        )
        .into_iter()
        .map(|r| {
            let s = match &r[0] {
                Some(Term::NamedNode(n)) => n
                    .as_str()
                    .strip_prefix("http://ex.org/")
                    .unwrap()
                    .to_string(),
                other => panic!("{other:?}"),
            };
            let lex = match &r[1] {
                Some(Term::Literal(l)) => l.value().to_string(),
                other => panic!("{other:?}"),
            };
            let cast = match &r[2] {
                Some(Term::Literal(l)) => l.value().to_string(),
                other => panic!("{other:?}"),
            };
            (s, lex, cast)
        })
        .collect();
        lex_and_cast.sort();
        assert_eq!(
            lex_and_cast,
            vec![
                ("d_inf_lower".into(), "inf".into(), "inf".into()),
                ("d_num".into(), "1.50".into(), "1.5".into()),
                ("d_pinf_upper".into(), "+INF".into(), "INF".into()),
                ("f_ninf_lower".into(), "-inf".into(), "-inf".into()),
            ]
        );
    };

    verify_queries(&store);

    // Rebuilding upgrades vocab.num to SPKVNUM4 (holding only the 2 valid numbers),
    // and a second call on the same handle returns None without rewriting the file.
    assert_eq!(store.add_numeric_column().unwrap(), Some(2));
    assert_eq!(store.add_numeric_column().unwrap(), None);
    drop(store);

    let clean = check(dir.path(), &CheckOptions { quick: false }).unwrap();
    assert_eq!(clean.status, Status::Ok, "{}", clean.to_text());

    let reopened = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert!(reopened.snapshot().generation.vocab.numeric().is_some());
    assert_eq!(reopened.add_numeric_column().unwrap(), None);
    verify_queries(&reopened);
}

/// Upgrading a legacy `SPKVNUM2` column whose only numeric entries were invalid spellings
/// (`"inf"^^xsd:double` and `"-inf"^^xsd:float`) returns `Some(0)`, removes the stale
/// `vocab.num` file, and leaves `check --full` clean.
#[test]
fn legacy_spkvnum2_invalid_only_column_removed_on_upgrade() {
    use sparkles_core::check::{CheckOptions, Status, check};
    use sparkles_core::id::Id;
    use sparkles_core::vocab::numeric::{DOUBLE, FILE, FLOAT, NOT_NUMERIC};

    let dir = tempfile::tempdir().unwrap();
    let ttl = r#"
@prefix ex: <http://ex.org/> .
@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .

ex:d_inf_lower ex:v "inf"^^xsd:double .
ex:f_ninf_lower ex:v "-inf"^^xsd:float .
"#;
    let gen_dir;
    {
        let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
        store
            .load(&[Source::from_bytes(
                ttl.as_bytes().to_vec(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        let snap = store.snapshot();
        assert!(snap.generation.vocab.numeric().is_none());
        gen_dir = snap.generation.dir.as_ref().unwrap().clone();
        let vocab_len = snap.generation.vocab.len();

        let mut covered = 0u64;
        let mut kinds = Vec::new();
        let mut values = Vec::new();
        for id in 0..vocab_len {
            let Some(Term::Literal(lit)) = snap.term(Id::vocab(id)) else {
                break;
            };
            covered += 1;
            let dt = lit.datatype().as_str();
            let (kind, bits) = if dt == oxrdf::vocab::xsd::DOUBLE.as_str() {
                match lit.value().parse::<f64>() {
                    Ok(v) => (DOUBLE, v.to_bits()),
                    Err(_) => (NOT_NUMERIC, 0),
                }
            } else if dt == oxrdf::vocab::xsd::FLOAT.as_str() {
                match lit.value().parse::<f32>() {
                    Ok(v) => (FLOAT, v.to_bits() as u64),
                    Err(_) => (NOT_NUMERIC, 0),
                }
            } else {
                (NOT_NUMERIC, 0)
            };
            kinds.push(kind);
            if kind & 8 != 0 {
                values.push(bits);
            }
        }
        assert_eq!(values.len(), 2);
        let positions = covered;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"SPKVNUM2");
        bytes.extend_from_slice(&vocab_len.to_le_bytes());
        bytes.extend_from_slice(&covered.to_le_bytes());
        bytes.extend_from_slice(&(values.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&positions.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        let mut word = 0u64;
        for (i, &k) in kinds.iter().enumerate() {
            word |= (k as u64) << ((i % 16) * 4);
            if (i + 1) % 16 == 0 {
                bytes.extend_from_slice(&word.to_le_bytes());
                word = 0;
            }
        }
        if !kinds.len().is_multiple_of(16) {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        assert!(positions <= 128);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for bits in &values {
            bytes.extend_from_slice(&bits.to_le_bytes());
        }
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&covered.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());

        std::fs::write(gen_dir.join(FILE), &bytes).unwrap();
    }

    let report = check(dir.path(), &CheckOptions { quick: false }).unwrap();
    assert_eq!(report.status, Status::Warning, "{}", report.to_text());

    let store = Store::open(dir.path(), StoreOptions::default()).unwrap();
    assert_eq!(store.add_numeric_column().unwrap(), Some(0));
    assert!(
        !gen_dir.join(FILE).exists(),
        "stale SPKVNUM2 file must be removed when zero valid numbers remain"
    );
    drop(store);

    let clean = check(dir.path(), &CheckOptions { quick: false }).unwrap();
    assert_eq!(clean.status, Status::Ok, "{}", clean.to_text());
}

/// Roadmap item 1071: STRSTARTS, STRENDS, CONTAINS, STRBEFORE and STRAFTER require both
/// matching language tags (case-insensitively) and matching base directions when both
/// arguments are language-tagged, across eager, streaming/cursor and string-run/keyfilter
/// pushdown paths, and preserve base direction on matching STRBEFORE/STRAFTER results.
#[test]
fn directional_string_function_compatibility_and_results() {
    use oxrdf::{BaseDirection, Literal, Term};
    use sparkles_core::sparql::Optimizations;

    let store = Store::in_memory(StoreOptions::default());
    // Populate >4,096 rows so cursor Morsels, CountFilterFromRuns and filter_scan_runs
    // all activate on the compacted base vocabulary.
    let mut nt = String::new();
    for i in 0..5_000usize {
        let lit = match i % 5 {
            0 => format!("\"Ada{i}end\"@en--ltr"),
            1 => format!("\"Ada{i}end\"@en"),
            2 => format!("\"Ada{i}end\"@en--rtl"),
            3 => format!("\"Ada{i}end\""),
            _ => format!("\"Ada{i}end\"@EN-US--ltr"),
        };
        nt.push_str(&format!("<urn:s:{i}> <urn:label> {lit} .\n"));
    }
    store
        .load(&[Source::from_bytes(
            nt.into_bytes(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();

    // Helper: run a query eagerly (ALL and NONE), via streaming cursors (batch 1, 3,
    // 4096 with FallbackPolicy::RejectMaterialization), and compare against the reference
    // evaluator when `compare_ref` is true.
    let check_all_modes = |store: &Store, q: &str, compare_ref: bool| -> Vec<Vec<Option<Term>>> {
        let snap = store.snapshot();
        let eager_all = query(
            snap.clone(),
            q,
            &QueryOptions {
                optimizations: Some(Optimizations::ALL),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("eager ALL failed for {q}: {e}"));
        let eager_none = query(
            snap.clone(),
            q,
            &QueryOptions {
                optimizations: Some(Optimizations::NONE),
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("eager NONE failed for {q}: {e}"));

        let mut want: Vec<Vec<Option<Term>>> = eager_all.application_rows();
        let mut none_rows: Vec<Vec<Option<Term>>> = eager_none.application_rows();
        let key = |r: &[Option<Term>]| {
            r.iter()
                .map(|t| {
                    t.as_ref()
                        .map_or_else(|| "UNDEF".to_string(), ToString::to_string)
                })
                .collect::<Vec<_>>()
                .join("\t")
        };
        want.sort_by_key(|r| key(r));
        none_rows.sort_by_key(|r| key(r));
        assert_eq!(want, none_rows, "eager ALL vs NONE: {q}");

        for batch_rows in [1usize, 3, 4096] {
            let cur = select_cursor(
                snap.clone(),
                q,
                &QueryOptions {
                    optimizations: Some(Optimizations::ALL),
                    no_cache: true,
                    ..Default::default()
                },
                &options(batch_rows),
            )
            .unwrap_or_else(|e| panic!("cursor(batch={batch_rows}) open failed for {q}: {e}"));
            let mut got = all(cur);
            got.sort_by_key(|r| key(r));
            assert_eq!(want, got, "cursor(batch={batch_rows}) vs eager: {q}");
        }

        if compare_ref {
            let mut quads = Vec::new();
            snap.for_each_quad(|quad| {
                quads.push(snap.quad_to_terms(quad).unwrap());
                Ok(())
            })
            .unwrap();
            let oracle = reference::Reference::new(quads);
            let (reference::check::Expected::Select(expected), _) =
                reference::check::under_engine_readings(&oracle, q).unwrap()
            else {
                panic!("expected SELECT from reference: {q}");
            };
            assert_eq!(
                reference::check::check_select(
                    &expected,
                    eager_all.variables(),
                    &eager_all.application_rows(),
                ),
                Ok(reference::check::Outcome::Compared),
                "reference mismatch for {q}"
            );
        }
        want
    };

    // 1. Scalar / BIND / SELECT expression compatibility matrix for all 5 functions across
    //    all 6 argument pairs (both in eager and streaming cursor execution).
    let small = Store::in_memory(StoreOptions::default());
    small
        .load(&[Source::from_bytes(
            b"<urn:x> <urn:p> \"seed\" .\n".to_vec(),
            RdfFormat::NTriples,
            None,
        )])
        .unwrap();

    let bool_t = Some(Term::Literal(Literal::from(true)));
    let dir_ltr = |s: &str, lang: &str| {
        Some(Term::Literal(
            Literal::new_directional_language_tagged_literal_unchecked(s, lang, BaseDirection::Ltr),
        ))
    };
    let dir_rtl = |s: &str, lang: &str| {
        Some(Term::Literal(
            Literal::new_directional_language_tagged_literal_unchecked(s, lang, BaseDirection::Rtl),
        ))
    };
    let plain = |s: &str| Some(Term::Literal(Literal::new_simple_literal(s)));

    // (arg1, arg2, compatible)
    let pairs: [(&str, &str, bool); 6] = [
        ("\"Ada1end\"@en--ltr", "\"Ada\"@en", false),
        ("\"Ada1end\"@en", "\"Ada\"@en--ltr", false),
        ("\"Ada1end\"@en--ltr", "\"Ada\"@en--rtl", false),
        ("\"Ada1end\"@en--ltr", "\"Ada\"@EN--ltr", true),
        ("\"Ada1end\"@en--ltr", "\"Ada\"", true),
        ("\"Ada1end\"", "\"Ada\"@en--ltr", false),
    ];
    for (a, b, ok) in pairs {
        for (func, arg2, expected) in [
            ("STRSTARTS", b, if ok { bool_t.clone() } else { None }),
            (
                "STRENDS",
                &b.replace("Ada", "end"),
                if ok { bool_t.clone() } else { None },
            ),
            (
                "CONTAINS",
                &b.replace("Ada", "1"),
                if ok { bool_t.clone() } else { None },
            ),
            (
                "STRBEFORE",
                &b.replace("Ada", "1"),
                if ok { dir_ltr("Ada", "en") } else { None },
            ),
            (
                "STRAFTER",
                &b.replace("Ada", "1"),
                if ok { dir_ltr("end", "en") } else { None },
            ),
        ] {
            let q =
                format!("SELECT ({func}({a}, {arg2}) AS ?r) WHERE {{ <urn:x> <urn:p> ?dummy }}");
            let rows = check_all_modes(&small, &q, true);
            assert_eq!(rows, vec![vec![expected]], "{q}");
        }
    }

    // 2. STRBEFORE / STRAFTER edge cases: empty needle, match at start/end, non-match
    //    returning simple "", RTL preservation, and SUBSTR/UCASE/LCASE/REPLACE preserving
    //    base direction.
    let result_cases: [(&str, Option<Term>); 14] = [
        (r#"STRBEFORE("abc"@en--ltr, "b")"#, dir_ltr("a", "en")),
        (
            r#"STRBEFORE("abc"@en--ltr, "b"@EN--ltr)"#,
            dir_ltr("a", "en"),
        ),
        (r#"STRBEFORE("abc"@en--ltr, "")"#, dir_ltr("", "en")),
        (r#"STRBEFORE("abc"@en--ltr, ""@en--ltr)"#, dir_ltr("", "en")),
        (r#"STRBEFORE("abc"@en--ltr, "a")"#, dir_ltr("", "en")),
        (r#"STRBEFORE("abc"@en--ltr, "z")"#, plain("")),
        (r#"STRBEFORE("abc"@en--ltr, "z"@en--ltr)"#, plain("")),
        (r#"STRAFTER("abc"@en--ltr, "b")"#, dir_ltr("c", "en")),
        (
            r#"STRAFTER("abc"@en--ltr, "b"@EN--ltr)"#,
            dir_ltr("c", "en"),
        ),
        (r#"STRAFTER("abc"@en--ltr, "")"#, dir_ltr("abc", "en")),
        (r#"STRAFTER("abc"@en--ltr, "z")"#, plain("")),
        (
            r#"STRBEFORE("abc"@ar--rtl, "b"@ar--rtl)"#,
            dir_rtl("a", "ar"),
        ),
        (
            r#"STRAFTER("abc"@ar--rtl, "b"@AR--rtl)"#,
            dir_rtl("c", "ar"),
        ),
        (r#"SUBSTR("abc"@en--ltr, 2, 1)"#, dir_ltr("b", "en")),
    ];
    for (expr, want) in result_cases {
        let q = format!("SELECT ({expr} AS ?r) WHERE {{ <urn:x> <urn:p> ?dummy }}");
        let rows = check_all_modes(&small, &q, true);
        assert_eq!(rows, vec![vec![want]], "{q}");
    }
    for (expr, want) in [
        (r#"UCASE("abc"@en--ltr)"#, dir_ltr("ABC", "en")),
        (r#"LCASE("ABC"@ar--rtl)"#, dir_rtl("abc", "ar")),
        (r#"REPLACE("abc"@en--ltr, "b", "X")"#, dir_ltr("aXc", "en")),
    ] {
        let q = format!("SELECT ({expr} AS ?r) WHERE {{ <urn:x> <urn:p> ?dummy }}");
        let rows = check_all_modes(&small, &q, true);
        assert_eq!(rows, vec![vec![want]], "{q}");
    }
    for (expr, want) in [
        (
            r#"<http://www.w3.org/2005/xpath-functions#normalize-space>("  a \t b  "@en--ltr)"#,
            dir_ltr("a b", "en"),
        ),
        (
            r#"<http://www.w3.org/2005/xpath-functions#normalize-space>("  a \n b  "@ar--rtl)"#,
            dir_rtl("a b", "ar"),
        ),
    ] {
        let q = format!("SELECT ({expr} AS ?r) WHERE {{ <urn:x> <urn:p> ?dummy }}");
        let rows = check_all_modes(&small, &q, false);
        assert_eq!(rows, vec![vec![want]], "{q}");
    }

    // 3. String-run / KeyFilter / CountFilterFromRuns / cursor Morsels pushdown paths on
    //    the 5,000-row store (both compacted base vocab and uncompacted delta vocab).
    let filter_cases: [(&str, usize); 14] = [
        // STRSTARTS
        (r#"STRSTARTS(?v, "Ada")"#, 5_000),
        (r#"STRSTARTS(?v, "Ada"@en)"#, 1_000),
        (r#"STRSTARTS(?v, "Ada"@en--ltr)"#, 1_000),
        (r#"STRSTARTS(?v, "Ada"@EN--ltr)"#, 1_000),
        (r#"STRSTARTS(?v, "Ada"@en--rtl)"#, 1_000),
        (r#"STRSTARTS(?v, "Ada"@en-us--ltr)"#, 1_000),
        (r#"STRSTARTS(?v, "Ada"@en-us)"#, 0),
        (r#"STRSTARTS(?v, "Ada"@en-us--rtl)"#, 0),
        // STRENDS
        (r#"STRENDS(?v, "end"@en--ltr)"#, 1_000),
        (r#"STRENDS(?v, "end"@en)"#, 1_000),
        (r#"STRENDS(?v, "end"@en--rtl)"#, 1_000),
        // CONTAINS
        (r#"CONTAINS(?v, "da"@EN--ltr)"#, 1_000),
        (r#"CONTAINS(?v, "da"@en-us)"#, 0),
        // STRBEFORE / STRAFTER inside FILTER
        (r#"STRBEFORE(?v, "end"@en--ltr) != """#, 1_000),
    ];
    for (pred, expected_count) in filter_cases {
        let count_q =
            format!("SELECT (COUNT(*) AS ?c) WHERE {{ ?s <urn:label> ?v FILTER({pred}) }}");
        let rows = check_all_modes(&store, &count_q, false);
        assert_eq!(
            rows,
            vec![vec![Some(Term::Literal(Literal::from(
                expected_count as i64
            )))]],
            "{count_q}"
        );

        let scan_q = format!("SELECT ?s ?v WHERE {{ ?s <urn:label> ?v FILTER({pred}) }}");
        let scan_rows = check_all_modes(&store, &scan_q, false);
        assert_eq!(scan_rows.len(), expected_count, "{scan_q}");
    }

    // Confirm the KeyFilter pushdown plan is actually active for directional needles.
    let plan_q = r#"SELECT (COUNT(*) AS ?c) WHERE { ?s <urn:label> ?v FILTER(STRSTARTS(?v, "Ada"@en-us--ltr)) }"#;
    let plan_res = query(
        store.snapshot(),
        plan_q,
        &QueryOptions {
            optimizations: Some(Optimizations::ALL),
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    let plan_text = format!("{:#?}", plan_res.plan());
    assert!(
        plan_text.contains("CountFilterFromRuns")
            && plan_text.contains("tested on vocabulary keys")
            && plan_text.contains("key ranges"),
        "expected KeyFilter count-from-runs plan: {plan_text}"
    );

    // Confirm the cursor morsel path actually runs on the compacted base scan for
    // directional needles at 4,096-row demand (requires an empty delta).
    for morsel_q in [
        r#"SELECT ?s ?v WHERE { ?s <urn:label> ?v FILTER(STRSTARTS(?v, "Ada"@en-us--ltr)) }"#,
        r#"SELECT ?s ?v WHERE { ?s <urn:label> ?v FILTER(CONTAINS(?v, "da"@EN--ltr)) }"#,
    ] {
        let mut cur = select_cursor(
            store.snapshot(),
            morsel_q,
            &QueryOptions {
                optimizations: Some(Optimizations::ALL),
                no_cache: true,
                ..Default::default()
            },
            &options(4096),
        )
        .unwrap();
        let mut morsel_rows = 0usize;
        while let Some(batch) = cur.next_batch().unwrap() {
            morsel_rows += batch.len();
        }
        assert_eq!(cur.status(), CursorStatus::Complete, "{morsel_q}");
        assert_eq!(morsel_rows, 1_000, "{morsel_q}");
        assert!(
            leaf_counter(cur.plan(), "morselRows") > 0,
            "expected cursor morsel path for {morsel_q}: {:#?}",
            cur.plan()
        );
    }

    // 4. Add delta terms (uncompacted update) and re-check directional filter compatibility
    //    across base + delta keys.
    sparkles_core::sparql::update::update(
        &store,
        r#"INSERT DATA {
            <urn:d:1> <urn:label> "AdaDelta1end"@en--ltr .
            <urn:d:2> <urn:label> "AdaDelta2end"@en .
            <urn:d:3> <urn:label> "AdaDelta3end"@en--rtl .
            <urn:d:4> <urn:label> "AdaDelta4end" .
        }"#,
        &QueryOptions::default(),
    )
    .unwrap();
    for (pred, expected_count) in [
        (r#"STRSTARTS(?v, "AdaDelta"@en--ltr)"#, 1usize),
        (r#"STRSTARTS(?v, "AdaDelta"@EN--ltr)"#, 1),
        (r#"STRSTARTS(?v, "AdaDelta"@en)"#, 1),
        (r#"STRSTARTS(?v, "AdaDelta"@en--rtl)"#, 1),
        (r#"STRSTARTS(?v, "AdaDelta")"#, 4),
        (r#"STRENDS(?v, "end"@en--ltr) && CONTAINS(?v, "Delta")"#, 1),
        (r#"STRAFTER(?v, "AdaDelta"@en--rtl) = "3end"@en--rtl"#, 1),
    ] {
        let scan_q = format!("SELECT ?s ?v WHERE {{ ?s <urn:label> ?v FILTER({pred}) }}");
        let rows = check_all_modes(&store, &scan_q, false);
        assert_eq!(rows.len(), expected_count, "delta: {scan_q}");
    }
}

/// Cursor BIND reuse across batches answers what the eager executor answers, whether the
/// keys repeat only across batches (sparse), within every batch (dense) or never (cheap
/// and unique), with or without the expression cache. A tight memory budget either gives the
/// same rows or refuses the query with a memory budget error, and the expression cache,
/// which BIND reuse needs, never refuses a query that ordinary evaluation fits.
#[test]
fn bind_reuse_across_batches_matches_eager_for_sparse_dense_and_unique_keys() {
    const ROWS: usize = 40_960;
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..ROWS {
        data += &format!(
            "<urn:r:{i}> <urn:sparse> \"k{}\" ; <urn:dense> \"d{}\" ; <urn:unique> \"u{i}\" .\n",
            i % 16_384,
            i % 64
        );
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let snap = store.snapshot();
    let queries = [
        "SELECT ?s ?h WHERE { ?s <urn:sparse> ?k BIND(SHA256(STR(?k)) AS ?h) }",
        "SELECT ?s ?h WHERE { ?s <urn:dense> ?k BIND(UCASE(STR(?k)) AS ?h) }",
        "SELECT ?s ?h WHERE { ?s <urn:unique> ?k BIND(STR(?k) AS ?h) }",
    ];
    for text in queries {
        let eager = query(snap.clone(), text, &Default::default())
            .unwrap()
            .application_rows();
        assert_eq!(eager.len(), ROWS, "{text}");
        let eager = bag(eager);
        let mut ordinary_fits = false;
        for cache in [false, true] {
            let optimizations = Some(sparkles_core::sparql::Optimizations {
                expr_cache: cache,
                ..Default::default()
            });
            for batch in [1000, 4096] {
                let opts = QueryOptions {
                    optimizations,
                    ..Default::default()
                };
                let cursor = select_cursor(snap.clone(), text, &opts, &options(batch)).unwrap();
                assert_eq!(
                    bag(all(cursor)),
                    eager,
                    "{text} cache={cache} batch={batch}"
                );
            }
            let tight = QueryOptions {
                max_memory_bytes: Some(1 << 20),
                optimizations,
                ..Default::default()
            };
            let outcome =
                select_cursor(snap.clone(), text, &tight, &options(4096)).and_then(|mut cursor| {
                    let mut rows = Vec::new();
                    while let Some(batch) = cursor.next_batch()? {
                        for row in 0..batch.len() {
                            rows.push(batch.row(row)?.values().to_vec());
                        }
                    }
                    Ok(rows)
                });
            match outcome {
                Ok(rows) => {
                    assert_eq!(bag(rows), eager, "{text} cache={cache} under 1 MiB");
                    ordinary_fits |= !cache;
                }
                Err(Error::BudgetExceeded(b)) => {
                    assert_eq!(b.kind, BudgetKind::Memory);
                    assert!(
                        !(cache && ordinary_fits),
                        "{text}: BIND reuse refused a query that ordinary evaluation fits"
                    );
                }
                Err(other) => panic!("{text} cache={cache} under 1 MiB: {other}"),
            }
        }
    }
}
