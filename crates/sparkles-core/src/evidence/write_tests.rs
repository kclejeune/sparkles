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
            validity: None,
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
    // one scan keeps about 75 KiB of identities beside 64 KiB of rows, so it fits in
    // 176 KiB, and two scans that keep the first one's identities do not
    let one = Preparation::new(crate::task::Control::none(), 176 << 10).unwrap();
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
    assert!(matches!(scan(176 << 10), Err(Error::BudgetExceeded(_))));
}

/// Each identity a `Reads` caches, found or not, is charged to its preparation.
#[test]
fn reads_charge_their_identity_cache() {
    use crate::store::{Store, StoreOptions};
    let s = Store::in_memory(StoreOptions::default());
    let snap = s.snapshot();
    let preparation = Preparation::new(crate::task::Control::none(), 1 << 20).unwrap();
    let mut reads = Reads::new(&preparation, &snap);
    let mut refused = false;
    for i in 0..20_000 {
        match reads.subject_triples(&format!("urn:c:{i:06}"), "urn:g", 1) {
            Ok(n) => assert_eq!(n, 0),
            Err(Error::BudgetExceeded(_)) => {
                refused = true;
                break;
            }
            Err(e) => panic!("{e}"),
        }
    }
    assert!(refused, "20,000 cached identities fit in 1 MiB uncharged");
}

