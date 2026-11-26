//! Support-aware visibility of C25 accepted projections (spec C25 §5 and §7, F18 §7.1,
//! roadmap item 582).

use oxrdf::{GraphName, Literal, NamedNode, Quad, Term, Triple};
use sparkles::Dataset;
use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
use sparkles::access::{GraphAccess, GraphRule, Graphs, Preparation};
use sparkles::error::{BudgetKind, Error};
use sparkles::evidence::{
    self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput,
    EvidenceLinkInput, EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, PassageInput,
    Polarity, Policy, ProposeOptions, RecordKey, RecordRef, RecordVersion, RenditionContent,
    RenditionInput, ReviewOptions, Settings, Sha256Digest, SourceContent, SourceRevisionInput,
    WithdrawalInput,
};
use sparkles::history::{At, HistoryOptions};
use sparkles::sparql::QueryOptions;
use sparkles::task::Control;
use sparkles_core::evidence::{GraphIdentity, StoredFactRef};
use std::sync::Arc;

const SOURCES_A: &str = "urn:ev:sources-a";
const SOURCES_B: &str = "urn:ev:sources-b";
const CLAIMS_A: &str = "urn:ev:claims-a";
const CLAIMS_B: &str = "urn:ev:claims-b";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const OUTPUT: &str = "urn:ev:accepted";
const PUBLIC: &str = "urn:public";
const SECRET: &str = "urn:secret";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn key(s: &str) -> RecordKey {
    RecordKey::new(s).unwrap()
}

fn sha(text: &str) -> Sha256Digest {
    Sha256Digest::of_bytes(text.as_bytes(), &Control::none()).unwrap()
}

fn all() -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

fn prep(limit: u64) -> Preparation {
    Preparation::new(Control::none(), limit).unwrap()
}

fn reading(graphs: &[&str]) -> Arc<GraphAccess> {
    Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(graphs.iter().copied(), &[])),
        Graphs::none(),
    ))
}

fn two_boundary_settings() -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES_A.into(), SOURCES_B.into()],
        proposals: vec![CLAIMS_A.into(), CLAIMS_B.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        projections: vec![OUTPUT.into()],
        policies: [(
            "main".to_string(),
            Policy {
                version: 1,
                admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
                required_scope: Vec::new(),
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![
                    SOURCES_A.into(),
                    SOURCES_B.into(),
                    CLAIMS_A.into(),
                    CLAIMS_B.into(),
                ],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: OUTPUT.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Settings::default()
    }
}

fn configured(activate_history: bool) -> Dataset {
    let ds = Dataset::memory();
    ds.store()
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(64),
            ..Default::default()
        })
        .unwrap();
    if activate_history {
        ds.store()
            .activate_memory_configuration_history(&prep(64 << 20))
            .unwrap();
    }
    evidence::configure(&ds, &two_boundary_settings(), &Control::none()).unwrap();
    ds
}

fn affected() -> Triple {
    Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve-1"))
}

fn affected_two() -> Triple {
    Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve-2"))
}

fn propose_in(
    ds: &Dataset,
    source_graph: &str,
    proposal_graph: &str,
    source_key: &str,
    occurrence: &str,
    proposition: Triple,
) -> (NamedNode, Sha256Digest) {
    let input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri(&format!("urn:source:{source_key}")),
            source_key: key(source_key),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha(source_key),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key(occurrence),
            revision: RecordRef::Local(key("rev")),
            proposition,
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:publisher"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        ..Default::default()
    };
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.source_graph = Some(iri(source_graph));
    o.proposal_graph = Some(iri(proposal_graph));
    let p = evidence::propose(
        None,
        ds,
        ds,
        &input,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let r = p.records.iter().find(|r| r.key == "claim").unwrap();
    (r.iri.clone(), r.digest)
}

