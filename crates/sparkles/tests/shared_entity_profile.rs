//! A01 resource acceptance over fixed synthetic data, not a resolution implementation.
use oxrdf::{NamedNode, Term};
use serde_json::Value;
use sparkles::{Dataset, io::RdfFormat, shared_entity};
use sparkles_core::evidence::{Sha256Digest, select_json_lexical_utf8};
use sparkles_core::task::Control;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::PathBuf;

const SR: &str = "urn:x-sparkles:representation:";
const MANIFEST: &str = include_str!("../../../testsuite/entity-representation/v1/manifest.json");

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testsuite/entity-representation/v1")
}

fn source(path: &str) -> String {
    std::fs::read_to_string(root().join(path)).unwrap()
}

fn json(path: &str) -> Value {
    serde_json::from_str(&source(path)).unwrap()
}

fn profile() -> &'static shared_entity::Profile {
    shared_entity::profile(1).unwrap()
}

fn query(name: &str) -> &'static str {
    profile()
        .competency_queries
        .iter()
        .find(|q| q.name == name)
        .unwrap()
        .query
}

fn data(base: &str, fixture: &str) -> Dataset {
    let ds = Dataset::memory();
    ds.load_str(&source(base), RdfFormat::Turtle).unwrap();
    ds.load_str(&source(fixture), RdfFormat::Turtle).unwrap();
    ds
}

fn hash(bytes: &[u8]) -> String {
    Sha256Digest::of_bytes(bytes, &Control::none())
        .unwrap()
        .to_string()
}

#[test]
fn explicit_versions_and_packaged_resource_digests() {
    for version in [0, 2, u32::MAX] {
        assert!(shared_entity::profile(version).is_none());
    }
    assert_eq!(profile().version, 1);
    assert_eq!(profile().iri, format!("{SR}profile:1"));
    let ds = Dataset::memory();
    for resource in [
        profile().vocabulary,
        profile().shapes,
        profile().policy,
        profile().manifest,
    ] {
        ds.load_str(resource, RdfFormat::Turtle).unwrap();
    }
    let mut expected: BTreeMap<String, String> = [
        (
            "vocabulary.ttl".into(),
            hash(profile().vocabulary.as_bytes()),
        ),
        ("shapes.ttl".into(), hash(profile().shapes.as_bytes())),
        (
            "policies/default.ttl".into(),
            hash(profile().policy.as_bytes()),
        ),
    ]
    .into();
    let mut names = BTreeSet::new();
    for resource in profile().competency_queries {
        assert!(names.insert(resource.name));
        spargebra::SparqlParser::new()
            .parse_query(resource.query)
            .unwrap_or_else(|e| panic!("{}: {e}", resource.name));
        expected.insert(
            format!("queries/{}", resource.name),
            hash(resource.query.as_bytes()),
        );
    }
    assert_eq!(names.len(), 8);
    let rows = ds
        .select(&format!(
            "PREFIX sr: <{SR}> SELECT ?path ?digest WHERE {{ ?resource sr:resourcePath ?path ; sr:contentDigest ?digest }}"
        ))
        .unwrap();
    let actual: BTreeMap<_, _> = rows
        .iter()
        .map(|r| {
            let text = |name| match r.get(name).unwrap() {
                Term::Literal(l) => l.value().to_owned(),
                other => panic!("{other}"),
            };
            (text("path"), text("digest"))
        })
        .collect();
    assert_eq!(rows.len(), expected.len());
    assert_eq!(actual, expected);
}

