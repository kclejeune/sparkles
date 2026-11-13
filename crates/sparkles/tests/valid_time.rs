//! C27 resource acceptance. Membership is checked against an independent finite
//! model that enumerates concrete intervals, rather than repeating the query formula.
use oxrdf::{Literal, Term};
use serde_json::{Value, json};
use sparkles::Dataset;
use sparkles::history::{At, SnapshotOptions};
use sparkles::io::RdfFormat;
use sparkles::sparql::{QueryOptions, QueryResult};
use sparkles::stored::Definition;
use sparkles::valid_time::profile;
use std::collections::BTreeMap;

const VT: &str = "urn:x-sparkles:valid-time:";
const GRAPH: &str = "urn:test:claims";
const PREFIX: &str = "@prefix vt: <urn:x-sparkles:valid-time:> .\n@prefix ev: <urn:x-sparkles:evidence:> .\n@prefix prov: <http://www.w3.org/ns/prov#> .\n@prefix xsd: <http://www.w3.org/2001/XMLSchema#> .\n@prefix ex: <urn:test:> .\n";
const NORMALIZATION: &str = "ex:n a vt:Normalization ; vt:status vt:Normalized ; vt:method ex:explicit-offset ; vt:methodVersion \"1\" ; vt:calendar vt:Gregorian .\n";

fn definition(query: usize) -> Definition {
    let resource = &profile(1).unwrap().queries[query];
    let definition: Definition = serde_json::from_str(resource.definition).unwrap();
    assert_eq!(definition.query, resource.query);
    definition.check().unwrap();
    definition
}

fn options(query: usize, mut given: BTreeMap<String, Value>) -> QueryOptions {
    given.insert("claimGraph".into(), json!(GRAPH));
    QueryOptions {
        initial_bindings: definition(query).bind(&given, &BTreeMap::new()).unwrap(),
        ..Default::default()
    }
}

fn instant(second: i32) -> String {
    format!("2026-01-01T00:00:{:02}Z", second + 10)
}

fn verdicts(result: QueryResult) -> BTreeMap<String, String> {
    let claim = result.vars.iter().position(|v| v == "claim").unwrap();
    let verdict = result.vars.iter().position(|v| v == "verdict").unwrap();
    let revision = result
        .vars
        .iter()
        .position(|v| v == "sourceRevision")
        .unwrap();
    let status = result
        .vars
        .iter()
        .position(|v| v == "normalizationStatus")
        .unwrap();
    let mut out = BTreeMap::new();
    for row in result.rows() {
        assert!(matches!(row[revision], Some(Term::NamedNode(_))));
        assert!(matches!(row[status], Some(Term::NamedNode(_))));
        let Term::NamedNode(claim) = row[claim].as_ref().unwrap() else {
            panic!()
        };
        let Term::Literal(verdict) = row[verdict].as_ref().unwrap() else {
            panic!()
        };
        assert!(
            out.insert(claim.as_str().into(), verdict.value().into())
                .is_none()
        );
    }
    out
}

fn run(ds: &Dataset, query: usize, given: Value) -> BTreeMap<String, String> {
    verdicts(
        ds.query_with(
            profile(1).unwrap().queries[query].query,
            &options(query, serde_json::from_value(given).unwrap()),
        )
        .unwrap(),
    )
}

#[derive(Clone, Copy, Debug)]
enum Endpoint {
    Known(i32, i32),
    Unknown,
    Unbounded,
}

fn boundary(id: &str, endpoint: Endpoint) -> String {
    let base = format!("ex:{id} a vt:Boundary ; ");
    match endpoint {
        Endpoint::Known(lo, hi) => format!(
            "{base}vt:kind vt:Known ; vt:earliest \"{}\"^^xsd:dateTime ; vt:latest \"{}\"^^xsd:dateTime ; vt:sourceValue \"source-{lo}-{hi}\" ; vt:precision vt:Instant .\n",
            instant(lo),
            instant(hi)
        ),
        Endpoint::Unknown => {
            format!("{base}vt:kind vt:Unknown ; vt:sourceValue \"unresolved\" .\n")
        }
        Endpoint::Unbounded => format!("{base}vt:kind vt:Unbounded .\n"),
    }
}

fn claim(id: &str, start: Endpoint, end: Endpoint) -> String {
    format!(
        "ex:{id} a ev:Claim ; prov:wasDerivedFrom ex:revision-{id} ; vt:validDuring ex:i-{id} .\nex:i-{id} a vt:Interval ; vt:start ex:s-{id} ; vt:end ex:e-{id} ; vt:normalization ex:n .\n{}{}",
        boundary(&format!("s-{id}"), start),
        boundary(&format!("e-{id}"), end)
    )
}

