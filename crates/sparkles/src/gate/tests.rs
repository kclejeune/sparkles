use super::*;
use crate::Dataset;
use crate::store::Snapshot;
use oxrdfio::RdfFormat;
use std::sync::Arc;

const PREFIXES: &str = "@prefix chk: <urn:x-sparkles:checks#> .\n\
    @prefix sh: <http://www.w3.org/ns/shacl#> .\n\
    @prefix rdfs: <http://www.w3.org/2000/01/rdf-schema#> .\n";

fn suite(body: &str) -> Result<Suite, SuiteError> {
    Suite::parse(&format!("{PREFIXES}{body}"), RdfFormat::Turtle)
}

fn refused(body: &str, needle: &str) {
    let e = suite(body).expect_err("the suite is refused");
    assert!(e.0.contains(needle), "{e} should mention {needle:?}");
}

#[test]
fn a_suite_parses_with_its_checks_in_iri_order() {
    let s = suite(
        "<urn:s> a chk:Suite ; rdfs:label \"Gate\" ; chk:check <urn:s#b>, <urn:s#a> .
         <urn:s#a> a chk:QueryCheck ; chk:severity sh:Warning ; chk:query \"ASK {}\" .
         <urn:s#b> a chk:ShaclCheck ; chk:shapesGraph <urn:shapes> .",
    )
    .unwrap();
    assert_eq!(s.iri().as_str(), "urn:s");
    assert_eq!(s.label(), Some("Gate"));
    let iris: Vec<_> = s.checks().iter().map(|c| c.iri.as_str()).collect();
    assert_eq!(iris, ["urn:s#a", "urn:s#b"]);
    assert_eq!(s.checks()[0].severity, Severity::Warning);
    assert_eq!(s.checks()[1].severity, Severity::Violation);
    assert!(s.revision().starts_with("sha256:"));
    // the same triples in another order and syntax have the same revision
    let nt = Suite::parse(s.canonical(), RdfFormat::NTriples).unwrap();
    assert_eq!(nt.revision(), s.revision());
    assert_eq!(nt, s);
}

#[test]
fn malformed_suites_are_refused() {
    refused("[] a chk:Suite .", "blank nodes");
    refused(
        "<urn:s> a chk:Suite ; chk:check [ a chk:QueryCheck ; chk:query \"ASK {}\" ] .",
        "blank nodes",
    );
    refused("<urn:x> rdfs:label \"x\" .", "no chk:Suite");
    refused(
        "<urn:a> a chk:Suite . <urn:b> a chk:Suite .",
        "more than one",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> . <urn:c> a chk:LintCheck .",
        "unknown check type",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" ; chk:severity sh:Fatal .",
        "chk:severity",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShaclCheck ; chk:shapesGraph <urn:g> ; chk:shapes \"\" .",
        "one of chk:shapesGraph and chk:shapes",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:storedQuery \"q\" .",
        "chk:storedQueryVersion",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" ; chk:schema \"x\" .",
        "does not take chk:schema",
    );
    refused(
        "<urn:s> a chk:Suite . <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .",
        "does not link",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .",
        "is not described",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> . <urn:c> rdfs:label \"c\" .",
        "needs a check type",
    );
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\", \"ASK { ?s ?p ?o }\" .",
        "has one value",
    );
    refused("<urn:s> a chk:Suite ; chk:frequency 3 .", "does not take");
    #[cfg(feature = "reasoning")]
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:DiagnosticsCheck ; chk:diagnostic \"no-such-check\" .",
        "unknown diagnostic",
    );
}

/// The heads of `ds` before and after the update `u`.
fn states(ds: &Dataset, setup: &str, u: &str) -> (Arc<Snapshot>, Arc<Snapshot>) {
    if !setup.is_empty() {
        ds.update(setup).unwrap();
    }
    let before = ds.store().snapshot();
    ds.update(u).unwrap();
    let after = ds.store().snapshot();
    (before, after)
}

fn run_on(s: &Suite, before: &Arc<Snapshot>, after: &Arc<Snapshot>) -> SuiteReport {
    run(
        s,
        &Inputs {
            before,
            after,
            stored: None,
            timeout: None,
        },
    )
}

#[test]
fn a_query_check_fails_on_solutions_or_true() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> a <urn:Orphan> }");
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:s#select>, <urn:s#ask>, <urn:s#warn>, <urn:s#empty> .
         <urn:s#select> a chk:QueryCheck ; chk:query \"SELECT ?s { ?s a <urn:Orphan> }\" .
         <urn:s#ask> a chk:QueryCheck ; chk:query \"ASK { ?s a <urn:Orphan> }\" .
         <urn:s#warn> a chk:QueryCheck ; chk:severity sh:Warning ;
            chk:query \"SELECT ?s { ?s a <urn:Orphan> }\" .
         <urn:s#empty> a chk:QueryCheck ; chk:query \"SELECT ?s { ?s a <urn:None> }\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    let by = |iri: &str| r.results.iter().find(|x| x.check.as_str() == iri).unwrap();
    assert_eq!(by("urn:s#select").status, Status::Failed);
    assert_eq!(by("urn:s#select").findings, 1);
    assert_eq!(by("urn:s#ask").status, Status::Failed);
    assert_eq!(by("urn:s#warn").status, Status::Failed);
    assert!(!by("urn:s#warn").blocks());
    assert_eq!(by("urn:s#empty").status, Status::Passed);
    assert!(!r.passed());
    let failed: Vec<_> = r.failed().map(|x| x.check.as_str()).collect();
    assert_eq!(failed, ["urn:s#ask", "urn:s#select"]);
    // the state before the change passes
    assert!(run_on(&s, &before, &before).passed());
}

#[test]
fn an_invalid_query_is_an_error_that_blocks() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> <urn:p> 1 }");
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"SELEKT\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Error);
    assert_eq!(r.results[0].code, Some("input-invalid"));
    assert!(!r.passed());
}

#[test]
fn a_stored_query_check_runs_the_pinned_version_only() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> a <urn:Orphan> }");
    let catalog = crate::stored::Catalog::open(None).unwrap();
    let def = |q: &str| -> crate::stored::Definition {
        serde_json::from_value(serde_json::json!({ "query": q })).unwrap()
    };
    let text = "SELECT ?s { ?s a <urn:Orphan> }";
    catalog
        .put("orphans", def(text), Default::default())
        .unwrap();
    let digest = sparkles_core::managed::settings_digest(text.as_bytes());
    let s = suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:c>, <urn:d>, <urn:e> .
         <urn:c> a chk:QueryCheck ; chk:storedQuery \"orphans\" ; chk:storedQueryVersion 1 ;
            chk:storedQueryDigest \"{digest}\" .
         <urn:d> a chk:QueryCheck ; chk:storedQuery \"orphans\" ; chk:storedQueryVersion 2 ;
            chk:storedQueryDigest \"{digest}\" .
         <urn:e> a chk:QueryCheck ; chk:storedQuery \"orphans\" ; chk:storedQueryVersion 1 ;
            chk:storedQueryDigest \"sha256:{zeros}\" .",
        zeros = "0".repeat(64)
    ))
    .unwrap();
    let r = run(
        &s,
        &Inputs {
            before: &before,
            after: &after,
            stored: Some(&catalog),
            timeout: None,
        },
    );
    assert_eq!(r.results[0].status, Status::Failed);
    assert_eq!(r.results[1].code, Some("input-missing"));
    assert_eq!(r.results[2].code, Some("input-invalid"));
    // without the catalog a stored query cannot run, and that blocks
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].code, Some("input-missing"));
}

