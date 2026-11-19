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

#[test]
fn the_unsatisfiable_classes_check_is_unavailable_in_every_build() {
    // a valid, consistent terminology with no exclusions or imports, which a classifier
    // would pass
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        "",
        "PREFIX owl: <http://www.w3.org/2002/07/owl#>
         PREFIX rdfs: <http://www.w3.org/2000/01/rdf-schema#>
         INSERT DATA { <urn:A> a owl:Class . <urn:B> a owl:Class .
           <urn:A> rdfs:subClassOf <urn:B> }",
    );
    for complete in [
        "",
        "; chk:requireComplete true",
        "; chk:requireComplete false",
        "; chk:graph <urn:onto>",
    ] {
        for (severity, blocks) in [("sh:Violation", true), ("sh:Warning", false)] {
            let s = suite(&format!(
                "<urn:s> a chk:Suite ; chk:check <urn:u>, <urn:v> .
                 <urn:u> a chk:UnsatisfiableClassesCheck ; chk:severity {severity} {complete} .
                 <urn:v> a chk:QueryCheck ; chk:query \"ASK {{ <urn:x> ?p ?o }}\" ."
            ))
            .unwrap();
            let case = format!("{severity} {complete:?}");
            // the suite parses and names the check, and a store of suites refuses it
            let unavailable: Vec<&str> = s.unavailable().iter().map(|c| c.iri.as_str()).collect();
            assert_eq!(unavailable, ["urn:u"], "{case}");
            let r = run_on(&s, &before, &after);
            assert_eq!(r.revision, s.revision(), "{case}");
            let u = &r.results[0];
            assert_eq!(u.check.as_str(), "urn:u", "{case}");
            assert_eq!(u.status, Status::Error, "{case}: {u:?}");
            assert_eq!(u.code, Some("check-unavailable"), "{case}: {u:?}");
            assert_eq!(u.findings, 0, "{case}: {u:?}");
            assert!(u.notes.is_empty(), "{case}: {u:?}");
            assert!(u.messages[0].contains("deferred"), "{case}: {u:?}");
            assert!(!u.messages[0].contains("cargo feature"), "{case}: {u:?}");
            assert_eq!(u.blocks(), blocks, "{case}: {u:?}");
            assert_eq!(r.passed(), !blocks, "{case}");
            // the ordinary check beside it still runs
            assert_eq!(
                r.results[1].status,
                Status::Passed,
                "{case}: {:?}",
                r.results[1]
            );
            // no bound replaces the deferral: a spent one, and one too large to add
            for timeout in [std::time::Duration::ZERO, std::time::Duration::MAX] {
                let r = run(
                    &s,
                    &Inputs {
                        before: &before,
                        after: &after,
                        stored: None,
                        timeout: Some(timeout),
                    },
                );
                let u = &r.results[0];
                let at = format!("{case} {timeout:?}");
                assert_eq!(u.code, Some("check-unavailable"), "{at}: {u:?}");
                assert!(u.messages[0].contains("deferred"), "{at}: {u:?}");
                assert_eq!(u.blocks(), blocks, "{at}: {u:?}");
            }
        }
    }
    refused(
        "<urn:s> a chk:Suite ; chk:check <urn:u> .
         <urn:u> a chk:UnsatisfiableClassesCheck ; chk:inferences true .",
        "does not take chk:inferences",
    );
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

#[cfg(feature = "reasoning")]
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
        "PREFIX owl: <http://www.w3.org/2002/07/owl#> INSERT DATA { <urn:A> owl:disjointWith <urn:B> .
           <urn:x> a <urn:A>, <urn:B> }",
    );
    let r = run_on(&s, &before, &after).results[0].clone();
    assert_eq!(r.status, Status::Failed, "{r:?}");
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

fn profile_refusal(result: &CheckResult, reason: &str) {
    assert_eq!(result.status, Status::Error, "{result:?}");
    assert_eq!(result.code, Some("input-invalid"), "{result:?}");
    assert!(result.blocks(), "{result:?}");
    assert!(
        result.messages[0].contains(reason),
        "expected {reason:?}: {result:?}"
    );
}

const NESTED_SERVICE: &str = "SELECT ?focus WHERE {
    VALUES ?focus { <urn:x> }
    BIND(EXISTS { SERVICE <urn:offline:unused> { ?a ?b ?c } } AS ?hit)
    FILTER(?hit)
}";

