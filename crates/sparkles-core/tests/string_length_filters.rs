//! Vocabulary-key length tests preserve scalar FILTER semantics and cursor bags.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{
    CursorOptions, FallbackPolicy, Optimizations, QueryOptions, query, select_cursor,
};
use sparkles_core::store::{Store, StoreOptions};

#[test]
fn parsed_inline_integer_bounds_select_vocabulary_key_filtering() {
    fn keys(p: &sparkles_core::sparql::PlanInfo) -> bool {
        p.description.contains("on vocabulary keys") || p.children.iter().any(keys)
    }
    let store = Store::in_memory(StoreOptions {
        bulk_threshold: 0,
        ..Default::default()
    });
    let data = (0..100)
        .map(|i| format!("<urn:s{i}> <urn:value> \"abc\" .\n"))
        .collect::<String>();
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    for condition in ["STRLEN(?v) > 2", "2 < STRLEN(?v)"] {
        let text =
            format!("SELECT (COUNT(*) AS ?n) WHERE {{ ?s <urn:value> ?v FILTER({condition}) }}");
        let r = query(
            store.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.rows(), vec![vec![Some(oxrdf::Literal::from(100).into())]]);
        assert!(keys(&r.plan), "{condition}: {:?}", r.plan);
    }
}

#[test]
fn length_filters_match_scalar_evaluation_and_stream_across_batches() {
    let store = Store::in_memory(StoreOptions::default());
    let terms = [
        "\"\"",
        "\"ascii\"",
        "\"é🙂\"",
        "\"é\"",
        "\"é🙂\"@fr",
        "\"é🙂\"@ar--rtl",
        "\"123\"^^<urn:datatype>",
        "\"001\"^^<http://www.w3.org/2001/XMLSchema#integer>",
        "1",
        "<urn:é🙂>",
        "_:blank",
    ];
    let mut data = String::new();
    for i in 0..2200 {
        data += &format!("<urn:s{i}> <urn:value> {} .\n", terms[i % terms.len()]);
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    // Both base and delta vocabulary keys must obey the same argument rules.
    update(
        &store,
        "INSERT DATA { <urn:delta> <urn:value> \"東京🙂\" }",
        &Default::default(),
    )
    .unwrap();
    let scalar = QueryOptions {
        no_cache: true,
        optimizations: Some(Optimizations::NONE),
        ..Default::default()
    };
    let fast = QueryOptions {
        no_cache: true,
        ..Default::default()
    };
    for argument in ["?v", "STR(?v)"] {
        for condition in [
            format!("STRLEN({argument}) > 2"),
            format!("2 >= STRLEN({argument})"),
            format!("STRLEN({argument}) < 0"),
            format!("STRLEN({argument}) >= 0"),
        ] {
            let text = format!("SELECT ?s ?v WHERE {{ ?s <urn:value> ?v FILTER({condition}) }}");
            let bag = |rows: Vec<Vec<Option<oxrdf::Term>>>| {
                let mut v = rows
                    .into_iter()
                    .map(|r| format!("{r:?}"))
                    .collect::<Vec<_>>();
                v.sort();
                v
            };
            let expected = bag(query(store.snapshot(), &text, &scalar).unwrap().rows());
            assert_eq!(
                bag(query(store.snapshot(), &text, &fast).unwrap().rows()),
                expected,
                "{condition}"
            );
            for batch_rows in [1, 7, 4096] {
                let mut cursor = select_cursor(
                    store.snapshot(),
                    &text,
                    &fast,
                    &CursorOptions {
                        batch_rows,
                        fallback: FallbackPolicy::RejectMaterialization,
                        ..Default::default()
                    },
                )
                .unwrap();
                let mut rows = Vec::new();
                while let Some(batch) = cursor.next_batch().unwrap() {
                    rows.extend((0..batch.len()).map(|i| batch.row(i).unwrap()));
                }
                assert_eq!(bag(rows), expected, "{condition}, batch={batch_rows}");
            }
        }
    }
}