#[cfg(feature = "shacl")]
#[test]
fn shacl_checks_read_their_shapes_from_the_state_before() {
    let ds = Dataset::memory();
    let shapes = "INSERT DATA { GRAPH <urn:shapes> {
        <urn:PersonShape> a <http://www.w3.org/ns/shacl#NodeShape> ;
          <http://www.w3.org/ns/shacl#targetClass> <urn:Person> ;
          <http://www.w3.org/ns/shacl#property> <urn:NameShape> .
        <urn:NameShape> <http://www.w3.org/ns/shacl#path> <urn:name> ;
          <http://www.w3.org/ns/shacl#minCount> 1 } }";
    // the change adds a nameless person and drops the shapes
    let (before, after) = states(
        &ds,
        shapes,
        "INSERT DATA { <urn:bob> a <urn:Person> } ; DROP GRAPH <urn:shapes>",
    );
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c>, <urn:w> .
         <urn:c> a chk:ShaclCheck ; chk:shapesGraph <urn:shapes> .
         <urn:w> a chk:ShaclCheck ; chk:severity sh:Info ;
            chk:shapes \"\"\"<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> ;
              <http://www.w3.org/ns/shacl#targetNode> <urn:bob> ;
              <http://www.w3.org/ns/shacl#class> <urn:Robot> .\"\"\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert_eq!(r.results[0].findings, 1);
    assert!(!r.results[0].messages.is_empty());
    assert_eq!(r.results[1].status, Status::Failed);
    assert!(!r.passed());
    // a shapes graph missing from the state before fails closed
    let r = run_on(&s, &after, &after);
    assert_eq!(r.results[0].code, Some("input-missing"));
    assert!(!r.passed());
}

#[cfg(feature = "shex")]
#[test]
fn a_shex_check_fails_on_a_nonconformant_node() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:bob> <urn:age> \"old\" }");
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShexCheck ;
            chk:schema \"\"\"PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
              <urn:P> { <urn:age> xsd:integer }\"\"\" ;
            chk:shapeMap \"<urn:bob>@<urn:P>\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert!(!r.passed());
}

#[cfg(feature = "reasoning")]
#[test]
fn diagnostics_checks_fail_on_findings() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        "INSERT DATA { <urn:A> <http://www.w3.org/2002/07/owl#disjointWith> <urn:B> }",
        "INSERT DATA { <urn:x> a <urn:A>, <urn:B> .
            <urn:C> <http://www.w3.org/2000/01/rdf-schema#subClassOf>
              <http://www.w3.org/2002/07/owl#Nothing> }",
    );
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:d> .
         <urn:d> a chk:DiagnosticsCheck ; chk:diagnostic \"disjoint-classes\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert!(run_on(&s, &before, &before).passed());
}

#[test]
fn a_check_the_build_cannot_run_blocks() {
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShexCheck ; chk:schema \"<urn:P> {}\" ; chk:shapeMap \"<urn:x>@<urn:P>\" .",
    )
    .unwrap();
    assert_eq!(s.unavailable().is_empty(), cfg!(feature = "shex"));
    if !cfg!(feature = "shex") {
        let ds = Dataset::memory();
        let snap = ds.store().snapshot();
        let r = run_on(&s, &snap, &snap);
        assert_eq!(r.results[0].code, Some("check-unavailable"));
        assert!(!r.passed());
    }
}

#[test]
fn independently_ordered_documents_have_one_canonical_form() {
    let a = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:s#a>, <urn:s#b> .
         <urn:s#a> a chk:QueryCheck ; chk:query \"ASK {}\" .
         <urn:s#b> a chk:QueryCheck ; chk:severity sh:Info ; chk:query \"ASK { ?s ?p ?o }\" .",
    )
    .unwrap();
    let b = suite(
        "<urn:s#b> chk:query \"ASK { ?s ?p ?o }\" ; chk:severity sh:Info ; a chk:QueryCheck .
         <urn:s#a> chk:query \"ASK {}\" ; a chk:QueryCheck .
         <urn:s> chk:check <urn:s#b>, <urn:s#a> ; a chk:Suite .",
    )
    .unwrap();
    assert_eq!(a.canonical(), b.canonical());
    assert_eq!(a.revision(), b.revision());
    let order = |s: &Suite| -> Vec<String> {
        s.checks()
            .iter()
            .map(|c| c.iri.as_str().to_string())
            .collect()
    };
    assert_eq!(order(&a), order(&b));
    let lines: Vec<&str> = a.canonical().lines().collect();
    let mut sorted = lines.clone();
    sorted.sort_unstable();
    assert_eq!(lines, sorted);
}

#[test]
fn the_suite_node_has_one_type_of_the_vocabulary() {
    refused(
        "<urn:s> a chk:Suite, chk:FutureType ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .",
        "chk:Suite",
    );
    refused(
        "<urn:s> a chk:Suite, chk:QueryCheck ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .",
        "chk:Suite",
    );
    // a type of another vocabulary is metadata, and a repeated triple is one triple
    suite(
        "<urn:s> a chk:Suite, <urn:other#Policy> ; chk:check <urn:c> .
         <urn:s> a chk:Suite .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .",
    )
    .unwrap();
}

#[test]
fn blank_nodes_inside_triple_terms_are_refused() {
    let nested = "<urn:s> a chk:Suite ; rdfs:comment <<( _:a <urn:p> <urn:o> )>> .";
    let deep =
        "<urn:s> a chk:Suite ; rdfs:comment <<( <urn:x> <urn:p> <<( <urn:y> <urn:q> _:b )>> )>> .";
    for doc in [nested, deep] {
        let text = format!("{PREFIXES}{doc}");
        // the RDF parser accepts the triple term, and the suite refuses it
        let parsed: Result<Vec<_>, _> = oxrdfio::RdfParser::from_format(RdfFormat::Turtle)
            .for_slice(text.as_bytes())
            .collect();
        assert!(parsed.is_ok(), "{parsed:?}");
        refused(doc, "blank nodes");
    }
    let ground =
        suite("<urn:s> a chk:Suite ; rdfs:comment <<( <urn:x> <urn:p> <urn:o> )>> .").unwrap();
    let again = Suite::parse(ground.canonical(), RdfFormat::NTriples).unwrap();
    assert_eq!(again.revision(), ground.revision());
}