#[test]
fn gate_query_profile_refuses_service_before_evaluation() {
    let ds = Dataset::memory();
    ds.update("INSERT DATA { GRAPH <urn:data> { <urn:x> <urn:p> <urn:y> } }")
        .unwrap();
    let snapshot = ds.store().snapshot();
    let run_query = |query: &str| {
        let literal = oxrdf::Literal::new_simple_literal(query);
        let suite = suite(&format!(
            "<urn:s> a chk:Suite ; chk:check <urn:c> .
             <urn:c> a chk:QueryCheck ; chk:query {literal} ."
        ))
        .unwrap();
        run_on(&suite, &snapshot, &snapshot).results.remove(0)
    };
    let queries = [
        NESTED_SERVICE,
        "ASK { SERVICE SILENT <urn:offline:unused> { ?s ?p ?o } }",
        "SELECT ?s {
            VALUES (?s ?endpoint) { (<urn:x> <urn:offline:unused>) }
            OPTIONAL { ?s ?p ?o FILTER(EXISTS { SERVICE ?endpoint { ?a ?b ?c } }) }
        }",
        "SELECT ?count WHERE {
            { SELECT (COUNT(IF(EXISTS { SERVICE <urn:offline:unused> { ?a ?b ?c } },
                                1, 0)) AS ?count) WHERE {} }
        }",
        "SELECT ?s { ?s ?p ?o }
         ORDER BY DESC(IF(EXISTS { SERVICE <urn:offline:unused> { ?a ?b ?c } }, 1, 0))",
    ];
    for query in queries {
        sparkles_core::sparql::parse_query(query, None, &[]).expect(query);
        profile_refusal(&run_query(query), "SERVICE");
    }
    assert_eq!(
        run_query("ASK { FILTER(CONTAINS('SERVICE', 'SERVICE')) }").status,
        Status::Failed
    );
    assert_eq!(
        run_query("ASK FROM <urn:data> { <urn:x> <urn:p> <urn:y> }").status,
        Status::Failed
    );
}

