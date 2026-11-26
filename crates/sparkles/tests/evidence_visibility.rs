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

#[test]
fn sparql_update_insert_where_cannot_exfiltrate_hidden_projections() {
    let ds = configured(false);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected_two());
    decide(&ds, &ca, Disposition::Accepted, None);
    decide(&ds, &cb, Disposition::Accepted, None);

    const SCRATCH: &str = "urn:scratch";
    // Restricted caller can read OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SCRATCH
    // and write SCRATCH — no TripleRules at all.
    let writer = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SCRATCH],
            &[],
        )),
        Graphs::Only(GraphRule::new([SCRATCH], &[])),
    ));
    let update = format!(
        "INSERT {{ GRAPH <{SCRATCH}> {{ ?s ?p ?o }} }} WHERE {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"
    );
    let opts = QueryOptions {
        graphs: Some(writer),
        ..Default::default()
    };
    ds.update_with(&update, &opts).unwrap();

    // Only `cve-1` (supported by CLAIMS_A/SOURCES_A) may have been copied; `cve-2` must not leak.
    let copied = sparkles::sparql::query(
        ds.store().snapshot(),
        &format!("SELECT ?o WHERE {{ GRAPH <{SCRATCH}> {{ <urn:release> <urn:affectedBy> ?o }} }} ORDER BY ?o"),
        &QueryOptions::default(),
    )
    .unwrap();
    let vals: Vec<String> = copied
        .rows()
        .unwrap()
        .iter()
        .map(|r| r.values()[0].as_ref().unwrap().to_string())
        .collect();
    assert_eq!(vals, vec!["<urn:cve-1>"]);
}

