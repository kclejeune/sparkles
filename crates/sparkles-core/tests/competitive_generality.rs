//! Independent data-oracle controls for the operators changed by competitive work.
//! The fixtures vary vocabulary, key skew, size, duplicate solutions and live updates.
use oxrdf::{NamedNode, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{
    CursorOptions, Optimizations, PlanInfo, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Snapshot, Store, StoreOptions};
use std::sync::Arc;

fn bag(rows: Vec<Vec<Option<Term>>>) -> Vec<String> {
    let mut rows: Vec<_> = rows.into_iter().map(|row| format!("{row:?}")).collect();
    rows.sort();
    rows
}

fn options(fast: bool) -> QueryOptions {
    QueryOptions {
        no_cache: true,
        optimizations: Some(if fast {
            Optimizations::ALL
        } else {
            Optimizations::NONE
        }),
        ..Default::default()
    }
}

fn iri(name: String) -> Option<Term> {
    Some(NamedNode::new(name).unwrap().into())
}

fn check_select(snapshot: &Arc<Snapshot>, text: &str, expected: Vec<Vec<Option<Term>>>) {
    fn keyed(p: &PlanInfo) -> bool {
        p.operator == "KeyedJoin" || p.children.iter().any(keyed)
    }
    let expected = bag(expected);
    for fast in [false, true] {
        let result = query(snapshot.clone(), text, &options(fast)).unwrap();
        assert_eq!(bag(result.rows()), expected, "fast={fast}: {text}");
        if fast {
            assert!(
                keyed(&result.plan),
                "the optimized join must run: {:#?}",
                result.plan
            );
        }
    }
    for batch_rows in [1, 31] {
        let mut cursor = select_cursor(
            snapshot.clone(),
            text,
            &options(true),
            &CursorOptions {
                batch_rows,
                ..Default::default()
            },
        )
        .unwrap();
        let mut rows = Vec::new();
        while let Some(batch) = cursor.next_batch().unwrap() {
            assert!(batch.len() <= batch_rows);
            rows.extend((0..batch.len()).map(|i| batch.row(i).unwrap()));
        }
        assert_eq!(bag(rows), expected, "cursor batch={batch_rows}: {text}");
    }
}

#[test]
fn equality_joins_match_a_data_oracle_across_sizes_skew_duplicates_and_updates() {
    for n in [4, 17, 65] {
        for skew in [false, true] {
            for literal in [false, true] {
                let store = Store::in_memory(StoreOptions::default());
                let key = |i: usize, stride: usize| {
                    if skew && !i.is_multiple_of(5) {
                        0
                    } else {
                        (i * stride + 3) % 11
                    }
                };
                let mut left: Vec<_> = (0..n).map(|i| (i, key(i, 7))).collect();
                let mut right: Vec<_> = (0..n).map(|i| (i, key(i, 3))).collect();
                let value = |key| {
                    if literal {
                        format!("\"café-🙂-{key}\"")
                    } else {
                        format!("<urn:audit:category:{key}>")
                    }
                };
                let mut data = String::new();
                for (side, rows) in [("left", &left), ("right", &right)] {
                    for &(i, key) in rows {
                        data += &format!(
                            "<urn:audit:{side}:{i}> <urn:audit:{side}-key> {} .\n",
                            value(key)
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
                let text = "SELECT ?left ?right WHERE { \
                    { ?left <urn:audit:left-key> ?a . ?right <urn:audit:right-key> ?b FILTER(?a = ?b) } \
                    VALUES ?repeat { 0 0 1 } }";
                let oracle = |left: &[(usize, usize)], right: &[(usize, usize)]| {
                    let mut rows = Vec::new();
                    for &(i, a) in left {
                        for &(j, b) in right {
                            if a == b {
                                for _ in 0..3 {
                                    rows.push(vec![
                                        iri(format!("urn:audit:left:{i}")),
                                        iri(format!("urn:audit:right:{j}")),
                                    ]);
                                }
                            }
                        }
                    }
                    rows
                };
                let expected_old = oracle(&left, &right);
                check_select(&old, text, expected_old.clone());
                let removed = right.remove(0);
                left.push((n, 0));
                update(
                    &store,
                    &format!(
                        "DELETE DATA {{ <urn:audit:right:0> <urn:audit:right-key> {} }}; \
                     INSERT DATA {{ <urn:audit:left:{n}> <urn:audit:left-key> {} }}",
                        value(removed.1),
                        value(0)
                    ),
                    &Default::default(),
                )
                .unwrap();
                check_select(&store.snapshot(), text, oracle(&left, &right));
                check_select(&old, text, expected_old);
            }
        }
    }
}

#[test]
fn pure_union_asks_match_full_select_answers_for_early_late_and_absent_matches() {
    for n in [1, 33, 129] {
        let store = Store::in_memory(StoreOptions::default());
        let data: String = (0..n)
            .map(|i| {
                format!(
                    "<urn:audit:record:{i}> <urn:audit:title> \"Document {i}\" ; \
             <urn:audit:owner> <urn:audit:owner:{i}> .\n"
                )
            })
            .collect();
        store
            .load(&[Source::from_bytes(
                data.into_bytes(),
                RdfFormat::Turtle,
                None,
            )])
            .unwrap();
        for sought in [0, n - 1, n + 1] {
            for reverse in [false, true] {
                let found = format!(
                    "{{ ?s <urn:audit:owner> <urn:audit:owner:{sought}> . BIND(1 AS ?kind) }}"
                );
                let absent = "{ ?s <urn:audit:title> ?label FILTER(BOUND(?label) = !BOUND(?label)) BIND(2 AS ?kind) }";
                let body = if reverse {
                    format!("{absent} UNION {found}")
                } else {
                    format!("{found} UNION {absent}")
                };
                let expected = sought < n;
                for fast in [false, true] {
                    let ask = query(
                        store.snapshot(),
                        &format!("ASK {{ {body} }}"),
                        &options(fast),
                    )
                    .unwrap();
                    let select = query(
                        store.snapshot(),
                        &format!("SELECT ?s ?kind {{ {body} }}"),
                        &options(fast),
                    )
                    .unwrap();
                    assert_eq!(
                        ask.boolean, expected,
                        "n={n}, reverse={reverse}, fast={fast}, sought={sought}"
                    );
                    assert_eq!(!select.table.is_empty(), expected);
                }
            }
        }
    }
}
