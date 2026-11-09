//! Claim states under a named policy at an evidence view (spec C25, "Recall and Ask
//! with claims").
//!
//! Each test states the occurrences and states it expects independently of the
//! operations and compares the read with them.

use oxrdf::{NamedNode, Triple};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs, Preparation};
use sparkles::evidence::{
    self, ClaimInput, ClaimKind, ClaimOrigin, ClaimState, ConflictMode, DecisionInput, Disposition,
    EvidenceInput, MatchingPolicy, Polarity, Policy, ProposeOptions, Proposed, RecordKey,
    RecordRef, RecordVersion, ReviewOptions, Settings, Sha256Digest, SourceContent,
    SourceRevisionInput, WithdrawalInput,
};
use sparkles::history::At;
use sparkles::store::BoundEvidenceView;
use sparkles::task::Control;
use std::sync::Arc;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const OUTPUT: &str = "urn:ev:accepted";
const IDENTITIES: &str = "urn:ev:identities";
const MAPS: &str = "urn:ev:maps";
const MAP: &str = "urn:ev:map";
const DOCS: &str = "urn:docs";

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

fn preparation() -> Preparation {
    Preparation::new(Control::none(), 64 << 20).unwrap()
}

fn settings(version: u32) -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        identity: vec![IDENTITIES.into(), MAPS.into()],
        projections: vec![OUTPUT.into(), MAP.into()],
        policies: [(
            "main".to_string(),
            Policy {
                version,
                admitted_kinds: vec![ClaimKind::Reported],
                required_scope: Vec::new(),
                conflicts: ConflictMode::AdmitLabelled,
                boundary: vec![SOURCES.into(), CLAIMS.into()],
                decisions: DECISIONS.into(),
                supports: SUPPORTS.into(),
                output: OUTPUT.into(),
            },
        )]
        .into_iter()
        .collect(),
        matching_policies: [(
            "people".to_string(),
            MatchingPolicy {
                version: 1,
                boundary: vec![IDENTITIES.into()],
                decisions: DECISIONS.into(),
                maps: MAPS.into(),
                output: MAP.into(),
            },
        )]
        .into_iter()
        .collect(),
        ..Settings::default()
    }
}

/// An in-memory dataset whose configuration history is activated, then configured.
fn configured() -> Dataset {
    let ds = Dataset::memory();
    ds.store()
        .activate_memory_configuration_history(&preparation())
        .unwrap();
    evidence::configure(&ds, &settings(1), &Control::none()).unwrap();
    ds
}

fn view(ds: &Dataset, at: &At) -> BoundEvidenceView {
    ds.store().evidence_view(at, &preparation()).unwrap()
}

fn head(ds: &Dataset) -> BoundEvidenceView {
    view(ds, &At::Head)
}

fn affected() -> Triple {
    Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve"))
}

fn occurrence(source: &str, polarity: Polarity, proposition: Triple) -> EvidenceInput {
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri(&format!("urn:source:{source}")),
            source_key: key(source),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha(source),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key("c"),
            revision: RecordRef::Local(key("rev")),
            proposition,
            kind: ClaimKind::Reported,
            polarity,
            attributed_author: iri("urn:publisher"),
            scope: Default::default(),
            confidence: Default::default(),
        }],
        ..Default::default()
    }
}

fn propose(ds: &Dataset, input: &EvidenceInput) -> (NamedNode, Sha256Digest) {
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri(CLAIMS));
    let p: Proposed =
        evidence::propose(None, ds, ds, input, &o, &mut |_, _| all(), &Control::none()).unwrap();
    let r = p.records.iter().find(|r| r.key == "claim").unwrap();
    (r.iri.clone(), r.digest)
}

fn claim(ds: &Dataset, source: &str) -> (NamedNode, Sha256Digest) {
    propose(ds, &occurrence(source, Polarity::Positive, affected()))
}

fn review(
    ds: &Dataset,
    target: &(NamedNode, Sha256Digest),
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
            target: target.0.clone(),
            target_digest: target.1,
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

fn withdraw(ds: &Dataset, target: &(NamedNode, Sha256Digest)) {
    evidence::withdraw(
        ds,
        ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: target.0.clone(),
            target_digest: target.1,
        }],
        &ReviewOptions::new(iri("urn:principal:reviewer")),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
}