#[test]
fn diff_changes_history_and_incremental_views_enforce_support_visibility() {
    let ds = configured(false);
    let c0 = ds.store().head_commit().seq;
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let cb = propose_in(&ds, SOURCES_B, CLAIMS_B, "src-b", "b1", affected_two());
    decide(&ds, &ca, Disposition::Accepted, None);
    decide(&ds, &cb, Disposition::Accepted, None);
    let c_after = ds.store().head_commit().seq;

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_out = reading(&[OUTPUT]);

    // 1. Store::diff between c0 and c_after must only report triples supported for the caller.
    let diff_a = ds
        .store()
        .diff(
            &At::Commit(c0),
            &At::Commit(c_after),
            &sparkles::store::DiffOptions {
                graphs: Some(reader_a.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    let out_added_a: Vec<String> = diff_a
        .iter()
        .filter(|(op, q)| {
            *op == sparkles::store::DiffOp::Add && q.graph_name == GraphName::NamedNode(iri(OUTPUT))
        })
        .map(|(_, q)| q.object.to_string())
        .collect();
    assert_eq!(out_added_a, vec!["<urn:cve-1>"]);

    let diff_out = ds
        .store()
        .diff(
            &At::Commit(c0),
            &At::Commit(c_after),
            &sparkles::store::DiffOptions {
                graphs: Some(reader_out.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(
        diff_out.is_empty(),
        "reader without support grants must see no OUTPUT diff, got {}",
        diff_out.len()
    );

    // 2. Store::changes refuses restricted callers subject to support-aware projection visibility.
    let ch_err = ds
        .store()
        .changes(
            c0,
            &sparkles::store::ChangesOptions {
                graphs: Some(reader_a.clone()),
                ..Default::default()
            },
        )
        .err()
        .expect("expected NotPermitted for changes()");
    assert!(
        matches!(ch_err, Error::NotPermitted(_)),
        "expected NotPermitted for changes(), got {ch_err:?}"
    );
    // Unrestricted caller still succeeds.
    assert!(
        ds.store()
            .changes(c0, &sparkles::store::ChangesOptions::default())
            .is_ok()
    );

    // 3. Store::history_changes and SPARQL HISTORY refuse restricted callers.
    let hq_err = ds
        .store()
        .history_changes(&sparkles::store::HistoryQuery {
            access: Some(reader_a.clone()),
            ..Default::default()
        })
        .expect_err("expected NotPermitted for history_changes()");
    assert!(
        matches!(hq_err, Error::NotPermitted(_)),
        "expected NotPermitted for history_changes(), got {hq_err:?}"
    );

    // 4. Incremental Circuit build and advance enforce support-aware visibility even when
    //    compiled without whole-query mode.
    let query_str =
        format!("SELECT ?o WHERE {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> ?o }} }}");
    let parsed = sparkles::sparql::parse_query(&query_str, None, &[]).unwrap();
    let compiled = sparkles::incr::compile(&parsed, None).unwrap();
    let mut circuit = sparkles::incr::Circuit::new(compiled, Some(reader_a.clone()));
    circuit
        .build(ds.store().snapshot(), &Control::none())
        .unwrap();
    let mut rows: Vec<String> = circuit
        .rows()
        .into_iter()
        .map(|(r, _)| r[0].as_ref().unwrap().to_string())
        .collect();
    rows.sort();
    assert_eq!(rows, vec!["<urn:cve-1>"]);

    // Withdraw Claim A: advancing the incremental circuit must retract `cve-1` even though
    // `cve-1` was never removed from OUTPUT or only CLAIMS_A changed.
    let _sub = ds.store().subscribe_deltas();
    withdraw(&ds, &ca);
    let d = sparkles::incr::DeltaSet::read(ds.store(), circuit.applied, &Control::none());
    circuit.advance(&d, &Control::none()).unwrap();
    assert!(
        circuit.rows().is_empty(),
        "withdrawing Claim A must hide cve-1 from incremental view"
    );
}

#[test]
fn stored_fact_enforces_branch_retention_blank_anchor_and_recursive_projection_visibility() {
    let ds = configured(false);
    // Configure a second policy `secondary` whose output graph is OUTPUT_2, backed by CLAIMS_B/SOURCES_B.
    const DECISIONS_2: &str = "urn:ev:decisions-2";
    const SUPPORTS_2: &str = "urn:ev:supports-2";
    const OUTPUT_2: &str = "urn:ev:accepted-2";
    let mut settings = two_boundary_settings();
    settings.decisions.push(DECISIONS_2.into());
    settings.supports.push(SUPPORTS_2.into());
    settings.projections.push(OUTPUT_2.into());
    settings.policies.insert(
        "secondary".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
            required_scope: Vec::new(),
            conflicts: ConflictMode::AdmitLabelled,
            boundary: vec![SOURCES_B.into(), CLAIMS_B.into()],
            decisions: DECISIONS_2.into(),
            supports: SUPPORTS_2.into(),
            output: OUTPUT_2.into(),
        },
    );
    evidence::configure(&ds, &settings, &Control::none()).unwrap();

    // 1. Accept a secret claim under `secondary` so `<urn:secret-fact> <urn:p> <urn:v>` is in OUTPUT_2.
    let secret_triple = Triple::new(iri("urn:secret-fact"), iri("urn:p"), iri("urn:v"));
    let sec_claim = propose_in(
        &ds,
        SOURCES_B,
        CLAIMS_B,
        "sb-1",
        "cb-1",
        secret_triple.clone(),
    );
    let mut ro2 = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro2.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "secondary",
        1,
        &[DecisionInput {
            key: "d2".into(),
            target: sec_claim.0,
            target_digest: sec_claim.1,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let proj2_commit = ds.store().head_commit().seq;

    // Propose a claim in CLAIMS_A citing that OUTPUT_2 projected triple as a StoredFact,
    // and accept it into OUTPUT under `main`.
    let propose_with_fact = |occurrence: &str, prop: Triple, fact: StoredFactRef| {
        let input = EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri(&format!("urn:source:{occurrence}")),
                source_key: key(occurrence),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha(occurrence),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("ref"),
                occurrence_key: key(occurrence),
                target: EvidenceRefTarget::StoredFact(fact),
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("claim"),
                occurrence_key: key(occurrence),
                revision: RecordRef::Local(key("rev")),
                proposition: prop,
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:publisher"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            links: vec![EvidenceLinkInput {
                version: RecordVersion::V1,
                key: key("link"),
                occurrence_key: key(occurrence),
                target: RecordRef::Local(key("claim")),
                evidence: RecordRef::Local(key("ref")),
                relation: EvidenceRelation::Supports,
                attributed_author: iri("urn:importer"),
            }],
            ..Default::default()
        };
        let mut po = ProposeOptions::new(iri("urn:principal:importer"));
        po.source_graph = Some(iri(SOURCES_A));
        po.proposal_graph = Some(iri(CLAIMS_A));
        let res = evidence::propose(
            None,
            &ds,
            &ds,
            &input,
            &po,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let rec = res.records.iter().find(|r| r.key == "claim").unwrap();
        decide(
            &ds,
            &(rec.iri.clone(), rec.digest),
            Disposition::Accepted,
            None,
        );
    };

    propose_with_fact(
        "from-proj2",
        affected(),
        StoredFactRef {
            dataset: ds.dataset_id(),
            branch: ds.store().branch_id(),
            commit: proj2_commit,
            graph: GraphIdentity::Named(iri(OUTPUT_2)),
            expected: secret_triple,
            anchor: None,
        },
    );

    // Reader with grant on OUTPUT_2 (and DECISIONS_2/SUPPORTS_2) but NOT CLAIMS_B/SOURCES_B
    // cannot see the projected triple in OUTPUT_2, so must NOT see `cve-1` in OUTPUT either!
    let reader_no_b = reading(&[
        OUTPUT,
        DECISIONS,
        SUPPORTS,
        CLAIMS_A,
        SOURCES_A,
        OUTPUT_2,
        DECISIONS_2,
        SUPPORTS_2,
    ]);
    assert!(
        output_objects_legacy(&ds, Some(reader_no_b)).is_empty(),
        "hidden projected source in OUTPUT_2 must not authorize OUTPUT"
    );
    // Reader who ALSO has CLAIMS_B and SOURCES_B CAN prove OUTPUT_2 and sees `cve-1`.
    let reader_with_b = reading(&[
        OUTPUT,
        DECISIONS,
        SUPPORTS,
        CLAIMS_A,
        SOURCES_A,
        OUTPUT_2,
        DECISIONS_2,
        SUPPORTS_2,
        CLAIMS_B,
        SOURCES_B,
    ]);
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_with_b)),
        vec!["<urn:cve-1>"]
    );

    // 2. Cross-branch StoredFact reference: write a fact on branch `other`, cite it on `main`.
    ds.store()
        .create_branch("other", &Default::default())
        .unwrap();
    let other_branch = ds.store().branch("other").unwrap();
    let other_triple = Triple::new(iri("urn:other-host"), iri("urn:runs"), iri("urn:other-rel"));
    {
        let mut t = other_branch.write();
        let mut labels = std::collections::HashMap::new();
        let ids = t
            .encode_quad(
                &Quad::new(
                    other_triple.subject.clone(),
                    other_triple.predicate.clone(),
                    other_triple.object.clone(),
                    GraphName::NamedNode(iri(SECRET)),
                ),
                &mut labels,
            )
            .unwrap();
        t.insert(ids).unwrap();
        t.commit().unwrap();
    }
    let other_commit = other_branch.head_commit().seq;
    propose_with_fact(
        "cross-branch",
        affected_two(),
        StoredFactRef {
            dataset: ds.dataset_id(),
            branch: other_branch.branch_id(),
            commit: other_commit,
            graph: GraphIdentity::Named(iri(SECRET)),
            expected: other_triple,
            anchor: None,
        },
    );
    // Even with SECRET granted on `main`, a cross-branch StoredFact cannot be proven from
    // `main`'s single-branch GraphAccess and must stay hidden for restricted readers.
    let reader_secret = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SECRET]);
    assert!(
        output_objects_legacy(&ds, Some(reader_secret)).is_empty(),
        "cross-branch storedFact must fail closed"
    );

    // 3. Blank-node StoredFact with reifier under predicate protection on the asserted triple.
    let bnode_reifier = iri("urn:reifier:b1");
    let bnode_expected = Triple::new(
        oxrdf::BlankNode::new_unchecked("b0"),
        iri("urn:secretPred"),
        iri("urn:val"),
    );
    {
        let mut t = ds.store().write();
        let mut labels = std::collections::HashMap::new();
        let g = GraphName::NamedNode(iri(SECRET));
        let q1 = Quad::new(
            bnode_expected.subject.clone(),
            bnode_expected.predicate.clone(),
            bnode_expected.object.clone(),
            g.clone(),
        );
        let q2 = Quad::new(
            bnode_reifier.clone(),
            iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies"),
            Term::Triple(Box::new(bnode_expected.clone())),
            g,
        );
        let i1 = t.encode_quad(&q1, &mut labels).unwrap();
        let i2 = t.encode_quad(&q2, &mut labels).unwrap();
        t.insert(i1).unwrap();
        t.insert(i2).unwrap();
        t.commit().unwrap();
    }
    let bnode_commit = ds.store().head_commit().seq;
    let cve3 = Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve-3"));
    propose_with_fact(
        "bnode-reifier",
        cve3,
        StoredFactRef {
            dataset: ds.dataset_id(),
            branch: ds.store().branch_id(),
            commit: bnode_commit,
            graph: GraphIdentity::Named(iri(SECRET)),
            expected: bnode_expected,
            anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(bnode_reifier)),
        },
    );
    let make_bnode_reader = |lifted: bool| {
        Arc::new(GraphAccess::with_triples(
            Graphs::Only(GraphRule::new(
                [OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SECRET],
                &[],
            )),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "hide-secret-pred".into(),
                        predicates: Some(vec!["urn:secretPred".into()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([SECRET], &[]))),
                        pattern: None,
                        prefixes: Default::default(),
                        hide_inferences: false,
                    }),
                    read: if lifted { Graphs::All } else { Graphs::none() },
                    write: Graphs::none(),
                }],
                caller: Caller::default(),
                limits: Limits::default(),
            },
        ))
    };
    assert!(
        !output_objects_legacy(&ds, Some(make_bnode_reader(false)))
            .contains(&"<urn:cve-3>".to_string()),
        "blank-node storedFact whose asserted triple predicate is protected must stay hidden"
    );
    assert!(
        output_objects_legacy(&ds, Some(make_bnode_reader(true)))
            .contains(&"<urn:cve-3>".to_string()),
        "lifted predicate protection must allow blank-node storedFact"
    );
}