fn decide(
    ds: &Dataset,
    claim: &(NamedNode, Sha256Digest),
    disposition: Disposition,
    supersedes: Option<&NamedNode>,
) -> NamedNode {
    let mut o = ReviewOptions::new(iri("urn:principal:reviewer"));
    o.if_head = Some(ds.store().head_commit().seq);
    let r = evidence::review(
        None,
        ds,
        ds,
        "main",
        1,
        &[DecisionInput {
            key: "d".into(),
            target: claim.0.clone(),
            target_digest: claim.1,
            disposition,
            supersedes: supersedes.cloned(),
        }],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    r.decisions[0].iri.clone()
}

fn withdraw(ds: &Dataset, claim: &(NamedNode, Sha256Digest)) {
    evidence::withdraw(
        ds,
        ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: claim.0.clone(),
            target_digest: claim.1,
        }],
        &ReviewOptions::new(iri("urn:principal:reviewer")),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
}

fn output_objects_legacy(ds: &Dataset, access: Option<Arc<GraphAccess>>) -> Vec<String> {
    let opts = QueryOptions {
        graphs: access,
        ..Default::default()
    };
    let mut rows: Vec<String> = ds
        .query_with(
            &format!(
                "SELECT ?o WHERE {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> ?o }} }}"
            ),
            &opts,
        )
        .unwrap()
        .rows()
        .unwrap()
        .into_iter()
        .filter_map(|r| r.values()[0].as_ref().map(ToString::to_string))
        .collect();
    rows.sort();
    rows
}

fn output_count_legacy(ds: &Dataset, access: Option<Arc<GraphAccess>>) -> i64 {
    let opts = QueryOptions {
        graphs: access,
        ..Default::default()
    };
    let rows = ds
        .query_with(
            &format!("SELECT (COUNT(*) AS ?n) WHERE {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"),
            &opts,
        )
        .unwrap()
        .rows()
        .unwrap();
    match rows.iter().next().unwrap().values()[0].as_ref() {
        Some(Term::Literal(l)) => l.value().parse().unwrap(),
        other => panic!("unexpected count: {other:?}"),
    }
}

fn output_ask_prepared(ds: &Dataset, access: Arc<GraphAccess>, object: &str) -> bool {
    let p = prep(64 << 20);
    let view = ds.store().prepare_read(access, &p).unwrap();
    let opts = QueryOptions::default();
    sparkles::sparql::query_source(
        view.read_source(),
        ds.store(),
        &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <{object}> }} }}"),
        &opts,
    )
    .unwrap()
    .boolean
}

#[test]
fn reader_with_output_and_one_live_support_sees_only_its_supported_triples() {
    let ds = configured(false);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected_two());
    decide(&ds, &ca, Disposition::Accepted, None);
    decide(&ds, &cb, Disposition::Accepted, None);

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_b = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_B, SOURCES_B]);
    let reader_output_only = reading(&[OUTPUT]);
    let reader_meta_only = reading(&[OUTPUT, DECISIONS, SUPPORTS]);
    let reader_no_meta = reading(&[OUTPUT, CLAIMS_A, SOURCES_A]);
    let reader_no_source = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A]);

    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a.clone())),
        vec!["<urn:cve-1>"]
    );
    assert_eq!(output_count_legacy(&ds, Some(reader_a.clone())), 1);
    assert!(output_ask_prepared(&ds, reader_a.clone(), "urn:cve-1"));
    assert!(!output_ask_prepared(&ds, reader_a, "urn:cve-2"));

    assert_eq!(
        output_objects_legacy(&ds, Some(reader_b.clone())),
        vec!["<urn:cve-2>"]
    );
    assert!(output_ask_prepared(&ds, reader_b.clone(), "urn:cve-2"));
    assert!(!output_ask_prepared(&ds, reader_b, "urn:cve-1"));

    for restricted in [
        reader_output_only,
        reader_meta_only,
        reader_no_meta,
        reader_no_source,
    ] {
        assert!(output_objects_legacy(&ds, Some(restricted.clone())).is_empty());
        assert_eq!(output_count_legacy(&ds, Some(restricted.clone())), 0);
        assert!(!output_ask_prepared(&ds, restricted.clone(), "urn:cve-1"));
        assert!(!output_ask_prepared(&ds, restricted, "urn:cve-2"));
    }

    // Unrestricted caller sees both projected triples.
    assert_eq!(
        output_objects_legacy(&ds, all()),
        vec!["<urn:cve-1>", "<urn:cve-2>"]
    );
    assert_eq!(output_count_legacy(&ds, None), 2);
}