#[test]
fn repeated_triples_count_once() {
    let one = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .",
    )
    .unwrap();
    let twice = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c>, <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\" .
         <urn:c> chk:query \"ASK {}\" .",
    )
    .unwrap();
    assert_eq!(one, twice);
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK {}\", \"ASK { ?s ?p ?o }\" .",
        "has one value",
    );
}

#[cfg(feature = "reasoning")]
#[test]
fn a_repeated_diagnostic_is_one_diagnostic() {
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:DiagnosticsCheck ; chk:diagnostic \"disjoint-classes\" .
         <urn:c> chk:diagnostic \"disjoint-classes\" .",
    )
    .unwrap();
    let again = Suite::parse(s.canonical(), RdfFormat::NTriples).unwrap();
    assert_eq!(s, again);
    match &s.checks()[0].kind {
        CheckKind::Diagnostics { diagnostics, .. } => assert_eq!(diagnostics.len(), 1),
        k => panic!("{k:?}"),
    }
}

/// A compact suite whose labels expand to about `n` times a 2 KiB IRI.
fn long_iris(n: usize) -> String {
    let base = format!("urn:x:{}", "a".repeat(2048));
    let mut doc = format!("@prefix l: <{base}> .\n{PREFIXES}<urn:s> a chk:Suite ; rdfs:seeAlso ");
    let objects: Vec<String> = (0..n).map(|i| format!("l:{i}")).collect();
    doc.push_str(&objects.join(", "));
    doc.push_str(" .\n");
    doc
}

#[test]
fn the_canonical_form_is_bounded_too() {
    let big = long_iris(600);
    assert!(big.len() < MAX_SUITE_BYTES);
    let e = Suite::parse(&big, RdfFormat::Turtle).expect_err("too large once expanded");
    assert!(e.0.contains("N-Triples"), "{e}");
    let small = Suite::parse(&long_iris(300), RdfFormat::Turtle).unwrap();
    assert!(small.canonical().len() <= MAX_SUITE_BYTES);
    let again = Suite::parse(small.canonical(), RdfFormat::NTriples).unwrap();
    assert_eq!(again.revision(), small.revision());
}

#[test]
fn stored_query_digests_have_the_digest_form() {
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:storedQuery \"q\" ; chk:storedQueryVersion 1 ;
            chk:storedQueryDigest \"sha256:00\" .",
        "chk:storedQueryDigest",
    );
}

#[test]
fn a_stored_query_without_a_digest_does_not_run() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> <urn:p> 1 }");
    let catalog = crate::stored::Catalog::open(None).unwrap();
    let def = |q: &str| -> crate::stored::Definition {
        serde_json::from_value(serde_json::json!({ "query": q })).unwrap()
    };
    catalog
        .put(
            "q",
            def("SELECT ?s { ?s a <urn:None> }"),
            Default::default(),
        )
        .unwrap();
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:storedQuery \"q\" ; chk:storedQueryVersion 1 .",
    )
    .unwrap();
    let inputs = Inputs {
        before: &before,
        after: &after,
        stored: Some(&catalog),
        timeout: None,
    };
    let r = run(&s, &inputs);
    assert_eq!(
        r.results[0].code,
        Some("input-invalid"),
        "{:?}",
        r.results[0]
    );
    assert!(!r.passed());

    // a query deleted and created again restarts at version 1, and the digest tells them apart
    let a = "SELECT ?s { ?s <urn:p> ?o }";
    let b = "SELECT ?s { ?s a <urn:None> }";
    catalog.delete("q", None).unwrap();
    catalog.put("q", def(a), Default::default()).unwrap();
    let pinned = |text: &str| {
        let digest = sparkles_core::managed::settings_digest(text.as_bytes());
        suite(&format!(
            "<urn:s> a chk:Suite ; chk:check <urn:c> .
             <urn:c> a chk:QueryCheck ; chk:storedQuery \"q\" ; chk:storedQueryVersion 1 ;
                chk:storedQueryDigest \"{digest}\" ."
        ))
        .unwrap()
    };
    let (sa, sb) = (pinned(a), pinned(b));
    assert_ne!(sa.revision(), sb.revision());
    assert_eq!(run(&sa, &inputs).results[0].status, Status::Failed);
    catalog.delete("q", None).unwrap();
    catalog.put("q", def(b), Default::default()).unwrap();
    assert_eq!(run(&sa, &inputs).results[0].code, Some("input-invalid"));
    assert!(run(&sb, &inputs).passed());
}

#[cfg(feature = "shacl")]
const SH_NS: &str = "http://www.w3.org/ns/shacl#";

/// Shapes that need every `urn:Person` to have a `urn:name`.
#[cfg(feature = "shacl")]
fn person_shapes() -> String {
    format!(
        "INSERT DATA {{ GRAPH <urn:shapes> {{
            <urn:PersonShape> a <{SH_NS}NodeShape> ; <{SH_NS}targetClass> <urn:Person> ;
              <{SH_NS}property> <urn:NameShape> .
            <urn:NameShape> <{SH_NS}path> <urn:name> ; <{SH_NS}minCount> 1 }} }}"
    )
}

fn shacl_suite(data_graph: &str) -> Result<Suite, SuiteError> {
    suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShaclCheck ; chk:shapesGraph <urn:shapes> {data_graph} ."
    ))
}

#[cfg(feature = "shacl")]
#[test]
fn a_shacl_check_selects_named_graphs_as_the_write_guard_does() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        &person_shapes(),
        "INSERT DATA { <urn:x> <urn:p> <urn:y> . GRAPH <urn:g> { <urn:bob> a <urn:Person> } }",
    );
    let status = |s: &Suite| run_on(s, &before, &after).results[0].clone();
    // the named graph, and every graph, see the nameless person
    for sel in [
        "; chk:dataGraph <urn:g>",
        "; chk:dataGraph chk:UnionGraph",
        "; chk:dataGraph <urn:g>, chk:DefaultGraph",
    ] {
        let r = status(&shacl_suite(sel).unwrap());
        assert_eq!(r.status, Status::Failed, "{sel}: {r:?}");
    }
    // without a selection the check reads the store's default graph, as the guard does
    assert_eq!(status(&shacl_suite("").unwrap()).status, Status::Passed);
    // a selected graph that holds no data in the candidate fails closed
    let r = status(&shacl_suite("; chk:dataGraph <urn:g>, <urn:absent>").unwrap());
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
    assert!(r.blocks());
}