#[test]
fn limited_history_apis_and_inherited_named_snapshot_preserve_pinned_config() {
    let ds = configured(true);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);
    let commit_active = ds.store().head_commit().seq;

    // Create a branch `feature` at `commit_active`, and create a bound named snapshot on `feature`
    // pinning the active policy configuration.
    ds.store()
        .create_branch("feature", &Default::default())
        .unwrap();
    let feature = ds.store().branch("feature").unwrap();
    let ev_view = ds
        .store()
        .evidence_view_with_branch(&feature, &At::Commit(commit_active), &prep(64 << 20))
        .unwrap();
    feature
        .create_bound_snapshot("pin-active", &ev_view, &Default::default())
        .unwrap();

    // Remove the review policy from live settings (detaching OUTPUT first, keeping its RDF).
    ds.store()
        .detach_managed(sparkles::managed::OwnerKind::Evidence, OUTPUT)
        .unwrap();
    let mut no_policy = two_boundary_settings();
    no_policy.policies.clear();
    no_policy.projections.clear();
    evidence::configure(&ds, &no_policy, &Control::none()).unwrap();

    let reader_meta = reading(&[OUTPUT, DECISIONS, SUPPORTS]);

    let ask_in = |snap: Arc<sparkles::store::Snapshot>, access: Arc<GraphAccess>| {
        sparkles::sparql::query(
            snap,
            &format!("ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve-1> }} }}"),
            &QueryOptions {
                graphs: Some(access),
                ..Default::default()
            },
        )
        .unwrap()
        .boolean
    };

    // 1. snapshot_at_limited pins historical config -> reader_meta cannot see `cve-1`.
    let (lim_snap, _) = ds
        .store()
        .snapshot_at_limited(
            &At::Commit(commit_active),
            &HistoryOptions::default(),
            64 << 20,
        )
        .unwrap();
    assert!(!ask_in(lim_snap, reader_meta.clone()));

    // 2. snapshot_at_with_branch_limited pins historical config.
    let (lim_br_snap, _, _) = ds
        .store()
        .snapshot_at_with_branch_limited(
            &feature,
            &At::Commit(commit_active),
            &HistoryOptions::default(),
            64 << 20,
        )
        .unwrap();
    assert!(!ask_in(lim_br_snap, reader_meta.clone()));

    // 3. snapshot_of_branch_limited pins historical config.
    let (lim_of_snap, _, _) = ds
        .store()
        .snapshot_of_branch_limited(
            feature.branch_id(),
            &At::Commit(commit_active),
            &HistoryOptions::default(),
            64 << 20,
        )
        .unwrap();
    assert!(!ask_in(lim_of_snap, reader_meta.clone()));

    // 4. snapshot_at_with_branch with At::Snapshot("pin-active") across an inherited commit hop
    //    preserves the named pin's configuration.
    let (named_br_snap, _, _) = ds
        .store()
        .snapshot_at_with_branch(
            &feature,
            &At::Snapshot("pin-active".into()),
            &HistoryOptions::default(),
        )
        .unwrap();
    assert!(!ask_in(named_br_snap, reader_meta));
}