#[test]
fn withdrawal_reacceptance_and_supersession_update_support_visibility() {
    let ds = configured(false);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);
    decide(&ds, &cb, Disposition::Accepted, None);

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_b = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_B, SOURCES_B]);
    let reader_meta = reading(&[OUTPUT, DECISIONS, SUPPORTS]);

    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a.clone())),
        vec!["<urn:cve-1>"]
    );
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_b.clone())),
        vec!["<urn:cve-1>"]
    );
    assert!(output_objects_legacy(&ds, Some(reader_meta.clone())).is_empty());

    // Withdraw Claim A: the triple remains in OUTPUT via Claim B, so Reader A loses
    // visibility while Reader B keeps it.
    withdraw(&ds, &ca);
    assert!(output_objects_legacy(&ds, Some(reader_a.clone())).is_empty());
    assert_eq!(output_count_legacy(&ds, Some(reader_a.clone())), 0);
    assert!(!output_ask_prepared(&ds, reader_a.clone(), "urn:cve-1"));
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_b.clone())),
        vec!["<urn:cve-1>"]
    );
    assert!(output_ask_prepared(&ds, reader_b.clone(), "urn:cve-1"));

    // Propose and accept a fresh Claim A2 in CLAIMS_A: Reader A sees the triple again.
    let ca2 = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a2", affected());
    let dec_a2 = decide(&ds, &ca2, Disposition::Accepted, None);
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a.clone())),
        vec!["<urn:cve-1>"]
    );
    assert!(output_ask_prepared(&ds, reader_a.clone(), "urn:cve-1"));

    // Supersede Decision A2 with Rejection: Reader A loses visibility again while Reader B keeps it.
    decide(&ds, &ca2, Disposition::Rejected, Some(&dec_a2));
    assert!(output_objects_legacy(&ds, Some(reader_a.clone())).is_empty());
    assert!(!output_ask_prepared(&ds, reader_a, "urn:cve-1"));
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_b.clone())),
        vec!["<urn:cve-1>"]
    );
}