#[cfg(feature = "shacl")]
#[test]
fn a_shacl_check_leaves_out_the_inferred_graph_unless_asked() {
    let ds = Dataset::memory();
    let inferred = sparkles_core::INFERRED_GRAPH;
    let (before, after) = states(
        &ds,
        &person_shapes(),
        &format!("INSERT DATA {{ GRAPH <{inferred}> {{ <urn:bob> a <urn:Person> }} }}"),
    );
    let union = shacl_suite("; chk:dataGraph chk:UnionGraph").unwrap();
    assert_eq!(
        run_on(&union, &before, &after).results[0].status,
        Status::Passed
    );
    let with = shacl_suite("; chk:dataGraph chk:UnionGraph ; chk:inferences true").unwrap();
    assert_eq!(
        run_on(&with, &before, &after).results[0].status,
        Status::Failed
    );
}

#[test]
fn shacl_data_graph_selections_that_hide_data_are_refused() {
    let refused_sel = |sel: &str, needle: &str| {
        let e = shacl_suite(sel).expect_err(sel);
        assert!(e.0.contains(needle), "{e} should mention {needle:?}");
    };
    refused_sel("; chk:dataGraph <urn:shapes>", "shapes graph");
    refused_sel("; chk:dataGraph <urn:g>, <urn:shapes>", "shapes graph");
    refused_sel(
        &format!("; chk:dataGraph <{}>", sparkles_core::INFERRED_GRAPH),
        "chk:inferences",
    );
    refused_sel("; chk:dataGraph <urn:x-arq:UnionGraph>", "chk:UnionGraph");
    refused_sel(
        "; chk:dataGraph <urn:x-sparkles:checks:runs>",
        "does not take",
    );
    refused_sel("; chk:dataGraph chk:UnionGraph, <urn:g>", "no other");
    let s = shacl_suite("; chk:dataGraph <urn:b>, <urn:a>").unwrap();
    match &s.checks()[0].kind {
        CheckKind::Shacl { data_graph, .. } => assert_eq!(
            *data_graph,
            DataGraph::Graphs(vec!["urn:a".into(), "urn:b".into()])
        ),
        k => panic!("{k:?}"),
    }
}

#[test]
fn a_check_past_its_bound_never_passes() {
    let ds = Dataset::memory();
    let snap = ds.store().snapshot();
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query \"ASK { <urn:none> ?p ?o }\" .",
    )
    .unwrap();
    let r = run(
        &s,
        &Inputs {
            before: &snap,
            after: &snap,
            stored: None,
            timeout: Some(std::time::Duration::ZERO),
        },
    );
    assert_eq!(
        r.results[0].code,
        Some("check-timeout"),
        "{:?}",
        r.results[0]
    );
    assert!(!r.passed());
}

#[cfg(feature = "ontology")]
const OWL: &str = "http://www.w3.org/2002/07/owl#";
#[cfg(feature = "ontology")]
const RDFS: &str = "http://www.w3.org/2000/01/rdf-schema#";

#[cfg(feature = "ontology")]
#[test]
fn the_el_classifier_finds_a_class_under_two_disjoint_classes() {
    let ds = Dataset::memory();
    // no asserted path leads to owl:Nothing, so the OWL RL diagnostic does not see it
    let (before, after) = states(
        &ds,
        &format!("INSERT DATA {{ <urn:A> <{OWL}disjointWith> <urn:B> }}"),
        &format!(
            "INSERT DATA {{ <urn:C> <{RDFS}subClassOf> <urn:A>, <urn:B> .
               GRAPH <urn:onto> {{ <urn:D> <{RDFS}subClassOf> <urn:E> }} }}"
        ),
    );
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:u>, <urn:v>, <urn:w> .
         <urn:u> a chk:UnsatisfiableClassesCheck .
         <urn:v> a chk:UnsatisfiableClassesCheck ; chk:graph <urn:onto> .
         <urn:w> a chk:UnsatisfiableClassesCheck ; chk:graph <urn:absent> .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert_eq!(r.results[0].findings, 1);
    assert!(
        r.results[0].messages[0].contains("urn:C"),
        "{:?}",
        r.results[0]
    );
    assert_eq!(r.results[1].status, Status::Passed, "{:?}", r.results[1]);
    assert_eq!(
        r.results[2].code,
        Some("input-missing"),
        "{:?}",
        r.results[2]
    );
    assert!(run_on(&s, &before, &before).results[0].status == Status::Passed);
}

#[test]
fn the_unsatisfiable_classes_check_needs_the_ontology_feature() {
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:u> . <urn:u> a chk:UnsatisfiableClassesCheck .",
    )
    .unwrap();
    assert_eq!(s.unavailable().is_empty(), cfg!(feature = "ontology"));
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:u> .
         <urn:u> a chk:UnsatisfiableClassesCheck ; chk:inferences true .",
        "does not take chk:inferences",
    );
    if !cfg!(feature = "ontology") {
        let ds = Dataset::memory();
        let snap = ds.store().snapshot();
        let r = run_on(&s, &snap, &snap);
        assert_eq!(r.results[0].code, Some("check-unavailable"));
        assert!(!r.passed());
    }
}

// ---- round 2 of the slice 1 review ---------------------------------------------------

#[test]
fn a_shapes_graph_is_not_a_reserved_graph() {
    for g in [
        "<urn:x-arq:DefaultGraph>",
        "<urn:x-arq:UnionGraph>",
        "<urn:x-sparkles:inferred>",
        "<urn:x-sparkles:checks:runs>",
        "chk:DefaultGraph",
    ] {
        for data in ["", "; chk:dataGraph chk:DefaultGraph"] {
            refused(
                &format!(
                    "<urn:s> a chk:Suite ; chk:check <urn:c> .
                     <urn:c> a chk:ShaclCheck ; chk:shapesGraph {g} {data} ."
                ),
                "chk:shapesGraph names a graph that holds the shapes",
            );
        }
    }
}

#[cfg(feature = "shacl")]
#[test]
fn named_shapes_validate_the_default_graph() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        &person_shapes(),
        "INSERT DATA { <urn:bob> a <urn:Person> }",
    );
    let s = shacl_suite("; chk:dataGraph chk:DefaultGraph").unwrap();
    let r = run_on(&s, &before, &after).results[0].clone();
    assert_eq!(r.status, Status::Failed, "{r:?}");
    // a selected default graph that holds nothing fails closed, as a named graph does
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        &person_shapes(),
        "INSERT DATA { GRAPH <urn:g> { <urn:bob> a <urn:Person> } }",
    );
    let r = run_on(&s, &before, &after).results[0].clone();
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
}

#[cfg(feature = "shacl")]
#[test]
fn shapes_that_target_nothing_are_refused() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:bob> a <urn:Person> }");
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShaclCheck ; chk:shapes \"<urn:S> a <http://www.w3.org/ns/shacl#NodeShape> ; <http://www.w3.org/ns/shacl#closed> true .\" .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after).results[0].clone();
    assert_eq!(r.code, Some("input-invalid"), "{r:?}");
    assert!(r.blocks());
}

