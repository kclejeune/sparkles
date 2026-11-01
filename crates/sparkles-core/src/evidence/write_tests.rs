//! Tests of the typed evidence writer's record encoding.

use super::*;
use crate::evidence::{
    ClaimInput, EvidenceInput, RecordVersion, SourceRevisionInput, prepare_input,
};

fn iri(s: &str) -> NamedNode {
    NamedNode::new_unchecked(s)
}

fn settings(sources: &[&str], proposals: &[&str]) -> Settings {
    Settings {
        enabled: true,
        source_records: sources.iter().map(|s| s.to_string()).collect(),
        proposals: proposals.iter().map(|s| s.to_string()).collect(),
        ..Settings::default()
    }
}

fn input(occurrence: &str, object: &str) -> EvidenceInput {
    let c = Control::none();
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: RecordKey::new("rev").unwrap(),
            source: iri("urn:source"),
            source_key: RecordKey::new("source").unwrap(),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: Sha256Digest::of_bytes(b"bytes", &c).unwrap(),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: RecordKey::new("claim").unwrap(),
            occurrence_key: RecordKey::new(occurrence).unwrap(),
            revision: RecordRef::Local(RecordKey::new("rev").unwrap()),
            proposition: Triple::new(iri("urn:s"), iri("urn:p"), iri(object)),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:author"),
            scope: Default::default(),
            confidence: Default::default(),
        }],
        ..Default::default()
    }
}

const DATASET: Uuid = Uuid::from_u128(0x1234_5678_1234_4234_8234_1234_5678_9abc);

fn plan_of(input: &EvidenceInput, settings: &Settings, key: Option<&str>) -> Result<Plan> {
    let c = Control::none();
    let prepared = prepare_input(input, &c)?;
    plan(
        &prepared,
        DATASET,
        settings,
        &ProposalOptions {
            principal: &iri("urn:alice"),
            idempotency_key: key,
            source_graph: None,
            proposal_graph: None,
        },
        &c,
    )
}

#[test]
fn identities_are_deterministic_and_scoped_by_principal_and_occurrence() {
    let s = settings(&["urn:s"], &["urn:c"]);
    let a = plan_of(&input("o1", "urn:o"), &s, Some("k")).unwrap();
    let b = plan_of(&input("o1", "urn:o"), &s, Some("k")).unwrap();
    assert_eq!(a.operation, b.operation);
    assert_eq!(a.payload_digest, b.payload_digest);
    for (x, y) in a.records.iter().zip(&b.records) {
        assert_eq!((&x.iri, x.digest), (&y.iri, y.digest));
    }
    // another occurrence is another claim of the same revision
    let c = plan_of(&input("o2", "urn:o"), &s, Some("k")).unwrap();
    assert_eq!(a.records[0].iri, c.records[0].iri);
    assert_ne!(a.records[1].iri, c.records[1].iri);
    assert_ne!(a.payload_digest, c.payload_digest);
    // other content under the same identity has another digest
    let d = plan_of(&input("o1", "urn:other"), &s, Some("k")).unwrap();
    assert_eq!(a.records[1].iri, d.records[1].iri);
    assert_ne!(a.records[1].digest, d.records[1].digest);
    // the key is scoped by the principal, and no key is a fresh operation
    assert_ne!(
        operation_id(DATASET, &iri("urn:alice"), "k"),
        operation_id(DATASET, &iri("urn:bob"), "k")
    );
    let e = plan_of(&input("o1", "urn:o"), &s, None).unwrap();
    assert_ne!(e.operation, a.operation);
    assert_eq!(e.records[1].iri, a.records[1].iri);
}

#[test]
fn graphs_follow_the_roles_and_must_be_named_when_ambiguous() {
    let one = settings(&["urn:s"], &["urn:c"]);
    let p = plan_of(&input("o", "urn:o"), &one, None).unwrap();
    assert_eq!(p.records[0].graph.as_str(), "urn:s");
    assert_eq!(p.records[1].graph.as_str(), "urn:c");
    assert_eq!(p.graphs, vec![iri("urn:c"), iri("urn:s")]);
    let two = settings(&["urn:s", "urn:t"], &["urn:c"]);
    assert!(matches!(
        plan_of(&input("o", "urn:o"), &two, None),
        Err(Error::Invalid(_))
    ));
    let mut disabled = one.clone();
    disabled.enabled = false;
    assert!(plan_of(&input("o", "urn:o"), &disabled, None).is_err());
}

#[test]
fn the_server_fields_and_the_marker_go_to_every_written_graph() {
    let s = settings(&["urn:s"], &["urn:c"]);
    let p = plan_of(&input("o", "urn:o"), &s, Some("k")).unwrap();
    let nonce = Uuid::new_v4();
    let stamp = Stamp {
        dataset: DATASET,
        branch: DATASET,
        commit: 7,
        time_ms: 1_767_261_600_000,
    };
    let quads = p.quads(&stamp, |_| Some(nonce), None, &[]).unwrap();
    let has = |s: &str, p: &str, o: &Term, g: &str| {
        quads.iter().any(|q| {
            q.subject.to_string() == format!("<{s}>")
                && q.predicate.as_str() == p
                && &q.object == o
                && q.graph_name.to_string() == format!("<{g}>")
        })
    };
    let op = p.operation_iri.as_str();
    for g in ["urn:s", "urn:c"] {
        assert!(has(
            op,
            &format!("{EV}graphProtection"),
            &string(&nonce.to_string()),
            g
        ));
        assert!(has(op, &format!("{EV}commit"), &integer(7), g));
    }
    let rev = p.records[0].iri.as_str();
    assert!(has(
        rev,
        &format!("{EV}firstAvailableAt"),
        &date_time(stamp.time_ms).into(),
        "urn:s"
    ));
    assert!(has(
        rev,
        &format!("{EV}availabilityReceipt"),
        &p.operation_iri.clone().into(),
        "urn:s"
    ));
    // a graph without a protection nonce cannot be written
    assert!(p.quads(&stamp, |_| None, None, &[]).is_err());
    // a skipped record writes none of its quads
    let skipped = p
        .quads(&stamp, |_| Some(nonce), None, &[true, false])
        .unwrap();
    assert!(
        !skipped
            .iter()
            .any(|q| q.subject.to_string() == format!("<{rev}>"))
    );
}