#[test]
fn triple_protections_on_projection_receipt_and_passage_selector_withhold_support() {
    let ds = configured(false);
    // Propose a claim backed by a passage (which writes `<passage>#selector` with `oa:start`).
    let pass_input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev-p"),
            source: iri("urn:source:pass-doc"),
            source_key: key("pass-doc"),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha("pass-doc"),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("rend-p"),
            occurrence_key: key("rend-p"),
            revision: RecordRef::Local(key("rev-p")),
            media_type: "text/plain".into(),
            language: Some("en".into()),
            conversion: sparkles::evidence::MethodRef {
                method: iri("urn:method:txt"),
                version: key("1"),
            },
            content: RenditionContent::NormalizedText("Release affected by cve-1.".into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("pass-p"),
            occurrence_key: key("pass-p"),
            revision: RecordRef::Local(key("rev-p")),
            rendition: RecordRef::Local(key("rend-p")),
            start: 0,
            end: 25,
            text_digest: sha("Release affected by cve-1"),
            quote: Some("Release affected by cve-1".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("ref-p"),
            occurrence_key: key("ref-p"),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Local(key("rev-p")),
                rendition: RecordRef::Local(key("rend-p")),
                passage: RecordRef::Local(key("pass-p")),
            },
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim-p"),
            occurrence_key: key("claim-p"),
            revision: RecordRef::Local(key("rev-p")),
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
            key: key("link-p"),
            occurrence_key: key("link-p"),
            target: RecordRef::Local(key("claim-p")),
            evidence: RecordRef::Local(key("ref-p")),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("urn:importer"),
        }],
        ..Default::default()
    };
    let mut po = ProposeOptions::new(iri("urn:principal:importer"));
    po.source_graph = Some(iri(SOURCES_A));
    po.proposal_graph = Some(iri(CLAIMS_A));
    let res = evidence::propose(
        None,
        &ds,
        &ds,
        &pass_input,
        &po,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let claim_rec = res.records.iter().find(|r| r.key == "claim-p").unwrap();
    decide(
        &ds,
        &(claim_rec.iri.clone(), claim_rec.digest),
        Disposition::Accepted,
        None,
    );

    let reader_with_prot = |pred: &str, graph: &str| {
        Arc::new(GraphAccess::with_triples(
            Graphs::Only(GraphRule::new(
                [OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A],
                &[],
            )),
            Graphs::none(),
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "prot".into(),
                        predicates: Some(vec![pred.into()]),
                        classes: None,
                        subclasses: false,
                        graphs: Some(Graphs::Only(GraphRule::new([graph], &[]))),
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
        ))
    };

    // Baseline: full reader_a sees `cve-1`.
    let base_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    assert_eq!(
        output_objects_legacy(&ds, Some(base_a.clone())),
        vec!["<urn:cve-1>"]
    );

    // 1. Masking `oa:start` on `<passage>#selector` in SOURCES_A hides `cve-1`.
    assert!(
        output_objects_legacy(
            &ds,
            Some(reader_with_prot("http://www.w3.org/ns/oa#start", SOURCES_A))
        )
        .is_empty(),
        "masked passage #selector must withhold support"
    );

    // 2. Masking `ev:outputProtection` on `ev:Projection` in SUPPORTS hides `cve-1`.
    assert!(
        output_objects_legacy(
            &ds,
            Some(reader_with_prot(
                "urn:x-sparkles:evidence:outputProtection",
                SUPPORTS
            ))
        )
        .is_empty(),
        "masked ev:Projection record must withhold support"
    );

    // 3. Masking `ev:Operation` receipt (`ev:payloadDigest`) in CLAIMS_A hides `cve-1`.
    assert!(
        output_objects_legacy(
            &ds,
            Some(reader_with_prot(
                "urn:x-sparkles:evidence:payloadDigest",
                CLAIMS_A
            ))
        )
        .is_empty(),
        "masked ev:Operation receipt must withhold support"
    );

    // 4. Hidden withdrawal still ends liveness even when `ev:withdraws` is masked from the reader.
    withdraw(&ds, &(claim_rec.iri.clone(), claim_rec.digest));
    assert!(
        output_objects_legacy(
            &ds,
            Some(reader_with_prot(
                "urn:x-sparkles:evidence:withdraws",
                CLAIMS_A
            ))
        )
        .is_empty(),
        "masked withdrawal must still end liveness"
    );
}