/// The suite of one unsatisfiable-classes check, with `extra` on the check.
#[cfg(feature = "ontology")]
fn unsat_suite(extra: &str) -> Suite {
    suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:u> .
         <urn:u> a chk:UnsatisfiableClassesCheck {extra} ."
    ))
    .unwrap()
}

/// The result of `s` on a dataset after the SPARQL update `u`.
#[cfg(feature = "ontology")]
fn unsat_after(s: &Suite, u: &str) -> CheckResult {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", u);
    run_on(s, &before, &after).results[0].clone()
}

/// `A` and `B` disjoint, `C` under `A`, `B` the domain of `r`, and `C` under the existential
/// `filler`, so that `C` is unsatisfiable once the existential is read.
#[cfg(feature = "ontology")]
fn domain_ontology(filler: &str) -> String {
    format!(
        "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}>
         INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> . <urn:C> rdfs:subClassOf <urn:A> .
           <urn:r> a owl:ObjectProperty ; rdfs:domain <urn:B> .
           <urn:C> rdfs:subClassOf _:e0 . {filler} }}"
    )
}

/// A chain of `n` existential restrictions on `r` from `_:e0`, ending in `urn:D`.
#[cfg(feature = "ontology")]
fn existentials(n: usize) -> String {
    let mut t = String::new();
    for i in 0..n {
        let next = if i + 1 == n {
            "<urn:D>".to_string()
        } else {
            format!("_:e{}", i + 1)
        };
        t.push_str(&format!(
            "_:e{i} a owl:Restriction ; owl:onProperty <urn:r> ; owl:someValuesFrom {next} .\n"
        ));
    }
    t
}

#[cfg(feature = "ontology")]
#[test]
fn an_owl_reader_error_is_not_an_axiom_left_out() {
    let s = unsat_suite("");
    // the readable control: membership in the existential puts C under B too
    let r = unsat_after(&s, &domain_ontology(&existentials(1)));
    assert_eq!(r.status, Status::Failed, "{r:?}");
    assert!(r.messages.iter().any(|m| m.contains("urn:C")), "{r:?}");
    // the same axiom nested past the reader's limit is an error, not a pass
    let r = unsat_after(
        &s,
        &domain_ontology(&existentials(sparkles_owl::read::MAX_NESTING + 2)),
    );
    assert_eq!(r.status, Status::Error, "{r:?}");
    assert_eq!(r.code, Some("input-invalid"), "{r:?}");
    // a restriction without its filler leaves its subclass axiom unmapped
    let r = unsat_after(
        &s,
        &domain_ontology("_:e0 a owl:Restriction ; owl:onProperty <urn:r> ."),
    );
    assert_eq!(r.code, Some("input-invalid"), "{r:?}");
    assert!(r.messages[0].contains("maps to no axiom"), "{r:?}");
}

#[cfg(feature = "ontology")]
#[test]
fn the_check_classifies_the_terminology_and_not_the_assertions() {
    let s = unsat_suite("");
    // one individual in two disjoint classes, as plain, annotated and class-expression
    // assertions: the terminology alone is consistent, so each form gives the same verdict
    for abox in [
        "<urn:x> a <urn:A>, <urn:B> .",
        "<urn:x> a <urn:A>, <urn:B> .
         [] a owl:Axiom ; owl:annotatedSource <urn:x> ;
            owl:annotatedProperty <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> ;
            owl:annotatedTarget <urn:B> ; rdfs:comment \"c\" .",
        "<urn:x> a [ a owl:Class ; owl:intersectionOf ( <urn:A> <urn:B> ) ] .",
    ] {
        let u = format!(
            "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}>
             INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> . {abox} }}"
        );
        let r = unsat_after(&s, &u);
        assert_eq!(r.status, Status::Passed, "{abox}: {r:?}");
        assert!(r.notes.is_empty(), "{abox}: {r:?}");
    }
}

#[cfg(all(feature = "ontology", feature = "reasoning"))]
#[test]
fn a_diagnostics_check_finds_an_individual_of_two_disjoint_classes() {
    let s = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:d> .
         <urn:d> a chk:DiagnosticsCheck .",
    )
    .unwrap();
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        "",
        &format!(
            "PREFIX owl: <{OWL}> INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> .
               <urn:x> a <urn:A>, <urn:B> }}"
        ),
    );
    let r = run_on(&s, &before, &after).results[0].clone();
    assert_eq!(r.status, Status::Failed, "{r:?}");
}

#[cfg(feature = "ontology")]
#[test]
fn axioms_left_out_are_a_warning_and_can_block() {
    // a nominal: whether C is satisfiable can depend on the assertions about a
    let u = format!(
        "PREFIX owl: <{OWL}> INSERT DATA {{ <urn:C> owl:equivalentClass [ a owl:Class ;
           owl:oneOf ( <urn:a> ) ] . <urn:D> owl:disjointWith <urn:C> . <urn:a> a <urn:D> }}"
    );
    let r = unsat_after(&unsat_suite(""), &u);
    assert_eq!(r.status, Status::Passed, "{r:?}");
    let note = r
        .notes
        .iter()
        .find(|n| n.code == "axioms-left-out")
        .expect("a note for the axioms left out");
    assert_eq!(note.severity, Severity::Warning);
    assert!(note.count >= 1);
    let r = unsat_after(&unsat_suite("; chk:requireComplete true"), &u);
    assert_eq!(r.code, Some("check-incomplete"), "{r:?}");
    assert!(r.blocks());
    // without nominals the assertions do not make the answer incomplete
    let r = unsat_after(
        &unsat_suite("; chk:requireComplete true"),
        &format!(
            "PREFIX owl: <{OWL}> INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> . <urn:x> a <urn:A> }}"
        ),
    );
    assert_eq!(r.status, Status::Passed, "{r:?}");
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:u> .
         <urn:u> a chk:UnsatisfiableClassesCheck ; chk:requireComplete \"yes\" .",
        "chk:requireComplete takes a boolean",
    );
}

#[cfg(feature = "ontology")]
#[test]
fn an_ontology_with_no_terminology_is_missing_input() {
    let s = unsat_suite("");
    // an empty default graph, and one with data but no axioms
    let ds = Dataset::memory();
    let snap = ds.store().snapshot();
    let r = run_on(&s, &snap, &snap).results[0].clone();
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
    let r = unsat_after(&s, "INSERT DATA { <urn:x> <urn:p> <urn:y> }");
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
}

