//! Singleton-pattern substitution preserves RDF term identity, multiplicities,
//! graph scope and the variables returned by the surrounding algebra.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::extensions::{
    ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarResult,
};
use sparkles_core::sparql::update::update;
use sparkles_core::sparql::{Optimizations, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn fixture() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    s.load(&[Source::from_bytes(
        br#"@prefix ex: <urn:> .
            ex:anchor ex:value 10; ex:single ex:alice; ex:blank _:one .
            ex:alice ex:value 8; ex:name "Alice" .
            ex:bob ex:value 12; ex:name "Bob" .
            _:one ex:name "Blank" .
            ex:anchor ex:many ex:alice, ex:bob .
            ex:anchor ex:lexical "01"^^<http://www.w3.org/2001/XMLSchema#integer> .
            ex:alice ex:lexical 1 .
            ex:g1 { ex:anchor ex:single ex:alice . ex:alice ex:name "Named Alice" . }
            ex:g2 { ex:anchor ex:single ex:bob . ex:bob ex:name "Named Bob" . }
        "#
        .to_vec(),
        RdfFormat::TriG,
        None,
    )])
    .unwrap();
    s
}

#[test]
fn point_patterns_preserve_graph_scope_terms_filters_and_algebra() {
    let s = fixture();
    for body in [
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n }",
        "SELECT * WHERE { <urn:anchor> <urn:blank> ?x . ?x <urn:name> ?n }",
        "SELECT * WHERE { <urn:anchor> <urn:missing> ?x . ?x <urn:name> ?n }",
        "SELECT * WHERE { <urn:anchor> <urn:many> ?x . ?x <urn:name> ?n }",
        "SELECT * WHERE { <urn:anchor> <urn:lexical> ?x . ?s <urn:lexical> ?x }",
        "SELECT * WHERE { <urn:anchor> <urn:value> ?a . ?s <urn:value> ?b FILTER(?b > ?a - 1 && ?b < ?a + 3) }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n FILTER(BOUND(?x) && sameTerm(?x, <urn:alice>)) }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n FILTER(EXISTS { ?x <urn:value> ?v }) }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . OPTIONAL { ?x <urn:name> ?n } }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . VALUES ?x { <urn:alice> <urn:alice> } }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . { SELECT ?x ?n WHERE { ?x <urn:name> ?n } } }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:missing>* ?y }",
        "SELECT * WHERE { <urn:anchor> <urn:single> ?x . BIND(STR(?x) AS ?s) }",
        "SELECT * WHERE { GRAPH <urn:g1> { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n } }",
        "SELECT * WHERE { GRAPH ?g { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n } }",
        "SELECT * FROM <urn:g1> FROM <urn:g2> WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?n }",
    ] {
        let run = |point_patterns| {
            let result = query(
                s.snapshot(),
                body,
                &QueryOptions {
                    no_cache: true,
                    optimizations: Some(Optimizations {
                        point_patterns,
                        ..Optimizations::ALL
                    }),
                    ..Default::default()
                },
            )
            .unwrap_or_else(|e| panic!("{body}: {e}"));
            let mut rows = result
                .rows()
                .into_iter()
                .map(|r| format!("{r:?}"))
                .collect::<Vec<_>>();
            rows.sort();
            rows
        };
        assert_eq!(run(true), run(false), "{body}");
    }
}

#[test]
fn point_patterns_read_the_current_delta_and_keep_old_snapshots() {
    let s = fixture();
    let old = s.snapshot();
    update(
        &s,
        "INSERT DATA { <urn:anchor> <urn:single> <urn:bob> }",
        &Default::default(),
    )
    .unwrap();
    let text = "SELECT ?x WHERE { <urn:anchor> <urn:single> ?x . ?x <urn:name> ?name }";
    for (snap, expected) in [(old, 1), (s.snapshot(), 2)] {
        for point_patterns in [true, false] {
            let r = query(
                snap.clone(),
                text,
                &QueryOptions {
                    no_cache: true,
                    optimizations: Some(Optimizations {
                        point_patterns,
                        ..Optimizations::ALL
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(r.rows().len(), expected);
        }
    }
}

#[test]
fn point_patterns_preserve_application_filter_evaluation_count() {
    let s = fixture();
    let text = "SELECT * WHERE { <urn:anchor> <urn:single> ?x . ?s <urn:name> ?n FILTER(<urn:test:tick>(?x)) }";
    let run = |point_patterns| {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let tick = move |_: &ScalarContext<'_>, _: &[oxrdf::Term]| -> ScalarResult {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(oxrdf::Literal::from(true).into())
        };
        let mut registry = ExtensionRegistry::builder();
        registry
            .register_scalar(ScalarDescriptor::new("urn:test:tick", 1..=1, tick).unwrap())
            .unwrap();
        let r = query(
            s.snapshot(),
            text,
            &QueryOptions {
                no_cache: true,
                extensions: Some(registry.build()),
                optimizations: Some(Optimizations {
                    point_patterns,
                    ..Optimizations::ALL
                }),
                ..Default::default()
            },
        )
        .unwrap();
        (r.rows().len(), calls.load(Ordering::SeqCst))
    };
    let baseline = run(false);
    assert_eq!(baseline.0, 3);
    assert_eq!(run(true), baseline);
}

#[test]
fn point_patterns_leave_volatile_filters_at_their_original_input() {
    fn assignments(p: &sparkles_core::sparql::PlanInfo) -> usize {
        usize::from(p.description.contains(":="))
            + p.children.iter().map(assignments).sum::<usize>()
    }
    let s = fixture();
    for (condition, substituted) in [
        ("BOUND(?x)", true),
        ("BOUND(?x) && RAND() >= 0", false),
        ("BOUND(?x) && isIRI(UUID())", false),
        ("BOUND(?x) && STRLEN(STRUUID()) > 0", false),
        ("BOUND(?x) && isBlank(BNODE())", false),
    ] {
        let text = format!(
            "SELECT * WHERE {{ <urn:anchor> <urn:single> ?x . ?s <urn:name> ?n FILTER({condition}) }}"
        );
        let r = query(
            s.snapshot(),
            &text,
            &QueryOptions {
                no_cache: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(r.rows().len(), 3);
        assert_eq!(
            assignments(&r.plan),
            usize::from(substituted),
            "{condition}"
        );
    }
}