#[test]
fn all_eight_queries_match_independently_authored_full_term_bindings() {
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    let fixtures = manifest["queryFixtures"].as_array().unwrap();
    assert_eq!(fixtures.len(), 8);
    for fixture in fixtures {
        let name = fixture["query"].as_str().unwrap();
        let expected = json(fixture["expected"].as_str().unwrap());
        let ds = data(
            fixture["base"].as_str().unwrap(),
            fixture["fixture"].as_str().unwrap(),
        );
        let result = ds.select(query(name)).unwrap();
        let columns: Vec<_> = expected["columns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(result.vars, columns, "{name} columns");
        let mut actual: Vec<Vec<Option<String>>> = result
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .map(|c| row.get(c).map(ToString::to_string))
                    .collect()
            })
            .collect();
        let mut expected: Vec<Vec<Option<String>>> =
            serde_json::from_value(expected["rows"].clone()).unwrap();
        actual.sort();
        expected.sort();
        assert_eq!(
            actual, expected,
            "{name} exact term rows, including multiplicity"
        );
    }
}

#[test]
fn retained_bytes_and_exact_c25_lexical_selections_match_the_frozen_sources() {
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    for file in manifest["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap();
        let bytes = std::fs::read(root().join(path)).unwrap();
        assert_eq!(hash(&bytes), file["digest"].as_str().unwrap(), "{path}");
    }
    let conflict = json("expected/source-conflicts.json");
    let original = source(conflict["original"].as_str().unwrap());
    let tampered = source(conflict["conflicting"].as_str().unwrap());
    assert_eq!(
        hash(original.as_bytes()),
        conflict["originalDigest"].as_str().unwrap()
    );
    assert_eq!(
        hash(tampered.as_bytes()),
        conflict["conflictingDigest"].as_str().unwrap()
    );
    assert_ne!(original, tampered);
    assert_ne!(hash(original.as_bytes()), hash(tampered.as_bytes()));
    let references = Dataset::memory();
    references
        .load_str(&source("valid/common.ttl"), RdfFormat::Turtle)
        .unwrap();
    for revision in json("expected/source-selections.json").as_array().unwrap() {
        let bytes = source(revision["path"].as_str().unwrap());
        assert_eq!(hash(bytes.as_bytes()), revision["digest"].as_str().unwrap());
        for selection in revision["selections"].as_array().unwrap() {
            let selected = select_json_lexical_utf8(
                &bytes,
                selection["pointer"].as_str().unwrap(),
                &Control::none(),
            )
            .unwrap();
            assert_eq!(selected.utf8, selection["selectedUtf8"].as_str().unwrap());
            assert_eq!(
                selected.digest.to_string(),
                selection["digest"].as_str().unwrap()
            );
            let reference = NamedNode::new(selection["reference"].as_str().unwrap()).unwrap();
            let digest = oxrdf::Literal::new_simple_literal(selected.digest.to_string());
            assert!(references.ask(&format!(
                "PREFIX ev: <urn:x-sparkles:evidence:> ASK {{ {reference} ev:valueDigest {digest} }}"
            )).unwrap());
        }
    }
}