#[cfg(feature = "ontology")]
#[test]
fn imports_resolve_to_asserted_graphs_only() {
    let s = unsat_suite("");
    let inferred = sparkles_core::INFERRED_GRAPH;
    let base = format!(
        "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}> INSERT DATA {{
           <urn:C> rdfs:subClassOf <urn:A>, <urn:B> ."
    );
    // a valid import supplies the disjointness that makes C unsatisfiable
    let r = unsat_after(
        &s,
        &format!(
            "{base} <urn:o> owl:imports <urn:dis> .
               GRAPH <urn:dis> {{ <urn:A> owl:disjointWith <urn:B> }} }}"
        ),
    );
    assert_eq!(r.status, Status::Failed, "{r:?}");
    // the same axiom in the materialized inferences is not read through an import
    let r = unsat_after(
        &s,
        &format!(
            "{base} <urn:o> owl:imports <{inferred}> .
               GRAPH <{inferred}> {{ <urn:A> owl:disjointWith <urn:B> }} }}"
        ),
    );
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
    // an import that resolves to no graph
    let r = unsat_after(&s, &format!("{base} <urn:o> owl:imports <urn:absent> }}"));
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
    // more imports than the bound
    let n = sparkles_reasoner::inputs::MAX_IMPORTS + 1;
    let mut u = base.clone();
    for i in 0..n {
        u.push_str(&format!(
            " <urn:o> owl:imports <urn:i{i}> . GRAPH <urn:i{i}> {{ <urn:X{i}> rdfs:subClassOf <urn:Y> }}"
        ));
    }
    u.push_str(" <urn:A> owl:disjointWith <urn:B> }");
    let r = unsat_after(&s, &u);
    assert_eq!(r.code, Some("input-missing"), "{r:?}");
    assert!(r.messages[0].contains("imports"), "{r:?}");
}

/// The candidate of `u` with one more quad whose object id decodes to no term.
#[cfg(feature = "ontology")]
fn with_undecodable_object(u: &str, s: &str, p: &str) -> Arc<Snapshot> {
    use crate::id::Id;
    use crate::index::Perm;
    let ds = Dataset::memory();
    ds.update(u).unwrap();
    let mut snap = (*ds.store().snapshot()).clone();
    let s = snap.lookup_iri(s).unwrap();
    let p = snap.lookup_iri(p).unwrap();
    let bad = [s, p, Id::vocab(1 << 40), Id::DEFAULT_GRAPH];
    for perm in [Perm::Gspo, Perm::Pos] {
        snap.delta.ins[perm.index()].insert(perm.to_key(&bad));
    }
    Arc::new(snap)
}

#[cfg(feature = "ontology")]
#[test]
fn an_undecodable_quad_is_an_error_not_a_quad_left_out() {
    let s = unsat_suite("");
    let ok = format!(
        "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}> INSERT DATA {{
           <urn:A> owl:disjointWith <urn:B> . <urn:C> rdfs:subClassOf <urn:A> .
           GRAPH <urn:other> {{ <urn:z> owl:imports <urn:elsewhere> }} }}"
    );
    for p in [format!("{OWL}imports"), format!("{RDFS}subClassOf")] {
        let snap = with_undecodable_object(&ok, "urn:C", &p);
        let r = run_on(&s, &snap, &snap).results[0].clone();
        assert_eq!(r.status, Status::Error, "{p}: {r:?}");
        assert!(r.blocks());
    }
}

#[test]
fn the_canonical_limit_stops_the_parser_before_the_rest() {
    // the expanded statements pass the limit before the malformed tail is reached
    let mut big = long_iris(600);
    big.push_str("<urn:broken> <urn:p> .\n");
    assert!(big.len() < MAX_SUITE_BYTES);
    let e = Suite::parse(&big, RdfFormat::Turtle).expect_err("too large once expanded");
    assert!(e.0.contains("N-Triples"), "{e}");
}

#[test]
fn the_accumulator_reads_no_triple_past_the_limit() {
    let iri = |i: usize| NamedNode::new_unchecked(format!("urn:x:{i:06}:{}", "a".repeat(4096)));
    let p = NamedNode::new_unchecked("http://www.w3.org/2000/01/rdf-schema#seeAlso");
    let s = NamedNode::new_unchecked("urn:s");
    let read = std::cell::Cell::new(0usize);
    // each line has the same length, newline included, and the first line past the
    // limit is the one that brings the total over it
    let line = format!("{} .", Triple::new(s.clone(), p.clone(), iri(0))).len() + 1;
    let last = MAX_SUITE_BYTES / line;
    let items = (0..).map(|i: usize| {
        read.set(read.get() + 1);
        assert!(
            i <= last,
            "the accumulator asked for a triple past the limit"
        );
        Ok(Triple::new(s.clone(), p.clone(), iri(i)))
    });
    let e = Suite::accumulate(items).expect_err("too large");
    assert!(e.0.contains("N-Triples"), "{e}");
    assert_eq!(read.get(), last + 1);
}

#[test]
fn the_canonical_limit_counts_every_byte() {
    // a suite padded so that its canonical N-Triples, final newline included, is exactly
    // the limit; a repeated triple adds nothing, and one more byte is refused
    let doc = |pad: usize, repeat: bool| {
        let mut d = format!(
            "<urn:s> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:x-sparkles:checks#Suite> .\n\
             <urn:s> <http://www.w3.org/2000/01/rdf-schema#comment> \"{}\" .\n",
            "a".repeat(pad)
        );
        if repeat {
            d.push_str("<urn:s> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:x-sparkles:checks#Suite> .\n");
        }
        d
    };
    let base = Suite::parse(&doc(0, false), RdfFormat::NTriples).unwrap();
    let pad = MAX_SUITE_BYTES - base.canonical().len();
    let exact = Suite::parse(&doc(pad, false), RdfFormat::NTriples).unwrap();
    assert_eq!(exact.canonical().len(), MAX_SUITE_BYTES);
    let triples = || {
        oxrdfio::RdfParser::from_format(RdfFormat::NTriples)
            .for_slice(doc(pad, true).as_bytes())
            .map(|q| q.unwrap())
            .map(|q| Triple::new(q.subject, q.predicate, q.object))
            .collect::<Vec<_>>()
    };
    assert!(Suite::from_triples(triples()).is_ok());
    // one byte more, counted in the canonical form; the raw bound is not reached
    let over = doc(pad + 1, false);
    assert!(over.len() <= MAX_SUITE_BYTES + 200);
    let e = Suite::from_triples(
        oxrdfio::RdfParser::from_format(RdfFormat::NTriples)
            .for_slice(over.as_bytes())
            .map(|q| q.unwrap())
            .map(|q| Triple::new(q.subject, q.predicate, q.object)),
    )
    .expect_err("one byte past the limit");
    assert!(e.0.contains("N-Triples"), "{e}");
}

// ---- round 3 of the slice 1 review ---------------------------------------------------

#[cfg(feature = "ontology")]
const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// `A` and `B` declared disjoint classes and `C` a declared class, with `body`.
#[cfg(feature = "ontology")]
fn disjoint_ontology(body: &str) -> String {
    format!(
        "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}> PREFIX rdf: <{RDF}>
         INSERT DATA {{ <urn:A> a owl:Class . <urn:B> a owl:Class . <urn:C> a owl:Class .
           <urn:A> owl:disjointWith <urn:B> . {body} }}"
    )
}

