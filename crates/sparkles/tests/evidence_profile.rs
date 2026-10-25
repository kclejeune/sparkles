//! C25's versioned RDF resource contract, separate from managed runtime writes.
use oxrdf::{Literal, Term};
use sparkles::{Dataset, evidence, io::RdfFormat};
use std::collections::{BTreeSet, HashSet};

const DATA: &str = include_str!("../../../testsuite/evidence/v1/valid.ttl");
const LEGACY: &str = include_str!("../../../testsuite/evidence/v1/legacy.trig");
const EX: &str = "https://example.org/";
const EV: &str = "urn:x-sparkles:evidence:";

fn profile() -> &'static evidence::Profile {
    evidence::profile(1).unwrap()
}

fn data() -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(DATA, RdfFormat::Turtle).unwrap();
    ds
}

fn query(name: &str) -> &'static str {
    profile()
        .competency_queries
        .iter()
        .find(|q| q.name == name)
        .unwrap()
        .query
}

fn value<'a>(row: &'a sparkles::Solution, variable: &str) -> &'a str {
    match row.get(variable).unwrap() {
        Term::NamedNode(n) => n.as_str(),
        Term::Literal(l) => l.value(),
        other => panic!("expected an IRI or literal for {variable}, found {other}"),
    }
}

fn digest(text: &str) -> String {
    let ds = Dataset::memory();
    let literal = Literal::new_simple_literal(text);
    let rows = ds
        .select(&format!("SELECT (SHA256({literal}) AS ?hash) WHERE {{}}"))
        .unwrap();
    format!("sha256:{}", value(rows.iter().next().unwrap(), "hash"))
}

#[test]
fn explicit_versions_and_all_packaged_queries_parse() {
    assert!(evidence::profile(0).is_none());
    assert!(evidence::profile(2).is_none());
    assert!(evidence::profile(u32::MAX).is_none());
    assert_eq!(profile().version, 1);
    let ds = Dataset::memory();
    ds.load_str(profile().vocabulary, RdfFormat::Turtle)
        .unwrap();
    ds.load_str(profile().shapes, RdfFormat::Turtle).unwrap();
    ds.load_str(profile().manifest, RdfFormat::Turtle).unwrap();
    let resource_digest = |id: &str, source: &str| {
        let rows = ds
            .select(&format!(
                "PREFIX ev: <{EV}> SELECT ?digest WHERE {{ <{EV}{id}> ev:contentDigest ?digest }}"
            ))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(value(rows.iter().next().unwrap(), "digest"), digest(source));
    };
    resource_digest("profile:1", profile().vocabulary);
    resource_digest("shapes:1", profile().shapes);
    for (prefix, resources) in [
        ("query", profile().competency_queries),
        ("compatibility", profile().compatibility_queries),
    ] {
        for resource in resources {
            resource_digest(&format!("{prefix}:1:{}", resource.name), resource.query);
        }
    }
    assert!(
        ds.ask("PREFIX ev: <urn:x-sparkles:evidence:> ASK { <urn:x-sparkles:evidence:profile:1> ev:profileVersion 1 }")
            .unwrap()
    );
    for resource in profile()
        .competency_queries
        .iter()
        .chain(profile().compatibility_queries)
    {
        spargebra::SparqlParser::new()
            .parse_query(resource.query)
            .unwrap_or_else(|e| panic!("{}: {e}", resource.name));
    }
}

#[test]
fn triple_terms_directional_languages_and_precise_literals_round_trip() {
    let ds = data();
    let mut encoded = Vec::new();
    ds.dump(&mut encoded, RdfFormat::NQuads).unwrap();
    let round_trip = Dataset::memory();
    round_trip
        .load_str(std::str::from_utf8(&encoded).unwrap(), RdfFormat::NQuads)
        .unwrap();
    let values = |ds: &Dataset| {
        ds.quads(None, None, None, None)
            .collect::<Result<HashSet<_>, _>>()
            .unwrap()
    };
    assert_eq!(values(&ds), values(&round_trip));
    let precision = ds
        .select("PREFIX ev: <urn:x-sparkles:evidence:> SELECT ?value WHERE { <https://example.org/confidence> ev:value ?value }")
        .unwrap();
    assert_eq!(
        value(precision.iter().next().unwrap(), "value"),
        "0.75000000000000000001"
    );
    let direction = ds
        .select("SELECT ?label WHERE { <https://example.org/note> <http://www.w3.org/2000/01/rdf-schema#label> ?label }")
        .unwrap();
    assert_eq!(
        direction
            .iter()
            .next()
            .unwrap()
            .get("label")
            .unwrap()
            .to_string(),
        "\"تنبيه\"@ar--rtl"
    );
}