#[test]
fn dual_role_boundary_graph_keeps_evidence_link_check_after_losing_proposal_role() {
    let ds = configured(false);
    // Give SOURCES_B BOTH `source_records` and `proposals` roles, so an EvidenceLink can be
    // written into SOURCES_B while the claim lives in CLAIMS_A.
    let mut dual = two_boundary_settings();
    dual.proposals.push(SOURCES_B.into());
    evidence::configure(&ds, &dual, &Control::none()).unwrap();

    // Propose Claim A in CLAIMS_A, and a source revision + rendition + passage + link in SOURCES_B.
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    let link_input = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev-b"),
            source: iri("urn:source:secret-b"),
            source_key: key("secret-b"),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha("secret-b"),
            },
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
            content: RenditionContent::NormalizedText("secret-b evidence".into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("pass-b"),
            occurrence_key: key("pass-b"),
            revision: RecordRef::Local(key("rev-b")),
            rendition: RecordRef::Local(key("rend-b")),
            start: 0,
            end: 8,
            text_digest: sha("secret-b"),
            quote: Some("secret-b".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("ref-b"),
            occurrence_key: key("ref-b"),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Local(key("rev-b")),
                rendition: RecordRef::Local(key("rend-b")),
                passage: RecordRef::Local(key("pass-b")),
            },
        }],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link-b"),
            occurrence_key: key("link-b"),
            target: RecordRef::Existing(ca.0.clone()),
            evidence: RecordRef::Local(key("ref-b")),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("urn:importer"),
        }],
        ..Default::default()
    };
    let mut po_b = ProposeOptions::new(iri("urn:principal:importer"));
    po_b.source_graph = Some(iri(SOURCES_B));
    po_b.proposal_graph = Some(iri(SOURCES_B));
    evidence::propose(
        None,
        &ds,
        &ds,
        &link_input,
        &po_b,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    decide(&ds, &ca, Disposition::Accepted, None);

    // Now remove SOURCES_B from `settings.proposals` while keeping it in `source_records`
    // and `policy.boundary` (its protection nonce is retained!).
    evidence::configure(&ds, &two_boundary_settings(), &Control::none()).unwrap();

    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let reader_ab = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A, SOURCES_B]);
    assert!(
        output_objects_legacy(&ds, Some(reader_a)).is_empty(),
        "authenticated link in dual-role boundary graph SOURCES_B must still hide cve-1 from reader_a"
    );
    assert_eq!(
        output_objects_legacy(&ds, Some(reader_ab)),
        vec!["<urn:cve-1>"]
    );
}

