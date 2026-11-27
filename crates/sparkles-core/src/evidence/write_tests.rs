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
            was_revision_of: None,
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
    // one indexed lookup keeps about 75 KiB of identities beside ~14 KiB of matched ids,
    // so it fits in 128 KiB, and two lookups that keep the first one's identities do not
    let one = Preparation::new(crate::task::Control::none(), 128 << 10).unwrap();
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
    assert!(matches!(scan(128 << 10), Err(Error::BudgetExceeded(_))));
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

#[test]
fn plan_held_collects_local_and_cross_dataset_stored_facts_sorted_by_dataset() {
    use crate::evidence::{EvidenceRefInput, EvidenceRefTarget, GraphIdentity, StoredFactRef};
    let ds_a = Uuid::from_u128(10);
    let ds_b = Uuid::from_u128(20);
    let branch_b = Uuid::from_u128(21);
    let mut batch = input("o1", "urn:o");
    batch.references = vec![
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: RecordKey::new("f_b2").unwrap(),
            occurrence_key: RecordKey::new("f_b2").unwrap(),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: ds_b,
                branch: branch_b,
                commit: 9,
                graph: GraphIdentity::Default,
                expected: Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o1")),
                anchor: None,
            }),
        },
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: RecordKey::new("f_local").unwrap(),
            occurrence_key: RecordKey::new("f_local").unwrap(),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: DATASET,
                branch: DATASET,
                commit: 3,
                graph: GraphIdentity::Default,
                expected: Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o2")),
                anchor: None,
            }),
        },
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: RecordKey::new("f_a").unwrap(),
            occurrence_key: RecordKey::new("f_a").unwrap(),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: ds_a,
                branch: ds_a,
                commit: 5,
                graph: GraphIdentity::Default,
                expected: Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o3")),
                anchor: None,
            }),
        },
        // duplicate of f_a's commit
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: RecordKey::new("f_a_dup").unwrap(),
            occurrence_key: RecordKey::new("f_a_dup").unwrap(),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: ds_a,
                branch: ds_a,
                commit: 5,
                graph: GraphIdentity::Default,
                expected: Triple::new(iri("urn:s"), iri("urn:p"), iri("urn:o4")),
                anchor: None,
            }),
        },
    ];
    let p = plan_of(&batch, &settings(&["urn:s"], &["urn:c"]), None).unwrap();
    let local = HeldCommit {
        branch: DATASET,
        commit: 3,
    };
    let mut expected = [
        DatasetHeldCommit {
            dataset: ds_a,
            commit: HeldCommit {
                branch: ds_a,
                commit: 5,
            },
        },
        DatasetHeldCommit {
            dataset: ds_b,
            commit: HeldCommit {
                branch: branch_b,
                commit: 9,
            },
        },
        DatasetHeldCommit {
            dataset: DATASET,
            commit: local,
        },
    ];
    expected.sort();
    assert_eq!(p.held_in(), expected);
    assert_eq!(p.held, [local]);
}