#[test]
fn supporting_evidence_links_and_stored_facts_must_be_readable() {
    let ds = configured(false);
    // Insert a secret fact in SECRET that only full readers can see.
    {
        let mut t = ds.store().write();
        let q = Quad::new(
            iri("urn:host"),
            iri("urn:runs"),
            iri("urn:release"),
            GraphName::NamedNode(iri(SECRET)),
        );
        let mut labels = std::collections::HashMap::new();
        let ids = t.encode_quad(&q, &mut labels).unwrap();
        t.insert(ids).unwrap();
        t.commit().unwrap();
    }
    let fact_commit = ds.store().head_commit().seq;

    // Claim 1 in CLAIMS_A has an ev:Supports link to a passage in SOURCES_B (unreadable by Reader A).
    let text = "Release is affected by cve-3.";
    let pass_input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev-b"),
            source: iri("urn:source:secret-doc"),
            source_key: key("secret-doc"),
            upstream_version: None,
            content: SourceContent::MetadataOnly { digest: sha(text) },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("rend-b"),
            occurrence_key: key("rend-b"),
            revision: RecordRef::Local(key("rev-b")),
            media_type: "text/plain".into(),
            language: Some("en".into()),
            conversion: sparkles::evidence::MethodRef {
                method: iri("urn:method:txt"),
                version: key("1"),
            },
            content: RenditionContent::NormalizedText(text.into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("pass-b"),
            occurrence_key: key("pass-b"),
            revision: RecordRef::Local(key("rev-b")),
            rendition: RecordRef::Local(key("rend-b")),
            start: 0,
            end: 7,
            quote: Some("Release".into()),
            text_digest: sha("Release"),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("ref-pass"),
            occurrence_key: key("ref-pass"),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Local(key("rev-b")),
                rendition: RecordRef::Local(key("rend-b")),
                passage: RecordRef::Local(key("pass-b")),
            },
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim-pass"),
            occurrence_key: key("claim-pass"),
            revision: RecordRef::Local(key("rev-b")),
            proposition: Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve-3")),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:publisher"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link-pass"),
            occurrence_key: key("link-pass"),
            target: RecordRef::Local(key("claim-pass")),
            evidence: RecordRef::Local(key("ref-pass")),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("urn:importer"),
        }],
        ..Default::default()
    };
    let mut po = ProposeOptions::new(iri("urn:principal:importer"));
    po.source_graph = Some(iri(SOURCES_B));
    po.proposal_graph = Some(iri(CLAIMS_A));
    let pass_res = evidence::propose(
        None,
        &ds,
        &ds,
        &pass_input,
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let pass_claim = pass_res
        .records
        .iter()
        .find(|r| r.key == "claim-pass")
        .unwrap();
    decide(
        &ds,
        &(pass_claim.iri.clone(), pass_claim.digest),
        Disposition::Accepted,
        None,
    );

    let claim_with_secret_passage = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev-a"),
            source: iri("urn:source:pub-doc"),
            source_key: key("pub-doc"),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha("pub-doc"),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("ref-fact"),
            occurrence_key: key("ref-fact"),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: ds.dataset_id(),
                branch: ds.store().branch_id(),
                commit: fact_commit,
                graph: GraphIdentity::Named(iri(SECRET)),
                expected: Triple::new(iri("urn:host"), iri("urn:runs"), iri("urn:release")),
                anchor: None,
            }),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim-fact"),
            occurrence_key: key("claim-fact"),
            revision: RecordRef::Local(key("rev-a")),
            proposition: affected(),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("urn:publisher"),
            scope: Default::default(),
            confidence: Default::default(),
            validity: None,
        }],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link-fact"),
            occurrence_key: key("link-fact"),
            target: RecordRef::Local(key("claim-fact")),
            evidence: RecordRef::Local(key("ref-fact")),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("urn:importer"),
        }],
        ..Default::default()
    };
    let mut po_a = ProposeOptions::new(iri("urn:principal:importer"));
    po_a.source_graph = Some(iri(SOURCES_A));
    po_a.proposal_graph = Some(iri(CLAIMS_A));
    let res = evidence::propose(
        None,
        &ds,
        &ds,
        &claim_with_secret_passage,
        &po_a,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let rec = res.records.iter().find(|r| r.key == "claim-fact").unwrap();
    decide(
        &ds,
        &(rec.iri.clone(), rec.digest),
        Disposition::Accepted,
        None,
    );

    // Also add an unrelated clean claim in CLAIMS_A.
    let clean = propose_in(&ds, SOURCES_A, CLAIMS_A, "pub-clean", "c2", affected_two());
    decide(&ds, &clean, Disposition::Accepted, None);

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_a_with_secret = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SECRET]);
    let reader_a_with_sources_b =
        reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SOURCES_B]);

    // Reader A cannot read SECRET (the storedFact supporting `cve-1`) nor SOURCES_B
    // (the passage/revision supporting `cve-3`), so only `cve-2` is visible.
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a)),
        vec!["<urn:cve-2>"]
    );
    // Reader A + SECRET can read the storedFact for `cve-1` (plus `cve-2`), still not `cve-3`.
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a_with_secret)),
        vec!["<urn:cve-1>", "<urn:cve-2>"]
    );
    // Reader A + SOURCES_B can read the passage for `cve-3` (plus `cve-2`), still not `cve-1`.
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_a_with_sources_b)),
        vec!["<urn:cve-2>", "<urn:cve-3>"]
    );
}