#[cfg(feature = "ontology")]
fn assert_invalid(r: &CheckResult, what: &str) {
    assert_eq!(r.status, Status::Error, "{what}: {r:?}");
    assert_eq!(r.code, Some("input-invalid"), "{what}: {r:?}");
    assert!(r.blocks(), "{what}: {r:?}");
}

#[cfg(feature = "ontology")]
#[test]
fn a_named_class_definition_is_read_or_refused() {
    let suites = [unsat_suite(""), unsat_suite("; chk:requireComplete true")];
    // the well-formed definition makes C the intersection of two disjoint classes
    let ok = disjoint_ontology("<urn:C> owl:intersectionOf ( <urn:A> <urn:B> ) .");
    let r = unsat_after(&suites[0], &ok);
    assert_eq!(r.status, Status::Failed, "{r:?}");
    assert!(r.messages.iter().any(|m| m.contains("urn:C")), "{r:?}");
    // the same list with its last rdf:rest missing loses the definition
    let broken = |p: &str| {
        disjoint_ontology(&format!(
            "<urn:C> owl:{p} _:l1 . _:l1 rdf:first <urn:A> ; rdf:rest _:l2 .
             _:l2 rdf:first <urn:B> ."
        ))
    };
    for s in &suites {
        for p in ["intersectionOf", "unionOf"] {
            assert_invalid(&unsat_after(s, &broken(p)), p);
        }
        let r = unsat_after(
            s,
            &disjoint_ontology(
                "<urn:C> owl:oneOf _:l1 . _:l1 rdf:first <urn:a> ; rdf:rest _:l2 .
                 _:l2 rdf:first <urn:b> .",
            ),
        );
        assert_invalid(&r, "oneOf");
        // a complement of a restriction without its filler
        let r = unsat_after(
            s,
            &disjoint_ontology(
                "<urn:r> a owl:ObjectProperty .
                 <urn:C> owl:complementOf [ a owl:Restriction ; owl:onProperty <urn:r> ] .",
            ),
        );
        assert_invalid(&r, "complementOf");
    }
    // the well-formed forms of the other three are read
    for body in [
        "<urn:C> owl:unionOf ( <urn:A> <urn:B> ) .",
        "<urn:C> owl:oneOf ( <urn:a> <urn:b> ) .",
        "<urn:C> owl:complementOf <urn:A> .",
    ] {
        let r = unsat_after(&suites[0], &disjoint_ontology(body));
        assert_ne!(r.status, Status::Error, "{body}: {r:?}");
    }
}

#[cfg(feature = "ontology")]
#[test]
fn a_structural_predicate_typed_as_an_annotation_property_is_refused() {
    let s = unsat_suite("");
    let axioms = "<urn:C> rdfs:subClassOf <urn:A>, <urn:B> .";
    let r = unsat_after(&s, &disjoint_ontology(axioms));
    assert_eq!(r.status, Status::Failed, "{r:?}");
    assert!(r.messages.iter().any(|m| m.contains("urn:C")), "{r:?}");
    // the subclass triples would become annotations and the disjointness alone would pass
    let r = unsat_after(
        &s,
        &disjoint_ontology(&format!(
            "rdfs:subClassOf a owl:AnnotationProperty . {axioms}"
        )),
    );
    assert_invalid(&r, "rdfs:subClassOf typed as an annotation property");
}

#[cfg(feature = "ontology")]
#[test]
fn a_class_disjoint_with_itself_is_not_dropped() {
    let s = unsat_suite("");
    for body in [
        "<urn:C> owl:disjointWith <urn:C> .",
        "[] a owl:AllDisjointClasses ; owl:members ( <urn:C> <urn:C> ) .",
    ] {
        let r = unsat_after(&s, &disjoint_ontology(body));
        // C is unsatisfiable: the check either finds it or refuses the input
        if r.status != Status::Failed {
            assert_invalid(&r, body);
        } else {
            assert!(
                r.messages.iter().any(|m| m.contains("urn:C")),
                "{body}: {r:?}"
            );
        }
    }
}

#[cfg(feature = "ontology")]
#[test]
fn a_terminology_repeated_in_two_graphs_reads_once() {
    let s = unsat_suite("; chk:graph <urn:g1>, <urn:g2>");
    // one request, so _:e0 is the same blank node in both graphs, as a graph copy leaves it
    let t = "<urn:A> owl:disjointWith <urn:B> . <urn:C> rdfs:subClassOf <urn:A>, _:e0 .
             <urn:r> a owl:ObjectProperty .
             _:e0 a owl:Restriction ; owl:onProperty <urn:r> ; owl:someValuesFrom <urn:B> .
             <urn:r> rdfs:domain <urn:B> .";
    let r = unsat_after(
        &s,
        &format!(
            "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}>
             INSERT DATA {{ GRAPH <urn:g1> {{ {t} }} GRAPH <urn:g2> {{ {t} }} }}"
        ),
    );
    assert_eq!(r.status, Status::Failed, "{r:?}");
    assert!(r.messages.iter().any(|m| m.contains("urn:C")), "{r:?}");
}

/// The result of the check on the consistent terminology `A owl:disjointWith B` with a
/// declared object property, and `abox` beside it.
#[cfg(feature = "ontology")]
fn with_abox(s: &Suite, abox: &str) -> CheckResult {
    unsat_after(
        s,
        &format!(
            "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}> PREFIX rdf: <{RDF}>
             INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> . <urn:r> a owl:ObjectProperty .
               {abox} }}"
        ),
    )
}

#[cfg(feature = "ontology")]
#[test]
fn an_assertion_outside_the_terminology_cannot_refuse_it() {
    let s = unsat_suite("");
    let alone = with_abox(&s, "");
    assert_eq!(alone.status, Status::Passed, "{alone:?}");
    let deep = existentials(sparkles_owl::read::MAX_NESTING + 2);
    for abox in [
        // a class assertion whose expression is past the reader's nesting limit
        format!("<urn:x> a _:e0 . {deep}"),
        // the same assertion annotated
        format!(
            "<urn:x> a _:e0 . {deep}
             [] a owl:Axiom ; owl:annotatedSource <urn:x> ; owl:annotatedProperty rdf:type ;
                owl:annotatedTarget _:e0 ; rdfs:comment \"c\" ."
        ),
    ] {
        let r = with_abox(&s, &abox);
        assert_eq!(r.status, alone.status, "{r:?}");
        assert_eq!(r.findings, alone.findings, "{r:?}");
        assert_eq!(r.notes, alone.notes, "{r:?}");
    }
    // a terminology axiom that shares the expression reads it, and so meets the error
    for s in [unsat_suite(""), unsat_suite("; chk:requireComplete true")] {
        let r = with_abox(
            &s,
            &format!("<urn:x> a _:e0 . <urn:C> rdfs:subClassOf _:e0 . {deep}"),
        );
        assert_invalid(&r, "shared expression");
    }
    // the readable control: a shared expression within the limit is read
    let r = with_abox(
        &s,
        &format!(
            "<urn:x> a _:e0 . <urn:C> rdfs:subClassOf _:e0 . {}",
            existentials(1)
        ),
    );
    assert_eq!(r.status, Status::Passed, "{r:?}");
}