#[test]
fn independent_occurrences_do_not_assert_propositions_or_collapse_polarity() {
    let ds = data();
    assert!(
        !ds.ask("ASK { <https://example.org/release> <https://example.org/affectedBy> <https://example.org/vulnerability> }")
            .unwrap()
    );
    let occurrences = ds.select(query("claim-occurrences.rq")).unwrap();
    let identities: BTreeSet<_> = occurrences.iter().map(|r| value(r, "claim")).collect();
    assert_eq!(
        identities,
        [
            "https://example.org/claim1",
            "https://example.org/claim2",
            "https://example.org/claim3"
        ]
        .into_iter()
        .collect()
    );
    let scopes = ds.select("PREFIX ex: <https://example.org/> SELECT ?claim ?scope WHERE { ?claim ex:configuration ?scope } ORDER BY ?claim").unwrap();
    let scopes: Vec<_> = scopes
        .iter()
        .map(|row| (value(row, "claim"), value(row, "scope")))
        .collect();
    assert_eq!(
        scopes,
        vec![
            ("https://example.org/claim1", "https://example.org/linux"),
            ("https://example.org/claim2", "https://example.org/linux"),
            ("https://example.org/claim3", "https://example.org/windows")
        ]
    );
    let propositions: HashSet<_> = occurrences
        .iter()
        .map(|r| r.get("proposition").unwrap().clone())
        .collect();
    assert_eq!(propositions.len(), 1);
    let status = ds.select(query("assertion-status.rq")).unwrap();
    assert_eq!(status.len(), 3);
    assert!(status.iter().all(|row| value(row, "asserted") == "false"));
    let conflicts = ds.select(query("opposite-polarities.rq")).unwrap();
    assert_eq!(conflicts.len(), 2);
    assert!(
        conflicts
            .iter()
            .all(|row| value(row, "negative") == format!("{EX}claim2"))
    );
}

#[test]
fn mentions_lineage_and_missing_support_do_not_become_corroboration() {
    let ds = data();
    let relations = ds.select(query("evidence-relations.rq")).unwrap();
    let relations: BTreeSet<_> = relations
        .iter()
        .map(|r| value(r, "relation").to_string())
        .collect();
    assert_eq!(
        relations,
        ["Supports", "Contradicts", "Mentions"]
            .map(|r| format!("{EV}{r}"))
            .into_iter()
            .collect()
    );
    let lineage = ds.select(query("source-lineage.rq")).unwrap();
    assert_eq!(lineage.len(), 3);
    let unknown = lineage
        .iter()
        .find(|r| value(r, "revision") == format!("{EX}revision2"))
        .unwrap();
    assert_eq!(value(unknown, "lineageStatus"), "unknown");
    let unsupported = ds.select(query("unsupported-claims.rq")).unwrap();
    let unsupported: BTreeSet<_> = unsupported
        .iter()
        .map(|r| value(r, "claim").to_string())
        .collect();
    assert_eq!(
        unsupported,
        [format!("{EX}claim2"), format!("{EX}claim3")]
            .into_iter()
            .collect()
    );
}

#[test]
fn retained_fixture_bytes_unicode_positions_and_json_pointer_digests_match() {
    let ds = data();
    let field = ds
        .select("PREFIX ev: <urn:x-sparkles:evidence:> SELECT ?digest ?pointer ?valueDigest WHERE { <https://example.org/revision1> ev:contentDigest ?digest . <https://example.org/fieldRef> ev:jsonPointer ?pointer ; ev:valueDigest ?valueDigest }")
        .unwrap();
    let field = field.iter().next().unwrap();
    let raw = include_str!("../../../testsuite/evidence/v1/source.json");
    assert_eq!(value(field, "digest"), digest(raw));
    let json: serde_json::Value = serde_json::from_str(raw).unwrap();
    let selected = json.pointer(value(field, "pointer")).unwrap();
    assert_eq!(selected, "Café");
    assert_eq!(
        value(field, "valueDigest"),
        digest(&serde_json::to_string(selected).unwrap())
    );
    let passages = ds.select(query("passage-selectors.rq")).unwrap();
    let passage = passages.iter().next().unwrap();
    let raw_text = include_str!("../../../testsuite/evidence/v1/source.txt");
    let start: usize = value(passage, "start").parse().unwrap();
    let end: usize = value(passage, "end").parse().unwrap();
    let rendition = ds.select("PREFIX ev: <urn:x-sparkles:evidence:> SELECT ?digest WHERE { <https://example.org/text> ev:contentDigest ?digest }").unwrap();
    assert_eq!(
        value(rendition.iter().next().unwrap(), "digest"),
        digest(raw_text)
    );
    let selected: String = raw_text.chars().skip(start).take(end - start).collect();
    assert_eq!(selected, "Café");
    assert_eq!(selected.chars().count(), 4);
    assert_eq!(selected.len(), 5);
    assert_eq!(value(passage, "quote"), selected);
    assert_eq!(value(passage, "digest"), digest(&selected));
}