#[cfg(feature = "shacl")]
fn inline_shacl(shapes: &str, selection: &str) -> Suite {
    let text = oxrdf::Literal::new_simple_literal(format!("@prefix sh: <{SH_NS}> .\n{shapes}"));
    suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShaclCheck ; chk:shapes {text} {selection} ."
    ))
    .unwrap()
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_refuses_access_to_the_candidate_copy_of_pinned_shapes() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        &format!(
            "PREFIX sh: <{SH_NS}>
             INSERT DATA {{ GRAPH <urn:shapes> {{
                 <urn:S> sh:targetNode <urn:x> ;
                     sh:sparql [ sh:select
                        'SELECT $this WHERE {{ GRAPH $shapesGraph {{ <urn:ref> <urn:flag> true }} }}'
                     ] .
                 <urn:ref> <urn:flag> true .
             }} }}"
        ),
        "DELETE DATA { GRAPH <urn:shapes> { <urn:ref> <urn:flag> true } }",
    );
    let suite = shacl_suite("").unwrap();
    profile_refusal(
        &run_on(&suite, &before, &after).results[0],
        "GRAPH is not allowed in the gate's SHACL profile",
    );
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_checks_all_embedded_query_forms() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        "",
        "INSERT DATA {
            GRAPH <urn:data> { <urn:x> <urn:flag> true }
            GRAPH <urn:other> { <urn:x> <urn:otherFlag> true }
        }",
    );
    let run_shapes = |shapes: &str| {
        let suite = inline_shacl(shapes, "; chk:dataGraph <urn:data>");
        run_on(&suite, &before, &after).results.remove(0)
    };
    for graph in [
        "<urn:other>",
        "?g",
        "<urn:x-arq:DefaultGraph>",
        "<urn:x-arq:UnionGraph>",
    ] {
        let shapes = format!(
            "<urn:S> sh:targetNode <urn:x> ; sh:sparql [ sh:select
              'SELECT $this WHERE {{ GRAPH {graph} {{ <urn:x> <urn:flag> true }} }}' ] ."
        );
        profile_refusal(
            &run_shapes(&shapes),
            "GRAPH is not allowed in the gate's SHACL profile",
        );
    }
    let shapes = [
        "<urn:S> sh:targetNode <urn:x>,
             [ sh:select '''SELECT ?result WHERE {
                 BIND(EXISTS { GRAPH <urn:other> { ?s ?p ?o } } AS ?result)
             }''' ] .",
        "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
           sh:validator [ a sh:SPARQLAskValidator ;
                           sh:ask 'ASK { GRAPH <urn:other> { ?s ?p ?o } }' ] .
         <urn:S> sh:targetNode <urn:x> .",
        "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
           sh:propertyValidator [ a sh:SPARQLSelectValidator ;
                   sh:select 'SELECT $this WHERE { GRAPH ?g { $this $PATH ?o } }' ] .
         <urn:S> sh:targetNode <urn:x> ;
             sh:property [ sh:path <urn:flag> ; <urn:param> true ] .",
        "<urn:f> a sh:SPARQLFunction ;
            sh:select 'SELECT ?o WHERE { GRAPH <urn:other> { ?s ?p ?o } }' .
         <urn:S> sh:targetNode <urn:x> .",
    ];
    for shapes in shapes {
        profile_refusal(
            &run_shapes(shapes),
            "GRAPH is not allowed in the gate's SHACL profile",
        );
    }
    for from in ["FROM <urn:other>", "FROM NAMED <urn:other>"] {
        for shapes in [
            format!(
                "<urn:S> sh:targetNode <urn:x> ; sh:sparql [ sh:select
                   'SELECT $this {from} WHERE {{ $this <urn:flag> true }}' ] ."
            ),
            format!(
                "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
                    sh:nodeValidator [ sh:select 'SELECT $this {from} WHERE {{}}' ] .
                 <urn:S> sh:targetNode <urn:x> ; <urn:param> true ."
            ),
            format!(
                "<urn:S> sh:targetNode
                    [ sh:select 'SELECT ?result {from} WHERE {{ ?result <urn:flag> true }}' ] ;
                    sh:class <urn:Required> ."
            ),
        ] {
            profile_refusal(
                &run_shapes(&shapes),
                "FROM and FROM NAMED are not allowed in the gate's SHACL profile",
            );
        }
    }
    let supported = [
        "<urn:S> sh:targetNode <urn:x> ; sh:sparql [ sh:select
           'SELECT $this WHERE { $this <urn:flag> true }' ] .",
        "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
           sh:propertyValidator [ a sh:SPARQLSelectValidator ;
                sh:select 'SELECT $this WHERE { $this $PATH $param }' ] .
         <urn:S> sh:targetNode <urn:x> ;
             sh:property [ sh:path <urn:flag> ; <urn:param> true ] .",
        "<urn:S> sh:targetNode
            [ sh:select 'SELECT ?result WHERE { ?result <urn:flag> true }' ] ;
            sh:class <urn:Required> .",
    ];
    for shapes in supported {
        let result = run_shapes(shapes);
        assert_eq!(result.status, Status::Failed, "{shapes}: {result:?}");
    }
    for constraint in [
        "sh:select 'SELECT $this WHERE { $this <urn:otherFlag> true }'",
        "sh:deactivated true ; sh:select 'SELECT $this WHERE { GRAPH ?g { ?s ?p ?o } }'",
    ] {
        let result = run_shapes(&format!(
            "<urn:S> sh:targetNode <urn:x> ; sh:sparql [ {constraint} ] ."
        ));
        assert_eq!(result.status, Status::Passed, "{result:?}");
    }
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_refuses_conflicting_node_expression_forms() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:ok> a <urn:Allowed> }");
    let run_target = |target: &str, class: &str| {
        let suite = inline_shacl(
            &format!(
                "@prefix rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> .
                 @prefix sparql: <http://www.w3.org/ns/sparql#> .
                 <urn:S> sh:targetNode {target} ; sh:class <urn:{class}> ."
            ),
            "",
        );
        run_on(&suite, &before, &after).results.remove(0)
    };
    let list = "[ rdf:first <urn:ok> ; rdf:rest rdf:nil ; <urn:annotation> 'a list' ]";
    assert_eq!(run_target(list, "Allowed").status, Status::Passed);
    assert_eq!(run_target(list, "Required").status, Status::Failed);
    for mixed in [
        "[ rdf:first <urn:ok> ; rdf:rest rdf:nil ; sh:select
           'SELECT ?result WHERE { GRAPH ?g { ?result ?p ?o } }' ]",
        "[ sh:select 'SELECT ?result WHERE { BIND(<urn:ok> AS ?result) }' ;
            sparql:STR ( <urn:ok> ) ]",
    ] {
        profile_refusal(
            &run_target(mixed, "Allowed"),
            "conflicting node expression forms",
        );
    }
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_requires_a_typed_constraint_deactivation_flag() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> <urn:flag> true }");
    let run_constraint = |flag: &str| {
        let suite = inline_shacl(
            &format!(
                "<urn:S> sh:targetNode <urn:x> ; sh:sparql [ sh:deactivated {flag} ;
                    sh:select 'SELECT $this WHERE {{ GRAPH ?g {{ ?s ?p ?o }} }}' ] ."
            ),
            "",
        );
        run_on(&suite, &before, &after).results.remove(0)
    };
    assert_eq!(run_constraint("true").status, Status::Passed);
    profile_refusal(
        &run_constraint("false"),
        "GRAPH is not allowed in the gate's SHACL profile",
    );
    for invalid in [
        "'true'",
        "'yes'^^<http://www.w3.org/2001/XMLSchema#boolean>",
        "true, false",
    ] {
        profile_refusal(
            &run_constraint(invalid),
            "sh:deactivated must be a single xsd:boolean literal",
        );
    }
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_preserves_path_variables_in_node_validators() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        "",
        "INSERT DATA { <urn:x> <urn:flag> true ; <urn:edge> <urn:mid> .
                       <urn:mid> <urn:leaf> true }",
    );
    for validator in ["nodeValidator", "validator"] {
        let suite = inline_shacl(
            &format!(
                "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
                    sh:{validator} [ a sh:SPARQLSelectValidator ;
                        sh:select 'SELECT $this $PATH WHERE {{ $this <urn:flag> $PATH }}' ] .
                 <urn:S> sh:targetNode <urn:x> ; <urn:param> true ."
            ),
            "",
        );
        let result = run_on(&suite, &before, &after).results.remove(0);
        assert_eq!(result.status, Status::Failed, "{validator}: {result:?}");
        assert_eq!(result.findings, 1, "{validator}: {result:?}");
    }
    let suite = inline_shacl(
        "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
             sh:validator [ a sh:SPARQLSelectValidator ;
                 sh:select 'SELECT $this WHERE { $this $PATH / <urn:leaf> $param }' ] .
         <urn:S> sh:targetNode <urn:x> ;
             sh:property [ sh:path <urn:edge> ; <urn:param> true ] .",
        "",
    );
    let result = run_on(&suite, &before, &after).results.remove(0);
    assert_eq!(result.status, Status::Failed, "{result:?}");
    assert_eq!(result.findings, 1, "{result:?}");
    let suite = inline_shacl(
        "<urn:C> a sh:ConstraintComponent ; sh:parameter [ sh:path <urn:param> ] ;
             sh:validator [ a sh:SPARQLAskValidator ; sh:ask 'ASK FROM $PATH {}' ] .
         <urn:S> sh:targetNode <urn:x> .",
        "",
    );
    profile_refusal(
        &run_on(&suite, &before, &after).results[0],
        "FROM and FROM NAMED are not allowed in the gate's SHACL profile",
    );
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_refuses_skipped_targets() {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> a <urn:Observed> }");
    let run_shapes = |shapes: &str| {
        let suite = inline_shacl(shapes, "");
        run_on(&suite, &before, &after).results.remove(0)
    };
    for (target, reason) in [
        (
            "sh:target [ a sh:SPARQLTarget ; sh:select 'SELECT $this { $this a <urn:Observed> }' ]",
            "sh:target",
        ),
        (
            "sh:targetNode [ sh:select <urn:not-a-string> ]",
            "sh:targetNode",
        ),
        ("sh:targetClass 'not-an-iri'", "sh:targetClass"),
        ("sh:targetSubjectsOf []", "sh:targetSubjectsOf"),
        ("sh:targetObjectsOf 'not-an-iri'", "sh:targetObjectsOf"),
        ("sh:targetWhere 'not-a-shape'", "sh:targetWhere"),
    ] {
        let mixed = format!("<urn:S> sh:targetNode <urn:valid> ; {target} .");
        profile_refusal(&run_shapes(&mixed), reason);
    }
    for node in ["[]", "[ <urn:description> 'a regular blank node' ]"] {
        let result = run_shapes(&format!(
            "<urn:S> sh:targetNode {node} ; sh:nodeKind sh:IRI ."
        ));
        assert_eq!(result.status, Status::Failed, "{node}: {result:?}");
    }
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_profile_preserves_a_described_pinned_blank_target() {
    let ds = Dataset::memory();
    let (before, after) = states(
        &ds,
        &format!(
            "PREFIX sh: <{SH_NS}>
             INSERT DATA {{ GRAPH <urn:shapes> {{
                 <urn:S> sh:targetNode [ <urn:description> 'ordinary node' ] ;
                     sh:nodeKind sh:IRI .
             }} }}"
        ),
        "INSERT DATA { <urn:x> a <urn:Observed> }",
    );
    let r = run_on(&shacl_suite("").unwrap(), &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert_eq!(r.results[0].findings, 1);
}

#[cfg(feature = "shex")]
fn shex_profile_suite(schema: &str, map: &str) -> Suite {
    let schema = oxrdf::Literal::new_simple_literal(schema);
    let map = oxrdf::Literal::new_simple_literal(map);
    suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShexCheck ; chk:schema {schema} ; chk:shapeMap {map} ."
    ))
    .unwrap()
}