// Enumerate all concrete realizations. Sentinels lie outside every generated
// selector and approximate only explicit infinities, never unknown endpoints.
fn oracle(start: Endpoint, end: Endpoint, q0: i32, q1: Option<i32>) -> &'static str {
    let outside_start =
        matches!(start, Endpoint::Known(lo, _) if q1.map_or(q0 < lo, |q1| q1 <= lo));
    let outside_end = matches!(end, Endpoint::Known(_, hi) if q0 >= hi);
    if outside_start || outside_end {
        return "outside";
    }
    if matches!(start, Endpoint::Unknown) || matches!(end, Endpoint::Unknown) {
        return "unknown";
    }
    let starts: Vec<_> = match start {
        Endpoint::Known(a, b) => (a..=b).collect(),
        Endpoint::Unbounded => vec![-100],
        Endpoint::Unknown => unreachable!(),
    };
    let ends: Vec<_> = match end {
        Endpoint::Known(a, b) => (a..=b).collect(),
        Endpoint::Unbounded => vec![100],
        Endpoint::Unknown => unreachable!(),
    };
    let mut values = vec![];
    for start in starts {
        for end in &ends {
            assert!(start < *end);
            let points: Vec<_> = (start..*end).collect();
            values.push(match q1 {
                None => points.contains(&q0),
                Some(q1) => (q0..q1).any(|t| points.contains(&t)),
            });
        }
    }
    if values.iter().all(|v| *v) {
        "definite"
    } else if values.iter().any(|v| *v) {
        "possible"
    } else {
        "outside"
    }
}

#[test]
fn explicit_versions_and_typed_definitions() {
    assert!(profile(0).is_none());
    assert!(profile(2).is_none());
    assert!(profile(u32::MAX).is_none());
    let p = profile(1).unwrap();
    assert_eq!(p.iri, format!("{VT}profile:1"));
    let precision =
        json!({"claimGraph":GRAPH,"validAt":"2026-01-01T00:00:00.000000000000000000001Z"});
    assert!(
        definition(0)
            .bind(
                &serde_json::from_value(precision).unwrap(),
                &BTreeMap::new()
            )
            .is_err()
    );
    let ds = Dataset::memory();
    ds.load_str(p.vocabulary, RdfFormat::Turtle).unwrap();
    for query in 0..2 {
        let definition = definition(query);
        let given = if query == 0 {
            json!({"claimGraph": GRAPH, "policy": "unknown-policy", "validAt": instant(1)})
        } else {
            json!({"claimGraph": GRAPH, "policy": "unknown-policy", "validFrom": instant(1), "validTo": instant(2)})
        };
        assert!(
            definition
                .bind(&serde_json::from_value(given).unwrap(), &BTreeMap::new())
                .is_err()
        );
        let value = if query == 0 {
            json!({"claimGraph": "not an iri", "validAt": instant(1)})
        } else {
            json!({"claimGraph": "not an iri", "validFrom": instant(1), "validTo": instant(2)})
        };
        assert!(
            definition
                .bind(&serde_json::from_value(value).unwrap(), &BTreeMap::new())
                .is_err()
        );
    }
}

#[test]
fn templates_match_independent_enumerated_interval_oracle() {
    use Endpoint::*;
    let starts = [Known(0, 0), Known(0, 2), Unknown, Unbounded];
    let ends = [Known(4, 4), Known(4, 6), Unknown, Unbounded];
    let mut data = format!("{PREFIX}{NORMALIZATION}");
    let mut cases = vec![];
    for (s, start) in starts.into_iter().enumerate() {
        for (e, end) in ends.into_iter().enumerate() {
            let id = format!("c{s}{e}");
            data.push_str(&claim(&id, start, end));
            cases.push((format!("urn:test:{id}"), start, end));
        }
    }
    let ds = Dataset::memory();
    ds.load_str_into(&data, RdfFormat::Turtle, GRAPH).unwrap();
    for q0 in -1..=7 {
        for (query, q1) in [(0, None), (1, Some(q0 + 1)), (1, Some(q0 + 3))] {
            let given = if query == 0 {
                json!({"validAt":instant(q0),"policy":"all"})
            } else {
                json!({"validFrom":instant(q0),"validTo":instant(q1.unwrap()),"policy":"all"})
            };
            let got = run(&ds, query, given);
            assert_eq!(got.len(), cases.len());
            for (id, start, end) in &cases {
                assert_eq!(
                    got[id],
                    oracle(*start, *end, q0, q1),
                    "{id} {start:?} {end:?} {q0} {q1:?}"
                );
            }
        }
    }
    // The negative selector must not turn missing knowledge into a false fact.
    let outside = run(&ds, 0, json!({"validAt":instant(3),"policy":"outsideOnly"}));
    assert!(outside.is_empty());
    let unknown = run(&ds, 0, json!({"validAt":instant(3),"policy":"unknownOnly"}));
    assert_eq!(unknown.len(), 7);
}