#[test]
fn triple_protections_interact_with_support_visibility_and_preserve_ordinary_graphs() {
    let ds = configured(false);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);
    decide(&ds, &cb, Disposition::Accepted, None);

    // Also assert the same triple in an ordinary graph PUBLIC.
    {
        let mut t = ds.store().write();
        let q = Quad::new(
            iri("urn:release"),
            iri("urn:affectedBy"),
            iri("urn:cve-1"),
            GraphName::NamedNode(iri(PUBLIC)),
        );
        let mut labels = std::collections::HashMap::new();
        let ids = t.encode_quad(&q, &mut labels).unwrap();
        t.insert(ids).unwrap();
        t.commit().unwrap();
    }

    // Protection hides `ev:claimKind` in CLAIMS_A unless caller has read grant on the protection.
    let make_view = |cleared: bool| {
        Arc::new(GraphAccess::with_triples(
            Graphs::Only(GraphRule::new(
                [
                    OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, CLAIMS_B, SOURCES_A, SOURCES_B, PUBLIC,
                ],
                &[],
            )),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "hide-claim-a".into(),
                        predicates: Some(vec!["urn:x-sparkles:evidence:claimKind".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([CLAIMS_A], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: if cleared { Graphs::All } else { Graphs::none() },
                    write: Graphs::none(),
                }],
                caller: Caller {
                    user: Some("reader".into()),
                    roles: if cleared {
                        vec!["cleared".into()]
                    } else {
                        vec!["analyst".into()]
                    },
                    groups: Vec::new(),
                },
                limits: Limits::default(),
            },
        ))
    };

    let restricted = make_view(false);
    let cleared = make_view(true);

    // While both A and B are live, restricted sees `cve-1` in OUTPUT via Claim B.
    assert_eq!(
        output_objects_legacy(&ds, Some(restricted.clone())),
        vec!["<urn:cve-1>"]
    );

    // Withdraw Claim B: now only Claim A (hidden by the triple protection) supports OUTPUT.
    withdraw(&ds, &cb);
    assert!(output_objects_legacy(&ds, Some(restricted.clone())).is_empty());
    assert!(!output_ask_prepared(&ds, restricted.clone(), "urn:cve-1"));
    // Cleared caller still sees it via Claim A.
    assert_eq!(
        output_objects_legacy(&ds, Some(cleared.clone())),
        vec!["<urn:cve-1>"]
    );
    assert!(output_ask_prepared(&ds, cleared, "urn:cve-1"));

    // The same triple in ordinary graph PUBLIC stays visible to the restricted caller.
    let pub_opts = QueryOptions {
        graphs: Some(restricted),
        ..Default::default()
    };
    let pub_ask = ds
        .query_with(
            &format!("ASK {{ GRAPH <{PUBLIC}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
            &pub_opts,
        )
        .unwrap()
        .boolean;
    assert!(pub_ask);
}