fn states(
    v: &BoundEvidenceView,
    claims: &[&NamedNode],
    access: Option<Arc<GraphAccess>>,
) -> Vec<Option<ClaimState>> {
    let claims: Vec<NamedNode> = claims.iter().map(|c| (*c).clone()).collect();
    evidence::claim_states(v, "main", &claims, access, &Control::none())
        .unwrap()
        .into_iter()
        .map(|o| o.map(|o| o.state))
        .collect()
}

/// A caller whose view hides `predicate` in `graph`.
fn masking(predicate: &str, graph: &str) -> Option<Arc<GraphAccess>> {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    Some(Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "masked".into(),
                    predicates: Some(vec![predicate.to_string()]),
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
    )))
}

fn reading(graphs: &[&str]) -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(graphs.iter().copied(), &[])),
        Graphs::All,
    )))
}

/// Every state follows the authenticated current decision, and a withdrawal wins.
#[test]
fn states_follow_current_decisions_and_withdrawals() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let b = claim(&ds, "mirror");
    assert_eq!(
        states(&head(&ds), &[&a.0, &b.0], all()),
        vec![Some(ClaimState::Unreviewed), Some(ClaimState::Unreviewed)]
    );
    let accepted = review(&ds, &a, Disposition::Accepted, None);
    review(&ds, &b, Disposition::Rejected, None);
    let v = head(&ds);
    let read = evidence::claim_states(&v, "main", &[a.0.clone()], all(), &Control::none()).unwrap();
    let o = read[0].clone().unwrap();
    assert_eq!(o.state, ClaimState::Accepted);
    assert_eq!(o.decision.as_ref(), Some(&accepted));
    assert_eq!(o.decision_version, Some(1));
    assert_eq!(o.graph, iri(CLAIMS));
    assert_eq!(o.proposition, affected());
    assert_eq!(o.polarity, Polarity::Positive);
    assert_eq!(o.by, Some(iri("urn:publisher")));
    assert_eq!(o.source, Some(iri("urn:source:advisory")));
    assert_eq!(o.origin, ClaimOrigin::Source);
    assert_eq!(states(&v, &[&b.0], all()), vec![Some(ClaimState::Rejected)]);
    // a reversal makes the later decision current
    let later = review(&ds, &a, Disposition::NeedsEvidence, Some(&accepted));
    let v = head(&ds);
    let read = evidence::claim_states(&v, "main", &[a.0.clone()], all(), &Control::none()).unwrap();
    assert_eq!(read[0].as_ref().unwrap().state, ClaimState::NeedsEvidence);
    assert_eq!(read[0].as_ref().unwrap().decision.as_ref(), Some(&later));
    withdraw(&ds, &b);
    assert_eq!(
        states(&head(&ds), &[&b.0], all()),
        vec![Some(ClaimState::Withdrawn)]
    );
    // not a claim at all
    assert_eq!(
        states(&head(&ds), &[&iri("urn:nothing")], all()),
        vec![None]
    );
}

/// A projected triple cites the claims of its live supports, and a withdrawn
/// occurrence leaves the surviving one.
#[test]
fn supports_name_live_occurrences_only() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let b = claim(&ds, "mirror");
    review(&ds, &a, Disposition::Accepted, None);
    review(&ds, &b, Disposition::Accepted, None);
    let mut both = vec![a.0.clone(), b.0.clone()];
    both.sort();
    let supports = |ds: &Dataset| {
        evidence::with_policy_claims(&head(ds), "main", all(), &Control::none(), |c| {
            c.supports(&[affected()])
        })
        .unwrap()
    };
    assert_eq!(supports(&ds), vec![both]);
    withdraw(&ds, &a);
    assert_eq!(supports(&ds), vec![vec![b.0.clone()]]);
    assert_eq!(
        states(&head(&ds), &[&a.0, &b.0], all()),
        vec![Some(ClaimState::Withdrawn), Some(ClaimState::Accepted)]
    );
}

/// Claims are found by the subject or object of their proposition, never by others.
#[test]
fn claims_about_entities_match_subject_or_object() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let other = propose(
        &ds,
        &occurrence(
            "unrelated",
            Polarity::Negative,
            Triple::new(iri("urn:x"), iri("urn:p"), iri("urn:y")),
        ),
    );
    let about = |entities: &[&str]| {
        let e: Vec<NamedNode> = entities.iter().map(|e| iri(e)).collect();
        evidence::with_policy_claims(&head(&ds), "main", all(), &Control::none(), |c| c.about(&e))
            .unwrap()
            .into_iter()
            .map(|o| o.claim)
            .collect::<Vec<_>>()
    };
    assert_eq!(about(&["urn:release"]), vec![a.0.clone()]);
    assert_eq!(about(&["urn:cve"]), vec![a.0.clone()]);
    assert_eq!(about(&["urn:y"]), vec![other.0.clone()]);
    assert!(about(&["urn:affectedBy"]).is_empty());
    let read = evidence::claim_states(
        &head(&ds),
        "main",
        &[other.0.clone()],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(read[0].as_ref().unwrap().polarity, Polarity::Negative);
}

