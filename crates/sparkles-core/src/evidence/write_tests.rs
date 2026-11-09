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
            identity_graph: None,
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

#[test]
fn the_form_of_retained_content_is_part_of_a_record_digest() {
    use crate::evidence::{MethodRef, RenditionContent, RenditionInput};
    let c = Control::none();
    let text = "some text";
    let digest = Sha256Digest::of_bytes(text.as_bytes(), &c).unwrap();
    let with = |content: RenditionContent| {
        let mut i = input("o", "urn:o");
        i.claims.clear();
        i.renditions = vec![RenditionInput {
            version: RecordVersion::V1,
            key: RecordKey::new("text").unwrap(),
            revision: RecordRef::Local(RecordKey::new("rev").unwrap()),
            occurrence_key: RecordKey::new("text").unwrap(),
            conversion: MethodRef {
                method: iri("urn:method"),
                version: RecordKey::new("1").unwrap(),
            },
            media_type: "text/plain".into(),
            language: None,
            content,
            expected_digest: None,
        }];
        let p = plan_of(&i, &settings(&["urn:s"], &["urn:c"]), None).unwrap();
        let r = p.records.iter().find(|r| r.key == "text").unwrap();
        (r.iri.clone(), r.digest)
    };
    let (a, da) = with(RenditionContent::MetadataOnly(digest));
    let (b, db) = with(RenditionContent::NormalizedText(text.into()));
    assert_eq!(a, b, "the same identity");
    assert_ne!(
        da, db,
        "a metadata-only rendition never stands for one with text"
    );
}

fn identity(entities: &[&str], relation: crate::evidence::IdentityRelation) -> EvidenceInput {
    EvidenceInput {
        identities: vec![crate::evidence::IdentityInput {
            version: RecordVersion::V1,
            key: RecordKey::new("match").unwrap(),
            occurrence_key: RecordKey::new("m1").unwrap(),
            entities: entities.iter().map(|e| iri(e)).collect(),
            relation,
            preferred: None,
            attributed_author: iri("urn:matcher"),
            method: None,
        }],
        ..Default::default()
    }
}

#[test]
fn identity_proposals_go_to_an_identity_graph_as_a_set_of_entities() {
    use crate::evidence::IdentityRelation::{DistinctEntities, PossibleMatch};
    let mut s = settings(&["urn:s"], &["urn:c"]);
    // no identity graph is configured
    assert!(plan_of(&identity(&["urn:a", "urn:b"], PossibleMatch), &s, None).is_err());
    s.identity = vec!["urn:i".into()];
    let a = plan_of(&identity(&["urn:a", "urn:b"], PossibleMatch), &s, Some("k")).unwrap();
    assert_eq!(a.graphs, vec![iri("urn:i")]);
    assert_eq!(a.records[0].graph.as_str(), "urn:i");
    assert_eq!(a.records[0].kind, RecordKind::IdentityProposal);
    // the entities are a set
    let b = plan_of(&identity(&["urn:b", "urn:a"], PossibleMatch), &s, Some("k")).unwrap();
    assert_eq!(a.records[0].iri, b.records[0].iri);
    assert_eq!(a.records[0].digest, b.records[0].digest);
    assert_eq!(a.payload_digest, b.payload_digest);
    // the relation is content, the entities are identity
    let c = plan_of(
        &identity(&["urn:a", "urn:b"], DistinctEntities),
        &s,
        Some("k"),
    )
    .unwrap();
    assert_eq!(a.records[0].iri, c.records[0].iri);
    assert_ne!(a.records[0].digest, c.records[0].digest);
    let d = plan_of(&identity(&["urn:a", "urn:c"], PossibleMatch), &s, Some("k")).unwrap();
    assert_ne!(a.records[0].iri, d.records[0].iri);
    // two identity graphs need a choice
    s.identity.push("urn:j".into());
    assert!(plan_of(&identity(&["urn:a", "urn:b"], PossibleMatch), &s, None).is_err());
    // a batch without identity proposals keeps its payload digest whatever the
    // identity role holds
    let mut t = settings(&["urn:s"], &["urn:c"]);
    let before = plan_of(&input("o", "urn:o"), &t, Some("k")).unwrap();
    t.identity = vec!["urn:i".into()];
    let after = plan_of(&input("o", "urn:o"), &t, Some("k")).unwrap();
    assert_eq!(before.payload_digest, after.payload_digest);
}

/// The claim identities a proposal-graph scan returns stay charged to the caller's
/// reservation after the scan returns, so scans of several graphs share one bound.
#[test]
fn reifiers_about_charges_the_identities_it_returns() {
    use crate::store::{Store, StoreOptions};
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("INSERT DATA { GRAPH <urn:g> {");
    for i in 0..200 {
        data.push_str(&format!(
            " <urn:claim:{i}:{}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <urn:e> <urn:p> <urn:o{i}> )>> .",
            "x".repeat(200)
        ));
    }
    data.push_str(" } }");
    crate::sparql::update::update(&s, &data, &crate::sparql::QueryOptions::default()).unwrap();
    let snap = s.snapshot();
    let wanted: BTreeSet<String> = ["urn:e".to_string()].into();
    let scan = |limit: u64| -> Result<Vec<NamedNode>> {
        let preparation = Preparation::new(crate::task::Control::none(), limit)?;
        let mut reads = Reads::new(&preparation, &snap);
        let mut held = preparation.reserve(0)?;
        let mut out = Vec::new();
        // the same graph twice stands for two graphs that each fit alone
        for _ in 0..2 {
            let (rows, more) = reads.reifiers_about(
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies",
                "urn:g",
                &wanted,
                1_000,
                &mut held,
            )?;
            assert!(!more);
            out.extend(rows);
        }
        Ok(out)
    };
    assert_eq!(scan(16 << 20).unwrap().len(), 400);
    // one scan keeps about 50 KiB of identities beside 64 KiB of rows, so it fits in
    // 140 KiB, and two scans that keep the first one's identities do not
    let one = Preparation::new(crate::task::Control::none(), 140 << 10).unwrap();
    let mut reads = Reads::new(&one, &snap);
    let mut held = one.reserve(0).unwrap();
    assert!(
        reads
            .reifiers_about(
                "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies",
                "urn:g",
                &wanted,
                1_000,
                &mut held,
            )
            .is_ok()
    );
    drop(held);
    assert!(matches!(scan(140 << 10), Err(Error::BudgetExceeded(_))));
}
