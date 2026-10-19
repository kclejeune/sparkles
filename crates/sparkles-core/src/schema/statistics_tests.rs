//! Reports measured from the index statistics, and the objects of one predicate.

use super::*;
use crate::io::{RdfFormat, Source};
use crate::sparql::QueryOptions;
use crate::store::{Store, StoreOptions};

const EX: &str = "http://ex.org/";
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

fn ex(l: &str) -> String {
    format!("{EX}{l}")
}

fn load(s: &Store, text: String, format: RdfFormat) {
    s.load(&[Source::from_bytes(text.into_bytes(), format, None)])
        .unwrap();
}

/// `n` subjects in five classes, each with an integer, an English name, a link to
/// another subject and, for every tenth, a plain comment, with labels on the vocabulary.
fn synthetic(n: usize) -> Store {
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    for i in 0..n {
        let sub = format!("<{EX}s{i}>");
        nt.push_str(&format!(
            "{sub} <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EX}C{}> .\n",
            i % 5
        ));
        nt.push_str(&format!("{sub} <{EX}val> \"{i}\"^^<{XSD_INTEGER}> .\n"));
        nt.push_str(&format!("{sub} <{EX}name> \"name {i}\"@en .\n"));
        nt.push_str(&format!("{sub} <{EX}ref> <{EX}s{}> .\n", (i * 7 + 1) % n));
        if i % 10 == 0 {
            nt.push_str(&format!("{sub} <{EX}note> \"note {i}\" .\n"));
        }
    }
    for c in 0..5 {
        nt.push_str(&format!(
            "<{EX}C{c}> <http://www.w3.org/2000/01/rdf-schema#label> \"Class {c}\"@en .\n"
        ));
    }
    nt.push_str(&format!(
        "<{EX}val> <http://www.w3.org/2000/01/rdf-schema#label> \"value\"@en .\n"
    ));
    load(&s, nt, RdfFormat::NTriples);
    s
}

fn stats_opts(graph: GraphSelection) -> SchemaOptions {
    SchemaOptions {
        graph,
        statistics_above: Some(0),
        ..Default::default()
    }
}

fn find_pred<'a>(r: &'a SchemaReport, iri: &str) -> Option<&'a PredicateEntry> {
    r.predicates.iter().find(|p| p.iri == iri)
}

fn find_class<'a>(r: &'a SchemaReport, iri: &str) -> Option<&'a ClassEntry> {
    r.classes.iter().find(|c| c.iri == iri)
}

/// The statistics report has the exact report's predicates, classes, counts and
/// declarations, and no objects.
fn assert_same_counts(stats: &SchemaReport, exact: &SchemaReport) {
    assert_eq!(stats.basis, Basis::Statistics);
    assert_eq!(exact.basis, Basis::Exact);
    let names =
        |r: &SchemaReport| -> Vec<String> { r.predicates.iter().map(|p| p.iri.clone()).collect() };
    assert_eq!(names(stats), names(exact));
    for (a, b) in stats.predicates.iter().zip(&exact.predicates) {
        assert_eq!(a.observed.triples, b.observed.triples, "{}", a.iri);
        assert_eq!(a.declared.labels, b.declared.labels, "{}", a.iri);
        assert!(a.observed.objects.literals.is_empty());
    }
    let classes = |r: &SchemaReport| -> Vec<(String, u64)> {
        r.classes
            .iter()
            .map(|c| (c.iri.clone(), c.observed.instances))
            .collect()
    };
    assert_eq!(classes(stats), classes(exact));
    assert_eq!(stats.totals.triples, exact.totals.triples);
    // the statistics report is not maintained from changes: it is cheap to compute again
    assert!(stats.maintenance.is_none());
}

#[test]
fn statistics_report_has_the_exact_counts_of_a_built_index() {
    let s = synthetic(2000);
    let snap = s.snapshot();
    let stats = discover(&snap, &stats_opts(GraphSelection::Default)).unwrap();
    let exact = discover(&snap, &SchemaOptions::default()).unwrap();
    assert_same_counts(&stats, &exact);
    for (a, b) in stats.predicates.iter().zip(&exact.predicates) {
        assert_eq!(
            (a.observed.distinct_subjects, a.observed.distinct_objects),
            (b.observed.distinct_subjects, b.observed.distinct_objects),
            "{}",
            a.iri
        );
    }
    assert_eq!(
        find_class(&stats, &ex("C1"))
            .unwrap()
            .declared
            .labels
            .first()
            .map(|l| l.value.as_str()),
        Some("Class 1")
    );
    // the JSON document names the basis, and an exact one leaves it out
    let j = serde_json::to_value(&stats).unwrap();
    assert_eq!(j["basis"], "statistics");
    assert!(serde_json::to_value(&exact).unwrap().get("basis").is_none());
}