#[cfg(feature = "ontology")]
#[test]
fn assertions_do_not_type_the_terminology() {
    let s = unsat_suite("");
    // r and D are undeclared, so the range axiom maps only if something types them
    let base = "<urn:r> rdfs:range <urn:D> .";
    let alone = with_abox_untyped(&s, base);
    for abox in [
        "<urn:x> a <urn:D> .",
        "<urn:x> a <urn:D> .
         [] a owl:Axiom ; owl:annotatedSource <urn:x> ; owl:annotatedProperty rdf:type ;
            owl:annotatedTarget <urn:D> ; rdfs:comment \"c\" .",
        "<urn:x> a [ a owl:Restriction ; owl:onProperty <urn:r> ;
            owl:someValuesFrom <urn:E> ] .",
        "[] a owl:NegativePropertyAssertion ; owl:sourceIndividual <urn:x> ;
            owl:assertionProperty <urn:r> ; owl:targetIndividual <urn:y> .",
    ] {
        let r = with_abox_untyped(&s, &format!("{base} {abox}"));
        assert!(
            r.status == alone.status && r.code == alone.code && r.findings == alone.findings,
            "{abox}: {r:?} against {alone:?}"
        );
    }
}

/// [`with_abox`] without the declared object property.
#[cfg(feature = "ontology")]
fn with_abox_untyped(s: &Suite, abox: &str) -> CheckResult {
    unsat_after(
        s,
        &format!(
            "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}> PREFIX rdf: <{RDF}>
             INSERT DATA {{ <urn:A> owl:disjointWith <urn:B> . {abox} }}"
        ),
    )
}

#[cfg(feature = "ontology")]
#[test]
fn an_axiom_the_classifier_leaves_out_is_a_warning_note() {
    // a has-value restriction with a literal of a datatype outside the classifier's map
    let left_out = "<urn:d> a owl:DatatypeProperty .
         <urn:A> rdfs:subClassOf [ a owl:Restriction ; owl:onProperty <urn:d> ;
            owl:hasValue \"x\"^^<urn:unknown-datatype> ] .";
    let u = |body: &str| {
        format!(
            "PREFIX owl: <{OWL}> PREFIX rdfs: <{RDFS}>
             INSERT DATA {{ <urn:A> a owl:Class . <urn:B> a owl:Class . {left_out} {body} }}"
        )
    };
    let report = |s: &Suite, body: &str| {
        let ds = Dataset::memory();
        let (before, after) = states(&ds, "", &u(body));
        run_on(s, &before, &after).to_json()
    };
    let result = |j: &serde_json::Value| j["results"][0].clone();
    // no finding, completeness not required: a pass with the warning note
    let j = report(&unsat_suite(""), "<urn:A> rdfs:subClassOf <urn:B> .");
    let r = result(&j);
    assert_eq!(r["status"], "passed", "{j}");
    assert_eq!(j["passed"], true, "{j}");
    assert_eq!(r["notes"][0]["code"], "axioms-left-out", "{j}");
    assert_eq!(r["notes"][0]["severity"], "warning", "{j}");
    assert_eq!(r["notes"][0]["count"], 1, "{j}");
    // completeness required and no finding: the gap blocks
    let j = report(
        &unsat_suite("; chk:requireComplete true"),
        "<urn:A> rdfs:subClassOf <urn:B> .",
    );
    let r = result(&j);
    assert_eq!(r["status"], "error", "{j}");
    assert_eq!(r["code"], "check-incomplete", "{j}");
    assert_eq!(r["findings"], 0, "{j}");
    assert_eq!(r["blocks"], true, "{j}");
    // a known violation stands as a failure, with the same note
    let j = report(
        &unsat_suite("; chk:requireComplete true"),
        "<urn:A> owl:disjointWith <urn:B> . <urn:C> rdfs:subClassOf <urn:A>, <urn:B> .",
    );
    let r = result(&j);
    assert_eq!(r["status"], "failed", "{j}");
    assert_eq!(r["findings"], 1, "{j}");
    assert_eq!(r["notes"][0]["code"], "axioms-left-out", "{j}");
    assert_eq!(r["notes"][0]["severity"], "warning", "{j}");
    assert_eq!(r["notes"][0]["count"], 1, "{j}");
}

#[cfg(feature = "shacl")]
#[test]
fn shapes_that_are_all_deactivated_are_refused() {
    let ds = Dataset::memory();
    let shapes = |deactivated: &str| {
        format!(
            "<urn:S> a <{SH_NS}NodeShape> ; <{SH_NS}targetNode> <urn:bob> ;
               <{SH_NS}deactivated> {deactivated} ;
               <{SH_NS}property> [ <{SH_NS}path> <urn:name> ; <{SH_NS}minCount> 1 ] ."
        )
    };
    let inline = |extra: &str| {
        suite(&format!(
            "<urn:s> a chk:Suite ; chk:check <urn:c> .
             <urn:c> a chk:ShaclCheck ; chk:shapes \"\"\"{extra}\"\"\" ."
        ))
        .unwrap()
    };
    let pinned = |text: &str| {
        let (before, after) = states(
            &ds,
            &format!("INSERT DATA {{ GRAPH <urn:shapes> {{ {text} }} }}"),
            "INSERT DATA { <urn:bob> a <urn:Person> }",
        );
        run_on(&shacl_suite("").unwrap(), &before, &after).results[0].clone()
    };
    let run_inline = |text: &str| {
        let ds = Dataset::memory();
        let (before, after) = states(&ds, "", "INSERT DATA { <urn:bob> a <urn:Person> }");
        run_on(&inline(text), &before, &after).results[0].clone()
    };
    for r in [run_inline(&shapes("true")), pinned(&shapes("true"))] {
        assert_eq!(r.code, Some("input-invalid"), "{r:?}");
        assert!(r.blocks());
    }
    // the active shape finds that Bob has no name
    let r = run_inline(&shapes("false"));
    assert_eq!(r.status, Status::Failed, "{r:?}");
    // an active shape whose target matches nothing beside an inactive one is a valid pass
    let r = run_inline(&format!(
        "{} <urn:T> a <{SH_NS}NodeShape> ; <{SH_NS}targetClass> <urn:Nobody> ;
           <{SH_NS}property> [ <{SH_NS}path> <urn:name> ; <{SH_NS}minCount> 1 ] .",
        shapes("true")
    ));
    assert_eq!(r.status, Status::Passed, "{r:?}");
}