#[test]
fn fixed_scalar_encoding_vectors_decode_and_conflict_example_is_exact() {
    for vector in json("expected/encoding-vectors.json").as_array().unwrap() {
        let hex = vector["encodedUtf8Hex"].as_str().unwrap();
        let bytes: Vec<_> = hex
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        let actual: String = serde_json::from_slice(&bytes).unwrap();
        let expected: String = vector["codepoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| char::from_u32(u32::from_str_radix(v.as_str().unwrap(), 16).unwrap()).unwrap())
            .collect();
        assert_eq!(actual, expected);
    }
    // This is the reviewed contract's fixed bytes, not a replay encoder's output.
    assert_eq!(
        source("expected/a12-conflict.json"),
        r#"{"code":"decision-conflict","entities":["https://example.org/A","https://example.org/B"],"decisions":["urn:example:d1","urn:example:d2","urn:example:d3","urn:example:d4"]}"#
    );
    let oracle = json("expected/semantic-oracle.json");
    let cases: BTreeSet<_> = oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(cases, (1..=15).map(|n| format!("A{n}")).collect());
}

#[test]
fn described_propositions_do_not_become_asserted_or_logically_identical() {
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    for fixture in manifest["queryFixtures"].as_array().unwrap() {
        let ds = data(
            fixture["base"].as_str().unwrap(),
            fixture["fixture"].as_str().unwrap(),
        );
        assert!(!ds.ask("PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#> ASK { ?claim rdf:reifies ?t . BIND(SUBJECT(?t) AS ?s) BIND(PREDICATE(?t) AS ?p) BIND(OBJECT(?t) AS ?o) { ?s ?p ?o } UNION { GRAPH ?g { ?s ?p ?o } } }").unwrap());
        assert!(!ds.ask("ASK { { ?s <http://www.w3.org/2002/07/owl#sameAs> ?o } UNION { GRAPH ?g { ?s <http://www.w3.org/2002/07/owl#sameAs> ?o } } }").unwrap());
    }
}

#[test]
fn isolated_public_example_and_unknown_boundaries_preserve_their_meanings() {
    let ds = data("valid/public-common.ttl", "valid/public-boundary.ttl");
    let rows = ds.select(query("descriptions.rq")).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows.iter()
            .next()
            .unwrap()
            .get("entity")
            .unwrap()
            .to_string(),
        "<urn:example:a01:A15:publicEntity>"
    );
    assert!(
        !ds.ask("PREFIX sr: <urn:x-sparkles:representation:> ASK { ?record a sr:ResolutionView }")
            .unwrap()
    );
    let software = data("valid/common.ttl", "valid/software-boundaries.ttl");
    assert!(!software.ask("PREFIX sr: <urn:x-sparkles:representation:> ASK { ?relation sr:relationKind sr:DeploymentUses }").unwrap());
    let health = data("valid/common.ttl", "valid/episode-observations.ttl");
    assert!(
        !health
            .ask("PREFIX sr: <urn:x-sparkles:representation:> ASK { ?observation sr:count 22 }")
            .unwrap()
    );
    let previous = health.select("PREFIX sr: <urn:x-sparkles:representation:> SELECT ?geometry WHERE { <urn:example:a01:A11:place1> sr:geometryVersion ?geometry }").unwrap();
    let successor = data("valid/common.ttl", "valid/geometry-successor.ttl");
    let next = successor.select("PREFIX sr: <urn:x-sparkles:representation:> SELECT ?geometry WHERE { <urn:example:a01:A11:place1> sr:geometryVersion ?geometry }").unwrap();
    assert_eq!(
        previous
            .iter()
            .next()
            .unwrap()
            .get("geometry")
            .unwrap()
            .to_string(),
        "<urn:example:a01:geometry1>"
    );
    assert_eq!(
        next.iter()
            .next()
            .unwrap()
            .get("geometry")
            .unwrap()
            .to_string(),
        "<urn:example:a01:geometry2>"
    );
}

#[test]
fn unicode_and_triple_terms_round_trip_without_changing_fixture_identity() {
    let ds = data("valid/common.ttl", "valid/identifiers.ttl");
    let mut bytes = Vec::new();
    ds.dump(&mut bytes, RdfFormat::NQuads).unwrap();
    let other = Dataset::memory();
    other
        .load_str(std::str::from_utf8(&bytes).unwrap(), RdfFormat::NQuads)
        .unwrap();
    let quads = |ds: &Dataset| {
        ds.quads(None, None, None, None)
            .collect::<Result<HashSet<_>, _>>()
            .unwrap()
    };
    assert_eq!(quads(&ds), quads(&other));
}