#[test]
fn graph_kind_preserves_default_versus_a_named_graph_with_the_same_term() {
    let ds = data();
    let rows = ds.select(query("stored-fact-references.rq")).unwrap();
    assert_eq!(rows.len(), 2);
    let default = rows
        .iter()
        .find(|r| value(r, "kind") == format!("{EV}DefaultGraph"))
        .unwrap();
    assert!(default.get("graph").is_none());
    let named = rows
        .iter()
        .find(|r| value(r, "kind") == format!("{EV}NamedGraph"))
        .unwrap();
    assert_eq!(value(named, "graph"), format!("{EV}DefaultGraph"));
}

#[test]
fn compatibility_views_preserve_ids_and_legacy_asserting_semantics_without_writes() {
    let ds = data();
    ds.load_str(LEGACY, RdfFormat::TriG).unwrap();
    let before = ds.head_commit();
    for resource in profile().compatibility_queries {
        let projected = ds.construct(resource.query).unwrap();
        assert!(!projected.is_empty(), "{}", resource.name);
        assert_eq!(ds.head_commit().seq, before.seq);
        let mapped = Dataset::memory();
        let text: String = projected.iter().map(|t| format!("{t} .\n")).collect();
        mapped.load_str(&text, RdfFormat::NTriples).unwrap();
        assert!(
            !mapped
                .ask("PREFIX ev: <urn:x-sparkles:evidence:> ASK { ?r a ev:Claim }")
                .unwrap()
        );
        assert!(
            !mapped
                .ask("PREFIX ev: <urn:x-sparkles:evidence:> ASK { ?r ev:firstAvailableAt ?time }")
                .unwrap()
        );
        if resource.name == "legacy-assertions.rq" {
            assert!(
                mapped
                    .ask("PREFIX ev: <urn:x-sparkles:evidence:> ASK { <https://example.org/legacyReifier> a ev:LegacyAssertion ; ev:legacyAssertion true }")
                    .unwrap()
            );
            assert!(mapped.ask("PREFIX ev: <urn:x-sparkles:evidence:> PREFIX prov: <http://www.w3.org/ns/prov#> ASK { <https://example.org/legacyReifier> ev:legacySource <https://example.org/legacySource> ; prov:wasGeneratedBy <https://example.org/legacyActivity> . <https://example.org/legacyActivity> prov:wasAssociatedWith <https://example.org/legacyPrincipal> }").unwrap());
            assert!(
                !mapped
                    .ask("ASK { <https://example.org/release> <https://example.org/affectedBy> <https://example.org/vulnerability> }")
                    .unwrap()
            );
        } else {
            assert!(mapped.ask("PREFIX ev: <urn:x-sparkles:evidence:> ASK { <https://example.org/uncitedRendition> a ev:LegacyRendition }").unwrap());
            assert!(
                mapped
                    .ask("PREFIX oa: <http://www.w3.org/ns/oa#> ASK { <https://example.org/legacyRendition#char=10,14> oa:hasSource <https://example.org/legacyRendition> ; oa:hasSelector ?s . ?s oa:start 10 ; oa:end 14 }")
                    .unwrap()
            );
        }
    }
    assert!(
        ds.ask("ASK { GRAPH <https://example.org/legacyGraph> { <https://example.org/release> <https://example.org/affectedBy> <https://example.org/vulnerability> } }")
            .unwrap()
    );
    assert!(ds.write_guard().is_none());
}

