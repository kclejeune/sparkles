//! Selective graph patterns must constrain UNION branches before their broad joins run.
use oxrdf::{Literal, Term};
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::{QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};

const BODY: &str = r#"
  ?anchor a <urn:Person>; <urn:name> "Anchor" .
  {
    ?paper <urn:creator> ?anchor, ?author .
    ?otherPaper <urn:creator> ?author, ?otherAuthor .
    ?otherAuthor <urn:name> ?name .
    FILTER(?author != ?anchor && ?otherPaper != ?paper &&
           ?otherAuthor != ?anchor && ?otherAuthor != ?author)
  } UNION {
    ?paper <urn:creator> ?anchor, ?author .
    ?author <urn:name> ?name .
    FILTER(?author != ?anchor)
  }
"#;

fn bibliographic_store(unrelated_papers: usize) -> Store {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from(
        "<urn:anchor> a <urn:Person>; <urn:name> \"Anchor\" .\n\
         <urn:alice> <urn:name> \"Alice\" .\n\
         <urn:bob> <urn:name> \"Bob\" .\n\
         <urn:paper:first> <urn:creator> <urn:anchor>, <urn:alice> .\n\
         <urn:paper:second> <urn:creator> <urn:alice>, <urn:bob> .\n\
         <urn:hub> a <urn:Person>; <urn:name> \"Hub\" .\n",
    );
    // This disconnected component has a quadratic number of two-hop coauthor walks.
    // None belongs to Anchor's component or contributes a result.
    for i in 0..unrelated_papers {
        data.push_str(&format!(
            "<urn:author:{i}> a <urn:Person>; <urn:name> \"Other {i}\" .\n\
             <urn:paper:{i}> <urn:creator> <urn:hub>, <urn:author:{i}> .\n"
        ));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    store
}

#[test]
fn a_selective_pattern_limits_work_inside_union_branches() {
    for unrelated in [16, 64] {
        let store = bibliographic_store(unrelated);
        for ask in [false, true] {
            let q = format!(
                "{} WHERE {{ {BODY} }}",
                if ask { "ASK" } else { "SELECT DISTINCT ?name" }
            );
            let result = query(
                store.snapshot(),
                &q,
                &QueryOptions {
                    no_cache: true,
                    ..Default::default()
                },
            )
            .unwrap();
            if ask {
                assert!(result.boolean);
            } else {
                let mut names = result.rows().unwrap().into_iter().collect::<Vec<_>>();
                names.sort_by_key(|row| row.values()[0].as_ref().unwrap().to_string());
                assert_eq!(
                    names.iter().map(|row| row.values()).collect::<Vec<_>>(),
                    [
                        vec![Some(Term::Literal(Literal::new_simple_literal("Alice")))],
                        vec![Some(Term::Literal(Literal::new_simple_literal("Bob")))]
                    ]
                    .iter()
                    .map(Vec::as_slice)
                    .collect::<Vec<_>>()
                );
            }
            eprintln!(
                "union work: unrelated={unrelated} ask={ask} rows_produced={}",
                result.rows_produced
            );
            // Scanning the unrelated input is permitted. Expanding all of its walks is not.
            assert!(
                result.rows_produced < (unrelated * 64 + 512) as u64,
                "unrelated={unrelated} ask={ask}: {} rows produced",
                result.rows_produced
            );
        }
    }
}

#[test]
fn an_ordinary_anchor_preserves_hidden_duplicates_and_union_bags() {
    let store = bibliographic_store(0);
    store
        .load(&[Source::from_bytes(
            b"<urn:anchor> <urn:tag> \"one\", \"two\" .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let anchor = "?anchor <urn:name> \"Anchor\"; <urn:tag> ?hidden";
    let union = "{ ?paper <urn:creator> ?anchor, ?author . ?author <urn:name> ?name FILTER(?author != ?anchor) } UNION { ?paper <urn:creator> ?anchor, ?author . ?author <urn:name> ?name FILTER(?author != ?anchor) }";
    for body in [
        format!("{anchor} {{ {union} }}"),
        format!("{{ {union} }} {anchor}"),
    ] {
        for distinct in [false, true] {
            let q = format!(
                "SELECT {} ?name WHERE {{ {body} }}",
                if distinct { "DISTINCT" } else { "" }
            );
            let r = query(store.snapshot(), &q, &QueryOptions::default()).unwrap();
            assert_eq!(r.len(), if distinct { 1 } else { 4 }, "{q}");
            for row in r.rows().unwrap() {
                assert_eq!(
                    row.values(),
                    vec![Some(Term::Literal(Literal::new_simple_literal("Alice")))]
                );
            }
        }
    }
}

#[test]
fn a_distributed_shared_anchor_preserves_repeated_union_bags() {
    let store = bibliographic_store(64);
    let long = "?paper <urn:creator> ?anchor, ?author . ?otherPaper <urn:creator> ?author, ?otherAuthor . ?otherAuthor <urn:name> ?name FILTER(?author != ?anchor && ?otherPaper != ?paper && ?otherAuthor != ?anchor && ?otherAuthor != ?author)";
    let short = "?paper <urn:creator> ?anchor, ?author . ?author <urn:name> ?name FILTER(?author != ?anchor)";
    let branches = [long, short, long, short]
        .map(|b| format!("{{ {b} }}"))
        .join(" UNION ");
    let anchor = "?anchor a <urn:Person>; <urn:name> \"Anchor\" .";
    for body in [
        format!("{anchor} {{ {branches} }}"),
        format!("{{ {branches} }} {anchor}"),
    ] {
        let result = query(
            store.snapshot(),
            &format!("SELECT ?name WHERE {{ {body} }}"),
            &QueryOptions::default(),
        )
        .unwrap();
        let mut names = result
            .rows()
            .unwrap()
            .into_iter()
            .map(|row| row.values()[0].as_ref().unwrap().to_string())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, ["\"Alice\"", "\"Alice\"", "\"Bob\"", "\"Bob\""]);
        assert!(
            result.rows_produced < 64 * 128 + 1024,
            "{} rows produced",
            result.rows_produced
        );
    }
}

#[test]
fn an_outer_binding_does_not_change_a_union_filter_scope() {
    let store = bibliographic_store(0);
    store
        .load(&[Source::from_bytes(
            b"<urn:anchor> <urn:tag> \"one\", \"two\" .".to_vec(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    for (filter, expected) in [
        ("BOUND(?outer)", 0),
        ("!BOUND(?outer)", 8),
        ("?outer = \"one\"", 0),
    ] {
        let branch = format!("?anchor <urn:tag> ?inner FILTER({filter})");
        let q = format!(
            "SELECT ?outer ?inner WHERE {{ ?anchor <urn:name> \"Anchor\"; <urn:tag> ?outer . {{ {branch} }} UNION {{ {branch} }} }}"
        );
        let r = query(store.snapshot(), &q, &QueryOptions::default()).unwrap();
        assert_eq!(r.len(), expected, "{q}");
    }
}

#[test]
fn an_empty_anchor_keeps_false_ask_and_empty_select() {
    let store = bibliographic_store(64);
    let body = BODY.replace("\"Anchor\"", "\"Missing\"");
    for projection in ["ASK", "SELECT DISTINCT ?name"] {
        let q = format!("{projection} WHERE {{ {body} }}");
        let r = query(store.snapshot(), &q, &QueryOptions::default()).unwrap();
        if projection == "ASK" {
            assert!(!r.boolean);
        } else {
            assert!(r.is_empty());
        }
    }
}

#[test]
fn a_broad_anchor_is_not_repeated_in_each_union_branch() {
    let store = Store::in_memory(StoreOptions::default());
    let data = (0..128)
        .map(|i| format!("<urn:s{i}> <urn:marker> <urn:target>; <urn:p> {i} .\n"))
        .collect::<String>();
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let q = "SELECT ?v WHERE { ?root <urn:marker> <urn:target> { ?root <urn:p> ?v } UNION { ?root <urn:p> ?v } }";
    let r = query(store.snapshot(), q, &QueryOptions::default()).unwrap();
    assert_eq!(r.len(), 256);
    fn has_marker(p: &sparkles_core::sparql::PlanInfo) -> bool {
        p.description.contains("urn:marker") || p.children.iter().any(has_marker)
    }
    fn check_union(p: &sparkles_core::sparql::PlanInfo) -> usize {
        if p.operator == "Union" {
            assert!(!p.children.iter().any(has_marker), "{p:#?}");
            1
        } else {
            p.children.iter().map(check_union).sum()
        }
    }
    assert_eq!(check_union(r.plan()), 1);
}

#[test]
fn the_replanned_union_streams_across_requested_batch_sizes() {
    use sparkles_core::sparql::{CursorOptions, FallbackPolicy, ask_streaming, select_cursor};
    let store = bibliographic_store(64);
    let select = format!("SELECT DISTINCT ?name WHERE {{ {BODY} }}");
    let ask = format!("ASK WHERE {{ {BODY} }}");
    let expected = query(store.snapshot(), &select, &QueryOptions::default())
        .unwrap()
        .rows()
        .unwrap();
    let mut expected = expected
        .into_iter()
        .map(|row| format!("{row:?}"))
        .collect::<Vec<_>>();
    expected.sort();
    for rows in [1, 7, 4096] {
        let options = CursorOptions {
            batch_rows: rows,
            fallback: FallbackPolicy::RejectMaterialization,
            ..Default::default()
        };
        let mut cursor = select_cursor(
            store.snapshot(),
            &select,
            &QueryOptions::default(),
            &options,
        )
        .unwrap();
        assert!(!cursor.plan().has_materialization());
        let mut actual = Vec::new();
        while let Some(batch) = cursor.next_batch().unwrap() {
            assert!(batch.len() <= rows);
            for row in 0..batch.len() {
                actual.push(format!("{:?}", batch.row(row).unwrap().values()));
            }
        }
        actual.sort();
        assert_eq!(actual, expected, "batch size {rows}");
        assert!(cursor.stats().rows_produced < 64 * 64 + 512);
        let result =
            ask_streaming(store.snapshot(), &ask, &QueryOptions::default(), &options).unwrap();
        assert!(result.boolean);
        assert!(result.rows_produced < 64 * 64 + 512);
    }
}

#[test]
fn a_small_anchor_with_expensive_reads_is_computed_once() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..256 {
        data.push_str(&format!("<urn:s:{:08}> a <urn:C> .\n", 2 * i + 2));
        data.push_str(&format!("<urn:s:{:08}> <urn:q> <urn:x> .\n", 2 * i + 1));
        for branch in 0..8 {
            data.push_str(&format!(
                "<urn:s:{:08}> <urn:r{branch}> \"v\" .\n",
                2 * i + 1
            ));
        }
    }
    data.push_str("<urn:shared> a <urn:C>; <urn:q> <urn:x> .\n");
    for branch in 0..8 {
        data.push_str(&format!("<urn:shared> <urn:r{branch}> \"v\" .\n"));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let branches = (0..8)
        .map(|branch| format!("{{ ?s <urn:r{branch}> ?v }}"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let q = format!("SELECT ?s ?v WHERE {{ ?s a <urn:C>; <urn:q> ?x . {{ {branches} }} }}");
    let result = query(
        store.snapshot(),
        &q,
        &QueryOptions {
            no_cache: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(result.len(), 8);
    for row in result.rows().unwrap() {
        assert_eq!(
            row.values(),
            vec![
                Some(Term::NamedNode(
                    oxrdf::NamedNode::new("urn:shared").unwrap()
                )),
                Some(Term::Literal(Literal::new_simple_literal("v")))
            ]
        );
    }
    fn reads_class(p: &sparkles_core::sparql::PlanInfo) -> bool {
        p.description.contains("urn:C") || p.children.iter().any(reads_class)
    }
    fn check(p: &sparkles_core::sparql::PlanInfo) -> usize {
        if p.operator == "Union" {
            assert!(
                !p.children.iter().any(reads_class),
                "repeated expensive anchor: {p:#?}"
            );
            1
        } else {
            p.children.iter().map(check).sum()
        }
    }
    assert_eq!(check(result.plan()), 1);
}

#[test]
fn outer_filters_keep_the_original_join_boundary() {
    let store = bibliographic_store(64);
    let anchor = "?anchor a <urn:Person>; <urn:name> \"Anchor\" .";
    let union = BODY.replace(anchor, "");
    for body in [
        format!("{anchor} {{ {union} }}"),
        format!("{{ {union} }} {anchor}"),
    ] {
        for expression in [
            "RAND() + IF(STR(?anchor) = \"x\", 1, 2) > 0",
            "STRLEN(STR(UUID())) + STRLEN(STR(?anchor)) > 0",
            "STRLEN(STRUUID()) + STRLEN(STR(?anchor)) > 0",
            "IF(isBlank(BNODE()), STRLEN(STR(?anchor)), 0) > 0",
            "YEAR(NOW()) + STRLEN(STR(?anchor)) > 0",
            "EXISTS { ?anchor <urn:name> ?n }",
            "STRLEN(STR(<http://jena.apache.org/ARQ/function#uuid>())) + STRLEN(STR(?anchor)) > 0",
            "STRLEN(<http://jena.apache.org/ARQ/function#struuid>()) + STRLEN(STR(?anchor)) > 0",
            "STRLEN(STR(<http://www.w3.org/2005/xpath-functions#apply>(<http://jena.apache.org/ARQ/function#uuid>))) + STRLEN(STR(?anchor)) > 0",
            "STRLEN(STR(?anchor)) > 0",
        ] {
            let q = format!("SELECT DISTINCT ?name WHERE {{ {body} FILTER({expression}) }}");
            let result = query(
                store.snapshot(),
                &q,
                &QueryOptions {
                    no_cache: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let mut names = result.rows().unwrap().into_iter().collect::<Vec<_>>();
            names.sort_by_key(|row| row.values()[0].as_ref().unwrap().to_string());
            assert_eq!(
                names.iter().map(|row| row.values()).collect::<Vec<_>>(),
                [
                    vec![Some(Term::Literal(Literal::new_simple_literal("Alice")))],
                    vec![Some(Term::Literal(Literal::new_simple_literal("Bob")))]
                ]
                .iter()
                .map(Vec::as_slice)
                .collect::<Vec<_>>(),
                "{q}"
            );
            fn has_anchor(p: &sparkles_core::sparql::PlanInfo) -> bool {
                p.description.contains("\"Anchor\"") || p.children.iter().any(has_anchor)
            }
            fn check_union(p: &sparkles_core::sparql::PlanInfo) -> usize {
                if p.operator == "Union" {
                    assert!(!p.children.iter().any(has_anchor), "{p:#?}");
                    1
                } else {
                    p.children.iter().map(check_union).sum()
                }
            }
            assert_eq!(check_union(result.plan()), 1, "{q}");
            fn rand_filters(p: &sparkles_core::sparql::PlanInfo) -> usize {
                usize::from(p.operator == "Filter" && p.description.contains("RAND()"))
                    + p.children.iter().map(rand_filters).sum::<usize>()
            }
            if expression.starts_with("RAND()") {
                assert_eq!(rand_filters(result.plan()), 1, "{q}");
            }
        }
    }
}

/// A UNION under a LIMIT reads its cheapest branch first in every execution mode, so a
/// query whose first branch must join two large inputs before its first row answers
/// from the cheap branch alone (item 827).
#[test]
fn a_limited_union_reads_its_cheap_branch_first_in_every_mode() {
    use sparkles_core::sparql::{CursorOptions, ExecutionMode, QueryExecution, query_execution};
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from("<urn:yes> <urn:cheap> 0 .\n");
    for i in 0..20_000 {
        data.push_str(&format!("<urn:a{i}> <urn:left> {} .\n", i % 5_000));
        data.push_str(&format!("<urn:b{i}> <urn:right> {} .\n", i % 5_000));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let union = "{ ?a <urn:left> ?v . ?b <urn:right> ?v } UNION { <urn:yes> <urn:cheap> ?v }";
    for text in [
        format!("SELECT ?v WHERE {{ {union} }} LIMIT 1"),
        format!("SELECT ?v WHERE {{ {union} }} LIMIT 1 OFFSET 0"),
        format!("ASK {{ {union} }}"),
    ] {
        for mode in [
            ExecutionMode::Eager,
            ExecutionMode::Auto,
            ExecutionMode::Streaming,
        ] {
            let mut execution = query_execution(
                store.snapshot(),
                &text,
                &QueryOptions::default(),
                &CursorOptions::default(),
                mode,
            )
            .unwrap();
            let produced = match &mut execution {
                QueryExecution::Select(cursor) => {
                    let mut rows = 0;
                    while let Some(batch) = cursor.next_batch().unwrap() {
                        rows += batch.len();
                    }
                    assert_eq!(rows, 1, "{text} under {mode:?}");
                    cursor.stats().rows_produced
                }
                QueryExecution::Eager(result) | QueryExecution::Ask(result) => {
                    let result = result.result();
                    if result.kind == sparkles_core::sparql::QueryKind::Ask {
                        assert!(result.boolean);
                    } else {
                        assert_eq!(result.len(), 1, "{text} under {mode:?}");
                    }
                    result.rows_produced
                }
                QueryExecution::Graph(_) => unreachable!(),
            };
            assert!(
                produced < 1_000,
                "{text} under {mode:?} produced {produced} rows"
            );
        }
    }
}

/// A LIMIT over a UNION whose branches share no variable, and an OFFSET past the
/// answer, give the rows the whole UNION would.
#[test]
fn limited_unions_without_shared_variables_and_offsets_past_the_answer() {
    use sparkles_core::sparql::{CursorOptions, ExecutionMode, QueryExecution, query_execution};
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::new();
    for i in 0..300 {
        data.push_str(&format!("<urn:a{i}> <urn:p> {i} .\n"));
    }
    data.push_str("<urn:b> <urn:q> \"b\" .\n");
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    for (text, rows) in [
        (
            "SELECT * WHERE { { ?a <urn:p> ?x } UNION { ?b <urn:q> ?y } } LIMIT 5",
            5,
        ),
        (
            "SELECT * WHERE { { ?a <urn:p> ?x } UNION { ?b <urn:q> ?y } } LIMIT 5 OFFSET 299",
            2,
        ),
        (
            "SELECT * WHERE { { ?a <urn:p> ?x } UNION { ?b <urn:q> ?y } } LIMIT 5 OFFSET 301",
            0,
        ),
        ("SELECT * WHERE { ?a <urn:p> ?x } LIMIT 10 OFFSET 1000", 0),
    ] {
        for mode in [
            ExecutionMode::Eager,
            ExecutionMode::Auto,
            ExecutionMode::Streaming,
        ] {
            let mut execution = query_execution(
                store.snapshot(),
                text,
                &QueryOptions::default(),
                &CursorOptions::default(),
                mode,
            )
            .unwrap();
            let got = match &mut execution {
                QueryExecution::Select(cursor) => {
                    let mut n = 0;
                    while let Some(batch) = cursor.next_batch().unwrap() {
                        n += batch.len();
                    }
                    n
                }
                QueryExecution::Eager(result) => result.result().len(),
                _ => unreachable!(),
            };
            assert_eq!(got, rows, "{text} under {mode:?}");
        }
    }
}

/// A UNION whose branches evaluate an expression with effects, or an EXISTS that may
/// call SERVICE, keeps its written branch order under a LIMIT, so that the effects
/// happen as written. The same branches without the effect go cheapest first.
#[test]
fn a_limited_union_with_effects_keeps_its_written_order() {
    let store = Store::in_memory(StoreOptions::default());
    let mut data = String::from("<urn:yes> <urn:cheap> 0 .\n");
    for i in 0..2_000 {
        data.push_str(&format!("<urn:a{i}> <urn:left> {} .\n", i % 500));
        data.push_str(&format!("<urn:b{i}> <urn:right> {} .\n", i % 500));
    }
    store
        .load(&[Source::from_bytes(
            data.into_bytes(),
            RdfFormat::Turtle,
            None,
        )])
        .unwrap();
    let first_branch = |filter: &str| {
        let text = format!(
            "PREFIX afn: <http://jena.apache.org/ARQ/function#> SELECT ?v WHERE {{ \
             {{ ?a <urn:left> ?v . ?b <urn:right> ?v {filter} }} \
             UNION {{ <urn:yes> <urn:cheap> ?v }} }} LIMIT 1"
        );
        let r = query(store.snapshot(), &text, &QueryOptions::default())
            .unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(r.len(), 1, "{text}");
        let plan = serde_json::to_string(r.plan()).unwrap();
        let left = plan.find("urn:left").expect("the expensive branch");
        let cheap = plan.find("urn:cheap").expect("the cheap branch");
        left < cheap
    };
    assert!(
        !first_branch(""),
        "a plain UNION reads its cheap branch first"
    );
    assert!(!first_branch("FILTER(?v != 7)"));
    for filter in [
        "FILTER(afn:wait(0))",
        "FILTER(afn:print(?v))",
        "FILTER(EXISTS { ?v <urn:cheap> ?w })",
        "BIND(IF(EXISTS { ?a <urn:cheap> ?w }, 1, 0) AS ?e)",
        "FILTER(STRLEN(STRUUID()) > 0)",
    ] {
        assert!(first_branch(filter), "{filter} keeps the written order");
    }
}