#[test]
fn missing_interval_and_unnormalized_source_remain_unknown() {
    let ds = Dataset::memory();
    ds.load_str_into(
        include_str!("../../../testsuite/valid-time/v1/unresolved.ttl"),
        RdfFormat::Turtle,
        GRAPH,
    )
    .unwrap();
    let all = run(
        &ds,
        0,
        json!({"validAt":"2026-01-01T00:00:00Z","policy":"all"}),
    );
    assert_eq!(all.len(), 5);
    assert!(all.values().all(|v| v == "unknown"));
    assert!(
        run(
            &ds,
            0,
            json!({"validAt":"2026-01-01T00:00:00Z","policy":"outsideOnly"})
        )
        .is_empty()
    );
}

#[test]
fn offsets_fractions_touching_and_selector_validation() {
    let ds = Dataset::memory();
    ds.load_str_into(
        include_str!("../../../testsuite/valid-time/v1/exact.ttl"),
        RdfFormat::Turtle,
        GRAPH,
    )
    .unwrap();
    let a = run(
        &ds,
        0,
        json!({"validAt":"2026-01-01T00:00:00.000000001Z","policy":"all"}),
    );
    let b = run(
        &ds,
        0,
        json!({"validAt":"2025-12-31T19:00:00.000000001-05:00","policy":"all"}),
    );
    assert_eq!(a, b);
    assert_eq!(a["urn:test:exact"], "definite");
    assert_eq!(
        run(
            &ds,
            0,
            json!({"validAt":"2026-01-01T00:00:00.000000002Z","policy":"all"})
        )["urn:test:exact"],
        "outside"
    );
    assert_eq!(
        run(
            &ds,
            1,
            json!({"validFrom":"2026-01-01T00:00:00.000000002Z","validTo":"2026-01-01T00:00:01Z","policy":"all"})
        )["urn:test:exact"],
        "outside"
    );
    assert!(
        run(
            &ds,
            0,
            json!({"validAt":"2026-01-01T00:00:00","policy":"all"})
        )
        .is_empty()
    );
    for end in ["2026-01-01T00:00:00Z", "2025-12-31T00:00:00Z"] {
        assert!(
            run(
                &ds,
                1,
                json!({"validFrom":"2026-01-01T00:00:00Z","validTo":end,"policy":"all"})
            )
            .is_empty()
        );
    }
}

#[test]
fn retained_commits_exclude_late_imports_and_preserve_corrections_without_assertion() {
    // Snapshot names describe the example knowledge states; authority comes from
    // actual pinned commits, never these names or client-written availability RDF.
    let ds = Dataset::memory();
    ds.load_str_into(
        &format!("{PREFIX}{NORMALIZATION}"),
        RdfFormat::Turtle,
        GRAPH,
    )
    .unwrap();
    ds.snapshots()
        .create("february", &At::Head, &SnapshotOptions::default())
        .unwrap();
    let before = ds.head_commit().seq;
    let mut data = format!(
        "{PREFIX}{}",
        claim("original", Endpoint::Known(0, 0), Endpoint::Known(4, 4))
    );
    data.push_str("ex:original <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( ex:item ex:relatedTo ex:thing )>> . ex:revision-original ev:firstAvailableAt \"2026-01-01T00:00:00Z\"^^xsd:dateTime ; ev:publishedAt \"2026-01-01T00:00:00Z\"^^xsd:dateTime .\nex:assessment-original ev:target ex:original ; ev:conclusion ev:Unreviewed .");
    ds.load_str_into(&data, RdfFormat::Turtle, GRAPH).unwrap();
    ds.snapshots()
        .create("march", &At::Head, &SnapshotOptions::default())
        .unwrap();
    let original = ds.head_commit().seq;
    ds.load_str_into(
        &format!(
            "{PREFIX}{}ex:revision-correction prov:wasRevisionOf ex:revision-original .",
            claim("correction", Endpoint::Known(1, 1), Endpoint::Known(3, 3))
        ),
        RdfFormat::Turtle,
        GRAPH,
    )
    .unwrap();
    let opts = options(
        0,
        serde_json::from_value(json!({"validAt":instant(2),"policy":"all"})).unwrap(),
    );
    let read = |seq| {
        let (snap, _) = ds
            .store()
            .snapshot_at(&At::Commit(seq), &Default::default())
            .unwrap();
        verdicts(
            sparkles::sparql::query(snap, profile(1).unwrap().queries[0].query, &opts).unwrap(),
        )
    };
    assert!(read(before).is_empty());
    assert_eq!(read(original).len(), 1);
    assert_eq!(read(ds.head_commit().seq).len(), 2);
    let (snap, _) = ds
        .store()
        .snapshot_at(&At::Commit(original), &Default::default())
        .unwrap();
    assert!(
        sparkles::sparql::query(
            snap,
            "ASK { GRAPH <urn:test:claims> { <urn:test:assessment-original> ?p ?o } }",
            &Default::default()
        )
        .unwrap()
        .boolean
    );
    assert!(!ds.query("ASK { GRAPH <urn:test:claims> { <urn:test:item> <urn:test:relatedTo> <urn:test:thing> } }").unwrap().boolean);
    assert!(ds.query("ASK { GRAPH <urn:test:claims> { <urn:test:revision-original> <urn:x-sparkles:evidence:firstAvailableAt> ?arbitraryClientValue } }").unwrap().boolean);
    // Ordinary RDF in an unowned graph accepts any value. This test proves snapshot
    // isolation only. The `availability` tests below cover A4's server-owned
    // availability and the refusal of backdating in owned graphs.
}

