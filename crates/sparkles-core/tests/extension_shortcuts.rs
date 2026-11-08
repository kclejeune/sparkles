//! Shortcuts that skip evaluations keep the evaluations of application callbacks:
//! comparisons decided on ids, probed OPTIONALs and ORDER BY keys taken from a BIND.
use oxrdfio::RdfFormat;
use sparkles_core::io::Source;
use sparkles_core::sparql::extensions::{
    ExtensionRegistry, ScalarContext, ScalarDescriptor, ScalarResult,
};
use sparkles_core::sparql::{Optimizations, PlanInfo, QueryOptions, query};
use sparkles_core::store::{Store, StoreOptions};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// 2,048 subjects with a negative value each and a rating for every other subject.
fn fixture() -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut ttl = String::from("@prefix ex: <urn:> .\n");
    for i in 0..2_048 {
        ttl.push_str(&format!("ex:s{i} ex:v {} .\n", -1 - i));
        if i % 2 == 0 {
            ttl.push_str(&format!("ex:s{i} ex:rating {} .\n", i % 10));
        }
    }
    s.load(&[Source::from_bytes(
        ttl.into_bytes(),
        RdfFormat::Turtle,
        None,
    )])
    .unwrap();
    s
}

/// Rows and callback invocations of `text`, with `iri` registered as a callback that
/// counts its calls and answers true from its second call on.
fn run(s: &Store, text: &str, iri: &str, opt: Optimizations) -> (Vec<String>, usize, PlanInfo) {
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let tick = move |_: &ScalarContext<'_>, _: &[oxrdf::Term]| -> ScalarResult {
        let n = count.fetch_add(1, Ordering::SeqCst);
        Ok(oxrdf::Literal::from(n > 0).into())
    };
    let mut registry = ExtensionRegistry::builder();
    registry
        .register_scalar(ScalarDescriptor::new(iri, 0..=1, tick).unwrap())
        .unwrap();
    let r = query(
        s.snapshot(),
        text,
        &QueryOptions {
            no_cache: true,
            extensions: Some(registry.build()),
            optimizations: Some(opt),
            ..Default::default()
        },
    )
    .unwrap();
    let rows = r
        .rows()
        .unwrap()
        .into_iter()
        .map(|row| format!("{row:?}"))
        .collect();
    (rows, calls.load(Ordering::SeqCst), r.plan().clone())
}

fn has_desc(p: &PlanInfo, needle: &str) -> bool {
    p.description.contains(needle) || p.children.iter().any(|c| has_desc(c, needle))
}

#[test]
fn a_callback_before_a_comparison_runs_on_every_row() {
    let s = fixture();
    let text = "SELECT ?s WHERE { ?s <urn:v> ?v BIND(?v AS ?w) FILTER(<urn:tick>(?w) && ?w > 0) }";
    let fast = run(&s, text, "urn:tick", Optimizations::ALL);
    let generic = run(
        &s,
        text,
        "urn:tick",
        Optimizations {
            range_pushdown: false,
            ..Optimizations::ALL
        },
    );
    assert_eq!(generic.1, 2_048);
    assert_eq!((&fast.0, fast.1), (&generic.0, generic.1));
    assert!(!has_desc(&fast.2, "decided on ids"));
}

#[test]
fn an_optional_with_a_callback_filter_reads_its_whole_pattern() {
    let s = fixture();
    let text = "SELECT ?r ?x WHERE { VALUES ?r { <urn:s0> } OPTIONAL { ?r <urn:rating> ?x FILTER(<urn:tick>(?x)) } }";
    let fast = run(&s, text, "urn:tick", Optimizations::ALL);
    let generic = run(
        &s,
        text,
        "urn:tick",
        Optimizations {
            probed_optional: false,
            ..Optimizations::ALL
        },
    );
    assert_eq!((&fast.0, fast.1), (&generic.0, generic.1));
    assert!(!has_desc(&fast.2, "probed for the left side's keys"));
}

#[test]
fn an_order_key_with_a_callback_in_the_xsd_namespace_is_evaluated_again() {
    let s = fixture();
    let text = "PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> SELECT ?s ?a WHERE { ?s <urn:v> ?v BIND(xsd:tick(?v) AS ?a) } ORDER BY xsd:tick(?v) ?s LIMIT 3";
    let fast = run(
        &s,
        text,
        "http://www.w3.org/2001/XMLSchema#tick",
        Optimizations::ALL,
    );
    assert!(!has_desc(&fast.2, "(?a)"), "{:#?}", fast.2);
    let generic = run(
        &s,
        text,
        "http://www.w3.org/2001/XMLSchema#tick",
        Optimizations {
            bound_order_keys: false,
            ..Optimizations::ALL
        },
    );
    assert_eq!(fast.1, generic.1);
}