#[cfg(feature = "shacl")]
#[test]
fn valid_profile_and_invalid_structural_overlays_have_independent_expected_results() {
    let shapes = sparkles_shacl::Shapes::parse(profile().shapes, RdfFormat::Turtle, None).unwrap();
    let validate = |ds: &Dataset| ds.validation().shacl(&shapes, &Default::default()).unwrap();
    assert!(validate(&data()).conforms);
    let invalid = [
        (
            "two propositions",
            include_str!("../../../testsuite/evidence/v1/invalid/duplicate-proposition.ttl"),
        ),
        (
            "not a triple",
            include_str!(
                "../../../testsuite/evidence/v1/invalid/asserted-term-instead-of-triple.ttl"
            ),
        ),
        (
            "unknown kind",
            include_str!("../../../testsuite/evidence/v1/invalid/unknown-kind.ttl"),
        ),
        (
            "short digest",
            include_str!("../../../testsuite/evidence/v1/invalid/short-digest.ttl"),
        ),
        (
            "missing timezone",
            include_str!("../../../testsuite/evidence/v1/invalid/missing-timezone.ttl"),
        ),
        (
            "empty span",
            include_str!("../../../testsuite/evidence/v1/invalid/empty-span.ttl"),
        ),
        (
            "unknown relation",
            include_str!("../../../testsuite/evidence/v1/invalid/unknown-relation.ttl"),
        ),
        (
            "confidence",
            include_str!("../../../testsuite/evidence/v1/invalid/confidence-out-of-range.ttl"),
        ),
        (
            "default graph",
            include_str!("../../../testsuite/evidence/v1/invalid/default-graph-with-name.ttl"),
        ),
        (
            "JSON pointer",
            include_str!("../../../testsuite/evidence/v1/invalid/invalid-json-pointer.ttl"),
        ),
        (
            "blank identity",
            include_str!("../../../testsuite/evidence/v1/invalid/blank-record-identity.ttl"),
        ),
        (
            "incomplete reference",
            include_str!("../../../testsuite/evidence/v1/invalid/incomplete-stored-reference.ttl"),
        ),
        (
            "missing extraction method",
            include_str!("../../../testsuite/evidence/v1/invalid/missing-extraction-method.ttl"),
        ),
        (
            "missing source key",
            include_str!("../../../testsuite/evidence/v1/invalid/missing-source-key.ttl"),
        ),
        (
            "mismatched passage revision",
            include_str!("../../../testsuite/evidence/v1/invalid/mismatched-passage-revision.ttl"),
        ),
        (
            "mismatched passage reference",
            include_str!("../../../testsuite/evidence/v1/invalid/mismatched-passage-reference.ttl"),
        ),
    ];
    for (name, overlay) in invalid {
        let ds = data();
        ds.load_str(overlay, RdfFormat::Turtle).unwrap();
        let report = validate(&ds);
        assert!(!report.conforms, "{name} unexpectedly conforms");
        assert!(report.violations() > 0, "{name}: {report:?}");
    }
}

#[cfg(feature = "shacl")]
#[test]
fn confidence_domain_preserves_exact_decimal_bounds_and_other_scales() {
    let shapes = sparkles_shacl::Shapes::parse(profile().shapes, RdfFormat::Turtle, None).unwrap();
    let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
        "../../../testsuite/evidence/v1/confidence-cases.json"
    ))
    .unwrap();
    for case in cases {
        let lexical = case["lexical"].as_str().unwrap();
        let datatype = case["datatype"].as_str().unwrap();
        let value = Literal::new_typed_literal(
            lexical,
            oxrdf::NamedNode::new(format!("http://www.w3.org/2001/XMLSchema#{datatype}")).unwrap(),
        );
        let ds = Dataset::memory();
        ds.load_str(&format!("PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#> <urn:confidence> a ev:Confidence ; ev:value {value} ; ev:scale ev:ZeroToOne ; prov:wasAttributedTo <urn:author> ."), RdfFormat::Turtle).unwrap();
        let report = ds.validation().shacl(&shapes, &Default::default()).unwrap();
        assert_eq!(
            report.conforms,
            case["conforms"].as_bool().unwrap(),
            "{lexical:?} as {datatype}: {report:?}"
        );
    }
    let ds = Dataset::memory();
    ds.load_str(&format!("PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#> [] a ev:Confidence ; ev:value \"high\" ; ev:scale <urn:ordinal-scale> ; prov:wasAttributedTo <urn:author> ."), RdfFormat::Turtle).unwrap();
    assert!(
        ds.validation()
            .shacl(&shapes, &Default::default())
            .unwrap()
            .conforms
    );
}