#[cfg(feature = "shacl")]
fn conforms(data: &str) -> bool {
    let ds = Dataset::memory();
    ds.load_str(data, RdfFormat::Turtle).unwrap();
    let shapes =
        sparkles_shacl::Shapes::parse(profile(1).unwrap().shapes, RdfFormat::Turtle, None).unwrap();
    sparkles_shacl::validate(&ds.store().snapshot(), &shapes, &Default::default())
        .unwrap()
        .conforms
}

#[cfg(feature = "shacl")]
#[test]
fn shapes_accept_profile_fixtures_and_reject_causal_boundary_mutations() {
    let valid = include_str!("../../../testsuite/valid-time/v1/exact.ttl");
    assert!(conforms(valid));
    assert!(conforms(
        &valid.replace("ex:s ", "_:s ").replace("ex:e ", "_:e ")
    ));
    assert!(conforms(include_str!(
        "../../../testsuite/valid-time/v1/unresolved.ttl"
    )));
    assert!(conforms(include_str!(
        "../../../testsuite/valid-time/v1/timezone-policy.ttl"
    )));
    for (old, new) in [
        ("00:00:00.000000002Z", "00:00:00.000000001Z"), // zero length
        ("00:00:00.000000002Z", "00:00:00Z"),           // reversed
        ("00:00:00.000000001Z", "00:00:00.000000001"),  // no offset
        ("00:00:00.000000001Z", "00:00:00.000000000000000000001Z"), // unsupported finite precision
        ("vt:kind vt:Known", "vt:kind vt:Unknown"),     // finite unknown
        ("vt:end ex:e", "vt:end ex:e, ex:s"),           // duplicate endpoint
        (
            "vt:earliest \"2026-01-01T00:00:00.000000001Z\"",
            "vt:earliest \"2026-01-01T00:00:00.000000003Z\"",
        ), // reversed endpoint envelope
    ] {
        assert_ne!(valid.replace(old, new), valid);
        assert!(
            !conforms(&valid.replace(old, new)),
            "mutation {old} -> {new}"
        );
    }
    let coarse = format!(
        "{PREFIX}{NORMALIZATION}{}ex:event vt:occurredAt ex:s-coarse .",
        claim("coarse", Endpoint::Known(0, 2), Endpoint::Known(4, 4))
    );
    assert!(!conforms(&coarse)); // uncertain event boundary is not an exact instant
    let overlap = format!(
        "{PREFIX}{NORMALIZATION}{}",
        claim("overlap", Endpoint::Known(0, 4), Endpoint::Known(3, 6))
    );
    assert!(!conforms(&overlap));
}

#[test]
fn manifest_binds_every_embedded_resource() {
    let p = profile(1).unwrap();
    let ds = Dataset::memory();
    ds.load_str(p.manifest, RdfFormat::Turtle).unwrap();
    for (name, source) in [
        ("profile:1".to_string(), p.vocabulary),
        ("shapes:1".to_string(), p.shapes),
    ]
    .into_iter()
    .chain(p.queries.iter().flat_map(|q| {
        [
            (format!("query:1:{}", q.name), q.query),
            (format!("definition:1:{}", q.name), q.definition),
        ]
    })) {
        let query = format!(
            "PREFIX vt: <{VT}> SELECT ?digest (CONCAT(\"sha256:\", SHA256({})) AS ?actual) WHERE {{ <{VT}{name}> vt:profileVersion 1 ; vt:contentDigest ?digest }}",
            Literal::new_simple_literal(source)
        );
        let rows = ds.select(&query).unwrap();
        assert_eq!(rows.len(), 1);
        let row = rows.iter().next().unwrap();
        assert_eq!(row.get("digest"), row.get("actual"), "{name}");
    }
}