/// The reifier index returns identical subjects across delta writes, full compaction,
/// base deletions, re-insertions, and persistent reopen, without scanning past a small
/// fallback limit.
#[test]
fn reifiers_about_indexed_matches_across_compaction_deletes_and_large_graphs() {
    use crate::store::{Store, StoreOptions};
    let tmp = tempfile::tempdir().unwrap();
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    let reifies = "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies";
    let lookup = |snap: &Snapshot, entities: &[&str]| -> (Vec<String>, bool) {
        let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
        let mut reads = Reads::new(&prep, snap);
        let mut held = prep.reserve(0).unwrap();
        let wanted: BTreeSet<String> = entities.iter().map(|e| e.to_string()).collect();
        let (rows, more) = reads
            .reifiers_about(reifies, "urn:g", &wanted, 5, &mut held)
            .unwrap();
        let mut names: Vec<String> = rows.into_iter().map(|n| n.into_string()).collect();
        names.sort();
        (names, more)
    };

    // Uncompacted delta: 20 unrelated quads + 2 matching claims (well past scan=5)
    let mut data = String::from("INSERT DATA { GRAPH <urn:g> {");
    for i in 0..20 {
        data.push_str(&format!(
            " <urn:other:{i}> <{reifies}> <<( <urn:u{i}> <urn:p> <urn:v{i}> )>> ."
        ));
    }
    data.push_str(&format!(
        " <urn:c1> <{reifies}> <<( <urn:target> <urn:p> <urn:o1> )>> .\
          <urn:c2> <{reifies}> <<( <urn:s2> <urn:p> <urn:target> )>> .\
          <urn:c3> <{reifies}> <<( <urn:s3> <urn:target> <urn:o3> )>> . }} }}"
    ));
    crate::sparql::update::update(&s, &data, &crate::sparql::QueryOptions::default()).unwrap();
    assert_eq!(
        lookup(&s.snapshot(), &["urn:target"]),
        (vec!["urn:c1".into(), "urn:c2".into()], false)
    );

    // Full compaction writes reifiers.bin and preserves exact results
    s.compact().unwrap();
    assert!(s.snapshot().generation.reifiers.file.is_some());
    assert_eq!(
        lookup(&s.snapshot(), &["urn:target"]),
        (vec!["urn:c1".into(), "urn:c2".into()], false)
    );

    // Delete a base reifier and insert a delta reifier in the same graph
    crate::sparql::update::update(
        &s,
        &format!(
            "DELETE DATA {{ GRAPH <urn:g> {{ <urn:c1> <{reifies}> <<( <urn:target> <urn:p> <urn:o1> )>> }} }};\
             INSERT DATA {{ GRAPH <urn:g> {{ <urn:c4> <{reifies}> <<( <urn:target> <urn:p> <urn:target> )>> }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        lookup(&s.snapshot(), &["urn:target"]),
        (vec!["urn:c2".into(), "urn:c4".into()], false)
    );

    // Re-insert the deleted base reifier (clears delta.del) and delete the delta reifier
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ GRAPH <urn:g> {{ <urn:c1> <{reifies}> <<( <urn:target> <urn:p> <urn:o1> )>> }} }};\
             DELETE DATA {{ GRAPH <urn:g> {{ <urn:c4> <{reifies}> <<( <urn:target> <urn:p> <urn:target> )>> }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    assert_eq!(
        lookup(&s.snapshot(), &["urn:target"]),
        (vec!["urn:c1".into(), "urn:c2".into()], false)
    );

    // Reopen persistent store and check
    drop(s);
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    assert!(s.snapshot().generation.reifiers.file.is_some());
    assert_eq!(
        lookup(&s.snapshot(), &["urn:target"]),
        (vec!["urn:c1".into(), "urn:c2".into()], false)
    );
}

#[test]
fn reifiers_about_indexed_rejects_synthetic_hash_collision() {
    use crate::store::{Store, StoreOptions};
    let s = Store::in_memory(StoreOptions::default());
    let reifies = crate::reifiers::RDF_REIFIES;
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ GRAPH <urn:g> {{\
             <urn:c_other> <{reifies}> <<( <urn:other_s> <urn:p> <urn:other_o> )>> .\
             <urn:c_match> <{reifies}> <<( <urn:target> <urn:p> <urn:o> )>> . }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let mut snap = (*s.snapshot()).clone();
    let g_id = snap.lookup_iri("urn:g").unwrap();
    let c_other_id = snap.lookup_iri("urn:c_other").unwrap();
    let other_tt = snap
        .lookup_term(&Term::Triple(Box::new(Triple::new(
            iri("urn:other_s"),
            iri("urn:p"),
            iri("urn:other_o"),
        ))))
        .unwrap();
    // Inject a synthetic hash-collision key for <urn:target> pointing at c_other's valid
    // rdf:reifies quad (whose triple term does not mention <urn:target>).
    let target_hash = crate::reifiers::term_hash(b"urn:target");
    snap.delta
        .reifiers_ins
        .insert([g_id.0, target_hash, c_other_id.0, other_tt.0]);

    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    let mut reads = Reads::new(&prep, &snap);
    let mut held = prep.reserve(0).unwrap();
    let wanted: BTreeSet<String> = ["urn:target".to_string()].into();
    let (rows, more) = reads
        .reifiers_about(reifies, "urn:g", &wanted, 100, &mut held)
        .unwrap();
    assert!(!more);
    assert_eq!(
        rows.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
        vec!["urn:c_match"]
    );
}

#[test]
fn reifiers_about_indexed_respects_triple_masks_on_base_and_delta() {
    use crate::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    use crate::access::{GraphAccess, Graphs};
    use crate::index::Perm;
    use crate::store::{Store, StoreOptions};

    let tmp = tempfile::tempdir().unwrap();
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    let reifies = crate::reifiers::RDF_REIFIES;
    // Base reifier in gen-0001 after compaction.
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ GRAPH <urn:g> {{\
             <urn:c_base> <{reifies}> <<( <urn:target> <urn:p> <urn:o1> )>> . }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    s.compact().unwrap();
    assert!(s.snapshot().generation.reifiers.file.is_some());

    // Delta reifier in the same graph.
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ GRAPH <urn:g> {{\
             <urn:c_delta> <{reifies}> <<( <urn:target> <urn:p> <urn:o2> )>> . }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let snap = s.snapshot();
    assert!(!snap.delta.reifiers_ins.is_empty());

    let lookup = |view: &Snapshot| -> Vec<String> {
        let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
        let mut reads = Reads::new(&prep, view);
        let mut held = prep.reserve(0).unwrap();
        let wanted: BTreeSet<String> = ["urn:target".to_string()].into();
        let (rows, more) = reads
            .reifiers_about(reifies, "urn:g", &wanted, 100, &mut held)
            .unwrap();
        assert!(!more);
        let mut names: Vec<String> = rows.into_iter().map(|n| n.into_string()).collect();
        names.sort();
        names
    };
    assert_eq!(lookup(&snap), vec!["urn:c_base", "urn:c_delta"]);

    // 1. Legacy triple mask hiding both the base and delta rdf:reifies quads.
    let mut hidden = Vec::new();
    snap.for_each_quad(|q| {
        if q[1] == snap.lookup_iri(reifies).unwrap() {
            hidden.push(Perm::Spo.to_key(q));
        }
        Ok(())
    })
    .unwrap();
    hidden.sort_unstable();
    assert_eq!(hidden.len(), 2);
    let legacy_masked = crate::access::triples::apply(&snap, hidden, "mask-reifies".into());
    assert!(
        legacy_masked.delta.reifiers_ins.is_empty(),
        "legacy mask should remove hidden delta entries from reifiers_ins"
    );
    assert!(lookup(&legacy_masked).is_empty());

    // 2. Controlled triple mask hiding rdf:reifies via GraphAccess rule.
    let access = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        TripleRules {
            rules: vec![Rule {
                protection: std::sync::Arc::new(Protection {
                    name: "hide-reifies".into(),
                    predicates: Some(vec![reifies.into()]),
                    classes: None,
                    subclasses: false,
                    graphs: None,
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    );
    let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
    let prepared = crate::access::triples::controlled::prepare(&access, &snap, &prep).unwrap();
    assert!(
        prepared.snapshot().delta.reifiers_ins.is_empty(),
        "controlled mask should remove hidden delta entries from reifiers_ins"
    );
    assert!(lookup(prepared.snapshot()).is_empty());
}

#[test]
fn reifiers_partial_compaction_rebuilds_and_removes_index() {
    use crate::store::{CompactOptions, PartialMode, Store, StoreOptions};

    let tmp = tempfile::tempdir().unwrap();
    let s = Store::open(tmp.path(), StoreOptions::default()).unwrap();
    let reifies = crate::reifiers::RDF_REIFIES;
    // Seed base vocabulary with all terms that subsequent partial-compaction edits reuse.
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ GRAPH <urn:g> {{\
             <urn:c1> <{reifies}> <<( <urn:e1> <urn:p> <urn:o1> )>> .\
             <urn:c2> <{reifies}> <<( <urn:e2> <urn:p> <urn:o2> )>> .\
             <urn:c3> <urn:unused> <<( <urn:e1> <urn:p> <urn:o2> )>> . }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    s.compact().unwrap();
    assert!(s.snapshot().generation.reifiers.file.is_some());

    let lookup = |snap: &Snapshot, entity: &str| -> Vec<String> {
        let prep = Preparation::new(Control::none(), 16 << 20).unwrap();
        let mut reads = Reads::new(&prep, snap);
        let mut held = prep.reserve(0).unwrap();
        let wanted: BTreeSet<String> = [entity.to_string()].into();
        let (rows, more) = reads
            .reifiers_about(reifies, "urn:g", &wanted, 100, &mut held)
            .unwrap();
        assert!(!more);
        let mut names: Vec<String> = rows.into_iter().map(|n| n.into_string()).collect();
        names.sort();
        names
    };

    // Edit using only existing base vocabulary IDs so PartialMode::Always runs partial compaction.
    crate::sparql::update::update(
        &s,
        &format!(
            "DELETE DATA {{ GRAPH <urn:g> {{ <urn:c1> <{reifies}> <<( <urn:e1> <urn:p> <urn:o1> )>> }} }};\
             INSERT DATA {{ GRAPH <urn:g> {{ <urn:c3> <{reifies}> <<( <urn:e1> <urn:p> <urn:o2> )>> }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rep.mode, "partial", "{rep:?}");
    assert!(s.snapshot().generation.reifiers.file.is_some());
    assert_eq!(lookup(&s.snapshot(), "urn:e1"), vec!["urn:c3"]);
    assert_eq!(lookup(&s.snapshot(), "urn:e2"), vec!["urn:c2"]);
    let gdir = s.snapshot().generation.dir.clone().unwrap();
    let (problems, _) = crate::reifiers::verify(&gdir, false, 10).unwrap();
    assert!(problems.is_empty(), "{problems:?}");

    // Delete all remaining reifiers and run partial compaction: reifiers.bin should be absent.
    crate::sparql::update::update(
        &s,
        &format!(
            "DELETE DATA {{ GRAPH <urn:g> {{\
             <urn:c2> <{reifies}> <<( <urn:e2> <urn:p> <urn:o2> )>> .\
             <urn:c3> <{reifies}> <<( <urn:e1> <urn:p> <urn:o2> )>> }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let rep = s
        .compact_with(&CompactOptions {
            partial: Some(PartialMode::Always),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(rep.mode, "partial", "{rep:?}");
    assert!(s.snapshot().generation.reifiers.file.is_none());
    assert!(s.snapshot().generation.reifiers.is_indexed());
    assert!(lookup(&s.snapshot(), "urn:e1").is_empty());
    assert!(lookup(&s.snapshot(), "urn:e2").is_empty());
}

#[test]
fn reifiers_dvocab_rollback_invalidates_cached_predicate_id() {
    use crate::io::{RdfFormat, Source};
    use crate::store::{Store, StoreOptions};

    let reifies = crate::reifiers::RDF_REIFIES;
    let reifier_quad = oxrdf::Quad::new(
        iri("urn:c_dry"),
        iri(reifies),
        Term::Triple(Box::new(Triple::new(
            iri("urn:e1"),
            iri("urn:p"),
            iri("urn:o1"),
        ))),
        GraphName::NamedNode(iri("urn:g")),
    );

    // 1. Dry-run preview rollback alone (no second rollback before unrelated predicate reuse).
    {
        let tmp = tempfile::tempdir().unwrap();
        let s = Store::open(&tmp.path().join("dry"), StoreOptions::default()).unwrap();
        let mut txn = s.write();
        let ids = txn
            .encode_quad(&reifier_quad, &mut Default::default())
            .unwrap();
        txn.insert(ids).unwrap();
        let _preview = txn.preview(crate::preview::DryRun::default()).unwrap();

        crate::sparql::update::update(
            &s,
            "INSERT DATA { GRAPH <urn:g> { <urn:x> <urn:other_pred> <<( <urn:e1> <urn:p> <urn:o1> )>> } }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        assert!(
            s.snapshot().delta.reifiers_ins.is_empty(),
            "unrelated predicate reusing dry-run rolled-back slot must not populate reifiers_ins"
        );
    }

    // 2. Explicit discard_terms rollback alone, followed by live vs reopened parity.
    {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("discard");
        let s = Store::open(&root, StoreOptions::default()).unwrap();
        {
            let mut txn = s.write();
            let ids = txn
                .encode_quad(&reifier_quad, &mut Default::default())
                .unwrap();
            txn.insert(ids).unwrap();
            txn.discard_terms().unwrap();
        }
        crate::sparql::update::update(
            &s,
            "INSERT DATA { GRAPH <urn:g> { <urn:x> <urn:other_pred> <<( <urn:e1> <urn:p> <urn:o1> )>> } }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        assert!(
            s.snapshot().delta.reifiers_ins.is_empty(),
            "unrelated predicate reusing discard_terms slot must not populate reifiers_ins"
        );

        // Now write a real rdf:reifies quad on the persistent store and compare live vs reopened.
        crate::sparql::update::update(
            &s,
            &format!(
                "INSERT DATA {{ GRAPH <urn:g> {{ <urn:c_real> <{reifies}> <<( <urn:e1> <urn:p> <urn:o1> )>> }} }}"
            ),
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        let live_keys: Vec<_> = s.snapshot().delta.reifiers_ins.iter().copied().collect();
        assert_eq!(live_keys.len(), 2);
        drop(s);

        let reopened = Store::open(&root, StoreOptions::default()).unwrap();
        let reopened_keys: Vec<_> = reopened
            .snapshot()
            .delta
            .reifiers_ins
            .iter()
            .copied()
            .collect();
        assert_eq!(live_keys, reopened_keys);
    }

    // 3. Failed transactional load rollback alone (first quad interns rdf:reifies, second fails parse).
    {
        let s = Store::in_memory(StoreOptions::default());
        let bad_nq = format!(
            "<urn:c_load> <{reifies}> <<( <urn:e1> <urn:p> <urn:o1> )>> <urn:g> .\n\
             not valid nquads\n"
        );
        assert!(
            s.load(&[Source::from_bytes(
                bad_nq.into_bytes(),
                RdfFormat::NQuads,
                None,
            )])
            .is_err()
        );
        crate::sparql::update::update(
            &s,
            "INSERT DATA { GRAPH <urn:g> { <urn:x> <urn:other_pred> <<( <urn:e1> <urn:p> <urn:o1> )>> } }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        assert!(
            s.snapshot().delta.reifiers_ins.is_empty(),
            "unrelated predicate reusing failed-load rolled-back slot must not populate reifiers_ins"
        );
    }

    // 4. Fork an in-memory branch after rollback and verify it did not inherit a stale predicate ID.
    {
        let mem_branch = Store::in_memory(StoreOptions::default());
        {
            let mut txn = mem_branch.write();
            let ids = txn
                .encode_quad(&reifier_quad, &mut Default::default())
                .unwrap();
            txn.insert(ids).unwrap();
            txn.discard_terms().unwrap();
        }
        crate::sparql::update::update(
            &mem_branch,
            "INSERT DATA { GRAPH <urn:g> { <urn:x> <urn:other_pred> <<( <urn:e1> <urn:p> <urn:o1> )>> } }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        mem_branch
            .create_branch("dev", &Default::default())
            .unwrap();
        let dev = mem_branch.branch("dev").unwrap();
        crate::sparql::update::update(
            &dev,
            "INSERT DATA { GRAPH <urn:g> { <urn:y> <urn:other_pred> <<( <urn:e2> <urn:p> <urn:o2> )>> } }",
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        assert!(dev.snapshot().delta.reifiers_ins.is_empty());
    }
}

#[test]
fn in_memory_early_write_admission_charges_proposed_reifier_keys() {
    use crate::Error;
    use crate::io::{RdfFormat, Source};
    use crate::store::{ReplaceTarget, Store, StoreOptions};

    let reifies = crate::reifiers::RDF_REIFIES;
    // One reifier quad with distinct IRI subject & object projects 448 + 128 = 576 bytes.
    // A cap of 500 sits strictly between the quad-only projection (448) and the full
    // reifier-aware projection (576), so early admission must refuse before interning terms.
    let nq = format!("<urn:c1> <{reifies}> <<( <urn:s1> <urn:p> <urn:o1> )>> <urn:g> .\n");
    let opts = StoreOptions {
        max_memory_bytes: Some(500),
        ..Default::default()
    };

    // 1. Transactional replace.
    let mem_replace = Store::in_memory(opts.clone());
    let err = mem_replace
        .replace(
            ReplaceTarget::Named(iri("urn:g")),
            &[Source::from_bytes(
                nq.as_bytes().to_vec(),
                RdfFormat::NQuads,
                None,
            )],
        )
        .unwrap_err();
    assert!(matches!(err, Error::StorageFull(_)), "got {err}");
    assert!(mem_replace.snapshot().is_empty());
    assert_eq!(
        mem_replace.snapshot().generation.dvocab.with(|v| v.len()),
        0,
        "early refusal of replace must not leak newly interned terms"
    );

    // 2. Transactional load.
    let mem_load = Store::in_memory(opts);
    let err = mem_load
        .load(&[Source::from_bytes(nq.into_bytes(), RdfFormat::NQuads, None)])
        .unwrap_err();
    assert!(matches!(err, Error::StorageFull(_)), "got {err}");
    assert!(mem_load.snapshot().is_empty());
    assert_eq!(
        mem_load.snapshot().generation.dvocab.with(|v| v.len()),
        0,
        "early refusal of load must not leak newly interned terms"
    );
}

/// Non-IRI subjects (blank nodes) do not consume the IRI-pair or IRI-subject limit
/// (including at `limit = 0`), partially masked subjects are skipped before counting,
/// `hides_subject_in_graph` checks the mask directly without count-cap saturation, and
/// `AdmittedScan` retains its preparation credit until dropped.
#[test]
fn admitted_scans_skip_non_iri_and_masked_subjects_and_retain_credit() {
    use crate::access::{GraphAccess, GraphRule, Graphs, triples};
    use crate::store::{Store, StoreOptions};
    use std::sync::Arc;

    let s = Store::in_memory(StoreOptions::default());
    let g1 = "urn:x-sparkles:graphrag:reports";
    let g2 = "urn:x-sparkles:graphrag:other";
    let mut trig = format!("GRAPH <{g1}> {{\n");
    for i in 0..5 {
        trig.push_str(&format!("  _:b{i} <urn:p> <urn:target> .\n"));
    }
    trig.push_str("  <urn:s:visible> <urn:p> <urn:target> .\n");
    trig.push_str("  <urn:s:masked> <urn:p> <urn:target> ; <urn:secret> \"hidden\" .\n");
    for i in 0..20 {
        trig.push_str(&format!("  <urn:s:masked> <urn:extra> \"v{i}\" .\n"));
        trig.push_str(&format!("  <urn:s:visible> <urn:extra> \"v{i}\" .\n"));
    }
    trig.push_str("}\n");
    trig.push_str(&format!(
        "GRAPH <{g2}> {{ <urn:s:visible> <urn:secret> \"other-graph\" . }}\n"
    ));
    crate::sparql::update::update(
        &s,
        &format!("INSERT DATA {{ {trig} }}"),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();

    let snap = s.snapshot();
    let target = Term::NamedNode(NamedNode::new_unchecked("urn:target"));

    // 1. Blank nodes never set `more = true` at limit 0 or consume positive limits.
    let s_only_bnode = Store::in_memory(StoreOptions::default());
    crate::sparql::update::update(
        &s_only_bnode,
        &format!("INSERT DATA {{ GRAPH <{g1}> {{ _:b0 <urn:p> <urn:target> . }} }}"),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let bnode_snap = s_only_bnode.snapshot();
    let prep = Preparation::new(crate::task::Control::none(), 1 << 20).unwrap();
    let mut bnode_reads = Reads::new(&prep, &bnode_snap);
    let zero_cross = bnode_reads
        .subjects_in_graphs_up_to("urn:p", &target, 0, false, |_| true)
        .unwrap();
    assert!(zero_cross.items.is_empty());
    assert!(!zero_cross.more);
    let zero_single = bnode_reads
        .visible_subjects_up_to("urn:p", &target, g1, 0, false)
        .unwrap();
    assert!(zero_single.items.is_empty());
    assert!(!zero_single.more);

    // Mix of blank nodes before named subjects finds the named subjects with limit = 2.
    let mut reads = Reads::new(&prep, &snap);
    let cross = reads
        .subjects_in_graphs_up_to("urn:p", &target, 2, false, |g| g == g1)
        .unwrap();
    assert!(!cross.more);
    assert_eq!(cross.items.len(), 2);

    // 2. Direct mask lookup detects hidden quads even when capped counts saturate, and
    //    is scoped to the requested graph.
    let mask_access = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([g1, g2], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    );
    let prepared = mask_access.masked_controlled(&snap, &prep).unwrap();
    let mut seen = Reads::from_prepared(&prep, &prepared);
    // A capped count of 10 saturates on both snapshots, missing the hidden quad.
    assert_eq!(reads.subject_triples("urn:s:masked", g1, 10).unwrap(), 10);
    assert_eq!(seen.subject_triples("urn:s:masked", g1, 10).unwrap(), 10);
    // Direct mask inspection does not saturate and distinguishes graphs.
    assert!(seen.hides_subject_in_graph("urn:s:masked", g1).unwrap());
    assert!(!seen.hides_subject_in_graph("urn:s:visible", g1).unwrap());
    assert!(seen.hides_subject_in_graph("urn:s:visible", g2).unwrap());

    // 3. `skip_masked = true` excludes partially masked subjects before checking `limit`.
    let only_visible = seen
        .subjects_in_graphs_up_to("urn:p", &target, 1, true, |g| g == g1)
        .unwrap();
    assert!(!only_visible.more);
    assert_eq!(only_visible.items.len(), 1);
    assert_eq!(only_visible.items[0].0.as_str(), "urn:s:visible");
    assert_eq!(only_visible.items[0].1.as_str(), g1);

    let single_visible = seen
        .visible_subjects_up_to("urn:p", &target, g1, 1, true)
        .unwrap();
    assert!(!single_visible.more);
    assert_eq!(single_visible.items.len(), 1);
    assert_eq!(single_visible.items[0].as_str(), "urn:s:visible");

    // 4. `AdmittedScan` retains its preparation reservation until dropped, and a tight
    //    allowance refuses before allocating oversized graph/subject IRIs.
    let long_store = Store::in_memory(StoreOptions::default());
    let long_graph = format!("urn:x-sparkles:graphrag:{}", "g".repeat(2048));
    let long_subj = format!("urn:subj:{}", "s".repeat(2048));
    crate::sparql::update::update(
        &long_store,
        &format!(
            "INSERT DATA {{ GRAPH <{long_graph}> {{ <{long_subj}> <urn:p> <urn:target> . }} }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let long_snap = long_store.snapshot();
    const LIMIT: u64 = 512 << 10;
    let prep_keep = Preparation::new(crate::task::Control::none(), LIMIT).unwrap();
    let mut long_reads = Reads::new(&prep_keep, &long_snap);
    let scan = long_reads
        .subjects_in_graphs_up_to("urn:p", &target, 10, false, |_| true)
        .unwrap();
    assert_eq!(scan.items.len(), 1);
    // While `scan` is alive, its 4 KiB+ of retained IRIs stay charged on top of cache.
    assert!(prep_keep.reserve(LIMIT - 4096).is_err());
    drop(scan);
    drop(long_reads);
    assert!(prep_keep.reserve(LIMIT - 4096).is_ok());
}

/// Scan vector growth in `visible_subjects_up_to` and `subjects_in_graphs_up_to`
/// reserves the full replacement buffer while holding the old buffer's credit
/// (peak = old_cap + new_cap), rather than admitting only the net delta.
#[test]
fn scan_buffer_growth_reserves_reallocation_peak() {
    use crate::store::{Store, StoreOptions};

    let s = Store::in_memory(StoreOptions::default());
    let g1 = "urn:g:peak-1";
    let g2 = "urn:g:peak-2";
    let mut trig = format!("GRAPH <{g1}> {{\n");
    for i in 0..129 {
        trig.push_str(&format!("<urn:s:{i:04}> <urn:p> <urn:target> .\n"));
    }
    trig.push_str("}\n");
    trig.push_str(&format!(
        "GRAPH <{g2}> {{ <urn:s:9999> <urn:p> <urn:target> . }}\n"
    ));
    crate::sparql::update::update(
        &s,
        &format!("INSERT DATA {{ {trig} }}"),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let snap = s.snapshot();
    let target = Term::NamedNode(NamedNode::new_unchecked("urn:target"));

    // 1. Verify peak reservation at the 128 -> 256 reallocation of `Buffer<Id>` in
    //    `visible_subjects_up_to`.
    let prep = Preparation::new(crate::task::Control::none(), 4 << 20).unwrap();
    let mut reads = Reads::new(&prep, &snap);
    // Warm `Reads` cache so `base_used` isolates scan buffer reservations.
    assert!(reads.iri_id("urn:p").unwrap().is_some());
    assert!(reads.iri_id("urn:target").unwrap().is_some());
    assert!(reads.iri_id(g1).unwrap().is_some());
    let base_used = prep.inner_used();
    let id_new_bytes = 256 * std::mem::size_of::<Id>();
    let id_old_bytes = 128 * std::mem::size_of::<Id>();
    let (count, low, _) =
        crate::test_alloc::record_admitted_allocations(&prep, id_new_bytes, || {
            let scan = reads
                .visible_subjects_up_to("urn:p", &target, g1, 200, false)
                .unwrap();
            assert_eq!(scan.items.len(), 129);
        });
    assert!(count >= 1, "expected 256-slot Id buffer allocation");
    assert!(
        low >= base_used + (id_old_bytes + id_new_bytes) as u64,
        "Id buffer growth reserved only {} above base, expected peak >= {}",
        low.saturating_sub(base_used),
        id_old_bytes + id_new_bytes
    );

    // 2. Verify peak reservation at the 128 -> 256 reallocation of `Buffer<(Id, Id)>` in
    //    `subjects_in_graphs_up_to`.
    let pair_new_bytes = 256 * std::mem::size_of::<(Id, Id)>();
    let pair_old_bytes = 128 * std::mem::size_of::<(Id, Id)>();
    let base_used = prep.inner_used();
    let (count, low, _) =
        crate::test_alloc::record_admitted_allocations(&prep, pair_new_bytes, || {
            let scan = reads
                .subjects_in_graphs_up_to("urn:p", &target, 200, false, |g| g == g1)
                .unwrap();
            assert_eq!(scan.items.len(), 129);
        });
    assert!(count >= 1, "expected 256-slot (Id, Id) buffer allocation");
    assert!(
        low >= base_used + (pair_old_bytes + pair_new_bytes) as u64,
        "pair buffer growth reserved only {} above base, expected peak >= {}",
        low.saturating_sub(base_used),
        pair_old_bytes + pair_new_bytes
    );

    // 3. A tight allowance between net new capacity (4096 B) and old+new peak (6144 B)
    //    must refuse with BudgetExceeded(Memory) at the 128 -> 256 growth step, before
    //    reaching `g2` (whose callback would cancel the scan).
    let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let control = crate::task::Control::with_cancel(cancel.clone());
    const BUDGET: u64 = 64 << 10;
    let tight = Preparation::new(control, BUDGET).unwrap();
    let mut tight_reads = Reads::new(&tight, &snap);
    assert!(tight_reads.iri_id("urn:p").unwrap().is_some());
    assert!(tight_reads.iri_id("urn:target").unwrap().is_some());
    // Leave 5,800 bytes: enough for accepted map (1,024 + 160) + g1 IRI (~120) + net
    // 256-slot pair vec (4,096) = ~5,400, but below the old+new peak (~7,450).
    let avail = tight.available_bytes();
    let _fill = tight.reserve(avail - 5800).unwrap();
    let err = match tight_reads.subjects_in_graphs_up_to("urn:p", &target, 200, false, |g| {
        if g == g2 {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            return false;
        }
        g == g1
    }) {
        Ok(_) => panic!("expected memory budget refusal at pair buffer reallocation peak"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            crate::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            })
        ),
        "expected Memory BudgetExceeded before g2 cancellation, got {err:?}"
    );
}

/// Pre-resolved `admitted` graph IDs skip unreadable oversized graph IRIs without
/// reading or charging their vocabulary entries, and `subjects_for_typed_object_in_graphs_up_to`
/// validates the predecessor's type and mask before counting edges against `limit`.
#[test]
fn admitted_graph_ids_skip_oversized_unreadable_graphs_and_check_object_type() {
    use crate::access::{GraphAccess, GraphRule, Graphs, triples};
    use crate::store::{Store, StoreOptions};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let s = Store::in_memory(StoreOptions::default());
    let g_ok = "urn:x-sparkles:graphrag:ok";
    let g_untyped = "urn:x-sparkles:graphrag:untyped";
    let g_masked = "urn:x-sparkles:graphrag:masked";
    let g_huge_unreadable = format!("urn:x-sparkles:graphrag:secret-{}", "z".repeat(140 * 1024));
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let report_cls = "urn:x-sparkles:graphrag#Report";
    let edit_of = "urn:x-sparkles:graphrag#editOf";
    let target_iri = "urn:report:base";
    let target = Term::NamedNode(NamedNode::new_unchecked(target_iri));

    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ \
                GRAPH <{g_huge_unreadable}> {{ \
                    <{target_iri}> <{rdf_type}> <{report_cls}> . \
                    <urn:edit:hidden> <{edit_of}> <{target_iri}> . \
                }} \
                GRAPH <{g_untyped}> {{ \
                    <urn:edit:untyped> <{edit_of}> <{target_iri}> . \
                }} \
                GRAPH <{g_masked}> {{ \
                    <{target_iri}> <{rdf_type}> <{report_cls}> ; <urn:secret> \"x\" . \
                    <urn:edit:masked> <{edit_of}> <{target_iri}> . \
                }} \
                GRAPH <{g_ok}> {{ \
                    <{target_iri}> <{rdf_type}> <{report_cls}> . \
                    <urn:edit:visible> <{edit_of}> <{target_iri}> . \
                }} \
            }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();

    let snap = s.snapshot();
    let mask_prep = Preparation::new(crate::task::Control::none(), 1 << 20).unwrap();
    let mask_access = GraphAccess::with_triples(
        Graphs::All,
        Graphs::none(),
        triples::TripleRules {
            rules: vec![triples::Rule {
                protection: Arc::new(triples::Protection {
                    name: "hide-secret".into(),
                    predicates: Some(vec!["urn:secret".into()]),
                    classes: None,
                    subclasses: false,
                    graphs: Some(Graphs::Only(GraphRule::new([g_masked], &[]))),
                    pattern: None,
                    prefixes: Default::default(),
                    hide_inferences: false,
                }),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: triples::Caller::default(),
            limits: triples::Limits::default(),
        },
    );
    let prepared = mask_access.masked_controlled(&snap, &mask_prep).unwrap();

    // Even under a 64 KiB budget (smaller than the 140 KiB unreadable graph IRI itself),
    // `admitted = Some(map)` rejects `g_huge_unreadable` by ID without touching vocab,
    // and `object_type` skips `g_untyped` and `g_masked` before counting against `limit = 1`.
    let tight = Preparation::new(crate::task::Control::none(), 64 << 10).unwrap();
    let mut seen = Reads::from_prepared(&tight, &prepared);
    let mut admitted = BTreeMap::new();
    for g in [g_ok, g_untyped, g_masked] {
        let id = seen.iri_id(g).unwrap().unwrap();
        admitted.insert(id, Arc::new(NamedNode::new_unchecked(g)));
    }
    let scan = seen
        .subjects_for_typed_object_in_graphs_up_to(
            edit_of,
            &target,
            rdf_type,
            report_cls,
            1,
            true,
            Some(&admitted),
            |_| true,
        )
        .unwrap();
    assert!(!scan.more);
    assert_eq!(scan.items.len(), 1);
    assert_eq!(scan.items[0].0.as_str(), "urn:edit:visible");
    assert_eq!(scan.items[0].1.as_str(), g_ok);
}

/// Mask preparation admits graphs before full term decoding: an exact allowlist rejects
/// a 512 KiB unreadable graph by ID under a 400 KiB budget (smaller than the IRI itself),
/// and a guarded wildcard rule checks it with `with_iri` instead of `decode_term`'s 128x
/// multiplier.
#[test]
fn mask_preparation_admits_graphs_before_full_decoding() {
    use crate::access::{
        Decider, Exemptions, GraphAccess, GraphRule, Graphs, GuardDecision, GuardSnapshot, triples,
    };
    use crate::store::{Store, StoreOptions};
    use std::sync::Arc;

    struct FixedGuard(Vec<String>);
    impl GuardDecision for FixedGuard {
        fn exempt(&self, _snap: &Arc<crate::store::Snapshot>) -> Vec<String> {
            self.0.clone()
        }
        fn exempt_controlled(
            &self,
            _snap: &GuardSnapshot<'_>,
            preparation: &Preparation,
        ) -> Result<Exemptions> {
            let mut out = Exemptions::new(preparation)?;
            for g in &self.0 {
                out.push(g)?;
            }
            Ok(out)
        }
    }

    let s = Store::in_memory(StoreOptions::default());
    let g_ok = "urn:x-sparkles:graphrag:ok";
    let g_huge_unreadable = format!("urn:x-sparkles:graphrag:secret-{}", "z".repeat(512 * 1024));
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let report_cls = "urn:x-sparkles:graphrag#Report";
    let secret_pred = "urn:secret:tag";

    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ \
                GRAPH <{g_huge_unreadable}> {{ \
                    <urn:hidden:r> <{rdf_type}> <{report_cls}> ; <{secret_pred}> \"x\" . \
                }} \
                GRAPH <{g_ok}> {{ \
                    <urn:visible:r> <{rdf_type}> <{report_cls}> . \
                    <urn:masked:r> <{rdf_type}> <{report_cls}> ; <{secret_pred}> \"x\" . \
                }} \
            }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();

    let snap = s.snapshot();
    let rules = || triples::TripleRules {
        rules: vec![triples::Rule {
            protection: Arc::new(triples::Protection {
                name: "hide-secret".into(),
                predicates: Some(vec![secret_pred.into()]),
                classes: None,
                subclasses: false,
                graphs: None,
                pattern: None,
                prefixes: Default::default(),
                hide_inferences: false,
            }),
            read: Graphs::none(),
            write: Graphs::none(),
        }],
        caller: triples::Caller::default(),
        limits: triples::Limits::default(),
    };

    // 1. Exact allowlist rejects the 512 KiB unreadable graph by ID under a 400 KiB cap
    //    (smaller than the unreadable graph IRI itself).
    let exact_prep = Preparation::new(crate::task::Control::none(), 400 << 10).unwrap();
    let exact_access = GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([g_ok], &[])),
        Graphs::none(),
        rules(),
    );
    let exact_view = exact_access
        .masked_controlled_read_only(&snap, &exact_prep)
        .unwrap();
    let mut exact_reads = Reads::from_prepared(&exact_prep, &exact_view);
    assert!(
        exact_reads
            .hides_subject_in_graph("urn:masked:r", g_ok)
            .unwrap()
    );
    assert!(
        !exact_reads
            .hides_subject_in_graph("urn:visible:r", g_ok)
            .unwrap()
    );

    // 2. Guarded wildcard rule checks the 512 KiB unreadable graph via `with_iri`
    //    instead of `with_term` (128x charge = ~64 MiB), succeeding under 2 MiB.
    let guard_prep = Preparation::new(crate::task::Control::none(), 2 << 20).unwrap();
    let guard_access = GraphAccess::with_triples(
        Graphs::All.guarded_by(
            "urn:x-sparkles:graphrag:*",
            Decider(Arc::new(FixedGuard(vec![g_ok.into()]))),
        ),
        Graphs::none(),
        rules(),
    );
    let guard_view = guard_access
        .masked_controlled_read_only(&snap, &guard_prep)
        .unwrap();
    let mut guard_reads = Reads::from_prepared(&guard_prep, &guard_view);
    assert!(
        guard_reads
            .hides_subject_in_graph("urn:masked:r", g_ok)
            .unwrap()
    );
    assert!(
        !guard_reads
            .hides_subject_in_graph("urn:visible:r", g_ok)
            .unwrap()
    );
}

/// On a compacted store (both plain front-coded and FSST vocabularies), exact graph
/// admission must not reconstruct a 512 KiB denied graph name sorting immediately before
/// the granted graph name in the same vocabulary block, whether the denied graph holds a
/// protected predicate or only an unrelated predicate.
#[test]
fn compacted_exact_graph_admission_ignores_preceding_oversized_denied_graph() {
    use crate::access::{GraphAccess, GraphRule, Graphs, triples};
    use crate::builder::{BuildOptions, VocabCodec};
    use crate::id::Tag;
    use crate::store::{Store, StoreOptions};
    use crate::vocab::FC_BLOCK;
    use std::sync::Arc;

    let g_denied = format!(
        "urn:x-sparkles:graphrag:a-denied-{}",
        "z".repeat(512 * 1024)
    );
    let g_ok = "urn:x-sparkles:graphrag:b-ok";
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let report_cls = "urn:x-sparkles:graphrag#Report";
    let secret_pred = "urn:secret:tag";
    let unrelated_pred = "urn:unrelated:tag";

    for codec in [VocabCodec::Front, VocabCodec::Fsst] {
        for denied_uses_protected_pred in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let opts = StoreOptions {
                build: BuildOptions {
                    vocab_codec: Some(codec),
                    ..Default::default()
                },
                ..Default::default()
            };
            {
                let s = Store::open(dir.path(), opts.clone()).unwrap();
                let denied_triple = if denied_uses_protected_pred {
                    format!("<urn:hidden:r> <{rdf_type}> <{report_cls}> ; <{secret_pred}> \"x\" .")
                } else {
                    format!("<urn:hidden:r> <{unrelated_pred}> \"x\" .")
                };
                crate::sparql::update::update(
                    &s,
                    &format!(
                        "INSERT DATA {{ \
                            GRAPH <{g_denied}> {{ {denied_triple} }} \
                            GRAPH <{g_ok}> {{ \
                                <urn:visible:r> <{rdf_type}> <{report_cls}> . \
                                <urn:masked:r> <{rdf_type}> <{report_cls}> ; <{secret_pred}> \"x\" . \
                            }} \
                        }}"
                    ),
                    &crate::sparql::QueryOptions::default(),
                )
                .unwrap();
                s.compact().unwrap();
            }

            let s = Store::open(dir.path(), opts).unwrap();
            let snap = s.snapshot();
            assert!(snap.delta.is_empty(), "expected compacted base-only store");
            let denied_id = snap.lookup_iri(&g_denied).expect("denied graph in base");
            let ok_id = snap.lookup_iri(g_ok).expect("granted graph in base");
            assert_eq!(denied_id.tag(), Tag::Vocab);
            assert_eq!(ok_id.tag(), Tag::Vocab);
            assert!(
                denied_id.payload() < ok_id.payload()
                    && denied_id.payload() as usize / FC_BLOCK
                        == ok_id.payload() as usize / FC_BLOCK,
                "denied graph ({}) must sort before granted graph ({}) in the same block \
                 (codec={codec:?}, protected={denied_uses_protected_pred})",
                denied_id.payload(),
                ok_id.payload()
            );

            let rules = triples::TripleRules {
                rules: vec![triples::Rule {
                    protection: Arc::new(triples::Protection {
                        name: "hide-secret".into(),
                        predicates: Some(vec![secret_pred.into()]),
                        classes: None,
                        subclasses: false,
                        graphs: None,
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: Graphs::none(),
                    write: Graphs::none(),
                }],
                caller: triples::Caller::default(),
                limits: triples::Limits::default(),
            };

            // 400 KiB cap is strictly smaller than the 512 KiB preceding denied graph IRI
            // while comfortably covering the 7-way parallel permutation headroom.
            let prep = Preparation::new(crate::task::Control::none(), 400 << 10).unwrap();
            let access = GraphAccess::with_triples(
                Graphs::Only(GraphRule::new([g_ok], &[])),
                Graphs::none(),
                rules,
            );
            let view = access
                .masked_controlled_read_only(&snap, &prep)
                .unwrap_or_else(|err| {
                    panic!(
                        "exact graph admission failed after compaction \
                         (codec={codec:?}, protected={denied_uses_protected_pred}): {err:?}"
                    )
                });
            let mut reads = Reads::from_prepared(&prep, &view);
            assert!(reads.hides_subject_in_graph("urn:masked:r", g_ok).unwrap());
            assert!(!reads.hides_subject_in_graph("urn:visible:r", g_ok).unwrap());
        }
    }
}

/// The predecessor `rdf:type` probe inside `subjects_for_typed_object_in_graphs_up_to`
/// uses controlled base-index reads charged to the same `Preparation` while the outer
/// POS scan workspace is live.
#[test]
fn typed_object_predecessor_probe_uses_controlled_base_index_reads() {
    use crate::store::{Store, StoreOptions};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let g_ok = "urn:x-sparkles:graphrag:compacted-probe";
    let rdf_type = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    let report_cls = "urn:x-sparkles:graphrag#Report";
    let edit_of = "urn:x-sparkles:graphrag#editOf";
    let target_iri = "urn:report:base";
    let target = Term::NamedNode(NamedNode::new_unchecked(target_iri));

    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let mut trig = format!(
            "GRAPH <{g_ok}> {{\n\
                <{target_iri}> <{rdf_type}> <{report_cls}> .\n\
                <urn:edit:1> <{edit_of}> <{target_iri}> .\n"
        );
        for i in 0..2000 {
            trig.push_str(&format!(
                "<urn:filler:{i:04}> <urn:filler:p> <urn:filler:o> .\n"
            ));
        }
        trig.push_str("}\n");
        crate::sparql::update::update(
            &s,
            &format!("INSERT DATA {{ {trig} }}"),
            &crate::sparql::QueryOptions::default(),
        )
        .unwrap();
        s.compact().unwrap();
    }

    // Reopen to ensure a cold BlockCache over the compacted base indexes.
    let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let snap = s.snapshot();
    assert!(
        snap.delta.is_empty(),
        "expected compacted base-only snapshot"
    );

    let run_with_headroom = |headroom: u64, check_type: bool| -> Result<usize> {
        let prep = Preparation::new(crate::task::Control::none(), 8 << 20)?;
        let mut reads = Reads::new(&prep, &snap);
        let g_id = reads.iri_id(g_ok)?.unwrap();
        let mut admitted = BTreeMap::new();
        admitted.insert(g_id, Arc::new(NamedNode::new_unchecked(g_ok)));
        // Pre-warm all lookup IDs so the remaining headroom measures only scan + probe workspaces.
        assert!(reads.iri_id(edit_of)?.is_some());
        assert!(reads.iri_id(target_iri)?.is_some());
        assert!(reads.iri_id(rdf_type)?.is_some());
        assert!(reads.iri_id(report_cls)?.is_some());
        let avail = prep.available_bytes();
        let _fill = prep.reserve(avail.saturating_sub(headroom))?;
        let scan = if check_type {
            reads.subjects_for_typed_object_in_graphs_up_to(
                edit_of,
                &target,
                rdf_type,
                report_cls,
                10,
                false,
                Some(&admitted),
                |_| true,
            )?
        } else {
            reads.subjects_in_admitted_graphs_up_to(
                edit_of,
                &target,
                10,
                false,
                Some(&admitted),
                |_| true,
            )?
        };
        Ok(scan.items.len())
    };

    // Normal budget succeeds against the cold compacted base.
    assert_eq!(run_with_headroom(4 << 20, true).unwrap(), 1);

    // Binary-search the tightest passing headroom for POS-only vs POS + nested SPO probe.
    let min_headroom = |check_type: bool| -> u64 {
        let mut lo = 0u64;
        let mut hi = 4 << 20;
        while lo + 256 < hi {
            let mid = lo + (hi - lo) / 2;
            match run_with_headroom(mid, check_type) {
                Ok(n) => {
                    assert_eq!(n, 1);
                    hi = mid;
                }
                Err(crate::Error::BudgetExceeded(crate::error::Budget {
                    kind: crate::error::BudgetKind::Memory,
                    ..
                })) => lo = mid,
                Err(err) => panic!("unexpected error probing headroom {mid}: {err:?}"),
            }
        }
        hi
    };

    let pos_only_min = min_headroom(false);
    let nested_spo_min = min_headroom(true);
    assert!(
        nested_spo_min > pos_only_min + 4096,
        "expected nested SPO base probe to reserve workspace above live POS scan \
         (pos_only_min={pos_only_min}, nested_spo_min={nested_spo_min})"
    );

    // Reopen again for a cold-cache verification at a headroom strictly between the two.
    drop(snap);
    drop(s);
    let cold = Store::open(dir.path(), StoreOptions::default()).unwrap();
    let cold_snap = cold.snapshot();
    let between = pos_only_min + (nested_spo_min - pos_only_min) / 2;
    let prep = Preparation::new(crate::task::Control::none(), 8 << 20).unwrap();
    let mut reads = Reads::new(&prep, &cold_snap);
    let g_id = reads.iri_id(g_ok).unwrap().unwrap();
    let mut admitted = BTreeMap::new();
    admitted.insert(g_id, Arc::new(NamedNode::new_unchecked(g_ok)));
    assert!(reads.iri_id(edit_of).unwrap().is_some());
    assert!(reads.iri_id(target_iri).unwrap().is_some());
    assert!(reads.iri_id(rdf_type).unwrap().is_some());
    assert!(reads.iri_id(report_cls).unwrap().is_some());
    let avail = prep.available_bytes();
    let _fill = prep.reserve(avail - between).unwrap();
    let err = match reads.subjects_for_typed_object_in_graphs_up_to(
        edit_of,
        &target,
        rdf_type,
        report_cls,
        10,
        false,
        Some(&admitted),
        |_| true,
    ) {
        Ok(_) => {
            panic!("expected Memory BudgetExceeded for nested SPO base read at headroom {between}")
        }
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            crate::Error::BudgetExceeded(crate::error::Budget {
                kind: crate::error::BudgetKind::Memory,
                ..
            })
        ),
        "expected Memory BudgetExceeded, got {err:?}"
    );
}

/// `Reads::subjects_in_graphs_up_to` invokes its `keep_graph` callback after releasing
/// the delta vocabulary read lock, so callbacks that resolve other delta terms on the
/// same snapshot cannot deadlock against a queued vocabulary writer.
#[test]
fn subjects_in_graphs_predicate_runs_outside_delta_vocab_lock() {
    use crate::store::{Store, StoreOptions};

    let s = Store::in_memory(StoreOptions::default());
    let g_delta = "urn:x-sparkles:graphrag:delta-lock-check";
    let other_delta = "urn:other:delta-term";
    crate::sparql::update::update(
        &s,
        &format!(
            "INSERT DATA {{ \
                GRAPH <{g_delta}> {{ \
                    <urn:s:1> <urn:p:1> <urn:o:1> . \
                    <{other_delta}> <urn:p:2> <urn:o:2> . \
                }} \
            }}"
        ),
        &crate::sparql::QueryOptions::default(),
    )
    .unwrap();
    let snap = s.snapshot();
    let prep = Preparation::new(crate::task::Control::none(), 4 << 20).unwrap();
    let mut reads = Reads::new(&prep, &snap);
    let target = Term::NamedNode(NamedNode::new_unchecked("urn:o:1"));

    let mut invoked = false;
    let scan = reads
        .subjects_in_graphs_up_to("urn:p:1", &target, 10, false, |graph_iri| {
            invoked = true;
            assert_eq!(graph_iri, g_delta);
            // If `with_iri` still held the delta vocabulary read guard, `try_write`
            // on the same thread would fail (returning `None`).
            let write_guard = snap
                .generation
                .dvocab
                .try_write_guard_for_test()
                .expect("keep_graph must run outside the delta vocabulary read lock");
            drop(write_guard);
            assert!(
                prep.lookup_iri(&snap, other_delta).unwrap().is_some(),
                "callback should be able to resolve another delta term safely"
            );
            true
        })
        .unwrap();

    assert!(invoked, "expected keep_graph predicate to run");
    assert!(!scan.more);
    assert_eq!(scan.items.len(), 1);
    assert_eq!(scan.items[0].0.as_str(), "urn:s:1");
    assert_eq!(scan.items[0].1.as_str(), g_delta);
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

#[test]
fn record_digest_distinguishes_duplicate_isomorphic_blank_expected_triples() {
    use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple};
    let s = NamedNode::new_unchecked("urn:uuid:11111111-1111-4111-8111-111111111111");
    let p_exp = NamedNode::new_unchecked("urn:x-sparkles:evidence:expectedTriple");
    let p_pred = NamedNode::new_unchecked("https://example.org/label");
    let t1 = Triple::new(
        s.clone(),
        p_exp.clone(),
        Term::Triple(Box::new(Triple::new(
            BlankNode::new_unchecked("a"),
            p_pred.clone(),
            Literal::new_simple_literal("alpha"),
        ))),
    );
    let t2 = Triple::new(
        s,
        p_exp,
        Term::Triple(Box::new(Triple::new(
            BlankNode::new_unchecked("b"),
            p_pred,
            Literal::new_simple_literal("alpha"),
        ))),
    );
    let single = record_digest(
        RecordKind::EvidenceRef,
        std::slice::from_ref(&t1),
        &Control::none(),
    )
    .unwrap();
    let same_twice = record_digest(
        RecordKind::EvidenceRef,
        &[t1.clone(), t1.clone()],
        &Control::none(),
    )
    .unwrap();
    let two_distinct = record_digest(RecordKind::EvidenceRef, &[t1, t2], &Control::none()).unwrap();
    assert_eq!(single, same_twice);
    assert_ne!(
        single, two_distinct,
        "two distinct RDF blank-node expectedTriple values must not collapse to one digest"
    );
}

#[test]
fn source_revision_predecessor_writes_prov_was_revision_of_and_rejects_cycles() {
    let s = settings(&["urn:s"], &["urn:c"]);
    let base = input("o1", "urn:o");
    let p_base = plan_of(&base, &s, Some("k")).unwrap();
    let rev_iri = p_base.records[0].iri.clone();
    let rev_digest = p_base.records[0].digest;
    assert!(
        !p_base.records[0]
            .triples
            .iter()
            .any(|t| t.predicate.as_str() == "http://www.w3.org/ns/prov#wasRevisionOf")
    );

    // Linking an existing predecessor keeps the revision's identity, writes
    // prov:wasRevisionOf, and changes both recordDigest and payloadDigest.
    let prior = iri("urn:uuid:22222222-2222-4222-8222-222222222222");
    let mut linked = base.clone();
    linked.revisions[0].was_revision_of = Some(RecordRef::Existing(prior.clone()));
    let p_linked = plan_of(&linked, &s, Some("k")).unwrap();
    assert_eq!(p_linked.records[0].iri, rev_iri);
    assert_ne!(p_linked.records[0].digest, rev_digest);
    assert_ne!(p_linked.payload_digest, p_base.payload_digest);
    assert!(p_linked.records[0].triples.iter().any(|t| {
        t.subject == rev_iri.clone().into()
            && t.predicate.as_str() == "http://www.w3.org/ns/prov#wasRevisionOf"
            && t.object == prior.clone().into()
    }));
    let nonce = Uuid::new_v4();
    let stamp = Stamp {
        dataset: DATASET,
        branch: DATASET,
        commit: 3,
        time_ms: 1_767_261_600_000,
    };
    let quads = p_linked.quads(&stamp, |_| Some(nonce), None, &[]).unwrap();
    assert!(quads.iter().any(|q| {
        q.subject == rev_iri.clone().into()
            && q.predicate.as_str() == "http://www.w3.org/ns/prov#wasRevisionOf"
            && q.object == prior.clone().into()
            && q.graph_name == GraphName::NamedNode(iri("urn:s"))
    }));

    // An existing-IRI self-link (targeting the revision's own minted IRI) is refused.
    let mut self_iri = base.clone();
    self_iri.revisions[0].was_revision_of = Some(RecordRef::Existing(rev_iri.clone()));
    let err = plan_of(&self_iri, &s, None).unwrap_err();
    assert!(err.to_string().contains("cannot revise itself"), "{err}");

    // A mixed Local/Existing cycle across two batch revisions is refused.
    let c = Control::none();
    let mut rev2 = base.revisions[0].clone();
    rev2.key = RecordKey::new("rev2").unwrap();
    rev2.content = SourceContent::MetadataOnly {
        digest: Sha256Digest::of_bytes(b"other-bytes", &c).unwrap(),
    };
    let mut two = base.clone();
    two.revisions.push(rev2);
    let p_two = plan_of(&two, &s, None).unwrap();
    let rev2_iri = p_two
        .records
        .iter()
        .find(|r| r.key == "rev2")
        .unwrap()
        .iri
        .clone();
    two.revisions[0].was_revision_of = Some(RecordRef::Existing(rev2_iri));
    two.revisions[1].was_revision_of = Some(RecordRef::Local(RecordKey::new("rev").unwrap()));
    let err = plan_of(&two, &s, None).unwrap_err();
    assert!(err.to_string().contains("cycle"), "{err}");
}

/// Allocation-free N-Triples and JSON-escaped byte estimates upper-bound actual
/// serialization, including control characters that expand up to 7x under N-Triples
/// plus JSON escaping.
#[test]
fn ntriples_and_json_escaped_byte_estimates_bound_actual_serialization() {
    let cases = vec![
        Triple::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            Literal::new_simple_literal("\0\x01\x07\x08\t\n\x0c\r\x1f\"\\\x7f\u{FFFE}\u{FFFF}abc"),
        ),
        Triple::new(
            oxrdf::BlankNode::new_unchecked("b0"),
            NamedNode::new_unchecked("urn:p"),
            Literal::new_language_tagged_literal_unchecked("hello\0world", "en-US"),
        ),
        Triple::new(
            NamedNode::new_unchecked("urn:s"),
            NamedNode::new_unchecked("urn:p"),
            Literal::new_typed_literal(
                "[\"\\u0000\"]",
                NamedNode::new_unchecked("http://example.org/dt"),
            ),
        ),
        Triple::new(
            NamedNode::new_unchecked("urn:outer"),
            NamedNode::new_unchecked("urn:reifies"),
            Term::Triple(Box::new(Triple::new(
                oxrdf::BlankNode::new_unchecked("inner"),
                NamedNode::new_unchecked("urn:q"),
                Literal::new_simple_literal("\0".repeat(256)),
            ))),
        ),
    ];
    for t in cases {
        let nt = t.to_string();
        let json = serde_json::to_string(&nt).unwrap();
        assert!(
            ntriples_triple_bytes(&t) >= nt.len() as u64,
            "ntriples estimate {} < actual {} for {nt}",
            ntriples_triple_bytes(&t),
            nt.len()
        );
        assert!(
            json_escaped_ntriples_triple_bytes(&t) >= json.len() as u64,
            "json estimate {} < actual {} for {json}",
            json_escaped_ntriples_triple_bytes(&t),
            json.len()
        );
    }
}

/// `subjects_up_to_held` and `objects_up_to_held` charge retained decoded clones to
/// the caller's reservation before cloning so long IRIs or literals cannot exceed the
/// preparation allowance.
#[test]
fn subjects_up_to_held_and_objects_up_to_held_charge_before_cloning() {
    use crate::store::{Store, StoreOptions};
    let s = Store::in_memory(StoreOptions::default());
    let mut data = String::from("INSERT DATA { GRAPH <urn:g> {");
    for i in 0..80 {
        data.push_str(&format!(
            " <urn:subj:{i}:{}> <urn:p> <urn:target> . <urn:head> <urn:rev> <urn:obj:{i}:{}> .",
            "x".repeat(1024),
            "y".repeat(1024)
        ));
    }
    data.push_str(" } }");
    crate::sparql::update::update(&s, &data, &crate::sparql::QueryOptions::default()).unwrap();
    let snap = s.snapshot();
    // 80 * ~1 KiB = ~80 KiB of retained IRIs per scan: both scans together fit in
    // 512 KiB, while a single scan refuses in 64 KiB.
    let roomy = Preparation::new(crate::task::Control::none(), 512 << 10).unwrap();
    let mut reads = Reads::new(&roomy, &snap);
    let mut held = roomy.reserve(0).unwrap();
    let (subs, more_s) = reads
        .subjects_up_to_held(
            "urn:p",
            &Term::NamedNode(NamedNode::new_unchecked("urn:target")),
            "urn:g",
            1000,
            &mut held,
        )
        .unwrap();
    assert_eq!(subs.len(), 80);
    assert!(!more_s);
    let (objs, more_o) = reads
        .objects_up_to_held("urn:head", "urn:rev", "urn:g", 1000, &mut held)
        .unwrap();
    assert_eq!(objs.len(), 80);
    assert!(!more_o);
    drop(held);

    let tight = Preparation::new(crate::task::Control::none(), 64 << 10).unwrap();
    let mut reads = Reads::new(&tight, &snap);
    let mut held = tight.reserve(0).unwrap();
    assert!(matches!(
        reads.subjects_up_to_held(
            "urn:p",
            &Term::NamedNode(NamedNode::new_unchecked("urn:target")),
            "urn:g",
            1000,
            &mut held,
        ),
        Err(Error::BudgetExceeded(_))
    ));
    drop(held);
    let mut held = tight.reserve(0).unwrap();
    assert!(matches!(
        reads.objects_up_to_held("urn:head", "urn:rev", "urn:g", 1000, &mut held),
        Err(Error::BudgetExceeded(_))
    ));
}

/// `ManagedWrite::record_protection` returns `None` for projection-only graphs so
/// `publish_quads` refuses to publish `ev:Operation` records into them.
#[test]
fn publish_quads_refuses_projection_only_graph() {
    let dir = tempfile::tempdir().unwrap();
    let store = crate::store::Store::open(
        &dir.path().join("ds"),
        crate::store::StoreOptions::default(),
    )
    .unwrap();
    let mut s = settings(&["urn:ev:sources"], &["urn:ev:claims"]);
    s.decisions.push("urn:ev:decisions".into());
    s.supports.push("urn:ev:supports".into());
    s.projections.push("urn:ev:output".into());
    s.policies.insert(
        "main".into(),
        crate::evidence::Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: crate::evidence::ConflictMode::Refuse,
            boundary: vec!["urn:ev:sources".into(), "urn:ev:claims".into()],
            decisions: "urn:ev:decisions".into(),
            supports: "urn:ev:supports".into(),
            output: "urn:ev:output".into(),
        },
    );
    let canonical = s.canonical(&Control::none()).unwrap();
    store
        .enable_managed(
            crate::managed::OwnerKind::Evidence,
            crate::managed::OwnerEntry::for_settings(1, s.managed_graphs(), canonical.bytes()),
            canonical.bytes(),
        )
        .unwrap();
    let cap = store
        .managed_writer(crate::managed::OwnerKind::Evidence, Uuid::new_v4())
        .unwrap();
    assert!(cap.is_projection_only("urn:ev:output"));
    assert!(cap.protection("urn:ev:output").is_some());
    assert_eq!(cap.record_protection("urn:ev:output"), None);
    let err = publish_quads(
        &Operation {
            iri: &iri("urn:op:1"),
            principal: &iri("urn:p"),
            payload_digest: Sha256Digest::of_bytes(b"p", &Control::none()).unwrap(),
            idempotency_key: None,
            hold: None,
            used: None,
        },
        &[iri("urn:ev:output")],
        &[],
        &Stamp {
            dataset: Uuid::new_v4(),
            branch: Uuid::new_v4(),
            commit: 1,
            time_ms: 0,
        },
        &|g| cap.record_protection(g.as_str()),
        &[],
    )
    .unwrap_err();
    assert_eq!(err.code(), "unsupported-profile");
}

/// On a compacted disk-backed snapshot where preceding graphs are ordered before the
/// requested graph (`id(g_before) < id(g_target)`), `subjects_up_to_held` and
/// `subjects_matching_up_to_held` must exercise the `found_g < g_id` exact-key fallback
/// via bounded `Perm::Pos` (`contains_pos_id`) rather than unbudgeted `Snapshot::contains`
/// (`Perm::Spo`), and the fallback probe itself must enforce `Preparation` memory and
/// cancellation limits.
#[test]
fn subjects_up_to_held_on_compacted_snapshot_with_preceding_graph_stays_bounded() {
    use crate::index::{ALL_COLS, Perm, SEG_ROWS};
    use crate::store::{Store, StoreOptions};

    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        // Place `<urn:s:0000> <urn:p> <urn:o>` in `SEG_ROWS + 2` preceding graphs
        // (`<urn:g:0000>..`) ordered before `<urn:z-target>`, plus `<urn:z-target>`
        // itself. In compacted `Perm::Pos`, `(p, o, s0, g:0000)` lands in segment 0
        // (2,048 rows) and `(p, o, s0, z-target)` lands in segment 1, so the initial
        // POS seek decodes only segment 0 while the `found_g < g_id` fallback must
        // probe segment 1 of `Perm::Pos` (never `Perm::Spo`).
        let mut data = String::from("INSERT DATA {");
        for g in 0..(SEG_ROWS + 2) {
            data.push_str(&format!(
                " GRAPH <urn:g:{g:04}> {{ <urn:s:0000> <urn:p> <urn:o> . }}"
            ));
        }
        data.push_str(" GRAPH <urn:g:0000> { <urn:s:0000> <urn:p2> <urn:o2> . }");
        data.push_str(
            " GRAPH <urn:z-target> { <urn:s:0000> <urn:p> <urn:o> . <urn:s:0000> <urn:p2> <urn:o2> . }",
        );
        data.push_str(" }");
        crate::sparql::update::update(&s, &data, &crate::sparql::QueryOptions::default()).unwrap();
        s.compact().unwrap();
    }

    let o = Term::NamedNode(NamedNode::new_unchecked("urn:o"));
    let o2 = Term::NamedNode(NamedNode::new_unchecked("urn:o2"));

    // 1. Cold reopen with roomy budget: `subjects_up_to_held` and
    // `subjects_matching_up_to_held` must decode `Perm::Pos` segment 0 (initial seek)
    // and segment 1 (the `found_g < g_id` `contains_pos_id` fallback), while leaving
    // `Perm::Spo` completely undecoded (proving `Snapshot::contains` was not used).
    let (p_id, o_id, s0_id, g_target, seg0_workspace, seg1_workspace) = {
        let s = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let snap = s.snapshot();
        let roomy = Preparation::new(crate::task::Control::none(), 2 << 20).unwrap();
        let g_before = roomy.lookup_iri(&snap, "urn:g:0000").unwrap().unwrap();
        let g_target = roomy.lookup_iri(&snap, "urn:z-target").unwrap().unwrap();
        let p_id = roomy.lookup_iri(&snap, "urn:p").unwrap().unwrap();
        let o_id = roomy.lookup_term(&snap, &o).unwrap().unwrap();
        let s0_id = roomy.lookup_iri(&snap, "urn:s:0000").unwrap().unwrap();
        assert!(
            g_before.0 < g_target.0,
            "compacted vocab must order urn:g:0000 ({}) before urn:z-target ({})",
            g_before.0,
            g_target.0
        );
        let pos_idx = snap.perm(Perm::Pos);
        let spo_idx = snap.perm(Perm::Spo);
        assert!(
            pos_idx.segmented(),
            "expected segmented compacted Perm::Pos"
        );
        let (r0_start, r0_end) = pos_idx.segment_rows(0, 0);
        let (r1_start, r1_end) = pos_idx.segment_rows(0, 1);
        let seg0_workspace = ((r0_end - r0_start) as u64) * 52 + 1024;
        let seg1_workspace = ((r1_end - r1_start) as u64) * 52 + 1024;
        assert!(
            seg1_workspace < seg0_workspace,
            "expected segment 1 workspace ({seg1_workspace}) < segment 0 workspace ({seg0_workspace})"
        );
        assert!(!snap.cache.has_segment(pos_idx, 0, 0, ALL_COLS));
        assert!(!snap.cache.has_segment(pos_idx, 0, 1, ALL_COLS));
        assert!(!snap.cache.has_segment(spo_idx, 0, 0, ALL_COLS));

        let mut reads = Reads::new(&roomy, &snap);
        let mut held = roomy.reserve(0).unwrap();
        let (subs, more) = reads
            .subjects_up_to_held("urn:p", &o, "urn:z-target", 1000, &mut held)
            .unwrap();
        assert_eq!(subs.len(), 1);
        assert!(!more);
        assert_eq!(subs[0].as_str(), "urn:s:0000");

        assert!(
            snap.cache.has_segment(pos_idx, 0, 0, ALL_COLS),
            "initial POS seek must decode Perm::Pos segment 0"
        );
        assert!(
            snap.cache.has_segment(pos_idx, 0, 1, ALL_COLS),
            "found_g < g_id fallback must decode Perm::Pos segment 1 via contains_pos_id"
        );
        assert!(
            !snap.cache.has_segment(spo_idx, 0, 0, ALL_COLS)
                && !snap.cache.has_segment(spo_idx, 0, 1, ALL_COLS),
            "found_g < g_id fallback must never touch Perm::Spo (Snapshot::contains)"
        );

        let (matched, more_m) = reads
            .subjects_matching_up_to_held(
                &[("urn:p", &o), ("urn:p2", &o2)],
                "urn:z-target",
                1000,
                &mut held,
            )
            .unwrap();
        assert_eq!(matched, subs);
        assert!(!more_m);
        assert!(
            !snap.cache.has_segment(spo_idx, 0, 0, ALL_COLS)
                && !snap.cache.has_segment(spo_idx, 0, 1, ALL_COLS),
            "multi-clause intersection must never touch Perm::Spo"
        );
        drop(held);
        (p_id, o_id, s0_id, g_target, seg0_workspace, seg1_workspace)
    };

    // 2. Cold reopen: verify the exact `contains_pos_id` fallback probe itself enforces
    // Preparation's memory budget and cancellation on segment 1 (strictly below segment
    // 0's workspace, proving it probes segment 1 directly without decoding segment 0 or
    // Perm::Spo).
    {
        let s_cold = Store::open(dir.path(), StoreOptions::default()).unwrap();
        let snap_cold = s_cold.snapshot();
        let pos_idx = snap_cold.perm(Perm::Pos);
        let spo_idx = snap_cold.perm(Perm::Spo);

        let tight = Preparation::new(crate::task::Control::none(), seg1_workspace - 1).unwrap();
        let reads_tight = Reads::new(&tight, &snap_cold);
        assert!(matches!(
            reads_tight.contains_pos_id(p_id, o_id, s0_id, g_target),
            Err(Error::BudgetExceeded(_))
        ));
        assert!(!snap_cold.cache.has_segment(pos_idx, 0, 1, ALL_COLS));
        assert!(!snap_cold.cache.has_segment(spo_idx, 0, 0, ALL_COLS));

        let cancelled_control = crate::task::Control::none();
        let cancelled = Preparation::new(cancelled_control.clone(), 2 << 20).unwrap();
        cancelled_control.cancel.cancel();
        let reads_cancelled = Reads::new(&cancelled, &snap_cold);
        assert!(matches!(
            reads_cancelled.contains_pos_id(p_id, o_id, s0_id, g_target),
            Err(Error::Cancelled)
        ));

        let seg1_only =
            Preparation::new(crate::task::Control::none(), seg1_workspace + 1024).unwrap();
        assert!(seg1_workspace + 1024 < seg0_workspace);
        let reads_seg1 = Reads::new(&seg1_only, &snap_cold);
        assert!(
            reads_seg1
                .contains_pos_id(p_id, o_id, s0_id, g_target)
                .unwrap()
        );
        assert!(snap_cold.cache.has_segment(pos_idx, 0, 1, ALL_COLS));
        assert!(!snap_cold.cache.has_segment(pos_idx, 0, 0, ALL_COLS));
        assert!(!snap_cold.cache.has_segment(spo_idx, 0, 0, ALL_COLS));
    }
}