/// A caller without the policy's grants, an unknown name and a matching policy all
/// answer with the same refusal.
#[test]
fn hidden_unknown_and_wrong_kind_policies_answer_alike() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let v = head(&ds);
    let message = |policy: &str, access: Option<Arc<GraphAccess>>| {
        let e = evidence::claim_states(&v, policy, &[a.0.clone()], access, &Control::none())
            .unwrap_err();
        assert_eq!(e.code(), "invalid", "{e:?}");
        e.to_string().replace(policy, "NAME")
    };
    let unknown = message("nothing", all());
    assert_eq!(message("people", all()), unknown);
    assert_eq!(
        message("main", reading(&[SOURCES, CLAIMS, SUPPORTS, OUTPUT])),
        unknown
    );
    assert_eq!(
        message("main", reading(&[SOURCES, CLAIMS, DECISIONS, OUTPUT])),
        unknown
    );
    assert!(!unknown.contains(DECISIONS) && !unknown.contains("version"));
    let m = |policy: &str, access: Option<Arc<GraphAccess>>| {
        evidence::check_matching_policy(&v, policy, access.as_ref())
            .unwrap_err()
            .to_string()
            .replace(policy, "NAME")
    };
    let unknown = m("nothing", all());
    assert_eq!(m("main", all()), unknown);
    assert_eq!(m("people", reading(&[DECISIONS, MAPS, MAP])), unknown);
    evidence::check_matching_policy(&v, "people", all().as_ref()).unwrap();
}

/// A readable claim whose current decision the caller's view hides is left out, and
/// an older decision never stands in for it.
#[test]
fn a_hidden_current_decision_hides_the_occurrence() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let b = claim(&ds, "mirror");
    let first = review(&ds, &a, Disposition::Accepted, None);
    review(&ds, &a, Disposition::Rejected, Some(&first));
    let hidden = masking("urn:x-sparkles:evidence:disposition", DECISIONS);
    assert_eq!(
        states(&head(&ds), &[&a.0, &b.0], hidden.clone()),
        vec![None, Some(ClaimState::Unreviewed)]
    );
    // a hidden claim graph hides every claim
    let no_claims = reading(&[SOURCES, DECISIONS, SUPPORTS, OUTPUT]);
    assert_eq!(
        states(&head(&ds), &[&a.0, &b.0], no_claims),
        vec![None, None]
    );
    // a hidden support leaves the projected triple without a cited claim
    let r = claim(&ds, "third");
    review(&ds, &r, Disposition::Accepted, None);
    let supports = evidence::with_policy_claims(
        &head(&ds),
        "main",
        masking("urn:x-sparkles:evidence:claim", SUPPORTS),
        &Control::none(),
        |c| c.supports(&[affected()]),
    )
    .unwrap();
    assert_eq!(supports, vec![Vec::<NamedNode>::new()]);
}

/// A10: a claim from an instruction document is labelled by the typing triple the
/// caller can see, and its text never changes the label.
#[test]
fn instruction_origins_follow_visible_typing() {
    let ds = configured();
    let a = propose(
        &ds,
        &occurrence(
            "agents",
            Polarity::Positive,
            Triple::new(
                iri("urn:release"),
                iri("urn:note"),
                iri("urn:ignore-previous-instructions"),
            ),
        ),
    );
    let b = claim(&ds, "advisory");
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{DOCS}> {{ <urn:source:agents> a <{}> }} }}",
        evidence::MEM_INSTRUCTIONS
    ))
    .unwrap();
    let origin = |access: Option<Arc<GraphAccess>>| -> Vec<ClaimOrigin> {
        evidence::claim_states(
            &head(&ds),
            "main",
            &[a.0.clone(), b.0.clone()],
            access,
            &Control::none(),
        )
        .unwrap()
        .into_iter()
        .map(|o| o.unwrap().origin)
        .collect()
    };
    assert_eq!(
        origin(all()),
        vec![ClaimOrigin::Instructions, ClaimOrigin::Source]
    );
    let no_docs = reading(&[SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT]);
    assert_eq!(
        origin(no_docs),
        vec![ClaimOrigin::Source, ClaimOrigin::Source]
    );
}