#[cfg(feature = "shacl")]
#[test]
fn positive_and_negative_structures_have_independent_expected_components() {
    let combined = format!(
        "{}\n{}\n{}",
        sparkles::evidence::profile(1).unwrap().shapes,
        sparkles::valid_time::profile(1).unwrap().shapes,
        profile().shapes
    );
    let shapes = sparkles_shacl::Shapes::parse(&combined, RdfFormat::Turtle, None).unwrap();
    let validate = |ds: &Dataset| ds.validation().shacl(&shapes, &Default::default()).unwrap();
    let manifest: Value = serde_json::from_str(MANIFEST).unwrap();
    for fixture in manifest["positiveFixtures"].as_array().unwrap() {
        let path = fixture["path"].as_str().unwrap();
        let report = validate(&data(fixture["base"].as_str().unwrap(), path));
        assert!(report.conforms, "{path}: {report:?}");
    }
    for case in json("cases.json")["structuralCases"].as_array().unwrap() {
        let base = case["base"].as_str().unwrap();
        let positive = validate(&data(base, case["positive"].as_str().unwrap()));
        assert!(
            positive.conforms,
            "{} positive sibling: {positive:?}",
            case["id"]
        );
        let report = validate(&data(base, case["invalid"].as_str().unwrap()));
        assert!(!report.conforms, "{} unexpectedly conforms", case["id"]);
        assert_eq!(
            report.results.len(),
            1,
            "{} incidental failures: {report:?}",
            case["id"]
        );
        let result = &report.results[0];
        assert_eq!(
            result.source_constraint_component.as_str(),
            format!(
                "http://www.w3.org/ns/shacl#{}",
                case["component"].as_str().unwrap()
            )
        );
        assert_eq!(
            result.focus_node,
            Term::NamedNode(NamedNode::new(case["focus"].as_str().unwrap()).unwrap())
        );
        assert_eq!(report.violations(), 1);
    }
}

#[test]
fn retained_input_manifest_payloads_bind_exact_selected_ids_and_digest_fields() {
    let index = json("expected/input-manifests.json");
    for record in index["records"].as_array().unwrap() {
        let raw = source(record["payload"].as_str().unwrap());
        assert_eq!(hash(raw.as_bytes()), record["digest"].as_str().unwrap());
        let payload: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(payload["profileVersion"], 1);
        assert_eq!(payload.as_object().unwrap().len(), 4);
        let expected = format!(
            "{{\"profileVersion\":1,\"inputIds\":{},\"requiredExtensions\":{},\"extensionIds\":{}}}",
            serde_json::to_string(&payload["inputIds"]).unwrap(),
            serde_json::to_string(&payload["requiredExtensions"]).unwrap(),
            serde_json::to_string(&payload["extensionIds"]).unwrap()
        );
        assert_eq!(raw, expected);
        let ds = data("valid/common.ttl", record["fixture"].as_str().unwrap());
        let manifest = NamedNode::new(record["manifest"].as_str().unwrap()).unwrap();
        for (field, predicate) in [
            ("inputIds", "inputId"),
            ("requiredExtensions", "requiredExtension"),
            ("extensionIds", "extension"),
        ] {
            let expected: Vec<String> = serde_json::from_value(payload[field].clone()).unwrap();
            let mut unique = expected.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(expected, unique);
            for id in &expected {
                NamedNode::new(id).unwrap();
            }
            let rows = ds
                .select(&format!(
                    "SELECT ?id WHERE {{ {manifest} <{SR}{predicate}> ?id }}"
                ))
                .unwrap();
            let mut actual: Vec<_> = rows
                .iter()
                .map(|r| match r.get("id").unwrap() {
                    Term::NamedNode(n) => n.as_str().to_owned(),
                    other => panic!("manifest input must be an IRI: {other}"),
                })
                .collect();
            actual.sort();
            assert_eq!(actual, expected);
        }
        let rows = ds
            .select(&format!(
                "SELECT ?digest WHERE {{ {manifest} <{SR}contentDigest> ?digest }}"
            ))
            .unwrap();
        let mut actual: Vec<_> = rows
            .iter()
            .map(|r| match r.get("digest").unwrap() {
                Term::Literal(l) => l.value().to_owned(),
                other => panic!("digest must be a literal: {other}"),
            })
            .collect();
        let mut expected: Vec<String> =
            serde_json::from_value(record["declaredDigests"].clone()).unwrap();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    }
    let complete = source("expected/a12-comparison.json");
    assert_eq!(
        hash(complete.as_bytes()),
        "sha256:b25e727c33a3de4cb9b73313b9dd072de0ec7dd56c3dd1690c2a9037d5c57ea2"
    );
    let complete: Value = serde_json::from_str(&complete).unwrap();
    assert_eq!(complete["assignments"].as_array().unwrap().len(), 3);
    assert_eq!(complete["conflicts"][0], json("expected/a12-conflict.json"));
    assert!(complete["unknowns"].as_array().unwrap().is_empty());
}