#[test]
fn every_policy_selects_only_its_explicit_verdicts() {
    let ds = Dataset::memory();
    let mut data = format!("{PREFIX}{NORMALIZATION}");
    for (id, start, end) in [
        ("exact", Endpoint::Known(0, 0), Endpoint::Known(4, 4)),
        ("uncertain", Endpoint::Known(0, 2), Endpoint::Known(4, 6)),
        ("unknown", Endpoint::Unknown, Endpoint::Unknown),
    ] {
        data.push_str(&claim(id, start, end));
    }
    ds.load_str_into(&data, RdfFormat::Turtle, GRAPH).unwrap();
    for q0 in [1, 5, 7] {
        for query in 0..2 {
            let base = if query == 0 {
                json!({"validAt":instant(q0)})
            } else {
                json!({"validFrom":instant(q0),"validTo":instant(q0+1)})
            };
            let mut all = base.clone();
            all["policy"] = json!("all");
            let all = run(&ds, query, all);
            for (policy, allowed) in [
                ("definiteOnly", vec!["definite"]),
                ("possibleOrDefinite", vec!["possible", "definite"]),
                ("outsideOnly", vec!["outside"]),
                ("unknownOnly", vec!["unknown"]),
            ] {
                let mut given = base.clone();
                given["policy"] = json!(policy);
                let expected: BTreeMap<_, _> = all
                    .iter()
                    .filter(|(_, v)| allowed.contains(&v.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                assert_eq!(run(&ds, query, given), expected, "{query} {q0} {policy}");
                if policy == "possibleOrDefinite" {
                    assert_eq!(run(&ds, query, base.clone()), expected);
                }
            }
        }
    }
}

#[test]
fn timezone_fixture_keeps_fold_choices_and_quarantines_gap_without_finite_bounds() {
    let ds = Dataset::memory();
    ds.load_str(
        include_str!("../../../testsuite/valid-time/v1/timezone-policy.ttl"),
        RdfFormat::Turtle,
    )
    .unwrap();
    assert!(ds.ask("PREFIX vt: <urn:x-sparkles:valid-time:> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#> ASK { <urn:test:fold-earlier> vt:earliest ?early ; vt:normalization ?en . <urn:test:fold-later> vt:earliest ?late ; vt:normalization ?ln . ?en vt:tzdbVersion \"2025b\" ; vt:foldChoice \"earlier\" . ?ln vt:tzdbVersion \"2025b\" ; vt:foldChoice \"later\" . FILTER(?early = \"2026-11-01T01:30:00-04:00\"^^xsd:dateTime && ?late = \"2026-11-01T01:30:00-05:00\"^^xsd:dateTime && ?early < ?late) }").unwrap());
    assert!(ds.ask("PREFIX vt: <urn:x-sparkles:valid-time:> ASK { <urn:test:gap> vt:kind vt:Unknown ; vt:sourceValue ?source ; vt:normalization ?n . ?n vt:status vt:NonexistentLocalTime ; vt:tzdbVersion \"2025b\" . FILTER NOT EXISTS { <urn:test:gap> vt:earliest|vt:latest ?finite } }").unwrap());
}

#[cfg(feature = "shacl")]
#[test]
fn normalization_shapes_reject_unsupported_normalized_calendar_and_unpinned_fold() {
    let valid = include_str!("../../../testsuite/valid-time/v1/timezone-policy.ttl");
    assert!(conforms(valid));
    assert!(!conforms(&valid.replace(
        "vt:calendar vt:Gregorian",
        "vt:calendar vt:UnknownCalendar"
    )));
    assert!(!conforms(
        &valid.replace("vt:tzdbVersion \"2025b\"", "vt:methodVersion \"1\"")
    ));
}

/// A4 through the authenticated evidence writer (spec C27 §4.3): local availability is
/// the time of the commit that publishes a revision, an as-known state selected by time
/// excludes later imports whatever their publication dates, and every attempt to
/// backdate availability is refused before it publishes.
mod availability {
    use super::*;
    use oxrdf::{NamedNode, Triple};
    use sparkles::access::GraphAccess;
    use sparkles::evidence::resolve::{RecordKind, RecordRequest, Resolution};
    use sparkles::evidence::{
        self, ClaimInput, ClaimKind, EvidenceInput, Polarity, ProposeOptions, Proposed, RecordKey,
        RecordRef, RecordVersion, Settings, Sha256Digest, SourceContent, SourceRevisionInput,
        SourceTimes,
    };
    use sparkles::task::Control;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    const SOURCES: &str = "urn:ev:sources";
    const CLAIMS: &str = "urn:ev:claims";
    const EV: &str = "urn:x-sparkles:evidence:";
    // A new dataset's root commit carries the wall clock, and commit times never
    // decrease, so the test clock runs in 2100, after any wall clock it meets.
    const PUBLISHED: &str = "2100-01-05T00:00:00Z";
    const JANUARY: i64 = 4_103_654_400_000;
    const FEBRUARY: i64 = 4_106_332_800_000;
    const MARCH: i64 = 4_108_752_000_000;
    const MARCH_LEXICAL: &str = "2100-03-15T00:00:00.000Z";

    fn iri(s: &str) -> NamedNode {
        NamedNode::new(s).unwrap()
    }

    fn key(s: &str) -> RecordKey {
        RecordKey::new(s).unwrap()
    }

    #[track_caller]
    fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
        match r {
            Err(e) => e.code().to_string(),
            Ok(v) => panic!("accepted: {v:?}"),
        }
    }

    /// A test clock the store reads for every commit time.
    fn clock(ds: &Dataset, at: i64) -> Arc<AtomicI64> {
        let now = Arc::new(AtomicI64::new(at));
        let read = now.clone();
        ds.store()
            .set_clock(Arc::new(move || read.load(Ordering::SeqCst)));
        now
    }

    fn configure(ds: &Dataset) {
        let settings = Settings {
            enabled: true,
            source_records: vec![SOURCES.into()],
            proposals: vec![CLAIMS.into()],
            ..Settings::default()
        };
        evidence::configure(ds, &settings, &Control::none()).unwrap();
    }

    /// A revision published in January and a claim about it, imported now.
    fn import(source: &str) -> EvidenceInput {
        EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri(&format!("urn:test:{source}")),
                source_key: key(source),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: Sha256Digest::of_bytes(source.as_bytes(), &Control::none()).unwrap(),
                },
                metadata: Default::default(),
                times: SourceTimes {
                    published_at: Some(Literal::new_typed_literal(
                        PUBLISHED,
                        iri("http://www.w3.org/2001/XMLSchema#dateTime"),
                    )),
                    reported_available_at: None,
                },
                lineage: Default::default(),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("claim"),
                occurrence_key: key(source),
                revision: RecordRef::Local(key("rev")),
                proposition: Triple::new(
                    iri("urn:test:release"),
                    iri("urn:test:affectedBy"),
                    iri(&format!("urn:test:{source}-issue")),
                ),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:test:publisher"),
                scope: Default::default(),
                confidence: Default::default(),
            }],
            ..Default::default()
        }
    }

    fn propose(ds: &Dataset, input: &EvidenceInput) -> sparkles::Result<Proposed> {
        evidence::propose(
            None,
            ds,
            ds,
            input,
            &ProposeOptions::new(iri("urn:x-sparkles:principal:importer")),
            &mut |_, _| Some(Arc::new(GraphAccess::all())),
            &Control::none(),
        )
    }

    fn record_iri(p: &Proposed, alias: &str) -> NamedNode {
        p.records
            .iter()
            .find(|r| r.key == alias)
            .unwrap()
            .iri
            .clone()
    }

    fn resolve(ds: &Dataset, record: &NamedNode) -> sparkles::Result<u64> {
        Resolution::new(Control::none())
            .unwrap()
            .record(
                "r",
                ds,
                ds,
                Some(Arc::new(GraphAccess::all())),
                &RecordRequest {
                    iri: record.clone(),
                    kinds: vec![RecordKind::SourceRevision],
                    content_digest: None,
                },
            )
            .map(|v| v.receipt.commit)
    }

    /// The state selected by `at`, as the claim template sees it with every verdict.
    fn claims_at(ds: &Dataset, at: At) -> BTreeMap<String, String> {
        let (snap, _) = ds.store().snapshot_at(&at, &Default::default()).unwrap();
        let resource = &profile(1).unwrap().queries[0];
        let mut given: BTreeMap<String, Value> =
            serde_json::from_value(json!({"validAt": PUBLISHED, "policy": "all"})).unwrap();
        given.insert("claimGraph".into(), json!(CLAIMS));
        let opts = QueryOptions {
            initial_bindings: definition(0).bind(&given, &BTreeMap::new()).unwrap(),
            ..Default::default()
        };
        verdicts(sparkles::sparql::query(snap, resource.query, &opts).unwrap())
    }

    /// The `ev:firstAvailableAt` values of `revision` at the head.
    fn availability(ds: &Dataset, revision: &NamedNode) -> Vec<String> {
        let r = ds
            .query(&format!(
                "SELECT ?t {{ GRAPH <{SOURCES}> {{ <{}> <{EV}firstAvailableAt> ?t }} }}",
                revision.as_str()
            ))
            .unwrap();
        r.rows()
            .into_iter()
            .map(|row| match row[0].as_ref().unwrap() {
                Term::Literal(l) => l.value().to_string(),
                t => panic!("{t}"),
            })
            .collect()
    }

    /// Every way a client can try to write availability into the source-record graph.
    fn backdating_attempts(ds: &Dataset, revision: &NamedNode) {
        let rev = revision.as_str();
        let early = "\"2100-01-15T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime>";
        let head = ds.store().head_commit();
        let before = availability(ds, revision);
        let updates = [
            format!(
                "INSERT DATA {{ GRAPH <{SOURCES}> {{ <{rev}> <{EV}firstAvailableAt> {early} }} }}"
            ),
            format!(
                "DELETE {{ GRAPH <{SOURCES}> {{ <{rev}> <{EV}firstAvailableAt> ?t }} }}
                 INSERT {{ GRAPH <{SOURCES}> {{ <{rev}> <{EV}firstAvailableAt> {early} }} }}
                 WHERE {{ GRAPH <{SOURCES}> {{ <{rev}> <{EV}firstAvailableAt> ?t }} }}"
            ),
            format!("DROP GRAPH <{SOURCES}>"),
        ];
        for u in &updates {
            assert_eq!(code(ds.update(u)), "profile-managed-graph", "{u}");
        }
        let patch = format!("TX .\nA <{rev}> <{EV}firstAvailableAt> {early} <{SOURCES}> .\nTC .\n");
        assert_eq!(
            code(ds.apply_patch(patch.as_bytes(), false)),
            "profile-managed-graph"
        );
        assert_eq!(
            code(ds.load_str_into(
                &format!("<{rev}> <{EV}firstAvailableAt> {early} ."),
                RdfFormat::NTriples,
                SOURCES,
            )),
            "profile-managed-graph"
        );
        // an open claim scope cannot carry it through the typed writer either
        let mut forged = import("forged");
        forged.claims[0].scope.insert(
            format!("{EV}firstAvailableAt"),
            vec![
                Literal::new_typed_literal(
                    "2100-01-15T00:00:00Z",
                    iri("http://www.w3.org/2001/XMLSchema#dateTime"),
                )
                .into(),
            ],
        );
        assert_eq!(code(propose(ds, &forged)), "invalid");
        // no refusal left a trace
        assert_eq!(ds.store().head_commit(), head);
        assert_eq!(availability(ds, revision), before);
    }

    #[test]
    fn a_march_import_about_january_is_absent_from_the_february_state() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let now = clock(&ds, JANUARY);
        ds.update("INSERT DATA { GRAPH <urn:test:other> { <urn:test:a> <urn:test:b> 1 } }")
            .unwrap();
        configure(&ds);
        now.store(MARCH, Ordering::SeqCst);
        let p = propose(&ds, &import("advisory")).unwrap();
        assert!(p.committed);
        let revision = record_iri(&p, "rev");
        // availability is the publishing commit's time, assigned by the server
        let commit = ds.store().commit(p.commit).unwrap();
        assert_eq!(commit.timestamp_ms, MARCH);
        assert_eq!(availability(&ds, &revision), [MARCH_LEXICAL]);
        assert_eq!(resolve(&ds, &revision).unwrap(), p.commit);
        // the claim is visible now, with unknown validity
        let claim = record_iri(&p, "claim");
        let now_rows = claims_at(&ds, At::Head);
        assert_eq!(
            now_rows.get(claim.as_str()).map(String::as_str),
            Some("unknown")
        );
        // the as-known February state predates its availability, though the revision
        // was published in January: publication alone admits nothing
        assert!(claims_at(&ds, At::Time(FEBRUARY)).is_empty());
        // selection by time admits the claim exactly from its availability on
        assert_eq!(
            claims_at(&ds, At::Time(MARCH))
                .get(claim.as_str())
                .map(String::as_str),
            Some("unknown")
        );
        assert!(claims_at(&ds, At::Time(MARCH - 1)).is_empty());
        let (feb, _) = ds
            .store()
            .snapshot_at(&At::Time(FEBRUARY), &Default::default())
            .unwrap();
        let ask = format!("ASK {{ GRAPH ?g {{ <{}> ?p ?o }} }}", revision.as_str());
        assert!(
            !sparkles::sparql::query(feb, &ask, &Default::default())
                .unwrap()
                .boolean
        );
        // every backdating attempt is refused and leaves no trace
        backdating_attempts(&ds, &revision);
        assert!(claims_at(&ds, At::Time(FEBRUARY)).is_empty());
    }

    #[test]
    fn availability_written_outside_the_typed_writer_never_authenticates_or_backdates() {
        let dir = tempfile::tempdir().unwrap();
        let ds = Dataset::open(dir.path()).unwrap();
        let now = clock(&ds, JANUARY);
        ds.update("INSERT DATA { GRAPH <urn:test:other> { <urn:test:a> <urn:test:b> 1 } }")
            .unwrap();
        configure(&ds);
        now.store(MARCH, Ordering::SeqCst);
        let p = propose(&ds, &import("advisory")).unwrap();
        let revision = record_iri(&p, "rev");
        // an administrator detaches the graph, rewrites availability to January and
        // protects the graph again
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "DELETE {{ GRAPH <{SOURCES}> {{ ?r <{EV}firstAvailableAt> ?t }} }}
             INSERT {{ GRAPH <{SOURCES}> {{ ?r <{EV}firstAvailableAt> \"2100-01-15T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ ?r <{EV}firstAvailableAt> ?t }} }}"
        ))
        .unwrap();
        configure(&ds);
        // the rewritten record no longer authenticates
        assert_eq!(code(resolve(&ds, &revision)), "origin-not-reproduced");
        // and the rewrite is a commit of its own time, so February still lacks it
        assert!(ds.store().head_commit().timestamp_ms >= MARCH);
        assert!(claims_at(&ds, At::Time(FEBRUARY)).is_empty());
        // ordinary RDF in an unowned graph claiming January availability is still only
        // data recorded in March
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <urn:test:copy> {{ <urn:test:copy-rev> a <{EV}SourceRevision> ; <{EV}firstAvailableAt> \"2100-01-15T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> ; <{EV}availabilityReceipt> <{}> }} }}",
            p.operation.as_str()
        ))
        .unwrap();
        assert_eq!(
            code(resolve(&ds, &iri("urn:test:copy-rev"))),
            "evidence-unavailable"
        );
        let (feb, _) = ds
            .store()
            .snapshot_at(&At::Time(FEBRUARY), &Default::default())
            .unwrap();
        assert!(
            !sparkles::sparql::query(
                feb,
                "ASK { GRAPH <urn:test:copy> { ?s ?p ?o } }",
                &Default::default()
            )
            .unwrap()
            .boolean
        );
    }

    #[test]
    fn availability_and_refusals_survive_restart_and_a_clock_set_back() {
        let dir = tempfile::tempdir().unwrap();
        let (first, revision) = {
            let ds = Dataset::open(dir.path()).unwrap();
            let now = clock(&ds, JANUARY);
            ds.update("INSERT DATA { GRAPH <urn:test:other> { <urn:test:a> <urn:test:b> 1 } }")
                .unwrap();
            configure(&ds);
            now.store(MARCH, Ordering::SeqCst);
            let p = propose(&ds, &import("advisory")).unwrap();
            (p.commit, record_iri(&p, "rev"))
        };
        let ds = Dataset::open(dir.path()).unwrap();
        // the recovered history keeps the publishing commit's time and the receipt
        assert_eq!(ds.store().commit(first).unwrap().timestamp_ms, MARCH);
        assert_eq!(availability(&ds, &revision), [MARCH_LEXICAL]);
        assert_eq!(resolve(&ds, &revision).unwrap(), first);
        assert!(claims_at(&ds, At::Time(FEBRUARY)).is_empty());
        // the reopened registry still refuses every backdating attempt
        backdating_attempts(&ds, &revision);
        // a server clock set back to January after the restart cannot stamp a later
        // import before the recovered head
        clock(&ds, JANUARY);
        let later = propose(&ds, &import("follow-up")).unwrap();
        let follow = record_iri(&later, "rev");
        assert_eq!(ds.store().commit(later.commit).unwrap().timestamp_ms, MARCH);
        assert_eq!(availability(&ds, &follow), [MARCH_LEXICAL]);
        assert!(claims_at(&ds, At::Time(FEBRUARY)).is_empty());
        assert_eq!(claims_at(&ds, At::Head).len(), 2);
        // both imports share the recovered head's time, so the selector at exactly that
        // time holds both and the millisecond before holds neither
        assert_eq!(claims_at(&ds, At::Time(MARCH)).len(), 2);
        assert!(claims_at(&ds, At::Time(MARCH - 1)).is_empty());
    }
}