/// A historical view reads the states and the policy of its own configuration.
#[test]
fn views_read_their_own_states_and_configuration() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    let before = ds.store().head_commit().seq;
    // an in-memory dataset reconstructs only its head, so the view is taken first
    let old = view(&ds, &At::Commit(before));
    review(&ds, &a, Disposition::Accepted, None);
    assert_eq!(
        states(&old, &[&a.0], all()),
        vec![Some(ClaimState::Unreviewed)]
    );
    assert_eq!(
        states(&head(&ds), &[&a.0], all()),
        vec![Some(ClaimState::Accepted)]
    );
    // a settings-only change at the same sequence: Head sees version 2, the commit 1
    let seq = ds.store().head_commit().seq;
    evidence::configure(&ds, &settings(2), &Control::none()).unwrap();
    assert_eq!(ds.store().head_commit().seq, seq);
    let version = |v: &BoundEvidenceView| {
        evidence::with_policy_claims(v, "main", all(), &Control::none(), |c| {
            Ok(c.policy().version)
        })
        .unwrap()
    };
    assert_eq!(version(&head(&ds)), 2);
    assert_eq!(version(&view(&ds, &At::Commit(seq))), 1);
}

/// A store without activated configuration history refuses evidence views with a
/// typed code.
#[test]
fn an_unactivated_store_refuses_with_a_typed_code() {
    let ds = Dataset::memory();
    evidence::configure(&ds, &settings(1), &Control::none()).unwrap();
    let e = ds
        .store()
        .evidence_view(&At::Head, &preparation())
        .unwrap_err();
    assert_eq!(e.code(), "configuration-history-inactive", "{e:?}");
    let plain = Dataset::memory();
    let e = plain
        .store()
        .evidence_view(&At::Head, &preparation())
        .unwrap_err();
    assert_eq!(e.code(), "configuration-history-inactive", "{e:?}");
}

/// Identity resolution at a view reads the view's map.
#[test]
fn identity_resolution_reads_the_view() {
    let ds = configured();
    let v = head(&ds);
    let r = evidence::resolve_entities_at(&v, "people", &[iri("urn:ana")], all(), &Control::none())
        .unwrap();
    assert_eq!(r.revision, None);
    assert_eq!(r.number, 0);
    assert_eq!(r.entities[0].representative, iri("urn:ana"));
}

/// A hidden support hides the occurrence it cites, as a state and as a claim about an
/// entity, so the occurrence never returns as a supplementary claim.
#[test]
fn a_hidden_support_hides_its_occurrence() {
    let ds = configured();
    let a = claim(&ds, "advisory");
    review(&ds, &a, Disposition::Accepted, None);
    let b = claim(&ds, "mirror");
    let hidden = masking("urn:x-sparkles:evidence:claim", SUPPORTS);
    assert_eq!(
        states(&head(&ds), &[&a.0, &b.0], hidden.clone()),
        vec![None, Some(ClaimState::Unreviewed)]
    );
    let about = evidence::with_policy_claims(&head(&ds), "main", hidden, &Control::none(), |c| {
        c.about(&[iri("urn:release")])
    })
    .unwrap();
    assert_eq!(
        about.into_iter().map(|o| o.claim).collect::<Vec<_>>(),
        vec![b.0.clone()]
    );
}