#[test]
fn simulated_withdrawals_have_distinct_c25_inputs_and_artifact_methods_remain_declared() {
    let inputs = json("expected/a5-selection-inputs.json");
    let ds = data("valid/common.ttl", "valid/support-lineage.ttl");
    let mut coordinates = BTreeSet::new();
    for state in inputs["states"].as_array().unwrap() {
        let projection = NamedNode::new(format!(
            "urn:example:a01:{}",
            state["projection"].as_str().unwrap()
        ))
        .unwrap();
        let coordinate = NamedNode::new(format!(
            "urn:example:a01:{}",
            state["coordinate"].as_str().unwrap()
        ))
        .unwrap();
        assert!(coordinates.insert(coordinate.clone()));
        assert!(
            ds.ask(&format!(
                "ASK {{ {projection} <{SR}recordedView> {coordinate} }}"
            ))
            .unwrap()
        );
        let selected = ds.select(&format!("PREFIX ev: <urn:x-sparkles:evidence:> SELECT ?decision WHERE {{ {projection} <{SR}inputManifest>/<{SR}inputId> ?decision . ?decision a ev:ReviewDecision }}")).unwrap();
        let actual: BTreeSet<_> = selected
            .iter()
            .map(|r| r.get("decision").unwrap().to_string())
            .collect();
        let expected: BTreeSet<_> = state["selectedDecisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| format!("<urn:example:a01:{}>", d.as_str().unwrap()))
            .collect();
        assert_eq!(actual, expected);
    }
    assert_eq!(coordinates.len(), 3);
    let artifact = json("expected/artifact-verification.json");
    assert_eq!(
        hash(source(artifact["payload"].as_str().unwrap()).as_bytes()),
        format!("sha256:{}", artifact["digest"].as_str().unwrap())
    );
    let verified = data("valid/common.ttl", "valid/artifact-verified.ttl");
    let method = NamedNode::new(artifact["method"].as_str().unwrap()).unwrap();
    assert!(
        verified
            .ask(&format!(
                "ASK {{ <urn:example:a01:verifiedArtifact> <{SR}verificationMethod> {method} }}"
            ))
            .unwrap()
    );
}

#[test]
fn cumulative_revision_choices_follow_fixed_causal_inputs() {
    let envelope = json("expected/a10-selection-inputs.json");
    let ds = data("valid/common.ttl", "valid/episode-observations.ttl");
    let ex = "urn:example:a01:";
    let app = "urn:example:a01:application:";
    assert_eq!(
        envelope["rule"],
        format!("{app}UniqueTerminalSelectedSourceRevision")
    );
    let mut coordinates = BTreeSet::new();
    let successors: Vec<_> = envelope["successors"].as_array().unwrap().iter().map(|edge| {
        let revision = edge["revision"].as_str().unwrap().to_owned();
        let previous = edge["supersedes"].as_str().unwrap().to_owned();
        assert!(ds.ask(&format!("ASK {{ <{ex}{revision}> <http://www.w3.org/ns/prov#wasRevisionOf> <{ex}{previous}> }}")).unwrap());
        (revision, previous)
    }).collect();
    for state in envelope["states"].as_array().unwrap() {
        let projection = state["projection"].as_str().unwrap();
        let view = state["view"].as_str().unwrap();
        let coordinate = state["coordinate"].as_str().unwrap();
        assert!(coordinates.insert(coordinate));
        assert!(ds.ask(&format!("ASK {{ <{ex}{projection}> <{SR}recordedView> <{ex}{coordinate}> ; <{SR}inputManifest> <{ex}{view}:manifest> . <{ex}{view}> <{SR}recordedView> <{ex}{coordinate}> . <{ex}{view}:manifest> <{SR}inputId> <{ex}A10:selectionProfile1> . <{ex}A10:selectionProfile1> <{app}revisionSelectionRule> <{app}UniqueTerminalSelectedSourceRevision> }}")).unwrap());
        let rows = ds
            .select(&format!(
                "SELECT ?id WHERE {{ <{ex}{view}:manifest> <{SR}inputId> ?id }}"
            ))
            .unwrap();
        let actual: BTreeSet<_> = rows
            .iter()
            .map(|row| row.get("id").unwrap().to_string())
            .collect();
        let selected: BTreeSet<_> = state["selectedRevisions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect();
        for candidate in envelope["candidates"].as_array().unwrap() {
            let revision = candidate["sourceRevision"].as_str().unwrap();
            assert_eq!(
                actual.contains(&format!("<{ex}{revision}>")),
                selected.contains(revision)
            );
        }
        for id in state["selectedClaimIds"].as_array().unwrap() {
            assert!(actual.contains(&format!("<{ex}{}>", id.as_str().unwrap())));
        }
        // Derive this fixed linear fixture from immutable source inputs only.
        // Projected sr:claim is read afterwards, solely for the output comparison.
        let superseded: BTreeSet<_> = successors
            .iter()
            .filter(|(revision, previous)| {
                selected.contains(revision) && selected.contains(previous)
            })
            .map(|(_, previous)| previous.clone())
            .collect();
        let terminal: Vec<_> = selected.difference(&superseded).collect();
        assert_eq!(terminal.len(), 1);
        let chosen = envelope["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|candidate| candidate["sourceRevision"].as_str().unwrap() == terminal[0].as_str())
            .unwrap();
        for key in ["sourceRevision", "observation", "relationClaim", "count"] {
            assert_eq!(chosen[key], state["expected"][key]);
        }
        let observation = chosen["observation"].as_str().unwrap();
        let relation = chosen["relationClaim"].as_str().unwrap();
        let revision = chosen["sourceRevision"].as_str().unwrap();
        assert!(actual.contains(&format!("<{ex}{relation}>")));
        assert!(actual.contains(&format!("<{ex}{}>", chosen["valueClaim"].as_str().unwrap())));
        let count = chosen["count"].as_i64().unwrap();
        assert!(ds.ask(&format!("ASK {{ <{ex}{observation}> <{SR}count> {count} . <{ex}{relation}> <http://www.w3.org/ns/prov#wasDerivedFrom> <{ex}{revision}> }}")).unwrap());
        for (property, value) in envelope["series"].as_object().unwrap() {
            if property == "episode" {
                continue;
            }
            let value = value.as_str().unwrap();
            let namespace = if property == "countMode" { SR } else { ex };
            assert!(
                ds.ask(&format!(
                    "ASK {{ <{ex}{observation}> <{SR}{property}> <{namespace}{value}> }}"
                ))
                .unwrap()
            );
        }
        let support = ds
            .select(&format!(
                "SELECT ?id WHERE {{ <{ex}{projection}> <{SR}claim> ?id }}"
            ))
            .unwrap();
        assert_eq!(support.len(), 1);
        assert_eq!(
            support
                .iter()
                .next()
                .unwrap()
                .get("id")
                .unwrap()
                .to_string(),
            format!("<{ex}{relation}>")
        );
    }
}
