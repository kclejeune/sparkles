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
    //    (smaller than the unreadable graph IRI itself, while comfortably covering the
    //    7-way parallel permutation headroom of `apply_controlled`).
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