/// Hidden claims take no place in the bound on claims about an entity.
#[test]
fn hidden_claims_do_not_count_toward_the_bound() {
    let ds = configured();
    let per = 1_000;
    for batch in 0..=(evidence::MAX_CLAIMS / per) {
        let mut input = occurrence(&format!("bulk{batch}"), Polarity::Positive, affected());
        let template = input.claims.pop().unwrap();
        for i in 0..per {
            let mut c = template.clone();
            c.key = key(&format!("claim{i}"));
            c.occurrence_key = key(&format!("c{i}"));
            c.proposition = Triple::new(
                iri("urn:release"),
                iri("urn:affectedBy"),
                iri(&format!("urn:cve:{batch}:{i}")),
            );
            input.claims.push(c);
        }
        let mut o = ProposeOptions::new(iri("urn:principal:importer"));
        o.proposal_graph = Some(iri(CLAIMS));
        evidence::propose(
            None,
            &ds,
            &ds,
            &input,
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
    }
    // a compacted store reads a masked view without materializing it
    ds.compact().unwrap();
    let big = Preparation::new(Control::none(), 1 << 30).unwrap();
    let v = ds.store().evidence_view(&At::Head, &big).unwrap();
    let hidden = masking("http://www.w3.org/ns/prov#wasAttributedTo", CLAIMS);
    let about = evidence::with_policy_claims(&v, "main", hidden, &Control::none(), |c| {
        c.about(&[iri("urn:release")])
    })
    .unwrap();
    assert!(about.is_empty());
    let all_about = evidence::with_policy_claims(&v, "main", all(), &Control::none(), |c| {
        c.about(&[iri("urn:release")])
    });
    assert!(all_about.is_err());
}

/// A visible typing triple in the default graph labels an instruction document, and
/// unreadable typing triples in many other graphs neither hide nor stand in for it.
#[test]
fn instruction_typing_is_read_from_every_readable_graph() {
    let ds = configured();
    let a = propose(
        &ds,
        &occurrence(
            "agents",
            Polarity::Positive,
            Triple::new(iri("urn:release"), iri("urn:note"), iri("urn:x")),
        ),
    );
    let class = evidence::MEM_INSTRUCTIONS;
    let mut update = String::from("INSERT DATA {");
    for i in 0..80 {
        update.push_str(&format!(
            " GRAPH <urn:hidden:{i}> {{ <urn:source:agents> a <{class}> }}"
        ));
    }
    update.push_str(&format!(
        " GRAPH <urn:docs:late> {{ <urn:source:agents> a <{class}> }} }}"
    ));
    ds.update(&update).unwrap();
    let origin = |access: Option<Arc<GraphAccess>>| {
        evidence::claim_states(&head(&ds), "main", &[a.0.clone()], access, &Control::none())
            .unwrap()[0]
            .clone()
            .unwrap()
            .origin
    };
    let base = [SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT];
    assert_eq!(origin(reading(&base)), ClaimOrigin::Source);
    let late: Vec<&str> = base.iter().copied().chain(["urn:docs:late"]).collect();
    assert_eq!(origin(reading(&late)), ClaimOrigin::Instructions);

    let ds = configured();
    let a = propose(
        &ds,
        &occurrence(
            "agents",
            Polarity::Positive,
            Triple::new(iri("urn:release"), iri("urn:note"), iri("urn:x")),
        ),
    );
    ds.update(&format!(
        "INSERT DATA {{ <urn:source:agents> a <{class}> }}"
    ))
    .unwrap();
    let read = evidence::claim_states(&head(&ds), "main", &[a.0.clone()], all(), &Control::none())
        .unwrap();
    assert_eq!(read[0].as_ref().unwrap().origin, ClaimOrigin::Instructions);
}

/// With no entities, identity resolution at a view still reads and checks the map.
#[test]
fn identity_resolution_of_no_entities_reads_the_map() {
    let ds = configured();
    let r =
        evidence::resolve_entities_at(&head(&ds), "people", &[], all(), &Control::none()).unwrap();
    assert_eq!(r.revision, None);
    assert!(r.entities.is_empty());
    let hidden = reading(&[SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT]);
    assert!(
        evidence::resolve_entities_at(&head(&ds), "people", &[], hidden, &Control::none()).is_err()
    );
    // a published map reports its revision without any entity
    publish_map(&ds);
    let r =
        evidence::resolve_entities_at(&head(&ds), "people", &[], all(), &Control::none()).unwrap();
    assert!(r.revision.is_some());
    assert_eq!(r.number, 1);
}

/// Accept a match of `urn:ana` and `urn:anna` under `people`, which publishes the
/// first map revision.
fn publish_map(ds: &Dataset) {
    let mut o = ProposeOptions::new(iri("urn:principal:matcher"));
    o.identity_graph = Some(iri(IDENTITIES));
    let p = evidence::propose(
        None,
        ds,
        ds,
        &EvidenceInput {
            identities: vec![evidence::IdentityInput {
                version: RecordVersion::V1,
                key: key("p"),
                occurrence_key: key("ana"),
                entities: vec![iri("urn:ana"), iri("urn:anna")],
                relation: evidence::IdentityRelation::PossibleMatch,
                preferred: None,
                attributed_author: iri("urn:matcher"),
                method: None,
            }],
            ..Default::default()
        },
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let r = p.records.iter().find(|r| r.key == "p").unwrap();
    let mut ro = ReviewOptions::new(iri("urn:principal:reviewer"));
    ro.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        ds,
        ds,
        "people",
        1,
        &[DecisionInput {
            key: "d".into(),
            target: r.iri.clone(),
            target_digest: r.digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
}