#[test]
fn statistics_report_follows_inserts_and_deletes() {
    let s = synthetic(500);
    let text = "PREFIX ex: <http://ex.org/>
        INSERT DATA { ex:n1 a ex:New ; ex:fresh 1 . ex:n2 a ex:New ; ex:fresh 2 . ex:s1 a ex:C4 . } ;
        DELETE WHERE { ?s ex:note ?o } ;
        DELETE DATA { ex:s0 ex:val 0 . }";
    crate::sparql::update::update(&s, text, &QueryOptions::default()).unwrap();
    let snap = s.snapshot();
    assert!(!snap.delta.is_empty());
    let stats = discover(&snap, &stats_opts(GraphSelection::Default)).unwrap();
    let exact = discover(&snap, &SchemaOptions::default()).unwrap();
    assert_same_counts(&stats, &exact);
    assert!(find_pred(&stats, &ex("note")).is_none());
    assert_eq!(find_pred(&stats, &ex("fresh")).unwrap().observed.triples, 2);
    assert_eq!(find_pred(&stats, &ex("val")).unwrap().observed.triples, 499);
    assert_eq!(
        find_class(&stats, &ex("New")).unwrap().observed.instances,
        2
    );
}

#[test]
fn statistics_apply_only_to_the_whole_store() {
    let s = synthetic(200);
    load(
        &s,
        format!("<{EX}g> {{ <{EX}x> <{EX}inGraph> 1 . }}"),
        RdfFormat::TriG,
    );
    let snap = s.snapshot();
    // the default graph leaves out the named graph's quad
    let r = discover(&snap, &stats_opts(GraphSelection::Default)).unwrap();
    assert_eq!(r.basis, Basis::Exact);
    let r = discover(&snap, &stats_opts(GraphSelection::Union)).unwrap();
    assert_eq!(r.basis, Basis::Statistics);
    assert_eq!(find_pred(&r, &ex("inGraph")).unwrap().observed.triples, 1);
    // a small store, subject classes and term totals are measured
    let small = SchemaOptions {
        statistics_above: Some(snap.len()),
        ..stats_opts(GraphSelection::Union)
    };
    assert_eq!(discover(&snap, &small).unwrap().basis, Basis::Exact);
    let classes = SchemaOptions {
        subject_classes: true,
        ..stats_opts(GraphSelection::Union)
    };
    assert_eq!(discover(&snap, &classes).unwrap().basis, Basis::Exact);
    let totals = SchemaOptions {
        term_totals: true,
        ..stats_opts(GraphSelection::Union)
    };
    assert_eq!(discover(&snap, &totals).unwrap().basis, Basis::Exact);
    assert_eq!(
        basis_for(&snap, &stats_opts(GraphSelection::Union)).unwrap(),
        Basis::Statistics
    );
}

#[test]
fn statistics_report_reads_no_more_blocks_as_the_store_grows() {
    // 400,000 and 1,000,000 quads: about 12 and 30 blocks of 32 Ki rows per permutation
    let mut reads = Vec::new();
    let mut exact = Vec::new();
    for n in [80_000, 200_000] {
        let s = synthetic(n);
        let snap = s.snapshot();
        let before = snap.cache.lookups();
        let r = discover(&snap, &stats_opts(GraphSelection::Default)).unwrap();
        assert_eq!(r.basis, Basis::Statistics);
        reads.push(snap.cache.lookups() - before);
        let before = snap.cache.lookups();
        discover(&snap, &SchemaOptions::default()).unwrap();
        exact.push(snap.cache.lookups() - before);
    }
    // the exact report reads every block of PSO and POS, so it grows with the store,
    // while the labels and declarations the statistics report reads do not
    assert!(2 * exact[1] > 3 * exact[0], "exact: {exact:?}");
    assert!(reads[1] <= reads[0] + 8, "statistics: {reads:?}");
    assert!(
        exact[1] > 5 * reads[1],
        "exact {exact:?}, statistics {reads:?}"
    );
}

