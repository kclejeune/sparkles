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
