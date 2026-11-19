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
    assert_eq!(s.iri.as_str(), "urn:s");
    assert_eq!(s.label.as_deref(), Some("Gate"));
    let iris: Vec<_> = s.checks.iter().map(|c| c.iri.as_str()).collect();
    assert_eq!(iris, ["urn:s#a", "urn:s#b"]);
    assert_eq!(s.checks[0].severity, Severity::Warning);
    assert_eq!(s.checks[1].severity, Severity::Violation);
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
         <urn:d> a chk:QueryCheck ; chk:storedQuery \"orphans\" ; chk:storedQueryVersion 2 .
         <urn:e> a chk:QueryCheck ; chk:storedQuery \"orphans\" ; chk:storedQueryVersion 1 ;
            chk:storedQueryDigest \"sha256:00\" ."
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
        "<urn:s> a chk:Suite ; chk:check <urn:d>, <urn:u> .
         <urn:d> a chk:DiagnosticsCheck ; chk:diagnostic \"disjoint-classes\" .
         <urn:u> a chk:UnsatisfiableClassesCheck .",
    )
    .unwrap();
    let r = run_on(&s, &before, &after);
    assert_eq!(r.results[0].status, Status::Failed, "{:?}", r.results[0]);
    assert_eq!(r.results[1].status, Status::Failed, "{:?}", r.results[1]);
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