#[test]
fn objects_of_a_small_predicate_are_exact() {
    let s = synthetic(300);
    let snap = s.snapshot();
    let exact = discover(&snap, &SchemaOptions::default()).unwrap();
    for p in ["name", "val", "ref", "note"] {
        let o = predicate_objects(&snap, &ex(p), &stats_opts(GraphSelection::Default), 1 << 20)
            .unwrap()
            .unwrap();
        assert_eq!(o.sampled, None);
        let want = &find_pred(&exact, &ex(p)).unwrap().observed.objects;
        assert_eq!(
            serde_json::to_value(&o.objects).unwrap(),
            serde_json::to_value(want).unwrap(),
            "{p}"
        );
    }
    assert!(
        predicate_objects(&snap, &ex("absent"), &SchemaOptions::default(), 10)
            .unwrap()
            .is_none()
    );
}

#[test]
fn objects_of_a_large_predicate_are_sampled() {
    // one predicate with 20,000 integers, 6,000 English and 2,000 German strings,
    // 2,000 plain strings, 1,000 dates and 1,500 IRIs
    let s = Store::in_memory(StoreOptions::default());
    let mut nt = String::new();
    let p = format!("<{EX}mixed>");
    for i in 0..20_000 {
        nt.push_str(&format!("<{EX}a{i}> {p} \"{i}\"^^<{XSD_INTEGER}> .\n"));
    }
    for i in 0..6_000 {
        nt.push_str(&format!("<{EX}b{i}> {p} \"text {i}\"@en .\n"));
    }
    for i in 0..2_000 {
        nt.push_str(&format!("<{EX}c{i}> {p} \"Text {i}\"@de .\n"));
        nt.push_str(&format!("<{EX}d{i}> {p} \"plain {i}\" .\n"));
    }
    for i in 0..1_000 {
        nt.push_str(&format!(
            "<{EX}e{i}> {p} \"2020-01-{:02}\"^^<http://www.w3.org/2001/XMLSchema#date> .\n",
            i % 28 + 1
        ));
    }
    for i in 0..1_500 {
        nt.push_str(&format!("<{EX}f{i}> {p} <{EX}o{i}> .\n"));
    }
    load(&s, nt, RdfFormat::NTriples);
    let snap = s.snapshot();
    let o = predicate_objects(&snap, &ex("mixed"), &SchemaOptions::default(), 2_000)
        .unwrap()
        .unwrap();
    let read = o.sampled.expect("a sample");
    assert!(read > 0 && read <= 2_000, "{read}");
    let k = &o.objects;
    // the kinds and the inline literals are exact
    assert_eq!(k.iri.map(|c| c.triples), Some(1_500));
    let group = |dt: &str| k.literals.iter().find(|g| g.datatype == dt);
    assert_eq!(group(XSD_INTEGER).unwrap().triples, 20_000);
    assert_eq!(
        group("http://www.w3.org/2001/XMLSchema#date")
            .unwrap()
            .triples,
        1_000
    );
    // the stored literals are split by the sample, and add up
    let tagged = group(RDF_LANG_STRING).expect("tagged strings");
    let plain = group(XSD_STRING).expect("plain strings");
    assert_eq!(tagged.triples + plain.triples, 10_000);
    assert!(
        (6_500..=9_500).contains(&tagged.triples),
        "tagged {}",
        tagged.triples
    );
    let langs: Vec<&str> = tagged
        .languages
        .iter()
        .flatten()
        .map(|l| l.lang.as_str())
        .collect();
    assert!(langs.contains(&"en") && langs.contains(&"de"), "{langs:?}");
    // the same call reads it in full with room for every quad
    let full = predicate_objects(&snap, &ex("mixed"), &SchemaOptions::default(), 1 << 20)
        .unwrap()
        .unwrap();
    assert_eq!(full.sampled, None);
    assert_eq!(
        full.objects
            .literals
            .iter()
            .find(|g| g.datatype == RDF_LANG_STRING)
            .unwrap()
            .triples,
        8_000
    );
}