/// A cache and a scan that hold a single identity charge their containers' first
/// allocations, which are larger than any one entry's share.
#[test]
fn single_identities_charge_their_first_allocations() {
    use crate::store::{Store, StoreOptions};
    const LIMIT: u64 = 96 << 10;
    let s = Store::in_memory(StoreOptions::default());
    crate::sparql::update::update(
        &s,
        "INSERT DATA { GRAPH <urn:g> { <urn:c> <http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies> <<( <urn:e> <urn:p> <urn:o> )>> } }",
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let snap = s.snapshot();
    // one cached identity keeps at least a root leaf charged
    let preparation = Preparation::new(crate::task::Control::none(), LIMIT).unwrap();
    let mut reads = Reads::new(&preparation, &snap);
    assert_eq!(reads.subject_triples("urn:missing", "urn:g", 1).unwrap(), 0);
    assert!(preparation.reserve(LIMIT - 1000).is_err());
    drop(reads);
    assert!(preparation.reserve(LIMIT - 1000).is_ok());
    // one returned identity keeps the vector's and the set's first allocations charged
    let preparation = Preparation::new(crate::task::Control::none(), LIMIT).unwrap();
    let mut reads = Reads::new(&preparation, &snap);
    let mut held = preparation.reserve(0).unwrap();
    let wanted: BTreeSet<String> = ["urn:e".to_string()].into();
    let (rows, _) = reads
        .reifiers_about(
            "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies",
            "urn:g",
            &wanted,
            10,
            &mut held,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    // the cache holds about 1.3 KiB, so a reservation that leaves 2 KiB fits only
    // when the identity is charged at less than about 700 bytes
    assert!(preparation.reserve(LIMIT - 2048).is_err());
    drop(held);
    assert!(preparation.reserve(LIMIT - 2048).is_ok());
}

/// An added boundary graph holding many unrelated or detached-epoch quads alongside a
/// small number of current proposals must not walk the unrelated graph rows.
#[test]
fn has_live_accepted_in_graphs_skips_unrelated_quads_in_added_graph() {
    use crate::store::{Store, StoreOptions};
    use oxrdf::{GraphName, Quad};

    let s = Store::in_memory(StoreOptions::default());
    let added_graph = iri("urn:ev:added");
    let other_graph = iri("urn:ev:other");
    let decisions_graph = iri("urn:ev:decisions");
    let policy = iri("urn:ev:policy:people");
    let added_nonce = Uuid::new_v4();
    let detached_nonce = Uuid::new_v4();
    let other_nonce = Uuid::new_v4();
    let decisions_nonce = Uuid::new_v4();

    let rdf_type = iri(RDF_TYPE);
    let proposal_cls = iri(&format!("{EV}IdentityProposal"));
    let decision_cls = iri(&format!("{EV}ReviewDecision"));
    let op_cls = iri(&format!("{EV}Operation"));
    let policy_pred = iri(&format!("{EV}reviewPolicy"));
    let target_pred = iri(&format!("{EV}target"));
    let disp_pred = iri(&format!("{EV}disposition"));
    let accepted = iri(&format!("{EV}Accepted"));
    let receipt_pred = iri(&format!("{EV}availabilityReceipt"));
    let prot_pred = iri(&format!("{EV}graphProtection"));
    let prop_p = iri("urn:unrelated:p");

    let mut quads: Vec<Quad> = Vec::with_capacity(21_000);
    // 20,000 unrelated quads in the added graph.
    for i in 0..20_000usize {
        quads.push(Quad::new(
            iri(&format!("urn:unrelated:s{i}")),
            prop_p.clone(),
            iri(&format!("urn:unrelated:o{i}")),
            GraphName::NamedNode(added_graph.clone()),
        ));
    }
    // 100 detached-epoch proposals in the added graph under an old nonce.
    let detached_op = iri("urn:op:detached");
    quads.push(Quad::new(
        detached_op.clone(),
        rdf_type.clone(),
        op_cls.clone(),
        GraphName::NamedNode(added_graph.clone()),
    ));
    quads.push(Quad::new(
        detached_op.clone(),
        prot_pred.clone(),
        string(&detached_nonce.to_string()),
        GraphName::NamedNode(added_graph.clone()),
    ));
    for i in 0..100usize {
        let p = iri(&format!("urn:proposal:detached-{i}"));
        quads.push(Quad::new(
            p.clone(),
            rdf_type.clone(),
            proposal_cls.clone(),
            GraphName::NamedNode(added_graph.clone()),
        ));
        quads.push(Quad::new(
            p.clone(),
            receipt_pred.clone(),
            detached_op.clone(),
            GraphName::NamedNode(added_graph.clone()),
        ));
        quads.push(Quad::new(
            p,
            prot_pred.clone(),
            string(&detached_nonce.to_string()),
            GraphName::NamedNode(added_graph.clone()),
        ));
    }
    // 500 accepted decisions targeting proposals in `other_graph` so `c_accepted`,
    // `c_policy`, and `c_proposals` are all much larger than `c_protected`.
    let other_op = iri("urn:op:other");
    quads.push(Quad::new(
        other_op.clone(),
        rdf_type.clone(),
        op_cls.clone(),
        GraphName::NamedNode(other_graph.clone()),
    ));
    quads.push(Quad::new(
        other_op.clone(),
        prot_pred.clone(),
        string(&other_nonce.to_string()),
        GraphName::NamedNode(other_graph.clone()),
    ));
    let dec_op = iri("urn:op:decisions");
    quads.push(Quad::new(
        dec_op.clone(),
        rdf_type.clone(),
        op_cls.clone(),
        GraphName::NamedNode(decisions_graph.clone()),
    ));
    quads.push(Quad::new(
        dec_op.clone(),
        prot_pred.clone(),
        string(&decisions_nonce.to_string()),
        GraphName::NamedNode(decisions_graph.clone()),
    ));
    for i in 0..500usize {
        let p = iri(&format!("urn:proposal:other-{i}"));
        quads.push(Quad::new(
            p.clone(),
            rdf_type.clone(),
            proposal_cls.clone(),
            GraphName::NamedNode(other_graph.clone()),
        ));
        quads.push(Quad::new(
            p.clone(),
            receipt_pred.clone(),
            other_op.clone(),
            GraphName::NamedNode(other_graph.clone()),
        ));
        quads.push(Quad::new(
            p.clone(),
            prot_pred.clone(),
            string(&other_nonce.to_string()),
            GraphName::NamedNode(other_graph.clone()),
        ));
        let d = iri(&format!("urn:decision:other-{i}"));
        quads.push(Quad::new(
            d.clone(),
            rdf_type.clone(),
            decision_cls.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        quads.push(Quad::new(
            d.clone(),
            policy_pred.clone(),
            policy.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        quads.push(Quad::new(
            d.clone(),
            target_pred.clone(),
            p,
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        quads.push(Quad::new(
            d.clone(),
            disp_pred.clone(),
            accepted.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        quads.push(Quad::new(
            d.clone(),
            receipt_pred.clone(),
            dec_op.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        quads.push(Quad::new(
            d,
            prot_pred.clone(),
            string(&decisions_nonce.to_string()),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
    }
    // 2 current-epoch unreviewed proposals in the added graph.
    let added_op = iri("urn:op:added");
    quads.push(Quad::new(
        added_op.clone(),
        rdf_type.clone(),
        op_cls.clone(),
        GraphName::NamedNode(added_graph.clone()),
    ));
    quads.push(Quad::new(
        added_op.clone(),
        prot_pred.clone(),
        string(&added_nonce.to_string()),
        GraphName::NamedNode(added_graph.clone()),
    ));
    let cur_p1 = iri("urn:proposal:added-1");
    let cur_p2 = iri("urn:proposal:added-2");
    for p in [&cur_p1, &cur_p2] {
        quads.push(Quad::new(
            p.clone(),
            rdf_type.clone(),
            proposal_cls.clone(),
            GraphName::NamedNode(added_graph.clone()),
        ));
        quads.push(Quad::new(
            p.clone(),
            receipt_pred.clone(),
            added_op.clone(),
            GraphName::NamedNode(added_graph.clone()),
        ));
        quads.push(Quad::new(
            p.clone(),
            prot_pred.clone(),
            string(&added_nonce.to_string()),
            GraphName::NamedNode(added_graph.clone()),
        ));
    }
    let insert_all = |qs: &[Quad]| {
        let mut t = s.write();
        let mut labels = std::collections::HashMap::new();
        for q in qs {
            let ids = t.encode_quad(q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    };
    insert_all(&quads);

    let snap = s.snapshot();
    let prep = Preparation::new(crate::task::Control::none(), 1 << 20).unwrap();
    let mut reads = Reads::new(&prep, &snap);
    let (found, stats) = reads
        .has_live_accepted_in_graphs_counted(
            &[(added_graph.clone(), Some(added_nonce))],
            &(decisions_graph.clone(), Some(decisions_nonce)),
            &policy,
        )
        .unwrap();
    assert!(!found);
    // Only the 2 current proposals + 1 operation carry `added_nonce` (3 rows total),
    // never the 20,000 unrelated quads or 100 detached-epoch proposals, and global
    // prefixes are not even counted for a small current-nonce set.
    assert_eq!(stats.count_calls, 1);
    assert!(
        stats.outer_rows <= 8,
        "expected selective scan over current-epoch candidates, scanned {} outer rows",
        stats.outer_rows
    );

    // Accepting one of the current proposals in `added_graph` must be detected while
    // keeping outer work bounded.
    let d_added = iri("urn:decision:added-1");
    insert_all(&[
        Quad::new(
            d_added.clone(),
            rdf_type,
            decision_cls,
            GraphName::NamedNode(decisions_graph.clone()),
        ),
        Quad::new(
            d_added.clone(),
            policy_pred,
            policy.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ),
        Quad::new(
            d_added.clone(),
            target_pred,
            cur_p1,
            GraphName::NamedNode(decisions_graph.clone()),
        ),
        Quad::new(
            d_added.clone(),
            disp_pred,
            accepted,
            GraphName::NamedNode(decisions_graph.clone()),
        ),
        Quad::new(
            d_added.clone(),
            receipt_pred,
            dec_op,
            GraphName::NamedNode(decisions_graph.clone()),
        ),
        Quad::new(
            d_added,
            prot_pred,
            string(&decisions_nonce.to_string()),
            GraphName::NamedNode(decisions_graph.clone()),
        ),
    ]);
    let snap2 = s.snapshot();
    let mut reads2 = Reads::new(&prep, &snap2);
    let (found2, stats2) = reads2
        .has_live_accepted_in_graphs_counted(
            &[(added_graph, Some(added_nonce))],
            &(decisions_graph, Some(decisions_nonce)),
            &policy,
        )
        .unwrap();
    assert!(found2);
    assert_eq!(stats2.count_calls, 1);
    assert!(
        stats2.outer_rows <= 8,
        "expected selective scan on accepted match, scanned {} outer rows",
        stats2.outer_rows
    );
}

/// On a compacted multi-block base with large unrelated global `ev:IdentityProposal` and
/// `ev:Accepted` ranges, candidate selection in `has_live_accepted_in_graphs_counted`
/// must never walk interior block metadata linearly, must skip global counts when the
/// added graph has few current-nonce rows, must keep metadata reads and edge decodes O(1)
/// when global counts are needed, and must enforce `Preparation` cancellation and memory
/// budgets on edge decodes.
#[test]
fn has_live_accepted_in_graphs_compacted_base_bounds_selection_and_obeys_preparation() {
    use crate::error::BudgetKind;
    use crate::index::Perm;
    use crate::store::{Store, StoreOptions};
    use crate::task::Control;
    use oxrdf::{GraphName, Quad};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();

    let added_graph = iri("urn:ev:added");
    let large_added_graph = iri("urn:ev:large-added");
    let decisions_graph = iri("urn:ev:decisions");
    let policy = iri("urn:ev:policy:people");
    let added_nonce = Uuid::new_v4();
    let large_nonce = Uuid::new_v4();
    let bulk_nonce = Uuid::new_v4();
    let decisions_nonce = Uuid::new_v4();

    let rdf_type = iri(RDF_TYPE);
    let proposal_cls = iri(&format!("{EV}IdentityProposal"));
    let decision_cls = iri(&format!("{EV}ReviewDecision"));
    let op_cls = iri(&format!("{EV}Operation"));
    let policy_pred = iri(&format!("{EV}reviewPolicy"));
    let target_pred = iri(&format!("{EV}target"));
    let disp_pred = iri(&format!("{EV}disposition"));
    let accepted = iri(&format!("{EV}Accepted"));
    let receipt_pred = iri(&format!("{EV}availabilityReceipt"));
    let prot_pred = iri(&format!("{EV}graphProtection"));

    let added_op = iri("urn:op:added");
    let cur_p1 = iri("urn:proposal:added-1");
    let cur_p2 = iri("urn:proposal:added-2");
    let dec_op = iri("urn:op:decisions");
    let other_p1 = iri("urn:proposal:policy-other-1");
    let other_p2 = iri("urn:proposal:policy-other-2");
    let other_d1 = iri("urn:decision:policy-other-1");
    let other_d2 = iri("urn:decision:policy-other-2");

    {
        let mut t = s.write();
        let mut labels = std::collections::HashMap::new();

        // Pre-encode shared predicate/object/subject/graph IDs for fast multi-block fill:
        // 1,000 subjects x 150 unrelated graphs = 150,000 rows per global prefix
        // (5+ compacted blocks of 32,768 rows each in Perm::Pos).
        let bulk_subjects: Vec<_> = (0..1_000usize)
            .map(|i| {
                t.intern_scoped(
                    &oxrdf::Term::NamedNode(iri(&format!("urn:bulk:s{i}"))),
                    &mut labels,
                )
                .unwrap()
            })
            .collect();
        let bulk_graphs: Vec<_> = (0..150usize)
            .map(|i| {
                t.intern_scoped(
                    &oxrdf::Term::NamedNode(iri(&format!("urn:bulk:g{i}"))),
                    &mut labels,
                )
                .unwrap()
            })
            .collect();
        let rdf_type_id = t
            .intern_scoped(&oxrdf::Term::NamedNode(rdf_type.clone()), &mut labels)
            .unwrap();
        let proposal_cls_id = t
            .intern_scoped(&oxrdf::Term::NamedNode(proposal_cls.clone()), &mut labels)
            .unwrap();
        let disp_pred_id = t
            .intern_scoped(&oxrdf::Term::NamedNode(disp_pred.clone()), &mut labels)
            .unwrap();
        let accepted_id = t
            .intern_scoped(&oxrdf::Term::NamedNode(accepted.clone()), &mut labels)
            .unwrap();
        let prot_pred_id = t
            .intern_scoped(&oxrdf::Term::NamedNode(prot_pred.clone()), &mut labels)
            .unwrap();
        let bulk_nonce_id = t
            .intern_scoped(&string(&bulk_nonce.to_string()), &mut labels)
            .unwrap();
        let large_nonce_id = t
            .intern_scoped(&string(&large_nonce.to_string()), &mut labels)
            .unwrap();
        let large_graph_id = t
            .intern_scoped(
                &oxrdf::Term::NamedNode(large_added_graph.clone()),
                &mut labels,
            )
            .unwrap();

        for &g_id in &bulk_graphs {
            for &s_id in &bulk_subjects {
                t.insert([s_id, rdf_type_id, proposal_cls_id, g_id])
                    .unwrap();
                t.insert([s_id, disp_pred_id, accepted_id, g_id]).unwrap();
            }
        }
        // Populate enough rows under `prot_pred` that `prot_pred` spans full 2,048-row
        // segments in the compacted Perm::Pos index.
        for &g_id in &bulk_graphs[..6] {
            for &s_id in &bulk_subjects {
                t.insert([s_id, prot_pred_id, bulk_nonce_id, g_id]).unwrap();
            }
        }
        // 80 current-epoch non-proposal records in `large_added_graph` (> SMALL_PROTECTED_LIMIT)
        // so candidate selection must compare against the multi-block global prefixes and
        // choose the tiny 2-row `policy` prefix.
        for &s_id in &bulk_subjects[..80] {
            t.insert([s_id, prot_pred_id, large_nonce_id, large_graph_id])
                .unwrap();
        }

        let mut push_quad = |q: Quad| {
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        };

        // Small `added_graph`: 1 operation + 2 current-epoch unreviewed proposals (3 rows).
        push_quad(Quad::new(
            added_op.clone(),
            rdf_type.clone(),
            op_cls.clone(),
            GraphName::NamedNode(added_graph.clone()),
        ));
        push_quad(Quad::new(
            added_op.clone(),
            prot_pred.clone(),
            string(&added_nonce.to_string()),
            GraphName::NamedNode(added_graph.clone()),
        ));
        for p in [&cur_p1, &cur_p2] {
            push_quad(Quad::new(
                p.clone(),
                rdf_type.clone(),
                proposal_cls.clone(),
                GraphName::NamedNode(added_graph.clone()),
            ));
            push_quad(Quad::new(
                p.clone(),
                receipt_pred.clone(),
                added_op.clone(),
                GraphName::NamedNode(added_graph.clone()),
            ));
            push_quad(Quad::new(
                p.clone(),
                prot_pred.clone(),
                string(&added_nonce.to_string()),
                GraphName::NamedNode(added_graph.clone()),
            ));
        }

        // Only 2 decisions under `policy` in `decisions_graph` (targeting unrelated proposals).
        push_quad(Quad::new(
            dec_op.clone(),
            rdf_type.clone(),
            op_cls.clone(),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        push_quad(Quad::new(
            dec_op.clone(),
            prot_pred.clone(),
            string(&decisions_nonce.to_string()),
            GraphName::NamedNode(decisions_graph.clone()),
        ));
        for (d, target) in [(&other_d1, &other_p1), (&other_d2, &other_p2)] {
            push_quad(Quad::new(
                d.clone(),
                rdf_type.clone(),
                decision_cls.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
            push_quad(Quad::new(
                d.clone(),
                policy_pred.clone(),
                policy.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
            push_quad(Quad::new(
                d.clone(),
                target_pred.clone(),
                target.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
            push_quad(Quad::new(
                d.clone(),
                disp_pred.clone(),
                accepted.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
            push_quad(Quad::new(
                d.clone(),
                receipt_pred.clone(),
                dec_op.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
            push_quad(Quad::new(
                d.clone(),
                prot_pred.clone(),
                string(&decisions_nonce.to_string()),
                GraphName::NamedNode(decisions_graph.clone()),
            ));
        }
        t.commit().unwrap();
    }

    s.compact().unwrap();
    let snap = s.snapshot();
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();

    // Confirm the compacted Perm::Pos base genuinely spans 5+ blocks for each global prefix.
    let rdf_type_id = prep.lookup_iri(&snap, RDF_TYPE).unwrap().unwrap();
    let proposal_cls_id = prep
        .lookup_iri(&snap, &format!("{EV}IdentityProposal"))
        .unwrap()
        .unwrap();
    let disp_pred_id = prep
        .lookup_iri(&snap, &format!("{EV}disposition"))
        .unwrap()
        .unwrap();
    let accepted_id = prep
        .lookup_iri(&snap, &format!("{EV}Accepted"))
        .unwrap()
        .unwrap();
    let (p_lo, p_hi) = snap
        .perm(Perm::Pos)
        .block_range(&[rdf_type_id.0, proposal_cls_id.0]);
    let (a_lo, a_hi) = snap
        .perm(Perm::Pos)
        .block_range(&[disp_pred_id.0, accepted_id.0]);
    assert!(
        p_hi - p_lo >= 5 && a_hi - a_lo >= 5,
        "expected >= 5 compacted blocks per global prefix, got proposals={} accepted={}",
        p_hi - p_lo,
        a_hi - a_lo
    );

    // Case A: Small added_nonce (3 rows) scans `protected` directly without counting
    // the multi-block global proposal/accepted/policy ranges.
    let mut reads = Reads::new(&prep, &snap);
    let (found, stats) = reads
        .has_live_accepted_in_graphs_counted(
            &[(added_graph.clone(), Some(added_nonce))],
            &(decisions_graph.clone(), Some(decisions_nonce)),
            &policy,
        )
        .unwrap();
    assert!(!found);
    assert_eq!(stats.count_calls, 1);
    assert!(stats.metadata_reads <= 4, "stats={stats:?}");
    assert!(stats.edge_decodes <= 2, "stats={stats:?}");
    assert!(stats.outer_rows <= 8, "stats={stats:?}");

    // Case B: Large added_nonce (80 rows > SMALL_PROTECTED_LIMIT) triggers all 4 counts,
    // sums interior blocks in O(1) without walking the 10+ compacted blocks, and chooses
    // the 2-row `policy` prefix.
    let mut reads_large = Reads::new(&prep, &snap);
    let (found_large, stats_large) = reads_large
        .has_live_accepted_in_graphs_counted(
            &[(large_added_graph.clone(), Some(large_nonce))],
            &(decisions_graph.clone(), Some(decisions_nonce)),
            &policy,
        )
        .unwrap();
    assert!(!found_large);
    assert_eq!(stats_large.count_calls, 4);
    assert!(
        stats_large.metadata_reads <= 16,
        "expected O(1) metadata reads across {}+ compacted blocks, got {stats_large:?}",
        (p_hi - p_lo) + (a_hi - a_lo)
    );
    assert!(stats_large.edge_decodes <= 8, "stats={stats_large:?}");
    assert!(
        stats_large.outer_rows <= 4,
        "expected 2-row policy scan, got {stats_large:?}"
    );

    // Case C: Preparation cancellation and tight memory budget are enforced during
    // compacted-base selection and edge decoding.
    let cancel = Arc::new(AtomicBool::new(false));
    let cancelled_prep = Preparation::new(Control::with_cancel(cancel.clone()), 16 << 20).unwrap();
    cancel.store(true, Ordering::Relaxed);
    let mut reads_cancelled = Reads::new(&cancelled_prep, &snap);
    assert!(matches!(
        reads_cancelled.has_live_accepted_in_graphs_counted(
            &[(large_added_graph.clone(), Some(large_nonce))],
            &(decisions_graph.clone(), Some(decisions_nonce)),
            &policy,
        ),
        Err(Error::Cancelled)
    ));

    // Reopen the store with a fresh cold block cache and run with a 16 KiB budget:
    // large enough for vocab lookups (~5 KiB), too small for a 2,048-row prepared edge
    // segment decode (~74 KiB).
    drop(reads_cancelled);
    drop(reads_large);
    drop(reads);
    drop(snap);
    drop(s);
    let s_cold = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let snap_cold = s_cold.snapshot();
    let tight_prep = Preparation::new(Control::none(), 16 << 10).unwrap();
    let mut reads_tight = Reads::new(&tight_prep, &snap_cold);
    assert!(matches!(
        reads_tight.has_live_accepted_in_graphs_counted(
            &[(large_added_graph, Some(large_nonce))],
            &(decisions_graph.clone(), Some(decisions_nonce)),
            &policy,
        ),
        Err(Error::BudgetExceeded(b)) if b.kind == BudgetKind::Memory
    ));

    // Positive detection on the compacted store when `cur_p1` is accepted.
    {
        let mut t = s_cold.write();
        let mut labels = std::collections::HashMap::new();
        let d_added = iri("urn:decision:added-compacted-1");
        for q in [
            Quad::new(
                d_added.clone(),
                rdf_type,
                decision_cls,
                GraphName::NamedNode(decisions_graph.clone()),
            ),
            Quad::new(
                d_added.clone(),
                policy_pred,
                policy.clone(),
                GraphName::NamedNode(decisions_graph.clone()),
            ),
            Quad::new(
                d_added.clone(),
                target_pred,
                cur_p1,
                GraphName::NamedNode(decisions_graph.clone()),
            ),
            Quad::new(
                d_added.clone(),
                disp_pred,
                accepted,
                GraphName::NamedNode(decisions_graph.clone()),
            ),
            Quad::new(
                d_added.clone(),
                receipt_pred,
                dec_op,
                GraphName::NamedNode(decisions_graph.clone()),
            ),
            Quad::new(
                d_added,
                prot_pred,
                string(&decisions_nonce.to_string()),
                GraphName::NamedNode(decisions_graph.clone()),
            ),
        ] {
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }
    s_cold.compact().unwrap();
    let snap_after = s_cold.snapshot();
    let mut reads_after = Reads::new(&prep, &snap_after);
    let (found_after, stats_after) = reads_after
        .has_live_accepted_in_graphs_counted(
            &[(added_graph, Some(added_nonce))],
            &(decisions_graph, Some(decisions_nonce)),
            &policy,
        )
        .unwrap();
    assert!(found_after);
    assert_eq!(stats_after.count_calls, 1);
    assert!(stats_after.metadata_reads <= 4, "stats={stats_after:?}");
    assert!(stats_after.edge_decodes <= 2, "stats={stats_after:?}");
    assert!(stats_after.outer_rows <= 8, "stats={stats_after:?}");
}