#[cfg(feature = "shex")]
fn run_shex_profile(schema: &str, map: &str) -> CheckResult {
    let ds = Dataset::memory();
    let (before, after) = states(&ds, "", "INSERT DATA { <urn:x> <urn:link> <urn:y> }");
    let suite = shex_profile_suite(schema, map);
    run_on(&suite, &before, &after).results.remove(0)
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_profile_sparql_selects_from_blank_node_named_graphs() {
    let ds = Dataset::from_store(crate::store::Store::in_memory(crate::store::StoreOptions {
        union_default_graph: true,
        ..Default::default()
    }));
    let before = ds.store().snapshot();
    ds.load_str("_:g { <urn:x> <urn:link> <urn:y> }", RdfFormat::TriG)
        .unwrap();
    let after = ds.store().snapshot();
    let schema = "<urn:S> { <urn:required> . }";
    for map in [
        "{FOCUS <urn:link> _}@<urn:S>",
        "SPARQL 'SELECT ?focus WHERE { ?focus <urn:link> ?o }'@<urn:S>",
    ] {
        let suite = shex_profile_suite(schema, map);
        let result = run_on(&suite, &before, &after).results.remove(0);
        assert_eq!(result.status, Status::Failed, "{map}: {result:?}");
        assert_eq!(result.findings, 1, "{map}: {result:?}");
        assert!(result.messages[0].contains("<urn:x>"), "{map}: {result:?}");
    }
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_profile_refuses_nonportable_blank_data_selectors() {
    let label = sparkles_core::store::bnode_for(crate::id::Id::bnode(1));
    let selectors = [
        "_:x@<urn:S>".to_string(),
        format!("_:{label}@<urn:S>", label = label.as_str()),
        "{FOCUS <urn:link> _:x}@<urn:S>".to_string(),
        "{_:x <urn:link> FOCUS}@<urn:S>".to_string(),
    ];
    for selector in selectors {
        profile_refusal(&run_shex_profile("<urn:S> {}", &selector), "blank node");
    }
    let blank_shape = run_shex_profile("_:shape { <urn:required> . }", "<urn:x>@_:shape");
    assert_eq!(blank_shape.status, Status::Failed, "{blank_shape:?}");
    assert_eq!(blank_shape.findings, 1, "{blank_shape:?}");
    assert_eq!(
        run_shex_profile("<urn:S> { <urn:req> . }", "{FOCUS <urn:absent> _}@<urn:S>").status,
        Status::Passed
    );
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_profile_refuses_compiled_semantic_actions() {
    for schema in [
        "<urn:S> { <urn:link> @<urn:T> } <urn:T> {} %<urn:unknown>% ",
        "%<urn:unknown>% <urn:S> {}",
        "<urn:S> { ( <urn:link> . ; <urn:unused> .? ) %<urn:unknown>% }",
    ] {
        profile_refusal(
            &run_shex_profile(schema, "<urn:x>@<urn:S>"),
            "semantic action",
        );
    }
    assert_eq!(
        run_shex_profile(
            "<urn:S> { <urn:link> @<urn:T> } <urn:T> {}",
            "<urn:x>@<urn:S>",
        )
        .status,
        Status::Passed
    );
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_profile_refuses_service_in_selector_expressions() {
    let schema = "<urn:S> { <urn:required> . }";
    let selector = format!("SPARQL '''{NESTED_SERVICE}'''@<urn:S>");
    profile_refusal(&run_shex_profile(schema, &selector), "SERVICE");
    assert_eq!(
        run_shex_profile(
            schema,
            "SPARQL 'SELECT ?focus WHERE { ?focus <urn:absent> ?o }'@<urn:S>",
        )
        .status,
        Status::Passed
    );
}

fn run_with_controls(suite: &Suite, snap: &Arc<Snapshot>, controls: &GateControls) -> CheckResult {
    run_controlled(
        suite,
        &Inputs {
            before: snap,
            after: snap,
            stored: None,
            timeout: None,
        },
        controls,
    )
    .results
    .remove(0)
}

fn gate_budget_refusal(result: &CheckResult) {
    assert_eq!(result.status, Status::Error, "{result:?}");
    assert_eq!(result.code, Some("budget-exceeded"), "{result:?}");
    assert!(result.blocks());
}

#[test]
fn gate_query_obeys_the_work_cap_and_earliest_deadline() {
    let ds = Dataset::memory();
    let snap = ds.store().snapshot();
    let check = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:QueryCheck ; chk:query 'SELECT ?x { VALUES ?x { 1 2 } }' .",
    )
    .unwrap();
    gate_budget_refusal(&run_with_controls(
        &check,
        &snap,
        &GateControls {
            budgets: crate::Budgets {
                max_rows_produced: Some(1),
                ..Default::default()
            },
            ..Default::default()
        },
    ));
    let expired = run_with_controls(
        &check,
        &snap,
        &GateControls {
            deadline: Some(std::time::Instant::now()),
            budgets: crate::Budgets {
                timeout: Some(std::time::Duration::from_secs(10)),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(expired.code, Some("check-timeout"), "{expired:?}");
    let overflow = run_with_controls(
        &check,
        &snap,
        &GateControls {
            budgets: crate::Budgets {
                timeout: Some(std::time::Duration::MAX),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(overflow.code, Some("input-invalid"), "{overflow:?}");
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_covers_setup_and_embedded_query_resources() {
    let ds = Dataset::memory();
    ds.update("INSERT DATA { <urn:x> <urn:p> 1, 2 }").unwrap();
    let snap = ds.store().snapshot();
    let inactive = inline_shacl("<urn:S> sh:targetNode <urn:x> ; sh:deactivated true .", "");
    let timeout = run_with_controls(
        &inactive,
        &snap,
        &GateControls {
            budgets: crate::Budgets {
                timeout: Some(std::time::Duration::ZERO),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    assert_eq!(timeout.code, Some("check-timeout"), "{timeout:?}");
    let expired = sparkles_shacl::Shapes::from_store_for_gate(
        &snap,
        "urn:missing",
        Some(std::time::Instant::now()),
    )
    .unwrap_err();
    assert!(matches!(
        expired.downcast_ref::<crate::Error>(),
        Some(crate::Error::Timeout)
    ));

    let nested = inline_shacl(
        "<urn:S> sh:targetNode <urn:x> ; sh:sparql [
            sh:select 'SELECT $this ?value { $this <urn:p> ?value }'
         ] .",
        "",
    );
    for budgets in [
        crate::Budgets {
            max_rows_produced: Some(1),
            ..Default::default()
        },
        crate::Budgets {
            max_rows: Some(1),
            ..Default::default()
        },
        crate::Budgets {
            max_memory_bytes: Some(0),
            ..Default::default()
        },
    ] {
        gate_budget_refusal(&run_with_controls(
            &nested,
            &snap,
            &GateControls {
                budgets,
                ..Default::default()
            },
        ));
    }
    assert_eq!(
        run_with_controls(&nested, &snap, &GateControls::default()).findings,
        2,
    );
}

#[cfg(feature = "shacl")]
#[test]
fn gate_shacl_bounds_nested_and_top_level_reports() {
    let ds = Dataset::memory();
    ds.update("INSERT DATA { <urn:x> <urn:list> (1 2) }")
        .unwrap();
    let snap = ds.store().snapshot();
    let capped = GateControls {
        max_results: Some(1),
        ..Default::default()
    };
    for shapes in [
        "<urn:S> sh:targetNode <urn:x> ; sh:class <urn:A> ; sh:nodeKind sh:Literal .",
        "<urn:S> sh:targetNode <urn:x> ;
            sh:property [ sh:path <urn:list> ; sh:memberShape [ sh:nodeKind sh:IRI ] ] .",
    ] {
        let check = inline_shacl(shapes, "");
        gate_budget_refusal(&run_with_controls(&check, &snap, &capped));
        assert_eq!(
            run_with_controls(&check, &snap, &GateControls::default()).status,
            Status::Failed
        );
    }
    let with_message = inline_shacl(
        &format!(
            "<urn:S> sh:targetNode <urn:x> ; sh:class <urn:Missing> ; sh:message '{}'.",
            "m".repeat(8192)
        ),
        "",
    );
    gate_budget_refusal(&run_with_controls(
        &with_message,
        &snap,
        &GateControls {
            max_report_bytes: Some(4096),
            ..Default::default()
        },
    ));
    let empty = inline_shacl(
        "<urn:S> sh:targetClass <urn:Missing> ; sh:closed true .",
        "",
    );
    assert_eq!(
        run_with_controls(
            &empty,
            &snap,
            &GateControls {
                max_results: Some(0),
                max_report_bytes: Some(0),
                ..Default::default()
            },
        )
        .status,
        Status::Passed
    );
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_bounds_selector_work_and_retained_results() {
    let ds = Dataset::memory();
    let snap = ds.store().snapshot();
    let check = suite(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:ShexCheck ; chk:schema '<urn:S> {}' ;
             chk:shapeMap \"SPARQL 'SELECT ?focus { VALUES ?focus { <urn:x> <urn:y> } }'@<urn:S>\" .",
    )
    .unwrap();
    for controls in [
        GateControls {
            budgets: crate::Budgets {
                max_rows_produced: Some(1),
                ..Default::default()
            },
            ..Default::default()
        },
        GateControls {
            max_results: Some(1),
            ..Default::default()
        },
        GateControls {
            max_report_bytes: Some(0),
            ..Default::default()
        },
    ] {
        gate_budget_refusal(&run_with_controls(&check, &snap, &controls));
    }
    assert_eq!(
        run_with_controls(&check, &snap, &GateControls::default()).status,
        Status::Passed
    );
}

#[cfg(feature = "shex")]
#[test]
fn gate_shex_selectors_spend_one_check_work_allowance() {
    let snap = Dataset::memory().store().snapshot();
    let query = "SELECT ?focus { VALUES ?focus { <urn:x> } }";
    let one = crate::sparql::query(snap.clone(), query, &Default::default()).unwrap();
    assert!(one.rows_produced > 0);
    let controls = GateControls {
        budgets: crate::Budgets {
            max_rows_produced: Some(one.rows_produced),
            ..Default::default()
        },
        ..Default::default()
    };
    let check = |count: usize| {
        let selector = |label| format!("SPARQL '{query}'@<urn:{label}>");
        let selectors = ["A", "B"]
            .into_iter()
            .take(count)
            .map(selector)
            .collect::<Vec<_>>()
            .join(", ");
        let map = oxrdf::Literal::new_simple_literal(selectors);
        suite(&format!(
            "<urn:s> a chk:Suite ; chk:check <urn:c> .
             <urn:c> a chk:ShexCheck ; chk:schema '<urn:A> {{}} <urn:B> {{}}' ; chk:shapeMap {map} ."
        ))
        .unwrap()
    };
    assert_eq!(
        run_with_controls(&check(1), &snap, &controls).status,
        Status::Passed
    );
    gate_budget_refusal(&run_with_controls(&check(2), &snap, &controls));
}

#[cfg(feature = "reasoning")]
fn gate_diagnostics_suite(inferences: bool) -> Suite {
    suite(&format!(
        "<urn:s> a chk:Suite ; chk:check <urn:c> .
         <urn:c> a chk:DiagnosticsCheck ; chk:diagnostic 'disjoint-classes' ;
             chk:inferences {inferences} ."
    ))
    .unwrap()
}

#[cfg(feature = "reasoning")]
#[test]
fn gate_diagnostics_obeys_the_native_default_and_inference_flag() {
    let store = |union_default_graph| {
        Dataset::from_store(crate::store::Store::in_memory(crate::store::StoreOptions {
            union_default_graph,
            ..Default::default()
        }))
    };
    let inferred = sparkles_core::INFERRED_GRAPH;
    let asserted = "<urn:A> <http://www.w3.org/2002/07/owl#disjointWith> <urn:B> .
                    <urn:x> a <urn:A> .";
    let union = store(true);
    union
        .update(&format!(
            "INSERT DATA {{
                GRAPH <urn:asserted> {{ {asserted} }}
                GRAPH <{inferred}> {{ <urn:x> a <urn:B> }}
            }}"
        ))
        .unwrap();
    let plain = store(false);
    plain
        .update(&format!(
            "INSERT DATA {{ {asserted} GRAPH <{inferred}> {{ <urn:x> a <urn:B> }} }}"
        ))
        .unwrap();
    for ds in [&union, &plain] {
        let snap = ds.store().snapshot();
        for (flag, status) in [(false, Status::Passed), (true, Status::Failed)] {
            let check = gate_diagnostics_suite(flag);
            let r = run_on(&check, &snap, &snap);
            assert_eq!(r.results[0].status, status, "{:?}", r.results[0]);
        }
    }
    use sparkles_reasoner::diagnostics::{Basis, DiagnoseOptions, diagnose_for_gate};
    let evidence = diagnose_for_gate(
        union.store().snapshot(),
        &DiagnoseOptions {
            checks: vec!["disjoint-classes".into()],
            inferences: true,
            ..Default::default()
        },
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(evidence.findings[0].basis, Basis::UsesInferences);
    union
        .update("INSERT DATA { GRAPH <urn:asserted> { <urn:x> a <urn:B> } }")
        .unwrap();
    let snap = union.store().snapshot();
    for flag in [false, true] {
        let result = run_on(&gate_diagnostics_suite(flag), &snap, &snap)
            .results
            .remove(0);
        assert_eq!(result.status, Status::Failed, "{result:?}");
    }

    let evidence = diagnose_for_gate(
        snap,
        &DiagnoseOptions {
            checks: vec!["disjoint-classes".into()],
            inferences: true,
            ..Default::default()
        },
        &Default::default(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(evidence.findings[0].basis, Basis::Asserted);

    let blank = store(true);
    blank
        .store()
        .load(&[sparkles_core::io::Source::from_bytes(
            format!("_:graph {{ {asserted} <urn:x> a <urn:B> . }}").into_bytes(),
            oxrdfio::RdfFormat::TriG,
            None,
        )])
        .unwrap();
    let snap = blank.store().snapshot();
    let result = run_on(&gate_diagnostics_suite(false), &snap, &snap)
        .results
        .remove(0);
    assert_eq!(result.status, Status::Failed, "{result:?}");

    let union_only_default = store(true);
    union_only_default
        .update(&format!(
            "INSERT DATA {{ {asserted} <urn:x> a <urn:B> .
                             GRAPH <urn:unrelated> {{ <urn:y> a <urn:C> }} }}"
        ))
        .unwrap();
    let snap = union_only_default.store().snapshot();
    for flag in [false, true] {
        assert!(run_on(&gate_diagnostics_suite(flag), &snap, &snap).passed());
    }
}

#[cfg(feature = "reasoning")]
#[test]
fn gate_diagnostics_bounds_queries_and_the_report() {
    let ds = Dataset::memory();
    ds.update(
        "INSERT DATA {
             <urn:A> <http://www.w3.org/2002/07/owl#disjointWith> <urn:B> .
             <urn:x> a <urn:A>, <urn:B> . <urn:y> a <urn:A>, <urn:B> .
         }",
    )
    .unwrap();
    let snap = ds.store().snapshot();
    let check = gate_diagnostics_suite(false);
    for controls in [
        GateControls {
            budgets: crate::Budgets {
                max_memory_bytes: Some(0),
                ..Default::default()
            },
            ..Default::default()
        },
        GateControls {
            max_results: Some(0),
            ..Default::default()
        },
        GateControls {
            max_report_bytes: Some(0),
            ..Default::default()
        },
    ] {
        gate_budget_refusal(&run_with_controls(&check, &snap, &controls));
    }
    let truncated = run_with_controls(
        &check,
        &snap,
        &GateControls {
            max_results: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(truncated.status, Status::Failed, "{truncated:?}");
    assert_eq!(truncated.findings, 1);
}

#[test]
fn a_note_is_reported_with_its_severity_and_never_blocks() {
    let note = Note {
        severity: Severity::Warning,
        code: "a-note",
        count: 2,
        message: "two things the check left out".to_string(),
    };
    let check = |status: Status, severity: Severity| CheckResult {
        check: oxrdf::NamedNode::new_unchecked("urn:c"),
        kind: "QueryCheck",
        severity,
        status,
        findings: u64::from(status == Status::Failed),
        code: None,
        messages: Vec::new(),
        notes: vec![note.clone()],
        millis: 0,
    };
    let report = |r: CheckResult| SuiteReport {
        suite: oxrdf::NamedNode::new_unchecked("urn:s"),
        revision: "sha256:0".to_string(),
        results: vec![r],
    };
    // a passing required check with a warning note passes, and the note is in the JSON
    let j = report(check(Status::Passed, Severity::Violation)).to_json();
    assert_eq!(j["passed"], true, "{j}");
    let r = &j["results"][0];
    assert_eq!(r["blocks"], false, "{j}");
    assert_eq!(r["notes"][0]["code"], "a-note", "{j}");
    assert_eq!(r["notes"][0]["severity"], "warning", "{j}");
    assert_eq!(r["notes"][0]["count"], 2, "{j}");
    assert_eq!(
        r["notes"][0]["message"], "two things the check left out",
        "{j}"
    );
    // a failure blocks for its own status, with the same note beside it
    let j = report(check(Status::Failed, Severity::Violation)).to_json();
    assert_eq!(j["passed"], false, "{j}");
    assert_eq!(j["results"][0]["notes"][0]["code"], "a-note", "{j}");
    // a note on an advisory check does not fail the suite either
    let j = report(check(Status::Failed, Severity::Info)).to_json();
    assert_eq!(j["passed"], true, "{j}");
}