#[test]
fn idempotent_masked_controlled_when_evidence_hides_entire_output_graph() {
    let ds = configured(false);
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);

    let reader_meta = reading(&[OUTPUT, DECISIONS, SUPPORTS]);
    let snap = ds.store().snapshot();
    // First mask hides every quad in OUTPUT.
    let v1 = reader_meta
        .masked_controlled(&snap, &prep(64 << 20))
        .unwrap();
    assert!(v1.has_mask());
    // Re-applying the exact same policy to the already-masked snapshot must succeed idempotently.
    let masked_snap = reader_meta.masked(&snap).unwrap();
    assert!(masked_snap.has_mask());
    let v2 = reader_meta
        .masked_controlled(&masked_snap, &prep(64 << 20))
        .unwrap();
    assert!(v2.has_mask());

    // Applying a DIFFERENT restricted policy to `masked_snap` must be rejected.
    let reader_a = reading(&[OUTPUT, DECISIONS, SUPPORTS, CLAIMS_A, SOURCES_A]);
    let err = reader_a
        .masked_controlled(&masked_snap, &prep(64 << 20))
        .err()
        .expect("different policy must not reuse another policy's mask");
    assert!(matches!(err, Error::Unsupported(_)), "got {err:?}");
}

#[test]
fn unavailable_managed_state_fails_closed_for_restricted_readers() {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    evidence::configure(&ds, &two_boundary_settings(), &Control::none()).unwrap();
    let ca = propose_in(&ds, SOURCES_A, CLAIMS_A, "src-a", "a1", affected());
    decide(&ds, &ca, Disposition::Accepted, None);
    drop(ds);

    // Corrupt `evidence.json` on disk so reopening puts `ManagedState::Unavailable`.
    std::fs::write(dir.path().join("evidence.json"), b"{invalid json").unwrap();
    let reopened = Dataset::open(dir.path()).unwrap();
    let reader_out = reading(&[OUTPUT]);
    let snap = reopened.store().snapshot();

    // Legacy and controlled masked views must fail closed for restricted readers.
    assert!(
        reader_out.masked(&snap).is_err(),
        "unavailable managed registry must fail closed on legacy mask"
    );
    assert!(
        reader_out
            .masked_controlled(&snap, &prep(64 << 20))
            .is_err(),
        "unavailable managed registry must fail closed on controlled mask"
    );
    // Unrestricted caller still reads the store.
    assert_eq!(output_objects_legacy(&reopened, None), vec!["<urn:cve-1>"]);
}