#[test]
fn historical_reads_branches_and_detached_projections_follow_support_liveness() {
    let ds = configured(true);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);
    let commit_a_live = ds.store().head_commit().seq;

    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected());
    decide(&ds, &cb, Disposition::Accepted, None);
    let commit_both_live = ds.store().head_commit().seq;

    // Fork a branch before withdrawal so `feature` retains Claim A live.
    ds.store()
        .create_branch("feature", &sparkles::branch::BranchOptions::default())
        .unwrap();

    // Withdraw Claim A on `main`.
    withdraw(&ds, &ca);

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_b = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_B, SOURCES_B]);
    let reader_meta = reading(&[OUTPUT, DECISIONS, SUPPORTS]);

    // At Head on main: Reader A does NOT see `affected`; Reader B DOES.
    assert!(output_objects_legacy(&ds, Some(reader_a.clone())).is_empty());
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_b.clone())),
        vec!["<urn:cve-1>"]
    );

    // Historical read at `commit_a_live` via `snapshot_at`: Reader A SEES it; Reader Meta does NOT.
    let (hist_snap, _) = ds
        .store()
        .snapshot_at(&At::Commit(commit_a_live), &HistoryOptions::default())
        .unwrap();
    let hist_a = sparkles::sparql::query(
        hist_snap.clone(),
        &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
        &QueryOptions {
            graphs: Some(reader_a.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(hist_a.boolean);
    let hist_meta = sparkles::sparql::query(
        hist_snap,
        &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
        &QueryOptions {
            graphs: Some(reader_meta.clone()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!hist_meta.boolean);

    // Historical read via `BoundEvidenceView` at `commit_both_live`: Reader A SEES it.
    let ev_view = ds
        .store()
        .evidence_view(&At::Commit(commit_both_live), &prep(64 << 20))
        .unwrap();
    let prepared_ev = ev_view
        .prepare_view(reader_a.clone(), &prep(64 << 20))
        .unwrap();
    assert!(
        sparkles::sparql::query_source(
            prepared_ev.read_source(),
            ds.store(),
            &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
            &QueryOptions::default(),
        )
        .unwrap()
        .boolean
    );

    // On branch `feature`, Claim A was never withdrawn, so Reader A sees it.
    let branch_store = ds.store().branch("feature").unwrap();
    let branch_view = branch_store
        .prepare_read(reader_a.clone(), &prep(64 << 20))
        .unwrap();
    assert!(
        sparkles::sparql::query_source(
            branch_view.read_source(),
            &branch_store,
            &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
            &QueryOptions::default(),
        )
        .unwrap()
        .boolean
    );

    // Detach and re-protect OUTPUT on main: Head fails closed for restricted readers
    // (nonces no longer match the Projection record), while historical `commit_a_live`
    // retains its pinned historical configuration so Reader A still sees `affected` there.
    ds.store()
        .detach_managed(sparkles::managed::OwnerKind::Evidence, OUTPUT)
        .unwrap();
    evidence::configure(&ds, &two_boundary_settings(), &Control::none()).unwrap();
    assert!(output_objects_legacy(&ds, Some(reader_b.clone())).is_empty());
    assert!(!output_ask_prepared(&ds, reader_b, "urn:cve-1"));

    let (hist_after_detach, _) = ds
        .store()
        .snapshot_at(&At::Commit(commit_a_live), &HistoryOptions::default())
        .unwrap();
    assert!(
        sparkles::sparql::query(
            hist_after_detach,
            &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
            &QueryOptions {
                graphs: Some(reader_a),
                ..Default::default()
            },
        )
        .unwrap()
        .boolean
    );
}

#[test]
fn bounded_memory_enforcement_refuses_tight_preparation_without_leaking() {
    let ds = configured(false);
    // Propose and accept several claims so projection visibility needs preparation memory.
    for i in 0..8 {
        let triple = Triple::new(
            iri(&format!("urn:item:{i}")),
            iri("urn:p"),
            Literal::new_simple_literal(format!("val-{i}")),
        );
        let c = propose_in(
            &ds,
            SOURCES_A,
            CLAIMS_A,
            &format!("s-{i}"),
            &format!("c-{i}"),
            triple,
        );
        decide(&ds, &c, Disposition::Accepted, None);
    }
    let reader_meta = reading(&[OUTPUT, DECISIONS, SUPPORTS]);
    // A tiny preparation limit (below what mask construction needs) must fail with
    // BudgetExceeded(Memory) rather than silently returning an unmasked snapshot.
    let tiny = prep(1024);
    let err = match reader_meta.masked_controlled(&ds.store().snapshot(), &tiny) {
        Err(e) => e,
        Ok(_) => panic!("expected memory budget error"),
    };
    assert!(
        matches!(err, Error::BudgetExceeded(ref b) if b.kind == BudgetKind::Memory),
        "expected memory budget error, got {err:?}"
    );
}
