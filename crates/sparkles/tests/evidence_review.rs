//! Review decisions, accepted projections and withdrawals (spec C25 §5, §6 and §9).
//!
//! Each test states the accepted set it expects independently of the operations, as the
//! triples the output graph must hold and the supports that must be live, and compares
//! the store with it after every step.

use oxrdf::{NamedNode, Term, Triple};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::evidence::{
    self, ClaimInput, ClaimKind, ConflictMode, DecisionInput, Disposition, EvidenceInput,
    EvidenceLinkInput, EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, Polarity, Policy,
    ProposeOptions, Proposed, RecordKey, RecordRef, RecordVersion, ReviewOptions, Reviewed,
    Settings, Sha256Digest, SourceContent, SourceRevisionInput, WithdrawalInput,
};
use sparkles::task::Control;
use std::sync::Arc;

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const OTHER: &str = "urn:ev:other";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const OUTPUT: &str = "urn:ev:accepted";
const EV: &str = "urn:x-sparkles:evidence:";

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

#[track_caller]
fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> String {
    match r {
        Err(e) => e.code().to_string(),
        Ok(v) => panic!("succeeded: {v:?}"),
    }
}

fn policy(mode: ConflictMode) -> Policy {
    Policy {
        version: 1,
        admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
        required_scope: Vec::new(),
        conflicts: mode,
        boundary: vec![SOURCES.into(), CLAIMS.into(), OTHER.into()],
        decisions: DECISIONS.into(),
        supports: SUPPORTS.into(),
        output: OUTPUT.into(),
    }
}

fn settings(mode: ConflictMode) -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into(), OTHER.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        projections: vec![OUTPUT.into()],
        policies: [("main".to_string(), policy(mode))].into_iter().collect(),
        ..Settings::default()
    }
}

fn configured(mode: ConflictMode) -> Dataset {
    let ds = Dataset::memory();
    evidence::configure(&ds, &settings(mode), &Control::none()).unwrap();
    ds
}

fn affected() -> Triple {
    Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve"))
}

fn revision(source_key: &str) -> SourceRevisionInput {
    SourceRevisionInput {
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
    }
}

fn claim(occurrence: &str, polarity: Polarity, proposition: Triple) -> ClaimInput {
    ClaimInput {
        version: RecordVersion::V1,
        key: key("claim"),
        occurrence_key: key(occurrence),
        revision: RecordRef::Local(key("rev")),
        proposition,
        kind: ClaimKind::Reported,
        polarity,
        attributed_author: iri("urn:publisher"),
        scope: Default::default(),
        confidence: Default::default(),
        validity: None,
    }
}

/// One source and one claim of it, in the claims graph.
fn occurrence(source: &str, polarity: Polarity) -> EvidenceInput {
    EvidenceInput {
        revisions: vec![revision(source)],
        claims: vec![claim("c", polarity, affected())],
        ..Default::default()
    }
}

fn propose(ds: &Dataset, input: &EvidenceInput) -> Proposed {
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri(CLAIMS));
    evidence::propose(None, ds, ds, input, &o, &mut |_, _| all(), &Control::none()).unwrap()
}

/// The claim of a proposal, with its record digest.
fn claim_of(p: &Proposed) -> (NamedNode, Sha256Digest) {
    let r = p.records.iter().find(|r| r.key == "claim").unwrap();
    (r.iri.clone(), r.digest)
}

fn reviewer() -> ReviewOptions {
    ReviewOptions::new(iri("urn:principal:reviewer"))
}

fn decision(
    k: &str,
    target: &(NamedNode, Sha256Digest),
    disposition: Disposition,
    supersedes: Option<&NamedNode>,
) -> DecisionInput {
    DecisionInput {
        key: k.into(),
        target: target.0.clone(),
        target_digest: target.1,
        disposition,
        supersedes: supersedes.cloned(),
    }
}

fn review_as(
    ds: &Dataset,
    decisions: &[DecisionInput],
    o: &ReviewOptions,
    access: Option<Arc<GraphAccess>>,
) -> sparkles::Result<Reviewed> {
    let mut o = o.clone();
    if o.if_head.is_none() {
        o.if_head = Some(ds.store().head_commit().seq);
    }
    evidence::review(
        None,
        ds,
        ds,
        "main",
        1,
        decisions,
        &o,
        &mut |_, _| access.clone(),
        &Control::none(),
    )
}

fn review(ds: &Dataset, decisions: &[DecisionInput]) -> sparkles::Result<Reviewed> {
    review_as(ds, decisions, &reviewer(), all())
}

fn withdraw(ds: &Dataset, target: &(NamedNode, Sha256Digest)) -> evidence::Withdrawn {
    evidence::withdraw(
        ds,
        ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: target.0.clone(),
            target_digest: target.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap()
}

/// The triples of the output graph, as N-Triples lines.
fn accepted(ds: &Dataset) -> Vec<String> {
    let mut out: Vec<String> = ds
        .select(&format!(
            "SELECT ?s ?p ?o WHERE {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"
        ))
        .unwrap()
        .iter()
        .map(|r| {
            format!(
                "{} {} {}",
                r.get("s").unwrap(),
                r.get("p").unwrap(),
                r.get("o").unwrap()
            )
        })
        .collect();
    out.sort();
    out
}

fn affected_line() -> Vec<String> {
    vec!["<urn:release> <urn:affectedBy> <urn:cve>".to_string()]
}

fn count(ds: &Dataset, class: &str, graph: &str) -> usize {
    ds.select(&format!(
        "SELECT ?x WHERE {{ GRAPH <{graph}> {{ ?x a <{EV}{class}> }} }}"
    ))
    .unwrap()
    .len()
}

/// A1 and A5: two occurrences of one proposition each own one support. Reversing or
/// withdrawing one leaves the triple to the other, and every decision remains.
#[test]
fn duplicate_occurrences_each_own_a_support() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let b = claim_of(&propose(&ds, &occurrence("mirror", Polarity::Positive)));
    assert_ne!(a.0, b.0, "two sources, two occurrences");
    // A2: before an acceptance, nothing asserts the proposition
    assert!(
        !ds.ask("ASK { <urn:release> <urn:affectedBy> <urn:cve> }")
            .unwrap()
    );
    assert!(
        !ds.ask("ASK { GRAPH ?g { <urn:release> <urn:affectedBy> <urn:cve> } }")
            .unwrap()
    );
    let first = review(
        &ds,
        &[
            decision("a", &a, Disposition::Accepted, None),
            decision("b", &b, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    assert!(first.committed);
    assert_eq!(accepted(&ds), affected_line());
    assert_eq!(count(&ds, "AcceptanceSupport", SUPPORTS), 2);
    assert_eq!(first.changed.inserted.len(), 1);
    let da = first.decisions[0].iri.clone();
    let db = first.decisions[1].iri.clone();
    // reversing a leaves b's support
    let r = review(&ds, &[decision("a2", &a, Disposition::Rejected, Some(&da))]).unwrap();
    assert_eq!(
        r.changed.supports_ended,
        vec![first.decisions[0].support.clone().unwrap()]
    );
    assert!(r.changed.removed.is_empty());
    assert_eq!(accepted(&ds), affected_line());
    // withdrawing b removes the last live support
    let w = withdraw(&ds, &b);
    assert_eq!(w.changed.removed.len(), 1, "{w:?}");
    assert!(accepted(&ds).is_empty());
    // accepting a again projects it again; b stays withdrawn
    let r2 = review(
        &ds,
        &[decision(
            "a3",
            &a,
            Disposition::Accepted,
            Some(&r.decisions[0].iri),
        )],
    )
    .unwrap();
    assert_eq!(accepted(&ds), affected_line());
    assert_eq!(
        code(review(
            &ds,
            &[decision("b2", &b, Disposition::Accepted, Some(&db))]
        )),
        "target-withdrawn"
    );
    // every decision and the withdrawal remain as immutable records
    assert_eq!(count(&ds, "ReviewDecision", DECISIONS), 4);
    assert_eq!(count(&ds, "Withdrawal", CLAIMS), 1);
    assert!(r2.decisions[0].support.is_some());
    // the projection is labelled with its policy, version and conflict mode
    assert_eq!(count(&ds, "Projection", SUPPORTS), 1);
}

/// A3: a negative occurrence is retained and never projected, and it conflicts with a
/// positive acceptance until it is rejected under the policy.
#[test]
fn negative_claims_are_recorded_and_conflict() {
    let ds = configured(ConflictMode::Refuse);
    let yes = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let no = claim_of(&propose(&ds, &occurrence("vendor", Polarity::Negative)));
    // accepting the negative claim records the decision without a projection
    let r = review(&ds, &[decision("n", &no, Disposition::Accepted, None)]).unwrap();
    assert_eq!(
        r.decisions[0].not_projected,
        Some("the claim is not positive")
    );
    assert!(accepted(&ds).is_empty());
    // the positive acceptance now meets it
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review(
            &ds,
            &[decision("y", &yes, Disposition::Accepted, None)]
        )),
        "evidence-conflict"
    );
    assert_eq!(
        ds.store().head_commit().seq,
        head,
        "a refusal writes nothing"
    );
    // rejecting the negative claim in the same batch decides it, in either order
    let nd = r.decisions[0].iri.clone();
    let ok = review(
        &ds,
        &[
            decision("y", &yes, Disposition::Accepted, None),
            decision("n2", &no, Disposition::Rejected, Some(&nd)),
        ],
    )
    .unwrap();
    assert!(ok.decisions[0].conflicts.is_empty());
    assert_eq!(accepted(&ds), affected_line());
    // no RDF negation was invented
    assert_eq!(
        ds.select(
            "SELECT * WHERE { GRAPH ?g { ?s <http://www.w3.org/2002/07/owl#complementOf> ?o } }"
        )
        .unwrap()
        .len(),
        0
    );
}

/// An `admitLabelled` policy admits a contradicted claim and names what contradicts it.
#[test]
fn a_labelled_policy_admits_conflicts_and_names_them() {
    let ds = configured(ConflictMode::AdmitLabelled);
    let yes = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let no = claim_of(&propose(&ds, &occurrence("vendor", Polarity::Negative)));
    let r = review(&ds, &[decision("y", &yes, Disposition::Accepted, None)]).unwrap();
    assert_eq!(r.decisions[0].conflicts, vec![no.0.clone()]);
    assert_eq!(accepted(&ds), affected_line());
    let support = r.decisions[0].support.clone().unwrap();
    assert!(ds.ask(&format!(
        "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> <{EV}conflictsWith> <{}> . ?p a <{EV}Projection> ; <{EV}conflictMode> <{EV}AdmitLabelled> }} }}",
        support.as_str(),
        no.0.as_str()
    ))
    .unwrap());
}

/// A6: stale preconditions refuse without writing a decision or a projected triple.
#[test]
fn stale_preconditions_refuse_and_write_nothing() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let first = review(&ds, &[decision("a", &a, Disposition::NeedsEvidence, None)]).unwrap();
    let head = ds.store().head_commit().seq;
    let mut stale = reviewer();
    stale.if_head = Some(head - 1);
    assert_eq!(
        code(review_as(
            &ds,
            &[decision(
                "a",
                &a,
                Disposition::Accepted,
                Some(&first.decisions[0].iri)
            )],
            &stale,
            all()
        )),
        "head-changed"
    );
    let changed = (a.0.clone(), sha("other"));
    assert_eq!(
        code(review(
            &ds,
            &[decision(
                "a",
                &changed,
                Disposition::Accepted,
                Some(&first.decisions[0].iri)
            )]
        )),
        "target-changed"
    );
    // no supersedes while a current decision exists
    assert_eq!(
        code(review(
            &ds,
            &[decision("a", &a, Disposition::Accepted, None)]
        )),
        "decision-superseded"
    );
    // the policy version the reviewer saw
    let mut o = reviewer();
    o.if_head = Some(head);
    let e = evidence::review(
        None,
        &ds,
        &ds,
        "main",
        2,
        &[decision(
            "a",
            &a,
            Disposition::Accepted,
            Some(&first.decisions[0].iri),
        )],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    );
    assert_eq!(code(e), "policy-changed");
    // a review without an expected head is invalid
    let e = evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision(
            "a",
            &a,
            Disposition::Accepted,
            Some(&first.decisions[0].iri),
        )],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    );
    assert_eq!(code(e), "invalid");
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(accepted(&ds).is_empty());
}

/// A14: a retry with the same key replays the first receipt and duplicates no support,
/// another payload under the key conflicts, and a keyless retry is stale.
#[test]
fn retries_never_duplicate_supports() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let mut o = reviewer();
    o.idempotency_key = Some("k".into());
    let head = ds.store().head_commit().seq;
    o.if_head = Some(head);
    let d = [decision("a", &a, Disposition::Accepted, None)];
    let first = review_as(&ds, &d, &o, all()).unwrap();
    assert!(first.committed);
    let again = review_as(&ds, &d, &o, all()).unwrap();
    assert!(again.replayed && !again.committed);
    assert_eq!(again.commit, first.commit);
    assert_eq!(again.decisions[0].iri, first.decisions[0].iri);
    assert_eq!(again.decisions[0].support, first.decisions[0].support);
    assert_eq!(count(&ds, "AcceptanceSupport", SUPPORTS), 1);
    assert_eq!(count(&ds, "ReviewDecision", DECISIONS), 1);
    let other = [decision("a", &a, Disposition::Rejected, None)];
    assert_eq!(
        code(review_as(&ds, &other, &o, all())),
        "idempotency-conflict"
    );
    // the same decision without a key is stale: the first one is now current
    assert_eq!(code(review(&ds, &d)), "decision-superseded");
    assert_eq!(count(&ds, "AcceptanceSupport", SUPPORTS), 1);
}

/// A11: an acceptance whose evidence holds a record the reviewer cannot read fails
/// exactly as a review of an unreadable target does.
#[test]
fn hidden_evidence_answers_as_an_unavailable_target() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    // a passage reference and a link to the claim, in a proposal graph the reviewer
    // cannot read; the passage itself is in the readable source graph
    let text = "Release 1.2 is affected.";
    let mut o = ProposeOptions::new(iri("urn:principal:importer"));
    o.proposal_graph = Some(iri(OTHER));
    let mut hidden_batch = EvidenceInput {
        revisions: vec![revision("notes")],
        ..Default::default()
    };
    hidden_batch.renditions.push(evidence::RenditionInput {
        version: RecordVersion::V1,
        key: key("text"),
        revision: RecordRef::Local(key("rev")),
        occurrence_key: key("text"),
        conversion: evidence::MethodRef {
            method: iri("urn:method:identity"),
            version: key("1"),
        },
        media_type: "text/plain".into(),
        language: None,
        content: evidence::RenditionContent::NormalizedText(text.into()),
        expected_digest: None,
    });
    hidden_batch.passages.push(evidence::PassageInput {
        version: RecordVersion::V1,
        key: key("span"),
        revision: RecordRef::Local(key("rev")),
        rendition: RecordRef::Local(key("text")),
        occurrence_key: key("span"),
        start: 0,
        end: 7,
        text_digest: sha(&text[0..7]),
        quote: None,
    });
    hidden_batch.references.push(EvidenceRefInput {
        version: RecordVersion::V1,
        key: key("ref"),
        occurrence_key: key("ref"),
        target: EvidenceRefTarget::Passage {
            revision: RecordRef::Local(key("rev")),
            rendition: RecordRef::Local(key("text")),
            passage: RecordRef::Local(key("span")),
        },
    });
    hidden_batch.links.push(EvidenceLinkInput {
        version: RecordVersion::V1,
        key: key("link"),
        occurrence_key: key("link"),
        target: RecordRef::Existing(a.0.clone()),
        evidence: RecordRef::Local(key("ref")),
        relation: EvidenceRelation::Supports,
        attributed_author: iri("urn:importer"),
    });
    evidence::propose(
        None,
        &ds,
        &ds,
        &hidden_batch,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let limited = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::All,
    )));
    let head = ds.store().head_commit().seq;
    let answer = |target: (NamedNode, Sha256Digest)| {
        let e = review_as(
            &ds,
            &[decision("a", &target, Disposition::Accepted, None)],
            &reviewer(),
            limited.clone(),
        )
        .unwrap_err();
        (e.code().to_string(), e.to_string())
    };
    let hidden = answer(a.clone());
    let missing = answer((iri("urn:uuid:00000000-0000-4000-8000-000000000000"), a.1));
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);
    assert_eq!(ds.store().head_commit().seq, head);
    // a reviewer who can read the link and its reference accepts
    assert!(review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).is_ok());
}

/// The managed-write integrity acceptance: direct edits of the decisions, supports
/// and output graphs refuse before publication and leave the accepted set unchanged.
#[test]
fn direct_edits_of_projection_graphs_are_refused() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    for g in [OUTPUT, SUPPORTS, DECISIONS] {
        let e = ds
            .update(&format!("DELETE WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}"))
            .unwrap_err();
        assert_eq!(e.code(), "profile-managed-graph", "{g}");
        let e = ds
            .update(&format!(
                "INSERT DATA {{ GRAPH <{g}> {{ <urn:x> <urn:y> <urn:z> }} }}"
            ))
            .unwrap_err();
        assert_eq!(e.code(), "profile-managed-graph", "{g}");
    }
    assert_eq!(accepted(&ds), affected_line());
}

/// A detach and new protection of a projection graph orphans its supports, so later
/// reviews and withdrawals under the policy refuse until it is reconciled.
#[test]
fn a_changed_protection_makes_the_projection_unavailable() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let b = claim_of(&propose(&ds, &occurrence("mirror", Polarity::Positive)));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review(
            &ds,
            &[decision("b", &b, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(
        code(evidence::withdraw(
            &ds,
            &ds,
            &[WithdrawalInput {
                key: "w".into(),
                target: a.0.clone(),
                target_digest: a.1,
            }],
            &reviewer(),
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

/// Grants: a reviewer needs update and read grants on the decisions, supports and
/// output graphs.
#[test]
fn a_reviewer_without_grants_is_refused() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let mut o = reviewer();
    o.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([DECISIONS, SUPPORTS], &[])),
    )));
    let e = review_as(
        &ds,
        &[decision("a", &a, Disposition::Accepted, None)],
        &o,
        all(),
    )
    .unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    let no_read = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([SOURCES, CLAIMS, DECISIONS, OUTPUT], &[])),
        Graphs::All,
    )));
    let e = review_as(
        &ds,
        &[decision("a", &a, Disposition::Accepted, None)],
        &reviewer(),
        no_read,
    )
    .unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    assert!(accepted(&ds).is_empty());
}

/// Policy admission: a kind the policy does not admit, or a missing required scope,
/// records the acceptance without a projection.
#[test]
fn the_policy_decides_what_projects() {
    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    s.policies.get_mut("main").unwrap().required_scope = vec!["urn:scope:release".into()];
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let mut hypothesis = occurrence("lab", Polarity::Positive);
    hypothesis.claims[0].kind = ClaimKind::Hypothesis;
    let h = claim_of(&propose(&ds, &hypothesis));
    let unscoped = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let mut scoped = occurrence("vendor", Polarity::Positive);
    scoped.claims[0].scope = [(
        "urn:scope:release".to_string(),
        vec![Term::NamedNode(iri("urn:release"))],
    )]
    .into_iter()
    .collect();
    let s = claim_of(&propose(&ds, &scoped));
    let r = review(
        &ds,
        &[
            decision("h", &h, Disposition::Accepted, None),
            decision("u", &unscoped, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    assert_eq!(
        r.decisions[0].not_projected,
        Some("the policy does not admit the claim's kind")
    );
    assert_eq!(
        r.decisions[1].not_projected,
        Some("the claim lacks a scope the policy requires")
    );
    assert!(accepted(&ds).is_empty());
    review(&ds, &[decision("s", &s, Disposition::Accepted, None)]).unwrap();
    assert_eq!(accepted(&ds), affected_line());
}

/// A dry run writes nothing, and a projection on a branch stays on the branch.
#[test]
fn dry_runs_and_branches() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let mut o = reviewer();
    o.dry_run = true;
    let head = ds.store().head_commit().seq;
    let r = review_as(
        &ds,
        &[decision("a", &a, Disposition::Accepted, None)],
        &o,
        all(),
    )
    .unwrap();
    assert!(!r.committed && r.preview.is_some());
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(accepted(&ds).is_empty());
    ds.store()
        .create_branch("dev", &Default::default())
        .unwrap();
    let dev = ds.branch("dev").unwrap();
    let mut o = reviewer();
    o.if_head = Some(dev.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &dev,
        "main",
        1,
        &[decision("a", &a, Disposition::Accepted, None)],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(accepted(&dev), affected_line());
    assert!(accepted(&ds).is_empty());
}

/// A13: withdrawal and policy changes are explicit; an existing withdrawal is reported
/// and not repeated, and no record is edited in place.
#[test]
fn a_withdrawal_is_recorded_once() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let first = withdraw(&ds, &a);
    assert!(!first.withdrawals[0].existing);
    let again = withdraw(&ds, &a);
    assert!(again.withdrawals[0].existing && again.withdrawals[0].iri.is_none());
    assert_eq!(count(&ds, "Withdrawal", CLAIMS), 1);
    // the withdrawal carries its time and makes no contrary claim
    assert!(ds.ask(&format!(
        "ASK {{ GRAPH <{CLAIMS}> {{ ?w a <{EV}Withdrawal> ; <{EV}withdraws> <{}> ; <http://www.w3.org/ns/prov#invalidatedAtTime> ?t }} }}",
        a.0.as_str()
    )).unwrap());
    assert_eq!(count(&ds, "Claim", CLAIMS), 1);
    // a rejection of a withdrawn claim is still recorded
    assert!(review(&ds, &[decision("a", &a, Disposition::Rejected, None)]).is_ok());
}

/// The decisions, supports, projection and withdrawal records the operations write
/// conform to the profile's shapes.
#[cfg(feature = "shacl")]
#[test]
fn written_records_conform_to_the_profile_shapes() {
    use oxrdfio::RdfFormat;
    let ds = configured(ConflictMode::AdmitLabelled);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let b = claim_of(&propose(&ds, &occurrence("vendor", Polarity::Negative)));
    let r = review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    review(
        &ds,
        &[decision(
            "a2",
            &a,
            Disposition::Accepted,
            Some(&r.decisions[0].iri),
        )],
    )
    .unwrap();
    withdraw(&ds, &b);
    // every record graph, merged into one default graph
    let copy = Dataset::memory();
    let mut nt = String::new();
    for g in [SOURCES, CLAIMS, DECISIONS, SUPPORTS] {
        for t in ds
            .construct(&format!(
                "CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}"
            ))
            .unwrap()
            .iter()
        {
            nt.push_str(&format!("{t} .\n"));
        }
    }
    copy.load_str(&nt, RdfFormat::NTriples).unwrap();
    let shapes = sparkles_shacl::Shapes::parse(
        evidence::profile(1).unwrap().shapes,
        RdfFormat::Turtle,
        None,
    )
    .unwrap();
    let report = copy
        .validation()
        .shacl(&shapes, &Default::default())
        .unwrap();
    assert!(report.conforms, "{report:?}");
    for class in [
        "ReviewDecision",
        "AcceptanceSupport",
        "Projection",
        "Withdrawal",
    ] {
        assert!(
            copy.ask(&format!("ASK {{ ?x a <{EV}{class}> }}")).unwrap(),
            "{class}"
        );
    }
}

/// A triple protection that hides a claim's proposition from the reviewer makes an
/// acceptance answer exactly as an unreadable target does.
#[test]
fn a_hidden_proposition_answers_as_an_unavailable_target() {
    use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let masked = Some(Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: Arc::new(Protection {
                    name: "propositions".into(),
                    predicates: Some(vec![
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies".into(),
                    ]),
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
    )));
    let answer = |target: (NamedNode, Sha256Digest)| {
        let e = review_as(
            &ds,
            &[decision("a", &target, Disposition::Accepted, None)],
            &reviewer(),
            masked.clone(),
        )
        .unwrap_err();
        (e.code().to_string(), e.to_string())
    };
    let hidden = answer(a.clone());
    let missing = answer((iri("urn:uuid:00000000-0000-4000-8000-000000000000"), a.1));
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);
    assert!(accepted(&ds).is_empty());
}

/// Content planted under a predictable keyed decision identity while the decisions
/// graph was detached is never authenticated by the keyed review.
#[test]
fn a_planted_decision_identity_refuses_the_review() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let mut o = reviewer();
    o.idempotency_key = Some("k".into());
    o.dry_run = true;
    let d = [decision("a", &a, Disposition::Accepted, None)];
    let planned = review_as(&ds, &d, &o, all()).unwrap().decisions[0]
        .iri
        .clone();
    evidence::detach(&ds, DECISIONS).unwrap();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{DECISIONS}> {{ <{}> <{EV}disposition> <{EV}Rejected> }} }}",
        planned.as_str()
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    o.dry_run = false;
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(review_as(&ds, &d, &o, all())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

/// A projection whose supports graph was detached and cleared refuses later
/// operations instead of leaving its triples without an owner.
#[test]
fn a_cleared_supports_graph_keeps_the_projection_unavailable() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let b = claim_of(&propose(&ds, &occurrence("mirror", Polarity::Positive)));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    evidence::detach(&ds, SUPPORTS).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    let w = evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w".into(),
            target: a.0.clone(),
            target_digest: a.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    );
    assert_eq!(code(w), "projection-unavailable");
    assert_eq!(
        code(review(
            &ds,
            &[decision("b", &b, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(accepted(&ds), affected_line());
}

/// A support admitted under an earlier policy version ends with its claim's
/// withdrawal even when the policy's boundary no longer holds the claim's graph.
#[test]
fn a_withdrawal_ends_supports_of_an_earlier_boundary() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    let mut s = settings(ConflictMode::Refuse);
    let p = s.policies.get_mut("main").unwrap();
    p.version = 2;
    p.boundary = vec![SOURCES.into(), OTHER.into()];
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let w = withdraw(&ds, &a);
    assert_eq!(w.changed.removed.len(), 1, "{w:?}");
    assert!(accepted(&ds).is_empty());
}

/// A replay answers the first receipt's projection notes and conflicts, and a
/// supersession of a support that a withdrawal already ended reports no change.
#[test]
fn replays_keep_their_metadata_and_ended_supports_are_reported_once() {
    let ds = configured(ConflictMode::AdmitLabelled);
    let yes = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let no = claim_of(&propose(&ds, &occurrence("vendor", Polarity::Negative)));
    let mut o = reviewer();
    o.idempotency_key = Some("k".into());
    o.if_head = Some(ds.store().head_commit().seq);
    let d = [
        decision("y", &yes, Disposition::Accepted, None),
        decision("n", &no, Disposition::Accepted, None),
    ];
    let first = review_as(&ds, &d, &o, all()).unwrap();
    let again = review_as(&ds, &d, &o, all()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.decisions, first.decisions);
    assert_eq!(first.decisions[0].conflicts, vec![no.0.clone()]);
    assert_eq!(
        first.decisions[1].not_projected,
        Some("the claim is not positive")
    );
    withdraw(&ds, &yes);
    let r = review(
        &ds,
        &[decision(
            "y2",
            &yes,
            Disposition::Rejected,
            Some(&first.decisions[0].iri),
        )],
    )
    .unwrap();
    assert!(r.changed.supports_ended.is_empty(), "{:?}", r.changed);
}

/// A contradiction link from a passage of a new source, in the claims graph.
fn contradicting_link(ds: &Dataset, target: &NamedNode) -> NamedNode {
    let text = "Release 1.2 is not affected.";
    let mut batch = EvidenceInput {
        revisions: vec![revision("rebuttal")],
        ..Default::default()
    };
    batch.renditions.push(evidence::RenditionInput {
        version: RecordVersion::V1,
        key: key("text"),
        revision: RecordRef::Local(key("rev")),
        occurrence_key: key("text"),
        conversion: evidence::MethodRef {
            method: iri("urn:method:identity"),
            version: key("1"),
        },
        media_type: "text/plain".into(),
        language: None,
        content: evidence::RenditionContent::NormalizedText(text.into()),
        expected_digest: None,
    });
    batch.passages.push(evidence::PassageInput {
        version: RecordVersion::V1,
        key: key("span"),
        revision: RecordRef::Local(key("rev")),
        rendition: RecordRef::Local(key("text")),
        occurrence_key: key("span"),
        start: 0,
        end: 7,
        text_digest: sha(&text[0..7]),
        quote: None,
    });
    batch.references.push(EvidenceRefInput {
        version: RecordVersion::V1,
        key: key("ref"),
        occurrence_key: key("ref"),
        target: EvidenceRefTarget::Passage {
            revision: RecordRef::Local(key("rev")),
            rendition: RecordRef::Local(key("text")),
            passage: RecordRef::Local(key("span")),
        },
    });
    batch.links.push(EvidenceLinkInput {
        version: RecordVersion::V1,
        key: key("link"),
        occurrence_key: key("link"),
        target: RecordRef::Existing(target.clone()),
        evidence: RecordRef::Local(key("ref")),
        relation: EvidenceRelation::Contradicts,
        attributed_author: iri("urn:importer"),
    });
    let p = propose(ds, &batch);
    p.records
        .iter()
        .find(|r| r.key == "link")
        .unwrap()
        .iri
        .clone()
}

/// A labelled acceptance against a negative claim and a later contradiction link
/// answers its conflicts in one order, first and on replay.
#[test]
fn conflicts_are_answered_in_one_order_on_replay() {
    let ds = configured(ConflictMode::AdmitLabelled);
    let no = claim_of(&propose(&ds, &occurrence("vendor", Polarity::Negative)));
    let yes = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    let link = contradicting_link(&ds, &yes.0);
    let mut o = reviewer();
    o.idempotency_key = Some("k".into());
    o.if_head = Some(ds.store().head_commit().seq);
    let d = [decision("y", &yes, Disposition::Accepted, None)];
    let first = review_as(&ds, &d, &o, all()).unwrap();
    let mut want = vec![no.0.clone(), link];
    want.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    assert_eq!(first.decisions[0].conflicts, want);
    let again = review_as(&ds, &d, &o, all()).unwrap();
    assert!(again.replayed);
    assert_eq!(again.decisions, first.decisions);
}

/// An occurrence asserting `<urn:release> <urn:affectedBy> <object>`.
fn occurrence_with(source_key: &str, object: &str, polarity: Polarity) -> EvidenceInput {
    let mut batch = occurrence(source_key, polarity);
    batch.claims[0].proposition =
        Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri(object));
    batch
}

fn reconcile(
    ds: &Dataset,
    policy: &str,
    version: Option<u32>,
    opts: &ReviewOptions,
) -> sparkles::Result<evidence::ReconciledProjection> {
    evidence::reconcile_projection(ds, ds, policy, version, opts, None, &Control::none())
}

/// Detaching and re-protecting the output graph leaves supports and decisions
/// authenticated: reconciliation rebuilds the output graph strictly from live vouched
/// supports (removing tampered ground and blank-node triples and restoring missing
/// vouched triples), records a fresh projection record superseding the old one, and
/// restores review, withdrawal and policy claim reads.
#[test]
fn reconciling_after_output_detach_rebuilds_from_vouched_supports() {
    let ds = configured(ConflictMode::Refuse);
    let prep = sparkles::access::Preparation::new(Control::none(), 16 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    let c = claim_of(&propose(
        &ds,
        &occurrence_with("s3", "urn:cve3", Polarity::Positive),
    ));
    let d = claim_of(&propose(
        &ds,
        &occurrence_with("s4", "urn:cve4", Polarity::Positive),
    ));
    let e = claim_of(&propose(
        &ds,
        &occurrence_with("s5", "urn:cve5", Polarity::Positive),
    ));

    let r1 = review(
        &ds,
        &[
            decision("a", &a, Disposition::Accepted, None),
            decision("b", &b, Disposition::Accepted, None),
            decision("c", &c, Disposition::Accepted, None),
            decision("d", &d, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    let sup_a = r1.decisions[0].support.clone().unwrap();
    let sup_b = r1.decisions[1].support.clone().unwrap();
    let dec_d = r1.decisions[3].iri.clone();

    // Withdraw C and supersede D with a rejection so only A and B remain live.
    withdraw(&ds, &c);
    review(
        &ds,
        &[decision("d2", &d, Disposition::Rejected, Some(&dec_d))],
    )
    .unwrap();

    // Detach OUTPUT, delete A's triple, inject bogus ground, blank-node subject,
    // blank-node object, triple-term with blank node, CDT literal with blank node,
    // and re-inserted withdrawn C's triple, then re-protect OUTPUT.
    evidence::detach(&ds, OUTPUT).unwrap();
    ds.update(&format!(
        "DELETE DATA {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve1> }} }} ; \
         INSERT DATA {{ GRAPH <{OUTPUT}> {{ \
             <urn:bogus> <urn:p> \"tampered\" . \
             _:b0 <urn:p> \"blank-tampered\" . \
             <urn:bogus> <urn:blankObj> _:b1 . \
             <urn:bogus> <urn:nested> <<( _:b2 <urn:q> <urn:r> )>> . \
             <urn:bogus> <urn:cdt> \"[_:a]\"^^<http://w3id.org/awslabs/neptune/SPARQL-CDTs/List> . \
             <urn:release> <urn:affectedBy> <urn:cve3> \
         }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();

    // Reviews, withdrawals and policy reads all report projection-unavailable.
    assert_eq!(
        code(review(
            &ds,
            &[decision("e", &e, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(
        code(evidence::withdraw(
            &ds,
            &ds,
            &[WithdrawalInput {
                key: "w".into(),
                target: a.0.clone(),
                target_digest: a.1,
            }],
            &reviewer(),
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Head, &prep)
        .unwrap();
    assert_eq!(
        code(evidence::claim_states(
            &view,
            "main",
            std::slice::from_ref(&a.0),
            None,
            &Control::none(),
        )),
        "projection-unavailable"
    );

    // Dry run previews exact rebuild without writing.
    let head = ds.store().head_commit().seq;
    let mut dry_opts = reviewer();
    dry_opts.dry_run = true;
    dry_opts.if_head = Some(head);
    let preview = reconcile(&ds, "main", Some(1), &dry_opts).unwrap();
    assert!(!preview.committed && !preview.replayed);
    assert!(preview.preview.is_some());
    assert_eq!(preview.changed.inserted.len(), 1);
    assert_eq!(preview.changed.removed.len(), 6);
    assert_eq!(ds.store().head_commit().seq, head);

    // Real reconciliation with idempotency key.
    let mut opts = reviewer();
    opts.idempotency_key = Some("rec-output-1".into());
    opts.if_head = Some(head);
    let rec = reconcile(&ds, "main", Some(1), &opts).unwrap();
    assert!(rec.committed && !rec.replayed);
    assert_eq!(rec.policy_version, 1);
    let mut expected_supports = vec![sup_a, sup_b];
    expected_supports.sort_by(|x, y| x.as_str().cmp(y.as_str()));
    assert_eq!(rec.vouched_supports, expected_supports);
    assert_eq!(rec.changed.inserted.len(), 1);
    assert_eq!(rec.changed.removed.len(), 6);
    assert_eq!(
        accepted(&ds),
        vec![
            "<urn:release> <urn:affectedBy> <urn:cve1>".to_string(),
            "<urn:release> <urn:affectedBy> <urn:cve2>".to_string(),
        ]
    );
    // Fresh projection record supersedes the earlier one through prov:wasRevisionOf.
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> a <{EV}Projection> ; <http://www.w3.org/ns/prov#wasRevisionOf> ?prior }} }}",
            rec.projection.as_str()
        ))
        .unwrap()
    );

    // Replay returns the first receipt; different payload conflicts.
    let replay = reconcile(&ds, "main", Some(1), &opts).unwrap();
    assert!(!replay.committed && replay.replayed);
    assert_eq!(replay.commit, rec.commit);
    assert_eq!(replay.projection, rec.projection);
    assert!(replay.changed.inserted.is_empty() && replay.changed.removed.is_empty());

    assert_eq!(
        code(reconcile(&ds, "main", None, &opts)),
        "idempotency-conflict"
    );

    // Subsequent review (at same policy version 1), withdrawal and policy claim reads succeed.
    review(&ds, &[decision("e", &e, Disposition::Accepted, None)]).unwrap();
    assert_eq!(accepted(&ds).len(), 3);
    withdraw(&ds, &a);
    assert_eq!(
        accepted(&ds),
        vec![
            "<urn:release> <urn:affectedBy> <urn:cve2>".to_string(),
            "<urn:release> <urn:affectedBy> <urn:cve5>".to_string(),
        ]
    );
    let view = ds
        .store()
        .evidence_view(&sparkles::history::At::Head, &prep)
        .unwrap();
    let states = evidence::claim_states(
        &view,
        "main",
        &[a.0.clone(), b.0.clone()],
        None,
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        states[0].as_ref().unwrap().state,
        evidence::ClaimState::Withdrawn
    );
    assert_eq!(
        states[1].as_ref().unwrap().state,
        evidence::ClaimState::Accepted
    );
}

/// Detaching and re-protecting decisions or supports (untouched or cleared) stops
/// prior supports from authenticating, so reconciliation empties the output graph,
/// records the new protections, and restores normal reviews and withdrawals.
#[test]
fn reconciling_after_decisions_or_supports_detach_empties_output_and_restores_operations() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    let c = claim_of(&propose(
        &ds,
        &occurrence_with("s3", "urn:cve3", Polarity::Positive),
    ));
    let d = claim_of(&propose(
        &ds,
        &occurrence_with("s4", "urn:cve4", Polarity::Positive),
    ));

    // 1. Detach & re-protect DECISIONS.
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    evidence::detach(&ds, DECISIONS).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    assert_eq!(
        code(review(
            &ds,
            &[decision("b", &b, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    let rec1 = reconcile(&ds, "main", None, &reviewer()).unwrap();
    assert!(rec1.vouched_supports.is_empty());
    assert_eq!(rec1.changed.removed.len(), 1);
    assert!(accepted(&ds).is_empty());

    // Normal review & withdrawal work again.
    review(&ds, &[decision("b", &b, Disposition::Accepted, None)]).unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve2>".to_string()]
    );

    // 2. Detach & re-protect SUPPORTS (leaving old triples intact).
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    assert_eq!(
        code(review(
            &ds,
            &[decision("c", &c, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    let rec2 = reconcile(&ds, "main", None, &reviewer()).unwrap();
    assert!(rec2.vouched_supports.is_empty());
    assert_eq!(rec2.changed.removed.len(), 1);
    assert!(accepted(&ds).is_empty());
    review(&ds, &[decision("c", &c, Disposition::Accepted, None)]).unwrap();
    assert_eq!(accepted(&ds).len(), 1);

    // 3. Detach, CLEAR, and re-protect SUPPORTS.
    evidence::detach(&ds, SUPPORTS).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    assert_eq!(
        code(review(
            &ds,
            &[decision("d", &d, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    let rec3 = reconcile(&ds, "main", None, &reviewer()).unwrap();
    assert!(rec3.vouched_supports.is_empty());
    assert_eq!(rec3.changed.removed.len(), 1);
    assert!(accepted(&ds).is_empty());

    // Reconciling an already-healthy projection reuses its active projection record.
    let proj_count_before = count(&ds, "Projection", SUPPORTS);
    let rec_noop = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert_eq!(rec_noop.projection, rec3.projection);
    assert_eq!(count(&ds, "Projection", SUPPORTS), proj_count_before);
    assert!(rec_noop.changed.inserted.is_empty() && rec_noop.changed.removed.is_empty());

    // Subsequent review & withdrawal succeed.
    review(&ds, &[decision("d", &d, Disposition::Accepted, None)]).unwrap();
    assert_eq!(accepted(&ds).len(), 1);
    withdraw(&ds, &d);
    assert!(accepted(&ds).is_empty());

    // Written reconciled projection records conform to profile shapes.
    #[cfg(feature = "shacl")]
    {
        use oxrdfio::RdfFormat;
        let copy = Dataset::memory();
        let mut nt = String::new();
        for g in [SOURCES, CLAIMS, DECISIONS, SUPPORTS] {
            for t in ds
                .construct(&format!(
                    "CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{g}> {{ ?s ?p ?o }} }}"
                ))
                .unwrap()
                .iter()
            {
                nt.push_str(&format!("{t} .\n"));
            }
        }
        copy.load_str(&nt, RdfFormat::NTriples).unwrap();
        let shapes = sparkles_shacl::Shapes::parse(
            evidence::profile(1).unwrap().shapes,
            RdfFormat::Turtle,
            None,
        )
        .unwrap();
        let report = copy
            .validation()
            .shacl(&shapes, &Default::default())
            .unwrap();
        assert!(report.conforms, "{report:?}");
    }
}

/// Reconciliation persists across persistent dataset reopen, isolates branches and
/// sibling policies sharing a supports graph, and enforces preconditions and grants.
#[test]
fn reconciliation_refusals_branches_and_restart_durability() {
    let dir = tempfile::tempdir().unwrap();
    let a = {
        let ds = Dataset::open(dir.path()).unwrap();
        evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
        let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
        review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
        evidence::detach(&ds, OUTPUT).unwrap();
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{OUTPUT}> {{ <urn:stray> <urn:p> 1 }} }}"
        ))
        .unwrap();
        // While still detached, reconciliation refuses with unsupported-profile.
        assert_eq!(
            code(reconcile(&ds, "main", None, &reviewer())),
            "unsupported-profile"
        );
        evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
        let rec = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
        assert_eq!(rec.vouched_supports.len(), 1);
        assert_eq!(rec.changed.removed.len(), 1);
        a
    };
    // Reopen from disk and verify durability and subsequent operations.
    {
        let ds = Dataset::open(dir.path()).unwrap();
        assert_eq!(accepted(&ds), affected_line());
        let b = claim_of(&propose(
            &ds,
            &occurrence_with("mirror", "urn:cve2", Polarity::Positive),
        ));
        review(&ds, &[decision("b", &b, Disposition::Accepted, None)]).unwrap();
        assert_eq!(accepted(&ds).len(), 2);
        withdraw(&ds, &a);
        assert_eq!(
            accepted(&ds),
            vec!["<urn:release> <urn:affectedBy> <urn:cve2>".to_string()]
        );
    }

    // Shared SUPPORTS graph across two policies: reconciling one leaves the other unavailable.
    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    let output2 = "urn:ev:accepted2";
    s.projections.push(output2.into());
    s.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: SUPPORTS.into(),
            output: output2.into(),
        },
    );
    s.identity.push("urn:ev:id".into());
    s.identity.push("urn:ev:maps".into());
    s.projections.push("urn:ev:map".into());
    s.matching_policies.insert(
        "people".into(),
        evidence::MatchingPolicy {
            version: 1,
            boundary: vec!["urn:ev:id".into()],
            decisions: DECISIONS.into(),
            maps: "urn:ev:maps".into(),
            output: "urn:ev:map".into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let a = claim_of(&propose(&ds, &occurrence("advisory", Polarity::Positive)));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    let mut o = reviewer();
    o.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "second",
        1,
        &[decision("a2", &a, Disposition::Accepted, None)],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    // Create branch before detaching SUPPORTS.
    ds.store()
        .create_branch("dev", &Default::default())
        .unwrap();
    let dev = ds.branch("dev").unwrap();

    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    // Refusals: unknown policy, matching policy, wrong version, stale ifHead, missing grants, masked access.
    assert!(matches!(
        reconcile(&ds, "missing", None, &reviewer()),
        Err(sparkles::Error::Invalid(_))
    ));
    assert!(matches!(
        reconcile(&ds, "people", None, &reviewer()),
        Err(sparkles::Error::Invalid(_))
    ));
    assert_eq!(
        code(reconcile(&ds, "main", Some(2), &reviewer())),
        "policy-changed"
    );
    let mut stale = reviewer();
    stale.if_head = Some(0);
    assert_eq!(code(reconcile(&ds, "main", None, &stale)), "head-changed");
    let mut no_write = reviewer();
    no_write.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([SUPPORTS], &[])),
    )));
    assert!(matches!(
        reconcile(&ds, "main", None, &no_write),
        Err(sparkles::Error::NotPermitted(_))
    ));
    let no_read = Some(Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([SUPPORTS, OUTPUT], &[])),
        Graphs::All,
    )));
    assert!(matches!(
        evidence::reconcile_projection(
            &ds,
            &ds,
            "main",
            None,
            &reviewer(),
            no_read,
            &Control::none()
        ),
        Err(sparkles::Error::NotPermitted(_))
    ));
    {
        use sparkles::access::triples::{Caller, Limits, Protection, Rule, TripleRules};
        let masked = Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            TripleRules {
                rules: vec![Rule {
                    protection: Arc::new(Protection {
                        name: "propositions".into(),
                        predicates: Some(vec![
                            "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies".into(),
                        ]),
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
        )));
        assert!(matches!(
            evidence::reconcile_projection(
                &ds,
                &ds,
                "main",
                None,
                &reviewer(),
                masked,
                &Control::none()
            ),
            Err(sparkles::Error::NotPermitted(_))
        ));
    }

    // Reconciling "main" on main restores "main" while "second" and branch "dev" stay unavailable.
    reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(accepted(&ds).is_empty());
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("m", "urn:cve2", Polarity::Positive),
    ));
    review(&ds, &[decision("b", &b, Disposition::Accepted, None)]).unwrap();
    let mut o2 = reviewer();
    o2.if_head = Some(ds.store().head_commit().seq);
    assert_eq!(
        code(evidence::review(
            None,
            &ds,
            &ds,
            "second",
            1,
            &[decision("b2", &b, Disposition::Accepted, None)],
            &o2,
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );
    // Branch dev still unavailable until reconciled on dev.
    let mut odev = reviewer();
    odev.if_head = Some(dev.store().head_commit().seq);
    assert_eq!(
        code(evidence::review(
            None,
            &ds,
            &dev,
            "main",
            1,
            &[decision("a3", &a, Disposition::Rejected, None)],
            &odev,
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );
    evidence::reconcile_projection(
        &ds,
        &dev,
        "main",
        Some(1),
        &reviewer(),
        None,
        &Control::none(),
    )
    .unwrap();
    assert!(accepted(&dev).is_empty());
    // Changing conflict mode without bumping version under unchanged protections refuses.
    let mut changed = s.clone();
    changed.policies.get_mut("main").unwrap().conflicts = ConflictMode::AdmitLabelled;
    evidence::configure(&ds, &changed, &Control::none()).unwrap();
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
}

/// Full compaction publishes a fresh generation at the same commit, remapping delta
/// and base physical IDs. Reconciliation fences on `generation.uid` before applying
/// physical deletes, refusing with `head-changed` (both dry run and live) rather than
/// dropping stray quads or touching unrelated quads, and succeeds cleanly on retry.
#[test]
fn reconciling_refuses_when_compaction_runs_between_scan_and_publish() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    // Compact once so A's triple lives in base vocab, then inject stray delta quads.
    ds.compact().unwrap();

    evidence::detach(&ds, OUTPUT).unwrap();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{OUTPUT}> {{ \
             <urn:stray> <urn:p> \"delta-stray\" . \
             _:b0 <urn:p> \"blank-stray\" \
         }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();

    let head_before = ds.store().head_commit().seq;
    let gen_before = ds.store().snapshot().generation.uid;
    let proj_count_before = count(&ds, "Projection", SUPPORTS);

    // 1. Dry run: compact between scan and publish while keeping the branch commit unchanged.
    let ds_hook = ds.clone();
    let compacted = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compacted_hook = compacted.clone();
    ds.store().set_failpoint(
        "evidence-reconcile-before-publish",
        Some(Arc::new(move |_| {
            if !compacted_hook.swap(true, std::sync::atomic::Ordering::SeqCst) {
                ds_hook.store().compact().unwrap();
            }
        })),
    );
    let mut dry_opts = reviewer();
    dry_opts.dry_run = true;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &dry_opts)),
        "head-changed"
    );
    ds.store()
        .set_failpoint("evidence-reconcile-before-publish", None);

    assert!(compacted.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(ds.store().head_commit().seq, head_before);
    assert_ne!(ds.store().snapshot().generation.uid, gen_before);
    assert_eq!(count(&ds, "Projection", SUPPORTS), proj_count_before);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ <urn:stray> <urn:p> \"delta-stray\" }} }}"
        ))
        .unwrap()
    );
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve1> }} }}"
        ))
        .unwrap()
    );

    // 2. Live run: add an unrelated delta triple so a second compaction advances
    // generation.uid again, then trigger compaction between scan and live publish.
    ds.update("INSERT DATA { GRAPH <urn:unrelated> { <urn:u> <urn:p> \"delta\" } }")
        .unwrap();
    let head_before_live = ds.store().head_commit().seq;
    let gen_before_live = ds.store().snapshot().generation.uid;
    let ds_hook = ds.clone();
    let compacted_live = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let compacted_live_hook = compacted_live.clone();
    ds.store().set_failpoint(
        "evidence-reconcile-before-publish",
        Some(Arc::new(move |_| {
            if !compacted_live_hook.swap(true, std::sync::atomic::Ordering::SeqCst) {
                ds_hook.store().compact().unwrap();
            }
        })),
    );
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "head-changed"
    );
    ds.store()
        .set_failpoint("evidence-reconcile-before-publish", None);

    assert!(compacted_live.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(ds.store().head_commit().seq, head_before_live);
    assert_ne!(ds.store().snapshot().generation.uid, gen_before_live);
    assert_eq!(count(&ds, "Projection", SUPPORTS), proj_count_before);
    // Both stray quads and A's retained triple are still untouched.
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ <urn:stray> <urn:p> \"delta-stray\" }} }}"
        ))
        .unwrap()
    );
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ <urn:release> <urn:affectedBy> <urn:cve1> }} }}"
        ))
        .unwrap()
    );

    // Retrying on the compacted generation succeeds cleanly and removes both stray quads.
    let rec = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec.committed);
    assert_eq!(rec.changed.removed.len(), 2);
    assert!(rec.changed.inserted.is_empty());
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );
}

/// Swapping the continuously protected outputs (or `(decisions, output)` pairs) of two
/// review policies without raising their policy versions is refused as
/// `projection-unavailable`, leaving both outputs and the branch head untouched.
#[test]
fn same_version_output_swap_between_policies_is_refused() {
    let ds = Dataset::memory();
    let decisions2 = "urn:ev:decisions2";
    let output2 = "urn:ev:accepted2";
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: SUPPORTS.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    let mut o = reviewer();
    o.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "second",
        1,
        &[decision("b", &b, Disposition::Accepted, None)],
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // Case 1: Swap outputs between "main" and "second" at version 1 while sharing DECISIONS.
    let mut swapped = s.clone();
    swapped.policies.get_mut("main").unwrap().output = output2.into();
    swapped.policies.get_mut("second").unwrap().output = OUTPUT.into();
    evidence::configure(&ds, &swapped, &Control::none()).unwrap();

    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(reconcile(&ds, "second", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve2> }} }}"
        ))
        .unwrap()
    );

    // Case 2: Separate decisions graphs (DECISIONS vs decisions2) sharing SUPPORTS,
    // swapping `(decisions, output)` at version 1 under unchanged protections must ALSO
    // be refused rather than letting `main` wipe `second`'s output graph.
    let ds2 = Dataset::memory();
    let mut s_sep = settings(ConflictMode::Refuse);
    s_sep.decisions.push(decisions2.into());
    s_sep.projections.push(output2.into());
    s_sep.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: decisions2.into(),
            supports: SUPPORTS.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds2, &s_sep, &Control::none()).unwrap();

    let a2 = claim_of(&propose(
        &ds2,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b2 = claim_of(&propose(
        &ds2,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(&ds2, &[decision("a", &a2, Disposition::Accepted, None)]).unwrap();
    let mut o2 = reviewer();
    o2.if_head = Some(ds2.store().head_commit().seq);
    evidence::review(
        None,
        &ds2,
        &ds2,
        "second",
        1,
        &[decision("b", &b2, Disposition::Accepted, None)],
        &o2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    let mut swapped_sep = s_sep.clone();
    swapped_sep.policies.get_mut("main").unwrap().decisions = decisions2.into();
    swapped_sep.policies.get_mut("main").unwrap().output = output2.into();
    swapped_sep.policies.get_mut("second").unwrap().decisions = DECISIONS.into();
    swapped_sep.policies.get_mut("second").unwrap().output = OUTPUT.into();
    evidence::configure(&ds2, &swapped_sep, &Control::none()).unwrap();

    let head2 = ds2.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds2, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(reconcile(&ds2, "second", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds2.store().head_commit().seq, head2);
    assert_eq!(
        accepted(&ds2),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );
    assert!(
        ds2.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve2> }} }}"
        ))
        .unwrap()
    );

    // Case 3: Separate supports graphs (SUPPORTS vs supports2) swapping continuously
    // protected `(supports, output)` pairs at version 1 must also be refused rather than
    // letting `main` see an empty supports graph and wipe `second`'s output.
    let supports2 = "https://example.org/ev/supports2";
    let ds3 = Dataset::memory();
    let mut s_sup = settings(ConflictMode::Refuse);
    s_sup.supports.push(supports2.into());
    s_sup.projections.push(output2.into());
    s_sup.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds3, &s_sup, &Control::none()).unwrap();

    let a3 = claim_of(&propose(
        &ds3,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b3 = claim_of(&propose(
        &ds3,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(&ds3, &[decision("a", &a3, Disposition::Accepted, None)]).unwrap();
    let mut o3 = reviewer();
    o3.if_head = Some(ds3.store().head_commit().seq);
    evidence::review(
        None,
        &ds3,
        &ds3,
        "second",
        1,
        &[decision("b", &b3, Disposition::Accepted, None)],
        &o3,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    let mut swapped_sup = s_sup.clone();
    swapped_sup.policies.get_mut("main").unwrap().supports = supports2.into();
    swapped_sup.policies.get_mut("main").unwrap().output = output2.into();
    swapped_sup.policies.get_mut("second").unwrap().supports = SUPPORTS.into();
    swapped_sup.policies.get_mut("second").unwrap().output = OUTPUT.into();
    evidence::configure(&ds3, &swapped_sup, &Control::none()).unwrap();

    let head3 = ds3.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds3, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(reconcile(&ds3, "second", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds3.store().head_commit().seq, head3);
    assert_eq!(
        accepted(&ds3),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );
    assert!(
        ds3.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve2> }} }}"
        ))
        .unwrap()
    );

    // Case 4: Asymmetric `(supports, output)` swap where `main` had no prior projection
    // while `second` still holds an active continuously protected v1 projection on `output2`.
    let ds4 = Dataset::memory();
    evidence::configure(&ds4, &s_sup, &Control::none()).unwrap();
    let b4 = claim_of(&propose(
        &ds4,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    let mut o4 = reviewer();
    o4.if_head = Some(ds4.store().head_commit().seq);
    evidence::review(
        None,
        &ds4,
        &ds4,
        "second",
        1,
        &[decision("b", &b4, Disposition::Accepted, None)],
        &o4,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    evidence::configure(&ds4, &swapped_sup, &Control::none()).unwrap();
    let head4 = ds4.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds4, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(reconcile(&ds4, "second", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds4.store().head_commit().seq, head4);
    assert!(
        ds4.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve2> }} }}"
        ))
        .unwrap()
    );
}

/// Control-character literals expand up to 6x in N-Triples and 7x in JSON; reconciliation
/// accounts for escaped formatted output before committing and refuses with
/// `incomplete-evidence` rather than exceeding the shared 64 MiB allowance.
#[test]
fn reconciling_refuses_escaped_literals_exceeding_memory_allowance() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();

    evidence::detach(&ds, OUTPUT).unwrap();
    // 150 stray triples * 96 KiB of `\u0000` = ~14.4 MiB raw lexical bytes,
    // which expand to ~86 MiB in N-Triples and ~101 MiB in JSON (> 64 MiB allowance).
    let escaped = "\\u0000".repeat(96 * 1024);
    let mut nq = String::with_capacity(150 * (escaped.len() + 64));
    for i in 0..150 {
        nq.push_str(&format!(
            "<urn:ctrl:{i}> <urn:p> \"{escaped}\" <{OUTPUT}> .\n"
        ));
    }
    ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();

    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "incomplete-evidence"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

/// Output staging charges per-change graph-name copies and retained triples under the
/// shared 64 MiB allowance, refusing with `incomplete-evidence` before staging a write
/// when a long output IRI with many stray triples would exceed the cap.
#[test]
fn reconciling_refuses_with_incomplete_evidence_under_long_output_iri_budget() {
    let ds = Dataset::memory();
    let long_output = format!("urn:ev:{}", "a".repeat(3900));
    let mut s = settings(ConflictMode::Refuse);
    s.projections = vec![long_output.clone()];
    s.policies.get_mut("main").unwrap().output = long_output.clone();
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();

    evidence::detach(&ds, &long_output).unwrap();
    // Insert 10,000 stray triples: each removal carries ~8 KiB of graph-name copies
    // plus triple storage, exceeding the 64 MiB allowance before staging.
    let mut nq = String::with_capacity(10_000 * (long_output.len() + 48));
    for i in 0..10_000 {
        nq.push_str(&format!("<urn:s:{i}> <urn:p> \"x\" <{long_output}> .\n"));
    }
    ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "incomplete-evidence"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

/// Keyed replay of a healthy-projection reuse records `prov:used` on the operation and
/// returns the original commit, projection IRI, policy version and output graph even
/// after a later output rotation/reconciliation and after a policy version + output change.
#[test]
fn keyed_replay_of_reused_projection_preserves_original_receipt() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();

    // First keyed reconciliation reuses the healthy v1 projection (omitting policyVersion).
    let mut opts = reviewer();
    opts.idempotency_key = Some("noop-replay-key".into());
    let first = reconcile(&ds, "main", None, &opts).unwrap();
    assert!(first.committed && !first.replayed);
    assert_eq!(first.policy_version, 1);
    assert_eq!(first.output.as_str(), OUTPUT);

    // Rotate OUTPUT protection and reconcile again (creating a superseding v1 projection).
    evidence::detach(&ds, OUTPUT).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    let second = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert_ne!(second.projection, first.projection);

    // Replaying the original key still returns the original v1 projection and commit.
    let replay_after_rotate = reconcile(&ds, "main", None, &opts).unwrap();
    assert!(!replay_after_rotate.committed && replay_after_rotate.replayed);
    assert_eq!(replay_after_rotate.commit, first.commit);
    assert_eq!(replay_after_rotate.projection, first.projection);
    assert_eq!(replay_after_rotate.policy_version, 1);
    assert_eq!(replay_after_rotate.output.as_str(), OUTPUT);

    // Raise policy to version 2 with a new output graph and publish a v2 projection.
    evidence::detach(&ds, OUTPUT).unwrap();
    let output2 = "urn:ev:accepted2";
    let mut s2 = settings(ConflictMode::Refuse);
    s2.projections = vec![output2.into()];
    {
        let p = s2.policies.get_mut("main").unwrap();
        p.version = 2;
        p.output = output2.into();
    }
    evidence::configure(&ds, &s2, &Control::none()).unwrap();
    let v2_rec = reconcile(&ds, "main", Some(2), &reviewer()).unwrap();
    assert_eq!(v2_rec.policy_version, 2);
    assert_eq!(v2_rec.output.as_str(), output2);

    // Replaying the original key (omitting policyVersion) STILL returns the first v1 receipt.
    let replay_after_v2 = reconcile(&ds, "main", None, &opts).unwrap();
    assert!(!replay_after_v2.committed && replay_after_v2.replayed);
    assert_eq!(replay_after_v2.commit, first.commit);
    assert_eq!(replay_after_v2.projection, first.projection);
    assert_eq!(replay_after_v2.policy_version, 1);
    assert_eq!(replay_after_v2.output.as_str(), OUTPUT);
}

/// When a policy moves to a new supports graph while its former supports graph remains
/// configured and protected, a caller whose read grants no longer include the former
/// supports graph is refused on keyed replay (never leaking the former graph's
/// projection IRI, version or output), while keyed conflict refusals stay intact.
#[test]
fn keyed_replay_refuses_metadata_from_unreadable_former_supports_graph() {
    let ds = Dataset::memory();
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";
    let mut s1 = settings(ConflictMode::Refuse);
    s1.supports.push(supports2.into());
    evidence::configure(&ds, &s1, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();

    // First keyed reconciliation records its operation and `prov:used` in SUPPORTS.
    let mut opts = reviewer();
    opts.idempotency_key = Some("moved-supports-replay-key".into());
    let first = reconcile(&ds, "main", None, &opts).unwrap();
    assert!(first.committed && !first.replayed);
    assert_eq!(first.policy_version, 1);
    assert_eq!(first.output.as_str(), OUTPUT);

    // Move policy "main" to version 2 with supports2 and output2 while keeping SUPPORTS
    // configured and protected.
    evidence::detach(&ds, OUTPUT).unwrap();
    let mut s2 = s1.clone();
    s2.projections = vec![output2.into()];
    {
        let p = s2.policies.get_mut("main").unwrap();
        p.version = 2;
        p.supports = supports2.into();
        p.output = output2.into();
    }
    evidence::configure(&ds, &s2, &Control::none()).unwrap();

    // Narrow grants: caller can read/write the new policy graphs + all configured
    // proposal graphs, NOT the former supports graph SUPPORTS.
    let narrowed = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, OTHER, DECISIONS, supports2, output2],
            &[],
        )),
        Graphs::Only(GraphRule::new([supports2, output2], &[])),
    ));
    let mut narrow_opts = opts.clone();
    narrow_opts.write = Some(narrowed.clone());

    // Different payload with the same key still returns idempotency-conflict.
    assert_eq!(
        code(evidence::reconcile_projection(
            &ds,
            &ds,
            "main",
            Some(2),
            &narrow_opts,
            Some(narrowed.clone()),
            &Control::none(),
        )),
        "idempotency-conflict"
    );

    // Replaying the same payload under narrowed grants refuses with NotPermitted instead
    // of disclosing SUPPORTS's projection IRI, version or output.
    let err = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        None,
        &narrow_opts,
        Some(narrowed),
        &Control::none(),
    )
    .unwrap_err();
    assert!(
        matches!(err, sparkles::Error::NotPermitted(_)),
        "expected NotPermitted, got {err}"
    );

    // With read access to the former supports graph retained, replay succeeds and returns
    // the original v1 receipt.
    let full = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [
                SOURCES, CLAIMS, OTHER, DECISIONS, SUPPORTS, supports2, output2,
            ],
            &[],
        )),
        Graphs::Only(GraphRule::new([supports2, output2], &[])),
    ));
    let mut full_opts = opts.clone();
    full_opts.write = Some(full.clone());
    let replayed = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        None,
        &full_opts,
        Some(full),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed.committed && replayed.replayed);
    assert_eq!(replayed.commit, first.commit);
    assert_eq!(replayed.projection, first.projection);
    assert_eq!(replayed.policy_version, 1);
    assert_eq!(replayed.output.as_str(), OUTPUT);
}

/// Including the operation in `reconciled_projection_iri` prevents self-collision and
/// self-supersession when reconciling again under the same `(version, nonces)`, while
/// preserving withdrawal finality across proposal-graph protection rotations:
/// (1) detaching and re-protecting a proposal graph invalidates prior claims and
/// withdrawals there, so withdrawn claims never return and unauthenticated claims are
/// dropped, minting a distinct `P2` superseding `P1` under unchanged `(decisions,
/// supports, output)` nonces; (2) bumping `v1 -> v2` and configuring back to `v1` under
/// unchanged nonces.
#[test]
fn reconciling_again_at_same_version_and_nonces_avoids_self_collision() {
    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(
        &ds,
        &[
            decision("a", &a, Disposition::Accepted, None),
            decision("b", &b, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    // Withdraw B before detaching OUTPUT; B must never return even if CLAIMS later rotates.
    withdraw(&ds, &b);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Reconcile once after an output detach so an active reconciled v1 projection P1 exists.
    evidence::detach(&ds, OUTPUT).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    let p1 = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Detach and re-protect CLAIMS (the proposal graph). Neither A nor B authenticates
    // under the new proposal protection, so B stays withdrawn (finality preserved), A is
    // removed because absence of withdrawal can no longer be established, and reconciliation
    // mints a distinct P2 superseding P1 (never P2 wasRevisionOf P2) under unchanged
    // (decisions, supports, output) nonces.
    evidence::detach(&ds, CLAIMS).unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();

    let p2 = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(p2.committed);
    assert_ne!(p2.projection, p1.projection);
    assert!(p2.changed.inserted.is_empty());
    assert_eq!(p2.changed.removed.len(), 1);
    assert!(accepted(&ds).is_empty());
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            p2.projection.as_str(),
            p1.projection.as_str()
        ))
        .unwrap()
    );
    assert!(
        !ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            p2.projection.as_str(),
            p2.projection.as_str()
        ))
        .unwrap()
    );

    // Trigger 2: bump v1 -> v2 and reconcile, then configure back to v1 under unchanged
    // nonces and reconcile again without colliding with P1 or P2.
    let mut s2 = settings(ConflictMode::Refuse);
    s2.policies.get_mut("main").unwrap().version = 2;
    evidence::configure(&ds, &s2, &Control::none()).unwrap();
    let p_v2 = reconcile(&ds, "main", Some(2), &reviewer()).unwrap();
    assert_ne!(p_v2.projection, p2.projection);

    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();
    let p_v1_again = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(p_v1_again.committed);
    assert_ne!(p_v1_again.projection, p1.projection);
    assert_ne!(p_v1_again.projection, p2.projection);
    assert_ne!(p_v1_again.projection, p_v2.projection);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            p_v1_again.projection.as_str(),
            p_v2.projection.as_str()
        ))
        .unwrap()
    );
    assert!(
        !ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            p_v1_again.projection.as_str(),
            p_v1_again.projection.as_str()
        ))
        .unwrap()
    );
}

/// Ordinary withdrawals must apply the same claim-authentication liveness rule as
/// reconciliation: if claim A (in CLAIMS) was withdrawn, CLAIMS was then detached and
/// re-protected, reconciliation kept the shared triple via claim B (in OTHER), and
/// claim B is later withdrawn, ordinary ownership must NOT treat A's dead support as
/// live and must remove the triple from OUTPUT.
#[test]
fn withdrawing_remaining_claim_after_proposal_rotation_and_reconcile_removes_triple() {
    let ds = configured(ConflictMode::Refuse);
    let s = settings(ConflictMode::Refuse);

    // Propose two distinct occurrences for the same proposition in CLAIMS and OTHER.
    let mut o1 = ProposeOptions::new(iri("urn:principal:importer"));
    o1.proposal_graph = Some(iri(CLAIMS));
    let p1 = evidence::propose(
        None,
        &ds,
        &ds,
        &occurrence_with("occ1", "urn:cve1", Polarity::Positive),
        &o1,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let c1 = claim_of(&p1);

    let mut o2 = ProposeOptions::new(iri("urn:principal:importer"));
    o2.proposal_graph = Some(iri(OTHER));
    let p2 = evidence::propose(
        None,
        &ds,
        &ds,
        &occurrence_with("occ2", "urn:cve1", Polarity::Positive),
        &o2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let c2 = claim_of(&p2);

    review(
        &ds,
        &[
            decision("a1", &c1, Disposition::Accepted, None),
            decision("a2", &c2, Disposition::Accepted, None),
        ],
    )
    .unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Withdraw c1; c2 still vouches for the triple.
    evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w1".into(),
            target: c1.0.clone(),
            target_digest: c1.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Rotate only CLAIMS (where c1 and its withdrawal live) and reconcile.
    evidence::detach(&ds, CLAIMS).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let rec = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert_eq!(rec.vouched_supports.len(), 1);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Now withdraw c2 via ordinary withdraw: c1's support must NOT keep the triple alive.
    let w2 = evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w2".into(),
            target: c2.0.clone(),
            target_digest: c2.1,
        }],
        &reviewer(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(w2.committed);
    assert_eq!(w2.changed.removed.len(), 1);
    assert!(accepted(&ds).is_empty());
}

/// Duplicate live supports owning the same projected triple both appear in
/// `vouched_supports` while inserting the triple once, and cancellation stops
/// reconciliation cleanly (both before acquiring the writer and mid-staging inside
/// the writer's physical-delete loop, rolling back staged deletions).
#[test]
fn reconciling_handles_duplicate_live_supports_and_cancellation() {
    let ds = configured(ConflictMode::Refuse);
    let a1 = claim_of(&propose(
        &ds,
        &occurrence_with("dup1", "urn:cve1", Polarity::Positive),
    ));
    let a2 = claim_of(&propose(
        &ds,
        &occurrence_with("dup2", "urn:cve1", Polarity::Positive),
    ));
    review(
        &ds,
        &[
            decision("d1", &a1, Disposition::Accepted, None),
            decision("d2", &a2, Disposition::Accepted, None),
        ],
    )
    .unwrap();

    evidence::detach(&ds, OUTPUT).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }} ; \
         INSERT DATA {{ GRAPH <{OUTPUT}> {{ \
             <urn:stray1> <urn:p> 1 . \
             <urn:stray2> <urn:p> 2 \
         }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &settings(ConflictMode::Refuse), &Control::none()).unwrap();

    // Pre-cancelled control refuses immediately.
    let pre = Control::none();
    pre.cancel.cancel();
    let head = ds.store().head_commit().seq;
    let proj_count = count(&ds, "Projection", SUPPORTS);
    assert!(matches!(
        evidence::reconcile_projection(&ds, &ds, "main", Some(1), &reviewer(), None, &pre),
        Err(sparkles::Error::Cancelled)
    ));
    assert_eq!(ds.store().head_commit().seq, head);

    // Cancellation arriving inside the writer's physical-delete loop after staging the
    // first deletion aborts the loop before reaching the second physical delete and rolls
    // back the staged deletion.
    let mid = Control::none();
    let cancel_hook = mid.cancel.clone();
    let delete_steps = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let steps_hook = delete_steps.clone();
    ds.store().set_failpoint(
        "evidence-reconcile-after-delete",
        Some(Arc::new(move |_| {
            let n = steps_hook.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            if n == 1 {
                cancel_hook.cancel();
            }
        })),
    );
    assert!(matches!(
        evidence::reconcile_projection(&ds, &ds, "main", Some(1), &reviewer(), None, &mid),
        Err(sparkles::Error::Cancelled)
    ));
    ds.store()
        .set_failpoint("evidence-reconcile-after-delete", None);
    assert_eq!(
        delete_steps.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "delete loop must stop before staging the second physical deletion"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(count(&ds, "Projection", SUPPORTS), proj_count);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ <urn:stray1> <urn:p> 1 . <urn:stray2> <urn:p> 2 }} }}"
        ))
        .unwrap()
    );

    // Normal reconciliation reports both live supports, removes both stray quads, and
    // inserts the shared triple once.
    let rec = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert_eq!(rec.vouched_supports.len(), 2);
    assert_eq!(rec.changed.inserted.len(), 1);
    assert_eq!(rec.changed.removed.len(), 2);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );
}

/// Moving a same-version policy's supports graph from A to an empty graph B while
/// keeping A continuously protected under another managed role (such as `decisions`)
/// retains A's protection nonce and must still be refused as `projection-unavailable`
/// without deleting the policy's accepted triples or advancing the branch head.
#[test]
fn same_version_supports_role_change_retaining_protection_is_refused() {
    use sparkles::managed::OwnerKind;

    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    let nonce_before = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS protected");

    // Move `main.supports` from SUPPORTS to a fresh empty graph `supports2` at version 1,
    // removing SUPPORTS from `settings.supports` while keeping SUPPORTS as an extra
    // decisions graph so its protection nonce is retained.
    let supports2 = "urn:ev:supports2";
    let mut moved = settings(ConflictMode::Refuse);
    moved.decisions.push(SUPPORTS.into());
    moved.supports = vec![supports2.into()];
    moved.policies.get_mut("main").unwrap().supports = supports2.into();
    evidence::configure(&ds, &moved, &Control::none()).unwrap();

    let nonce_after = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS still protected");
    assert_eq!(
        nonce_after, nonce_before,
        "role change within managed graphs must preserve SUPPORTS protection nonce"
    );

    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(review(
            &ds,
            &[decision("b", &b, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Asymmetric rival takeover with a role change: `second` (v1) owns `(supports2, output2)`;
    // `main` (v1, no prior projection) takes `(SUPPORTS, output2)` while `supports2` moves
    // from `supports` to `decisions` (retaining its nonce) and `second` moves to `supports3`.
    let supports3 = "urn:ev:supports3";
    let output2 = "urn:ev:accepted2";
    let ds2 = Dataset::memory();
    let mut s2 = settings(ConflictMode::Refuse);
    s2.supports.push(supports2.into());
    s2.projections.push(output2.into());
    s2.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds2, &s2, &Control::none()).unwrap();
    let c = claim_of(&propose(
        &ds2,
        &occurrence_with("s3", "urn:cve3", Polarity::Positive),
    ));
    let mut o2 = reviewer();
    o2.if_head = Some(ds2.store().head_commit().seq);
    evidence::review(
        None,
        &ds2,
        &ds2,
        "second",
        1,
        &[decision("c", &c, Disposition::Accepted, None)],
        &o2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let s2_nonce_before = ds2
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(supports2)
        .copied()
        .expect("supports2 protected");

    let mut takeover = s2.clone();
    takeover.decisions.push(supports2.into());
    takeover.supports = vec![SUPPORTS.into(), supports3.into()];
    takeover.policies.get_mut("main").unwrap().output = output2.into();
    takeover.policies.get_mut("second").unwrap().supports = supports3.into();
    takeover.policies.get_mut("second").unwrap().output = OUTPUT.into();
    evidence::configure(&ds2, &takeover, &Control::none()).unwrap();
    let s2_nonce_after = ds2
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(supports2)
        .copied()
        .expect("supports2 still protected");
    assert_eq!(s2_nonce_after, s2_nonce_before);

    let head2 = ds2.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds2, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds2.store().head_commit().seq, head2);
    assert!(
        ds2.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve3> }} }}"
        ))
        .unwrap()
    );
}

/// Ordinary review authorizes only the requested policy's own graphs and must not fail
/// with `incomplete-evidence` or disclose content from an unrelated, unreadable supports
/// graph even when that hidden graph exceeds `MAX_EVIDENCE` `ev:Projection` records,
/// while still refusing a real same-version ownership conflict on the policy's output.
#[test]
fn ordinary_review_ignores_unrelated_unreadable_supports_graph() {
    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";
    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), OTHER.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));

    // Caller may read/write ONLY `main`'s graphs, not `other`'s `supports2`/`output2`.
    let main_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::Only(GraphRule::new([DECISIONS, SUPPORTS, OUTPUT], &[])),
    ));

    let review_restricted = |ds: &Dataset, key_name: &str, target: &(NamedNode, Sha256Digest)| {
        let mut opts = reviewer();
        opts.if_head = Some(ds.store().head_commit().seq);
        opts.write = Some(main_only.clone());
        let access = main_only.clone();
        evidence::review(
            None,
            ds,
            ds,
            "main",
            1,
            &[decision(key_name, target, Disposition::Accepted, None)],
            &opts,
            &mut |_, _| Some(access.clone()),
            &Control::none(),
        )
    };

    // Baseline before hidden records exist in `supports2`: review succeeds.
    let first = review_restricted(&ds, "a", &a).unwrap();
    assert!(first.committed);
    assert!(first.decisions[0].support.is_some());
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // Populate the unrelated, unreadable `supports2` with MAX_EVIDENCE + 5 `ev:Projection`
    // records (crossing the 10,000-record scan bound), including unauthenticated noise
    // naming `main`'s policy IRI, then re-protect `supports2`.
    evidence::detach(&ds, supports2).unwrap();
    let main_policy = evidence::review::policy_iri("main");
    let other_policy = evidence::review::policy_iri("other");
    let mut nq = String::with_capacity(10_010 * 160);
    for i in 0..10_005 {
        nq.push_str(&format!(
            "<urn:hidden-proj:{i}> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <{EV}Projection> <{supports2}> .\n\
             <urn:hidden-proj:{i}> <{EV}reviewPolicy> <{}> <{supports2}> .\n",
            if i < 5 {
                main_policy.as_str()
            } else {
                other_policy.as_str()
            }
        ));
    }
    ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    // The exact same graph-restricted review on `main` must still succeed identically.
    let second = review_restricted(&ds, "b", &b).unwrap();
    assert!(second.committed);
    assert!(second.decisions[0].support.is_some());
    assert_eq!(
        accepted(&ds),
        vec![
            "<urn:release> <urn:affectedBy> <urn:cve1>".to_string(),
            "<urn:release> <urn:affectedBy> <urn:cve2>".to_string(),
        ]
    );

    // Real ownership conflict in an unreadable supports graph must still refuse:
    // `other` accepts and then withdraws a claim on `output2` (leaving `output2` empty
    // while `other`'s active v1 projection in `supports2` still owns `output2`), then
    // `main` (v1, no prior projection) swaps onto `output2` under unchanged protections.
    let ds2 = Dataset::memory();
    let mut s_conflict = settings(ConflictMode::Refuse);
    s_conflict.decisions.push(decisions2.into());
    s_conflict.supports.push(supports2.into());
    s_conflict.projections.push(output2.into());
    s_conflict.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds2, &s_conflict, &Control::none()).unwrap();
    let c1 = claim_of(&propose(
        &ds2,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let c2 = claim_of(&propose(
        &ds2,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    let mut o_other = reviewer();
    o_other.if_head = Some(ds2.store().head_commit().seq);
    evidence::review(
        None,
        &ds2,
        &ds2,
        "other",
        1,
        &[decision("c1", &c1, Disposition::Accepted, None)],
        &o_other,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    withdraw(&ds2, &c1);

    let mut swapped = s_conflict.clone();
    swapped.policies.get_mut("main").unwrap().output = output2.into();
    swapped.policies.get_mut("other").unwrap().output = OUTPUT.into();
    evidence::configure(&ds2, &swapped, &Control::none()).unwrap();

    // Caller has access ONLY to `main`'s new graphs (`output2`, NOT `supports2`).
    let main_new_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, DECISIONS, SUPPORTS, output2],
            &[],
        )),
        Graphs::Only(GraphRule::new([DECISIONS, SUPPORTS, output2], &[])),
    ));
    let mut opts = reviewer();
    opts.if_head = Some(ds2.store().head_commit().seq);
    opts.write = Some(main_new_only.clone());
    let head_conflict = ds2.store().head_commit().seq;
    assert_eq!(
        code(evidence::review(
            None,
            &ds2,
            &ds2,
            "main",
            1,
            &[decision("c2", &c2, Disposition::Accepted, None)],
            &opts,
            &mut |_, _| Some(main_new_only.clone()),
            &Control::none(),
        )),
        "projection-unavailable"
    );
    assert_eq!(ds2.store().head_commit().seq, head_conflict);
}

/// Turning a former supports graph A into another policy's dedicated output while
/// retaining A's protection nonce must not allow the new policy's reconciliation to
/// erase A's continuously authenticated evidence history or clear the original policy's
/// drift guard. Detaching A explicitly rotates its nonce and then allows reconciliation.
#[test]
fn former_supports_graph_as_dedicated_output_protects_history_and_drift() {
    use sparkles::managed::OwnerKind;

    let ds = configured(ConflictMode::Refuse);
    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));
    review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    let nonce_before = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS protected");

    // Move `main.supports` to empty `supports2` at version 1 while removing SUPPORTS from
    // `settings.supports` and assigning SUPPORTS as the dedicated output of a new policy.
    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let supports3 = "urn:ev:supports3";
    let mut moved = settings(ConflictMode::Refuse);
    moved.decisions.push(decisions2.into());
    moved.supports = vec![supports2.into(), supports3.into()];
    moved.projections.push(SUPPORTS.into());
    moved.policies.get_mut("main").unwrap().supports = supports2.into();
    moved.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: decisions2.into(),
            supports: supports3.into(),
            output: SUPPORTS.into(),
        },
    );
    evidence::configure(&ds, &moved, &Control::none()).unwrap();

    let nonce_after = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS still protected");
    assert_eq!(nonce_after, nonce_before);

    let head = ds.store().head_commit().seq;
    // Reconciling `other` must refuse rather than erasing `SUPPORTS`'s authenticated history.
    assert_eq!(
        code(reconcile(&ds, "other", Some(1), &reviewer())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ ?p a <{EV}Projection> ; <{EV}reviewPolicy> <{}> }} }}",
            evidence::review::policy_iri("main").as_str()
        ))
        .unwrap(),
        "SUPPORTS must retain main's authenticated ev:Projection history"
    );
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SUPPORTS}> {{ ?op a <{EV}Operation> }} }}"
        ))
        .unwrap(),
        "SUPPORTS must retain its authenticated ev:Operation receipts"
    );

    // `main` must still refuse at version 1 and leave OUTPUT intact.
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(review(
            &ds,
            &[decision("b", &b, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(
        accepted(&ds),
        vec!["<urn:release> <urn:affectedBy> <urn:cve1>".to_string()]
    );

    // After explicitly detaching and re-protecting SUPPORTS (rotating its nonce), `other`
    // can reconcile SUPPORTS as a fresh empty output.
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &moved, &Control::none()).unwrap();
    let rec = reconcile(&ds, "other", Some(1), &reviewer()).unwrap();
    assert!(rec.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"))
            .unwrap()
    );
}

/// Ordinary review's same-version drift check must be physically scoped by protection
/// nonces so arbitrarily many ordinary `(ev:reviewPolicy, requestedPolicy)` statements in
/// unrelated, unreadable graphs never trigger the scan's 1,024-row checkpoint work.
#[test]
fn ordinary_review_skips_unrelated_unreadable_policy_rows() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";
    let hidden_unmanaged = "urn:unrelated:hidden";

    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), OTHER.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));

    let main_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::Only(GraphRule::new([DECISIONS, SUPPORTS, OUTPUT], &[])),
    ));

    let mut opts = reviewer();
    opts.if_head = Some(ds.store().head_commit().seq);
    opts.write = Some(main_only.clone());
    evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision("a", &a, Disposition::Accepted, None)],
        &opts,
        &mut |_, _| Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();

    // Load 3,000 ordinary RDF rows matching `(ev:reviewPolicy, main)` into an unrelated,
    // unreadable unmanaged graph (no `ev:Projection` type, no protection marker, no managed
    // role) and another 3,000 unauthenticated rows into unreadable `supports2`.
    let main_policy = evidence::review::policy_iri("main");
    let mut nq = String::with_capacity(6_000 * 120);
    for i in 0..3_000 {
        nq.push_str(&format!(
            "<urn:unrelated:noise:{i}> <{EV}reviewPolicy> <{}> <{hidden_unmanaged}> .\n",
            main_policy.as_str()
        ));
    }
    ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();

    evidence::detach(&ds, supports2).unwrap();
    let mut nq2 = String::with_capacity(3_000 * 120);
    for i in 0..3_000 {
        nq2.push_str(&format!(
            "<urn:hidden:noise:{i}> <{EV}reviewPolicy> <{}> <{supports2}> .\n",
            main_policy.as_str()
        ));
    }
    ds.load_str(&nq2, oxrdfio::RdfFormat::NQuads).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let checkpoints = Arc::new(AtomicUsize::new(0));
    let cp = checkpoints.clone();
    let control = Control {
        progress: sparkles::task::Progress::new(move |_, stage| {
            if stage == "scanning permission rows" {
                cp.fetch_add(1, Ordering::Relaxed);
            }
        }),
        ..Control::none()
    };

    opts.if_head = Some(ds.store().head_commit().seq);
    let second = evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision("b", &b, Disposition::Accepted, None)],
        &opts,
        &mut |_, _| Some(main_only.clone()),
        &control,
    )
    .unwrap();
    assert!(second.committed);
    assert!(second.decisions[0].support.is_some());
    assert_eq!(
        checkpoints.load(Ordering::Relaxed),
        0,
        "unrelated unreadable graphs must not extend ordinary review's physical scan range"
    );
    assert_eq!(
        accepted(&ds),
        vec![
            "<urn:release> <urn:affectedBy> <urn:cve1>".to_string(),
            "<urn:release> <urn:affectedBy> <urn:cve2>".to_string(),
        ]
    );
}

/// When a former supports graph A moves to another policy's dedicated output while
/// retaining its protection nonce, keyed reconciliation replay must still discover the
/// original operation and linked projection in A, enforce read grants and payload
/// conflicts, and return the original receipt without advancing the branch head.
#[test]
fn keyed_replay_finds_operation_after_supports_becomes_dedicated_output() {
    for reuse_projection in [false, true] {
        check_keyed_replay_in_former_supports(reuse_projection, None);
    }
}

enum LegacyChange {
    WithdrawMetadata(&'static str),
    WithdrawProjectionField(&'static str),
    PropertyOnFreshOperation(NamedNode),
    SharedKeyOtherPrincipal,
    ReplaceCommit,
    ProjectExtraCommit,
    ConflictingCommit,
    ConflictingKey,
    KeyOnProjection,
    WithdrawGeneration,
    WithdrawLink,
    ProjectReceipt,
}

#[test]
fn legacy_replay_refuses_a_withdrawn_shared_reuse_link() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::WithdrawLink));
}

#[test]
fn legacy_replay_refuses_a_withdrawn_shared_commit() {
    check_keyed_replay_in_former_supports(
        true,
        Some(LegacyChange::WithdrawMetadata("ev:commit ?o")),
    );
}

#[test]
fn legacy_replay_refuses_a_live_projected_replacement_commit() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::ReplaceCommit));
}

#[test]
fn legacy_replay_ignores_an_additional_live_projected_commit() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::ProjectExtraCommit));
}

#[test]
fn legacy_replay_refuses_conflicting_stamps_after_supports_reset() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::ConflictingCommit));
}

#[test]
fn legacy_replay_reserves_a_stored_key_even_on_a_different_operation_iri() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::ConflictingKey));
}

#[test]
fn legacy_replay_ignores_a_key_on_a_non_operation_record() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::KeyOnProjection));
}

#[test]
fn legacy_replay_reserves_the_sole_publishers_key_after_withdrawal() {
    for change in [
        LegacyChange::WithdrawMetadata("a ev:Operation"),
        LegacyChange::WithdrawMetadata("ev:graphProtection ?o"),
        LegacyChange::WithdrawProjectionField("ev:availabilityReceipt ?o"),
        LegacyChange::WithdrawProjectionField("ev:graphProtection ?o"),
    ] {
        check_keyed_replay_in_former_supports(false, Some(change));
    }
}

#[test]
fn legacy_replay_ignores_a_projected_property_on_a_fresh_operation() {
    for predicate in [
        oxrdf::vocab::rdfs::LABEL.into_owned(),
        iri(&format!("{EV}idempotencyKey")),
    ] {
        check_keyed_replay_in_former_supports(
            true,
            Some(LegacyChange::PropertyOnFreshOperation(predicate)),
        );
    }
}

#[test]
fn legacy_replay_scopes_a_live_shared_key_to_its_original_principal() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::SharedKeyOtherPrincipal));
}

#[test]
fn legacy_replay_requires_each_shared_operation_field() {
    for field in [
        "a ev:Operation",
        "a prov:Activity",
        "prov:wasAssociatedWith ?o",
        "prov:endedAtTime ?o",
        "ev:datasetId ?o",
        "ev:branchId ?o",
        "ev:payloadDigest ?o",
        "ev:graphProtection ?o",
        "ev:idempotencyKey ?o",
    ] {
        check_keyed_replay_in_former_supports(true, Some(LegacyChange::WithdrawMetadata(field)));
    }
}

#[test]
fn legacy_replay_prefers_intact_reuse_link_to_projected_receipt() {
    check_keyed_replay_in_former_supports(true, Some(LegacyChange::ProjectReceipt));
}

#[test]
fn legacy_replay_recovers_created_projection_after_generation_edge_withdrawal() {
    check_keyed_replay_in_former_supports(false, Some(LegacyChange::WithdrawGeneration));
}

fn check_keyed_replay_in_former_supports(reuse_projection: bool, legacy: Option<LegacyChange>) {
    use sparkles::managed::OwnerKind;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let ds = Dataset::open(&root).unwrap();
    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let supports3 = "urn:ev:supports3";
    let output2 = "urn:ev:accepted2";
    let mut s1 = settings(ConflictMode::Refuse);
    s1.decisions.push(decisions2.into());
    s1.supports.push(supports2.into());
    s1.supports.push(supports3.into());
    let ds = if legacy.is_some() {
        // The legacy output policy already had a healthy projection before this
        // graph also began accepting records under its original nonce.
        let mut original = s1.clone();
        original.supports.retain(|g| g != SUPPORTS);
        original.projections.push(SUPPORTS.into());
        original.policies.get_mut("main").unwrap().supports = supports2.into();
        let mut other = policy(ConflictMode::Refuse);
        other.decisions = decisions2.into();
        other.supports = supports3.into();
        other.output = SUPPORTS.into();
        original.policies.insert("other".into(), other);
        evidence::configure(&ds, &original, &Control::none()).unwrap();
        assert!(
            reconcile(&ds, "other", Some(1), &reviewer())
                .unwrap()
                .committed
        );
        drop(ds);
        legacy_settings(&root, &s1);
        Dataset::open(&root).unwrap()
    } else {
        evidence::configure(&ds, &s1, &Control::none()).unwrap();
        ds
    };

    if reuse_projection {
        let a = claim_of(&propose(
            &ds,
            &occurrence_with("s1", "urn:cve1", Polarity::Positive),
        ));
        review(&ds, &[decision("a", &a, Disposition::Accepted, None)]).unwrap();
    }

    // First keyed reconciliation with omitted policyVersion writes its operation only in SUPPORTS.
    let mut opts = reviewer();
    opts.idempotency_key = Some("dedicated-output-replay-key".into());
    let first = reconcile(&ds, "main", None, &opts).unwrap();
    assert!(first.committed && !first.replayed);
    assert_eq!(first.policy_version, 1);
    assert_eq!(first.output.as_str(), OUTPUT);

    if legacy.is_none() {
        let mut second = opts.clone();
        second.principal = iri("urn:principal:other-reviewer");
        let independent = reconcile(&ds, "main", None, &second).unwrap();
        assert!(independent.committed);
        assert_ne!(independent.operation, first.operation);
    }

    let op = &first.operation;
    let projection = &first.projection;
    let link = if reuse_projection {
        format!("{op} <http://www.w3.org/ns/prov#used> {projection}")
    } else {
        format!("{projection} <{EV}availabilityReceipt> {op}")
    };
    assert!(
        ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {link} }} }}"))
            .unwrap()
    );

    let nonce_before = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS protected");

    // Raise `main` to v2 with fresh `supports2` / `output2` while reassigning `SUPPORTS`
    // from `settings.supports` to `settings.projections` as `other`'s dedicated output.
    evidence::detach(&ds, OUTPUT).unwrap();
    let mut s2 = s1.clone();
    s2.supports = vec![supports2.into(), supports3.into()];
    s2.projections = vec![output2.into(), SUPPORTS.into()];
    {
        let p = s2.policies.get_mut("main").unwrap();
        p.version = 2;
        p.supports = supports2.into();
        p.output = output2.into();
    }
    s2.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: decisions2.into(),
            supports: supports3.into(),
            output: SUPPORTS.into(),
        },
    );
    evidence::configure(&ds, &s2, &Control::none()).unwrap();

    let nonce_after = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(SUPPORTS)
        .copied()
        .expect("SUPPORTS still protected");
    assert_eq!(nonce_after, nonce_before);

    let ds = if let Some(case) = legacy {
        drop(ds);
        let ds = legacy_projection_writer(&root, SUPPORTS);
        let triples = match case {
            LegacyChange::WithdrawMetadata(field)
            | LegacyChange::WithdrawProjectionField(field) => {
                let subject = if matches!(case, LegacyChange::WithdrawProjectionField(_)) {
                    projection
                } else {
                    op
                };
                let result = ds
                    .construct(&format!(
                    "PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#>
                     CONSTRUCT {{ {subject} {field} }} WHERE {{ GRAPH <{SUPPORTS}> {{ {subject} {field} }} }}"
                ))
                    .unwrap();
                assert_eq!(result.len(), 1, "{field}");
                result.triples().to_vec()
            }
            LegacyChange::PropertyOnFreshOperation(ref predicate) => vec![Triple::new(
                evidence::operation_iri(evidence::operation_id(
                    ds.dataset_id(),
                    &opts.principal,
                    "extra-key",
                )),
                predicate.clone(),
                oxrdf::Literal::new_simple_literal("extra-key"),
            )],
            LegacyChange::SharedKeyOtherPrincipal => vec![Triple::new(
                op.clone(),
                iri(&format!("{EV}idempotencyKey")),
                oxrdf::Literal::new_simple_literal(opts.idempotency_key.as_deref().unwrap()),
            )],
            LegacyChange::ConflictingKey | LegacyChange::KeyOnProjection => vec![Triple::new(
                if matches!(case, LegacyChange::KeyOnProjection) {
                    projection.clone()
                } else {
                    op.clone()
                },
                iri(&format!("{EV}idempotencyKey")),
                oxrdf::Literal::new_simple_literal("extra-key"),
            )],
            LegacyChange::ReplaceCommit
            | LegacyChange::ProjectExtraCommit
            | LegacyChange::ConflictingCommit => {
                let stamp = |seq: u64| {
                    Triple::new(
                        op.clone(),
                        iri(&format!("{EV}commit")),
                        oxrdf::Literal::new_typed_literal(
                            seq.to_string(),
                            oxrdf::vocab::xsd::INTEGER,
                        ),
                    )
                };
                let mut triples = vec![];
                if matches!(case, LegacyChange::ReplaceCommit) {
                    triples.push(stamp(first.commit));
                }
                triples.push(stamp(first.commit + 100));
                triples
            }
            LegacyChange::WithdrawGeneration => vec![Triple::new(
                projection.clone(),
                iri("http://www.w3.org/ns/prov#wasGeneratedBy"),
                op.clone(),
            )],
            LegacyChange::WithdrawLink => vec![Triple::new(
                op.clone(),
                iri("http://www.w3.org/ns/prov#used"),
                projection.clone(),
            )],
            LegacyChange::ProjectReceipt => {
                let shadow = iri("urn:legacy:shadow:projection");
                use oxrdf::Literal;
                vec![
                    Triple::new(
                        shadow.clone(),
                        oxrdf::vocab::rdf::TYPE,
                        iri(&format!("{EV}Projection")),
                    ),
                    Triple::new(
                        shadow.clone(),
                        iri(&format!("{EV}graphProtection")),
                        Literal::new_simple_literal(nonce_before.to_string()),
                    ),
                    Triple::new(
                        shadow.clone(),
                        iri(&format!("{EV}reviewPolicy")),
                        first.policy.clone(),
                    ),
                    Triple::new(
                        shadow.clone(),
                        iri(&format!("{EV}availabilityReceipt")),
                        op.clone(),
                    ),
                    Triple::new(
                        shadow.clone(),
                        iri(&format!("{EV}policyVersion")),
                        Literal::new_simple_literal("99"),
                    ),
                    Triple::new(
                        shadow,
                        iri(&format!("{EV}output")),
                        iri("urn:legacy:shadow:output"),
                    ),
                ]
            }
        };
        let shared = triples[0].clone();
        let claims = project_legacy_triples(&ds, "other", triples);
        drop(ds);
        legacy_settings(&root, &s2);
        let ds = Dataset::open(&root).unwrap();
        if matches!(
            case,
            LegacyChange::KeyOnProjection | LegacyChange::PropertyOnFreshOperation(_)
        ) {
            opts.idempotency_key = Some("extra-key".into());
            assert!(reconcile(&ds, "main", None, &opts).unwrap().committed);
            return;
        }
        let assert_conflict = |opts: &ReviewOptions| {
            let head = ds.store().head_commit().seq;
            let err = reconcile(&ds, "main", None, opts).unwrap_err();
            assert_eq!(err.code(), "immutable-record-conflict");
            assert!(matches!(err, sparkles::Error::Managed(e) if e.status() == 409));
            assert_eq!(ds.store().head_commit().seq, head);
            assert!(
                !ds.ask(&format!("ASK {{ GRAPH <{supports2}> {{ ?s ?p ?o }} }}"))
                    .unwrap()
            );
        };
        if matches!(case, LegacyChange::SharedKeyOtherPrincipal) {
            assert_conflict(&opts);
            opts.principal = iri("urn:principal:other-reviewer");
            let independent = reconcile(&ds, "main", None, &opts).unwrap();
            assert!(independent.committed && !independent.replayed);
            assert_eq!(independent.policy_version, 2);
            assert_ne!(independent.operation, first.operation);
            return;
        }
        if matches!(
            case,
            LegacyChange::ConflictingCommit | LegacyChange::ConflictingKey
        ) {
            evidence::detach(&ds, supports3).unwrap();
            evidence::configure(&ds, &s2, &Control::none()).unwrap();
            if matches!(case, LegacyChange::ConflictingKey) {
                opts.idempotency_key = Some("extra-key".into());
            }
            assert_conflict(&opts);
            return;
        }
        if matches!(
            case,
            LegacyChange::WithdrawMetadata(_)
                | LegacyChange::WithdrawProjectionField(_)
                | LegacyChange::ReplaceCommit
        ) {
            assert_conflict(&opts);
            withdraw(&ds, &claims[0]);
            assert!(
                !ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {shared} }} }}"))
                    .unwrap()
            );
            if !matches!(
                case,
                LegacyChange::WithdrawProjectionField("ev:availabilityReceipt ?o")
            ) {
                assert!(
                    ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {link} }} }}"))
                        .unwrap()
                );
            }
            if !matches!(case, LegacyChange::WithdrawMetadata("ev:idempotencyKey ?o")) {
                assert!(ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {op} <{EV}idempotencyKey> \"dedicated-output-replay-key\" }} }}")).unwrap());
            }
            if matches!(case, LegacyChange::ReplaceCommit) {
                assert!(
                    ds.ask(&format!(
                        "ASK {{ GRAPH <{SUPPORTS}> {{ {op} <{EV}commit> {} }} }}",
                        first.commit + 100
                    ))
                    .unwrap()
                );
            }
            assert_conflict(&opts);
            if !reuse_projection {
                let head = ds.store().head_commit().seq;
                assert_eq!(
                    code(reconcile(&ds, "other", Some(1), &reviewer())),
                    "immutable-record-conflict"
                );
                assert_eq!(ds.store().head_commit().seq, head);
                evidence::configure(&ds, &s2, &Control::none()).unwrap();
                let registry = ds.store().managed_graphs().unwrap().unwrap();
                let entry = &registry.owners[&OwnerKind::Evidence];
                assert!(!entry.projection_only.contains(SUPPORTS));
                assert_eq!(entry.protections[SUPPORTS], nonce_before);
                assert_conflict(&opts);
            }
            return;
        }
        if matches!(case, LegacyChange::WithdrawLink) {
            // While live, the support makes even this genuine reuse edge
            // indistinguishable from a projection of the same quad.
            assert_conflict(&opts);
            withdraw(&ds, &claims[0]);
            assert!(
                !ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {link} }} }}"))
                    .unwrap()
            );
            assert_conflict(&opts);
            return;
        }
        if matches!(case, LegacyChange::WithdrawGeneration) {
            withdraw(&ds, &claims[0]);
            assert!(!ds.ask(&format!("ASK {{ GRAPH <{SUPPORTS}> {{ {projection} <http://www.w3.org/ns/prov#wasGeneratedBy> {op} }} }}")).unwrap());
        }
        ds
    } else {
        ds
    };
    let head = ds.store().head_commit().seq;

    // Narrow grants omitting SUPPORTS: payload conflict still returns idempotency-conflict,
    // and matching replay refuses with NotPermitted without advancing the head.
    let narrowed = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, OTHER, DECISIONS, supports2, output2],
            &[],
        )),
        Graphs::Only(GraphRule::new([supports2, output2], &[])),
    ));
    let mut narrow_opts = opts.clone();
    narrow_opts.write = Some(narrowed.clone());
    assert_eq!(
        code(evidence::reconcile_projection(
            &ds,
            &ds,
            "main",
            Some(2),
            &narrow_opts,
            Some(narrowed.clone()),
            &Control::none(),
        )),
        "idempotency-conflict"
    );
    let err = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        None,
        &narrow_opts,
        Some(narrowed),
        &Control::none(),
    )
    .unwrap_err();
    assert!(
        matches!(err, sparkles::Error::NotPermitted(_)),
        "expected NotPermitted, got {err}"
    );
    assert_eq!(ds.store().head_commit().seq, head);

    // Full grants including SUPPORTS: replay discovers the original operation and linked
    // projection in SUPPORTS, returns the v1 receipt, and leaves the head unchanged.
    let full = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [
                SOURCES, CLAIMS, OTHER, DECISIONS, SUPPORTS, supports2, output2,
            ],
            &[],
        )),
        Graphs::Only(GraphRule::new([supports2, output2], &[])),
    ));
    let mut full_opts = opts.clone();
    full_opts.write = Some(full.clone());
    let replayed = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        None,
        &full_opts,
        Some(full),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed.committed && replayed.replayed);
    assert_eq!(replayed.commit, first.commit);
    assert_eq!(replayed.projection, first.projection);
    assert_eq!(replayed.policy_version, 1);
    assert_eq!(replayed.output.as_str(), OUTPUT);
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{supports2}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "replay must not write a duplicate operation into supports2"
    );
}

/// Ordinary and evidence-vocabulary assertions projected into a dedicated output graph
/// (including `rdf:type ev:Operation`, `ev:graphProtection` with the output's current
/// nonce, or all nine mandatory operation triples) must not be mistaken for
/// evidence-owner operation history or pollute keyed replay of a valid receipt.
#[test]
fn projected_operation_assertions_do_not_block_output_or_keyed_replay() {
    use oxrdf::Literal;
    use sparkles::managed::OwnerKind;

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const PROV: &str = "http://www.w3.org/ns/prov#";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";

    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into(), OTHER.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    // Publish an initial projection on `main` so `OUTPUT`'s protection nonce is visible.
    let init = claim_of(&propose(
        &ds,
        &occurrence_with("init", "urn:cve:init", Polarity::Positive),
    ));
    review(&ds, &[decision("init", &init, Disposition::Accepted, None)]).unwrap();

    let out_nonce = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT protected");
    let out2_nonce = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(output2)
        .copied()
        .expect("output2 protected");

    let propose_triple = |src: &str, t: Triple| {
        let input = EvidenceInput {
            revisions: vec![revision(src)],
            claims: vec![claim("c", Polarity::Positive, t)],
            ..Default::default()
        };
        claim_of(&propose(&ds, &input))
    };

    // Project both a 2-triple `(ev:Operation, ev:graphProtection)` pair and a complete
    // 9-triple `ev:Operation` lookalike carrying `OUTPUT`'s current nonce in one review.
    let op2 = iri("urn:proj:op2");
    let op9 = iri("urn:proj:op9");
    let ds_id = ds.dataset_id().to_string();
    let branch_id = ds.store().branch_id().to_string();
    let payload_sha = sha("projected-op-payload").to_string();
    let dt = Literal::new_typed_literal("2026-11-26T00:00:00.000Z", iri(&format!("{XSD}dateTime")));
    let commit_int = Literal::new_typed_literal("99", iri(&format!("{XSD}integer")));

    let triples_to_project: Vec<(&str, Triple)> = vec![
        (
            "p2-type",
            Triple::new(op2.clone(), iri(RDF_TYPE), iri(&format!("{EV}Operation"))),
        ),
        (
            "p2-prot",
            Triple::new(
                op2.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(out_nonce.to_string()),
            ),
        ),
        (
            "p9-type",
            Triple::new(op9.clone(), iri(RDF_TYPE), iri(&format!("{EV}Operation"))),
        ),
        (
            "p9-act",
            Triple::new(op9.clone(), iri(RDF_TYPE), iri(&format!("{PROV}Activity"))),
        ),
        (
            "p9-assoc",
            Triple::new(
                op9.clone(),
                iri(&format!("{PROV}wasAssociatedWith")),
                iri("urn:principal:reviewer"),
            ),
        ),
        (
            "p9-ended",
            Triple::new(op9.clone(), iri(&format!("{PROV}endedAtTime")), dt.clone()),
        ),
        (
            "p9-ds",
            Triple::new(
                op9.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds_id.clone()),
            ),
        ),
        (
            "p9-br",
            Triple::new(
                op9.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(branch_id.clone()),
            ),
        ),
        (
            "p9-commit",
            Triple::new(op9.clone(), iri(&format!("{EV}commit")), commit_int.clone()),
        ),
        (
            "p9-digest",
            Triple::new(
                op9.clone(),
                iri(&format!("{EV}payloadDigest")),
                Literal::new_simple_literal(payload_sha.clone()),
            ),
        ),
        (
            "p9-prot",
            Triple::new(
                op9.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(out_nonce.to_string()),
            ),
        ),
    ];
    let mut decs = Vec::new();
    for (k, t) in triples_to_project {
        let c = propose_triple(k, t);
        decs.push(decision(k, &c, Disposition::Accepted, None));
    }
    let projected = review(&ds, &decs).unwrap();
    assert!(projected.committed);

    // A subsequent projecting review on `main` must succeed rather than refusing with
    // `immutable-record-conflict`.
    let follow = claim_of(&propose(
        &ds,
        &occurrence_with("follow", "urn:cve:follow", Polarity::Positive),
    ));
    let follow_res = review(
        &ds,
        &[decision("follow", &follow, Disposition::Accepted, None)],
    )
    .unwrap();
    assert!(follow_res.committed);

    // Reconciliation under unchanged protections must also succeed.
    let rec1 = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec1.committed);

    // Detaching and re-protecting `DECISIONS` leaves `OUTPUT`'s and `SUPPORTS`'s nonces
    // unchanged; reconciliation must still succeed and clear the projected assertions.
    evidence::detach(&ds, DECISIONS).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let out_nonce_after = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT still protected");
    assert_eq!(out_nonce_after, out_nonce);

    let cleared = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(cleared.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "reconciliation after detaching decisions must clear projected assertions"
    );

    // Now test keyed replay when another policy's unreadable dedicated output `output2`
    // holds an ordinary `<op_iri> rdf:type ev:Operation` assertion and also a full
    // 9-triple projected copy with `output2`'s nonce.
    let mut keyed_opts = reviewer();
    keyed_opts.idempotency_key = Some("projected-op-replay-key".into());
    let first = reconcile(&ds, "main", Some(1), &keyed_opts).unwrap();
    assert!(first.committed && !first.replayed);
    let real_op = first.operation.clone();

    // Project just `<real_op> a ev:Operation` into `output2` via policy `other`.
    let other_only_type = propose_triple(
        "o2-type",
        Triple::new(
            real_op.clone(),
            iri(RDF_TYPE),
            iri(&format!("{EV}Operation")),
        ),
    );
    let mut o_other = reviewer();
    o_other.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "other",
        1,
        &[decision(
            "o2t",
            &other_only_type,
            Disposition::Accepted,
            None,
        )],
        &o_other,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // Replay on `main` under narrow grants omitting `output2` and `supports2`: must NOT
    // treat the ordinary projected `rdf:type ev:Operation` in `output2` as an
    // unauthenticated copy (`immutable-record-conflict`) or require read access to `output2`.
    let main_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, OTHER, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::Only(GraphRule::new([SUPPORTS, OUTPUT], &[])),
    ));
    let mut narrow_replay = keyed_opts.clone();
    narrow_replay.write = Some(main_only.clone());
    let head_before = ds.store().head_commit().seq;
    let replayed1 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed1.committed && replayed1.replayed);
    assert_eq!(replayed1.commit, first.commit);
    assert_eq!(replayed1.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_before);

    // Also project the remaining 8 mandatory `ev:Operation` triples (including
    // `output2`'s own protection nonce!) for `<real_op>` into `output2`: replay on
    // `main` must still recognize them as support-projected and return the original receipt.
    let extra_triples: Vec<(&str, Triple)> = vec![
        (
            "o2-act",
            Triple::new(
                real_op.clone(),
                iri(RDF_TYPE),
                iri(&format!("{PROV}Activity")),
            ),
        ),
        (
            "o2-assoc",
            Triple::new(
                real_op.clone(),
                iri(&format!("{PROV}wasAssociatedWith")),
                iri("urn:principal:reviewer"),
            ),
        ),
        (
            "o2-ended",
            Triple::new(real_op.clone(), iri(&format!("{PROV}endedAtTime")), dt),
        ),
        (
            "o2-ds",
            Triple::new(
                real_op.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds_id),
            ),
        ),
        (
            "o2-br",
            Triple::new(
                real_op.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(branch_id),
            ),
        ),
        (
            "o2-commit",
            Triple::new(real_op.clone(), iri(&format!("{EV}commit")), commit_int),
        ),
        (
            "o2-digest",
            Triple::new(
                real_op.clone(),
                iri(&format!("{EV}payloadDigest")),
                Literal::new_simple_literal(payload_sha),
            ),
        ),
        (
            "o2-prot",
            Triple::new(
                real_op.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(out2_nonce.to_string()),
            ),
        ),
    ];
    let mut other_decs = Vec::new();
    for (k, t) in extra_triples {
        let c = propose_triple(k, t);
        other_decs.push(decision(k, &c, Disposition::Accepted, None));
    }
    o_other.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "other",
        1,
        &other_decs,
        &o_other,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    let head_before2 = ds.store().head_commit().seq;
    let replayed2 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed2.committed && replayed2.replayed);
    assert_eq!(replayed2.commit, first.commit);
    assert_eq!(replayed2.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_before2);

    // Detach and re-protect `supports2` (first retaining old `supports2`, then clearing
    // `supports2` completely) while `output2` keeps its unchanged protection nonce:
    // `main`'s keyed replay under narrow grants must still succeed without returning
    // `409 idempotency-conflict` or `403 NotPermitted`.
    evidence::detach(&ds, supports2).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let out2_nonce_after_retain = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(output2)
        .copied()
        .expect("output2 still protected");
    assert_eq!(out2_nonce_after_retain, out2_nonce);
    let head_after_s2_retain = ds.store().head_commit().seq;
    let replayed3 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed3.committed && replayed3.replayed);
    assert_eq!(replayed3.commit, first.commit);
    assert_eq!(replayed3.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_after_s2_retain);

    evidence::detach(&ds, supports2).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{supports2}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let head_after_s2_clear = ds.store().head_commit().seq;
    let replayed4 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed4.committed && replayed4.replayed);
    assert_eq!(replayed4.commit, first.commit);
    assert_eq!(replayed4.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_after_s2_clear);
}

/// Rotating `SUPPORTS` (both retaining and clearing old `SUPPORTS`) while `OUTPUT`
/// remains continuously protected must still let `reconcile_projection` empty a
/// 9-triple projected `ev:Operation` lookalike from `OUTPUT` instead of refusing with
/// `409 immutable-record-conflict`, and promoting a dedicated output to a record role
/// without detaching must rotate its nonce so projected lookalikes never authenticate.
#[test]
fn projected_operation_lookalike_empties_after_supports_rotation() {
    use sparkles::managed::OwnerKind;

    let ds = Dataset::memory();
    let s = settings(ConflictMode::Refuse);
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let ev_entry = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .clone();
    let out_nonce = ev_entry
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT protected");
    assert!(ev_entry.projection_only.contains(OUTPUT));

    // 1. Project a 9-triple lookalike into OUTPUT, then detach & re-protect SUPPORTS
    // while RETAINING old SUPPORTS quads and keeping OUTPUT continuously protected.
    project_lookalike(
        &ds,
        "main",
        "retain",
        iri("urn:proj:op-retain"),
        out_nonce,
        None,
    );
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let out_nonce_1 = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT still protected");
    assert_eq!(out_nonce_1, out_nonce);

    let rec_retain = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec_retain.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "reconcile_projection after SUPPORTS rotation (retained) must empty OUTPUT"
    );

    // 2. Project a 9-triple lookalike again, then detach, CLEAR, and re-protect SUPPORTS
    // while keeping OUTPUT continuously protected.
    project_lookalike(
        &ds,
        "main",
        "clear",
        iri("urn:proj:op-clear"),
        out_nonce,
        None,
    );
    evidence::detach(&ds, SUPPORTS).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let out_nonce_2 = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT still protected");
    assert_eq!(out_nonce_2, out_nonce);

    let rec_clear = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec_clear.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "reconcile_projection after SUPPORTS rotation (cleared) must empty OUTPUT"
    );

    // 3. Promoting a dedicated projection output into a record role without detaching
    // must drop it from `projection_only` and rotate its nonce so any projected
    // lookalike cannot escalate into an authenticated operation record.
    project_lookalike(
        &ds,
        "main",
        "promote",
        iri("urn:proj:op-promote"),
        out_nonce,
        None,
    );
    let mut promoted = s.clone();
    promoted.supports.push(OUTPUT.into());
    promoted.projections = vec!["urn:ev:accepted-new".into()];
    promoted.policies.get_mut("main").unwrap().output = "urn:ev:accepted-new".into();
    evidence::configure(&ds, &promoted, &Control::none()).unwrap();
    let promoted_entry = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .clone();
    let promoted_nonce = promoted_entry
        .protections
        .get(OUTPUT)
        .copied()
        .expect("OUTPUT now protected as record role");
    assert_ne!(promoted_nonce, out_nonce);
    assert!(!promoted_entry.projection_only.contains(OUTPUT));
    assert!(
        promoted_entry
            .projection_only
            .contains("urn:ev:accepted-new")
    );
}

/// Ordinary review's drift and output-history guards must skip both authentic
/// projections accumulated in another policy's unreadable supports graph and ordinary
/// RDF noise carrying published protection nonces in unrelated graphs.
#[test]
fn ordinary_review_skips_unrelated_authentic_projections_and_nonce_noise() {
    use oxrdf::{GraphName, Literal, Quad};
    use sparkles::managed::OwnerKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const PROV: &str = "http://www.w3.org/ns/prov#";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";
    let hidden_unmanaged = "urn:unrelated:nonce-noise";

    let ds = Dataset::memory();
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), OTHER.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &s, &Control::none()).unwrap();

    let a = claim_of(&propose(
        &ds,
        &occurrence_with("s1", "urn:cve1", Polarity::Positive),
    ));
    let b = claim_of(&propose(
        &ds,
        &occurrence_with("s2", "urn:cve2", Polarity::Positive),
    ));

    let main_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::Only(GraphRule::new([DECISIONS, SUPPORTS, OUTPUT], &[])),
    ));

    let mut opts = reviewer();
    opts.if_head = Some(ds.store().head_commit().seq);
    opts.write = Some(main_only.clone());
    evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision("a", &a, Disposition::Accepted, None)],
        &opts,
        &mut |_, _| Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();

    let protections = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .clone();
    let d2_nonce = protections[decisions2];
    let s2_nonce = protections[supports2];
    let o2_nonce = protections[output2];
    let out_nonce = protections[OUTPUT];

    // Write 2,000 authentic `ev:Projection` records of `other` into unreadable `supports2`
    // carrying `supports2`'s real current protection nonce.
    {
        let op_uuid = uuid::Uuid::new_v4();
        let op_iri = evidence::operation_iri(op_uuid);
        let cap = ds
            .store()
            .managed_writer(OwnerKind::Evidence, op_uuid)
            .unwrap();
        let mut t = ds.store().write();
        t.set_managed(cap).unwrap();
        let (seq, _) = t.managed_stamp().unwrap();
        let g = GraphName::NamedNode(iri(supports2));
        let mut labels = std::collections::HashMap::new();
        let mut push_quad = |q: Quad| {
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        };
        let mut push = |sub: NamedNode, pred: NamedNode, obj: Term| {
            push_quad(Quad::new(sub, pred, obj, g.clone()));
        };
        let s2_lit: Term = Literal::new_simple_literal(s2_nonce.to_string()).into();
        let d2_lit: Term = Literal::new_simple_literal(d2_nonce.to_string()).into();
        let o2_lit: Term = Literal::new_simple_literal(o2_nonce.to_string()).into();
        push(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{EV}Operation")).into(),
        );
        push(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{PROV}Activity")).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{PROV}wasAssociatedWith")),
            iri("urn:principal:reviewer").into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{PROV}endedAtTime")),
            Literal::new_typed_literal("2026-11-26T00:00:00.000Z", iri(&format!("{XSD}dateTime")))
                .into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}datasetId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}branchId")),
            Literal::new_simple_literal(ds.store().branch_id().to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}commit")),
            Literal::new_typed_literal(seq.to_string(), iri(&format!("{XSD}integer"))).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}payloadDigest")),
            Literal::new_simple_literal(sha("other-history").to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}graphProtection")),
            s2_lit.clone(),
        );

        let other_policy = evidence::review::policy_iri("other");
        let main_policy = evidence::review::policy_iri("main");
        let proj_class: Term = iri(&format!("{EV}Projection")).into();
        let refuse_iri: Term = iri(&format!("{EV}Refuse")).into();
        let out2_iri: Term = iri(output2).into();
        let supports2_node = iri(supports2);
        let noise_g = GraphName::NamedNode(iri(hidden_unmanaged));
        // Interleave physical subject IDs `A0 < B0 < A1 < B1 ...` between `supports2`'s
        // authentic version-1 projections of `other` and ordinary `(ev:reviewPolicy, main)`
        // assertions in `hidden_unmanaged` (the exact Sol r7 P2-2 interleaved layout).
        for i in 0..2_000u32 {
            let p_iri = iri(&format!("urn:interleave:{i:04}:a"));
            let mut add = |sub: NamedNode, pred: NamedNode, obj: Term| {
                let q = Quad::new(sub, pred, obj, g.clone());
                let ids = t.encode_quad(&q, &mut labels).unwrap();
                t.insert(ids).unwrap();
            };
            add(p_iri.clone(), iri(RDF_TYPE), proj_class.clone());
            add(
                p_iri.clone(),
                iri(&format!("{EV}reviewPolicy")),
                other_policy.clone().into(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}policyVersion")),
                Literal::new_simple_literal("1").into(),
            );
            add(p_iri.clone(), iri(&format!("{EV}output")), out2_iri.clone());
            add(
                p_iri.clone(),
                iri(&format!("{EV}conflictMode")),
                refuse_iri.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}decisionsProtection")),
                d2_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}supportsProtection")),
                s2_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}outputProtection")),
                o2_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}availabilityReceipt")),
                op_iri.clone().into(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}graphProtection")),
                s2_lit.clone(),
            );
            for idx_q in sparkles_core::evidence::review::projection_index_quads(
                &p_iri,
                &supports2_node,
                s2_nonce,
                o2_nonce,
                other_policy.as_str(),
                1,
                output2,
                &[],
            ) {
                let ids = t.encode_quad(&idx_q, &mut labels).unwrap();
                t.insert(ids).unwrap();
            }
            let b_iri = iri(&format!("urn:interleave:{i:04}:b"));
            let noise_q = Quad::new(
                b_iri,
                iri(&format!("{EV}reviewPolicy")),
                main_policy.clone(),
                noise_g.clone(),
            );
            let ids = t.encode_quad(&noise_q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }

    // Also load 3,000 ordinary RDF rows carrying `supports2`'s `ev:supportsProtection`
    // nonce, 3,000 carrying `OUTPUT`'s `ev:outputProtection` nonce, and 3,000 carrying
    // `OUTPUT`'s `ev:graphProtection` nonce into an unrelated unreadable graph.
    let mut nq = String::with_capacity(9_000 * 140);
    for i in 0..3_000 {
        nq.push_str(&format!(
            "<urn:noise:sup:{i}> <{EV}supportsProtection> \"{s2_nonce}\" <{hidden_unmanaged}> .\n\
             <urn:noise:out:{i}> <{EV}outputProtection> \"{out_nonce}\" <{hidden_unmanaged}> .\n\
             <urn:noise:gp:{i}> <{EV}graphProtection> \"{out_nonce}\" <{hidden_unmanaged}> .\n"
        ));
    }
    ds.load_str(&nq, oxrdfio::RdfFormat::NQuads).unwrap();

    let checkpoints = Arc::new(AtomicUsize::new(0));
    let cp = checkpoints.clone();
    let control = Control {
        progress: sparkles::task::Progress::new(move |_, stage| {
            if stage == "scanning permission rows" {
                cp.fetch_add(1, Ordering::Relaxed);
            }
        }),
        ..Control::none()
    };

    opts.if_head = Some(ds.store().head_commit().seq);
    let second = evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision("b", &b, Disposition::Accepted, None)],
        &opts,
        &mut |_, _| Some(main_only.clone()),
        &control,
    )
    .unwrap();
    assert!(second.committed);
    assert_eq!(
        checkpoints.load(Ordering::Relaxed),
        0,
        "authentic projections of unrelated policies and ordinary nonce noise must not trigger scan checkpoints"
    );
}

/// Dismissing many superseded projection revisions in a former supports graph must scale
/// linearly in the number of retained revisions rather than reopening the full
/// `(ev:supportsProtection, nonce)` prefix per prior.
#[test]
fn same_version_drift_supersession_is_linear_across_retained_revisions() {
    use oxrdf::{GraphName, Literal, Quad};
    use sparkles::managed::OwnerKind;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const PROV: &str = "http://www.w3.org/ns/prov#";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";
    const REVISIONS: usize = 1_500;

    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";
    let noise_graph = "urn:unrelated:rev-noise";

    let ds = Dataset::memory();
    let mut s1 = settings(ConflictMode::Refuse);
    s1.supports.push(supports2.into());
    evidence::configure(&ds, &s1, &Control::none()).unwrap();

    let protections = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .protections
        .clone();
    let d_nonce = protections[DECISIONS];
    let s_nonce = protections[SUPPORTS];
    let o_nonce = protections[OUTPUT];

    // Write a chain of 1,500 authenticated v1 `ev:Projection` records in `SUPPORTS`,
    // terminated by an authenticated v2 `ev:Projection` superseding the last v1 record,
    // so every v1 record is superseded and must be dismissed.
    {
        let op_uuid = uuid::Uuid::new_v4();
        let op_iri = evidence::operation_iri(op_uuid);
        let cap = ds
            .store()
            .managed_writer(OwnerKind::Evidence, op_uuid)
            .unwrap();
        let mut t = ds.store().write();
        t.set_managed(cap).unwrap();
        let (seq, _) = t.managed_stamp().unwrap();
        let g = GraphName::NamedNode(iri(SUPPORTS));
        let mut labels = std::collections::HashMap::new();
        let mut push = |sub: NamedNode, pred: NamedNode, obj: Term| {
            let q = Quad::new(sub, pred, obj, g.clone());
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        };
        let s_lit: Term = Literal::new_simple_literal(s_nonce.to_string()).into();
        let d_lit: Term = Literal::new_simple_literal(d_nonce.to_string()).into();
        let o_lit: Term = Literal::new_simple_literal(o_nonce.to_string()).into();
        push(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{EV}Operation")).into(),
        );
        push(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{PROV}Activity")).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{PROV}wasAssociatedWith")),
            iri("urn:principal:reviewer").into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{PROV}endedAtTime")),
            Literal::new_typed_literal("2026-11-26T00:00:00.000Z", iri(&format!("{XSD}dateTime")))
                .into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}datasetId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}branchId")),
            Literal::new_simple_literal(ds.store().branch_id().to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}commit")),
            Literal::new_typed_literal(seq.to_string(), iri(&format!("{XSD}integer"))).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}payloadDigest")),
            Literal::new_simple_literal(sha("main-chain").to_string()).into(),
        );
        push(
            op_iri.clone(),
            iri(&format!("{EV}graphProtection")),
            s_lit.clone(),
        );

        let main_policy = evidence::review::policy_iri("main");
        let proj_class: Term = iri(&format!("{EV}Projection")).into();
        let refuse_iri: Term = iri(&format!("{EV}Refuse")).into();
        let out_iri: Term = iri(OUTPUT).into();
        let supports_node = iri(SUPPORTS);
        let noise_g = GraphName::NamedNode(iri(noise_graph));
        let first_prior = iri("urn:main:proj:0000");

        let push_proj = |t: &mut sparkles_core::store::WriteTxn<'_>,
                         labels: &mut std::collections::HashMap<String, sparkles_core::id::Id>,
                         p_iri: &NamedNode,
                         ver: u32,
                         prior: Option<&NamedNode>| {
            let mut add = |sub: NamedNode, pred: NamedNode, obj: Term| {
                let q = Quad::new(sub, pred, obj, g.clone());
                let ids = t.encode_quad(&q, labels).unwrap();
                t.insert(ids).unwrap();
            };
            add(p_iri.clone(), iri(RDF_TYPE), proj_class.clone());
            add(
                p_iri.clone(),
                iri(&format!("{EV}reviewPolicy")),
                main_policy.clone().into(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}policyVersion")),
                Literal::new_simple_literal(ver.to_string()).into(),
            );
            add(p_iri.clone(), iri(&format!("{EV}output")), out_iri.clone());
            add(
                p_iri.clone(),
                iri(&format!("{EV}conflictMode")),
                refuse_iri.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}decisionsProtection")),
                d_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}supportsProtection")),
                s_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}outputProtection")),
                o_lit.clone(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}availabilityReceipt")),
                op_iri.clone().into(),
            );
            add(
                p_iri.clone(),
                iri(&format!("{EV}graphProtection")),
                s_lit.clone(),
            );
            let mut priors_slice: Vec<&str> = Vec::new();
            if let Some(prev_iri) = prior {
                add(
                    p_iri.clone(),
                    iri(&format!("{PROV}wasRevisionOf")),
                    prev_iri.clone().into(),
                );
                priors_slice.push(prev_iri.as_str());
            }
            for idx_q in sparkles_core::evidence::review::projection_index_quads(
                p_iri,
                &supports_node,
                s_nonce,
                o_nonce,
                main_policy.as_str(),
                ver,
                OUTPUT,
                &priors_slice,
            ) {
                let ids = t.encode_quad(&idx_q, labels).unwrap();
                t.insert(ids).unwrap();
            }
        };

        // Write `urn:main:proj:0000`, then interleave 3,000 foreign-graph
        // `prov:wasRevisionOf` noise edges targeting `urn:main:proj:0000` with
        // same-policy projection subjects BEFORE inserting `urn:main:proj:0000`'s
        // genuine successor (`urn:main:proj:final`) so noise IDs sit strictly before
        // the genuine successor (the exact Sol r7 P2-2 interleaved supersession layout).
        push_proj(&mut t, &mut labels, &first_prior, 1, None);
        let mut prev: Option<NamedNode> = None;
        for i in 1..REVISIONS {
            for j in 0..2 {
                let noise_i = (i - 1) * 2 + j;
                let noise_q = Quad::new(
                    iri(&format!("urn:rev-noise:{noise_i:05}")),
                    iri(&format!("{PROV}wasRevisionOf")),
                    first_prior.clone(),
                    noise_g.clone(),
                );
                let ids = t.encode_quad(&noise_q, &mut labels).unwrap();
                t.insert(ids).unwrap();
            }
            let p_iri = iri(&format!("urn:main:proj:{i:04}"));
            push_proj(&mut t, &mut labels, &p_iri, 1, prev.as_ref());
            prev = Some(p_iri);
        }
        // Genuine v2 successor of BOTH `first_prior` (after all 3,000 interleaved noise
        // edges) and the tail of the v1 chain.
        let tail = prev.expect("chain tail");
        let final_proj = iri("urn:main:proj:final");
        push_proj(&mut t, &mut labels, &final_proj, 2, Some(&tail));
        {
            let q = Quad::new(
                final_proj.clone(),
                iri(&format!("{PROV}wasRevisionOf")),
                first_prior.clone(),
                g.clone(),
            );
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
            let idx_q = Quad::new(
                sparkles_core::evidence::review::projection_revision_index_iri(
                    s_nonce,
                    first_prior.as_str(),
                ),
                iri(sparkles_core::evidence::review::INDEXED_PROJECTION),
                final_proj,
                g.clone(),
            );
            let ids = t.encode_quad(&idx_q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }

    // Move `main` to fresh `supports2` / `output2` at version 1 while keeping `SUPPORTS`
    // continuously protected. Because all 1,500 v1 records in `SUPPORTS` are superseded
    // by the v2 record, `main` v1 has no active v1 drift in `SUPPORTS` and may project.
    evidence::detach(&ds, OUTPUT).unwrap();
    let mut s2 = s1.clone();
    s2.projections = vec![output2.into()];
    {
        let p = s2.policies.get_mut("main").unwrap();
        p.supports = supports2.into();
        p.output = output2.into();
    }
    evidence::configure(&ds, &s2, &Control::none()).unwrap();

    let claim_a = claim_of(&propose(
        &ds,
        &occurrence_with("lin", "urn:cve:lin", Polarity::Positive),
    ));
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let cp = checkpoints.clone();
    let control = Control {
        progress: sparkles::task::Progress::new(move |_, stage| {
            if stage == "scanning permission rows" {
                cp.fetch_add(1, Ordering::Relaxed);
            }
        }),
        ..Control::none()
    };
    let mut opts = reviewer();
    opts.if_head = Some(ds.store().head_commit().seq);
    let reviewed = evidence::review(
        None,
        &ds,
        &ds,
        "main",
        1,
        &[decision("lin", &claim_a, Disposition::Accepted, None)],
        &opts,
        &mut |_, _| all(),
        &control,
    )
    .unwrap();
    assert!(reviewed.committed);
    let cp_count = checkpoints.load(Ordering::Relaxed);
    assert!(
        cp_count <= 16,
        "dismissing {REVISIONS} superseded revisions with interleaved foreign revision noise must scale linearly (got {cp_count} checkpoints)"
    );

    // Also verify the guard is non-vacuous: adding one active (unsuperseded) v1
    // projection of `main` in `SUPPORTS` must still be caught as same-version drift.
    {
        let op_uuid = uuid::Uuid::new_v4();
        let op_iri = evidence::operation_iri(op_uuid);
        let cap = ds
            .store()
            .managed_writer(OwnerKind::Evidence, op_uuid)
            .unwrap();
        let mut t = ds.store().write();
        t.set_managed(cap).unwrap();
        let (seq, _) = t.managed_stamp().unwrap();
        let supports_node = iri(SUPPORTS);
        let g = GraphName::NamedNode(supports_node.clone());
        let mut labels = std::collections::HashMap::new();
        let mut add = |sub: NamedNode, pred: NamedNode, obj: Term| {
            let q = Quad::new(sub, pred, obj, g.clone());
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        };
        let s_lit: Term = Literal::new_simple_literal(s_nonce.to_string()).into();
        let d_lit: Term = Literal::new_simple_literal(d_nonce.to_string()).into();
        let o_lit: Term = Literal::new_simple_literal(o_nonce.to_string()).into();
        add(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{EV}Operation")).into(),
        );
        add(
            op_iri.clone(),
            iri(RDF_TYPE),
            iri(&format!("{PROV}Activity")).into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{PROV}wasAssociatedWith")),
            iri("urn:principal:reviewer").into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{PROV}endedAtTime")),
            Literal::new_typed_literal("2026-11-26T00:00:00.000Z", iri(&format!("{XSD}dateTime")))
                .into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{EV}datasetId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()).into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{EV}branchId")),
            Literal::new_simple_literal(ds.store().branch_id().to_string()).into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{EV}commit")),
            Literal::new_typed_literal(seq.to_string(), iri(&format!("{XSD}integer"))).into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{EV}payloadDigest")),
            Literal::new_simple_literal(sha("main-active-v1").to_string()).into(),
        );
        add(
            op_iri.clone(),
            iri(&format!("{EV}graphProtection")),
            s_lit.clone(),
        );

        let active_v1 = iri("urn:main:proj:active-v1");
        let main_policy = evidence::review::policy_iri("main");
        add(
            active_v1.clone(),
            iri(RDF_TYPE),
            iri(&format!("{EV}Projection")).into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}reviewPolicy")),
            main_policy.clone().into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}policyVersion")),
            Literal::new_simple_literal("1").into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}output")),
            iri(OUTPUT).into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}conflictMode")),
            iri(&format!("{EV}Refuse")).into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}decisionsProtection")),
            d_lit,
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}supportsProtection")),
            s_lit.clone(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}outputProtection")),
            o_lit,
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}availabilityReceipt")),
            op_iri.into(),
        );
        add(
            active_v1.clone(),
            iri(&format!("{EV}graphProtection")),
            s_lit,
        );
        for idx_q in sparkles_core::evidence::review::projection_index_quads(
            &active_v1,
            &supports_node,
            s_nonce,
            o_nonce,
            main_policy.as_str(),
            1,
            OUTPUT,
            &[],
        ) {
            let ids = t.encode_quad(&idx_q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }
    let claim_b = claim_of(&propose(
        &ds,
        &occurrence_with("lin2", "urn:cve:lin2", Polarity::Positive),
    ));
    opts.if_head = Some(ds.store().head_commit().seq);
    assert_eq!(
        code(evidence::review(
            None,
            &ds,
            &ds,
            "main",
            1,
            &[decision("lin2", &claim_b, Disposition::Accepted, None)],
            &opts,
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );
}

fn project_lookalike(
    ds: &Dataset,
    policy_name: &str,
    tag: &str,
    op_iri: NamedNode,
    nonce: uuid::Uuid,
    replay_key: Option<&str>,
) {
    use oxrdf::Literal;

    const RDF_TYPE: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#type";
    const PROV: &str = "http://www.w3.org/ns/prov#";
    const XSD: &str = "http://www.w3.org/2001/XMLSchema#";

    let ds_id = ds.dataset_id().to_string();
    let branch_id = ds.store().branch_id().to_string();
    let payload_sha = sha(&format!("legacy-lookalike-{tag}")).to_string();
    let dt = Literal::new_typed_literal("2026-11-26T00:00:00.000Z", iri(&format!("{XSD}dateTime")));
    let commit_int = Literal::new_typed_literal("99", iri(&format!("{XSD}integer")));
    let mut triples = vec![
        (
            "type",
            Triple::new(
                op_iri.clone(),
                iri(RDF_TYPE),
                iri(&format!("{EV}Operation")),
            ),
        ),
        (
            "act",
            Triple::new(
                op_iri.clone(),
                iri(RDF_TYPE),
                iri(&format!("{PROV}Activity")),
            ),
        ),
        (
            "assoc",
            Triple::new(
                op_iri.clone(),
                iri(&format!("{PROV}wasAssociatedWith")),
                iri("urn:principal:reviewer"),
            ),
        ),
        (
            "ended",
            Triple::new(op_iri.clone(), iri(&format!("{PROV}endedAtTime")), dt),
        ),
        (
            "ds",
            Triple::new(
                op_iri.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds_id),
            ),
        ),
        (
            "br",
            Triple::new(
                op_iri.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(branch_id),
            ),
        ),
        (
            "commit",
            Triple::new(op_iri.clone(), iri(&format!("{EV}commit")), commit_int),
        ),
        (
            "digest",
            Triple::new(
                op_iri.clone(),
                iri(&format!("{EV}payloadDigest")),
                Literal::new_simple_literal(payload_sha),
            ),
        ),
        (
            "prot",
            Triple::new(
                op_iri.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(nonce.to_string()),
            ),
        ),
    ];
    if let Some(key) = replay_key {
        triples.push((
            "key",
            Triple::new(
                op_iri,
                iri(&format!("{EV}idempotencyKey")),
                Literal::new_simple_literal(key),
            ),
        ));
    }
    let mut decs = Vec::new();
    for (k, t) in triples {
        let key = format!("{tag}-{k}");
        let input = EvidenceInput {
            revisions: vec![revision(&key)],
            claims: vec![claim("c", Polarity::Positive, t)],
            ..Default::default()
        };
        let c = claim_of(&propose(ds, &input));
        decs.push(decision(&key, &c, Disposition::Accepted, None));
    }
    let mut opts = reviewer();
    opts.if_head = Some(ds.store().head_commit().seq);
    let res = evidence::review(
        None,
        ds,
        ds,
        policy_name,
        1,
        &decs,
        &opts,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(res.committed);
}

// A closed, pre-581 registry retained the nonce of every continuously protected
// graph across role changes. Both configurations in these fixtures own the same graphs.
fn legacy_settings(root: &std::path::Path, settings: &Settings) {
    let path = root.join("managed.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let canonical = settings.canonical(&Control::none()).unwrap();
    raw["format"] = 2.into();
    raw["owners"]["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("projectionOnly");
    raw["owners"]["evidence"]["settingsDigest"] =
        sparkles::managed::settings_digest(canonical.bytes()).into();
    raw["revision"] = (raw["revision"].as_u64().unwrap() + 1).into();
    std::fs::write(root.join("evidence.json"), canonical.bytes()).unwrap();
    std::fs::write(path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
}

// Allow the fixture's ordinary review to write into a former record graph as the
// format-2 writer did. Restore format 2 before exercising replay or withdrawal.
fn legacy_projection_writer(root: &std::path::Path, graph: &str) -> Dataset {
    let path = root.join("managed.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["format"] = 4.into();
    raw["owners"]["evidence"]["projectionOnly"] = serde_json::json!([graph]);
    std::fs::write(path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    Dataset::open(root).unwrap()
}

fn project_legacy_triples(
    ds: &Dataset,
    policy: &str,
    triples: Vec<Triple>,
) -> Vec<(NamedNode, Sha256Digest)> {
    let mut claims = Vec::new();
    let mut decisions = Vec::new();
    for (i, triple) in triples.into_iter().enumerate() {
        let key = format!("legacy-edge-{i}");
        let c = claim_of(&propose(
            ds,
            &EvidenceInput {
                revisions: vec![revision(&key)],
                claims: vec![claim("c", Polarity::Positive, triple)],
                ..Default::default()
            },
        ));
        decisions.push(decision(&key, &c, Disposition::Accepted, None));
        claims.push(c);
    }
    let mut opts = reviewer();
    opts.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        ds,
        ds,
        policy,
        1,
        &decisions,
        &opts,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    claims
}

#[test]
fn legacy_output_with_a_dangling_receipt_rotates_on_promotion() {
    check_legacy_output_edge(true);
}

#[test]
fn legacy_output_with_a_bare_generation_edge_rotates_on_promotion() {
    check_legacy_output_edge(false);
}

fn check_legacy_output_edge(receipt: bool) {
    use sparkles::managed::OwnerKind;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let mut s = settings(ConflictMode::Refuse);
    let ds = Dataset::open(&root).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let nonce = ds.store().managed_graphs().unwrap().unwrap().owners[&OwnerKind::Evidence]
        .protections[OUTPUT];
    let op = iri("urn:example:activity");
    project_lookalike(&ds, "main", "edge", op.clone(), nonce, None);
    let other = iri("urn:example:unrelated");
    let edge = if receipt {
        Triple::new(op, iri(&format!("{EV}availabilityReceipt")), other)
    } else {
        Triple::new(other, iri("http://www.w3.org/ns/prov#wasGeneratedBy"), op)
    };
    project_legacy_triples(&ds, "main", vec![edge]);
    drop(ds);
    legacy_settings(&root, &s);

    let ds = Dataset::open(&root).unwrap();
    assert!(
        reconcile(&ds, "main", Some(1), &reviewer())
            .unwrap()
            .committed
    );
    assert_eq!(accepted(&ds).len(), 10);
    s.supports.push(OUTPUT.into());
    s.projections = vec!["urn:ev:new-output".into()];
    s.policies.get_mut("main").unwrap().output = s.projections[0].clone();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let registry = ds.store().managed_graphs().unwrap().unwrap();
    assert_ne!(
        registry.owners[&OwnerKind::Evidence].protections[OUTPUT],
        nonce
    );
}

#[test]
fn legacy_output_without_history_ignores_more_than_the_replay_probe_limit() {
    use sparkles_core::evidence::review::MAX_EVIDENCE;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let mut s = settings(ConflictMode::Refuse);
    let mut other = s.policies["main"].clone();
    other.decisions = "urn:ev:other-decisions".into();
    other.supports = "urn:ev:other-supports".into();
    other.output = "urn:ev:other-output".into();
    s.decisions.push(other.decisions.clone());
    s.supports.push(other.supports.clone());
    s.projections.push(other.output.clone());
    s.policies.insert("other".into(), other);
    let ds = Dataset::open(&root).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let mut opts = reviewer();
    let key_name = "first-reconciliation";
    opts.idempotency_key = Some(key_name.into());
    let op = evidence::operation_iri(evidence::operation_id(
        ds.dataset_id(),
        &opts.principal,
        key_name,
    ));
    let used = iri("http://www.w3.org/ns/prov#used");
    for start in (0..=MAX_EVIDENCE).step_by(250) {
        let mut input = EvidenceInput {
            revisions: vec![revision(&format!("batch-{start}"))],
            ..Default::default()
        };
        for i in start..(start + 250).min(MAX_EVIDENCE + 1) {
            let name = format!("claim-{i}");
            let triple = Triple::new(op.clone(), used.clone(), iri(&format!("urn:input:{i}")));
            let mut c = claim(&name, Polarity::Positive, triple);
            c.key = key(&name);
            input.claims.push(c);
        }
        let proposed = propose(&ds, &input);
        let decisions: Vec<_> = proposed
            .records
            .iter()
            .filter(|r| r.key.starts_with("claim-"))
            .map(|r| {
                decision(
                    &r.key,
                    &(r.iri.clone(), r.digest),
                    Disposition::Accepted,
                    None,
                )
            })
            .collect();
        assert_eq!(decisions.len(), input.claims.len());
        let mut review_opts = reviewer();
        review_opts.if_head = Some(ds.store().head_commit().seq);
        let reviewed = evidence::review(
            None,
            &ds,
            &ds,
            "other",
            1,
            &decisions,
            &review_opts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        assert!(reviewed.decisions.iter().all(|d| d.support.is_some()));
    }
    drop(ds);
    legacy_settings(&root, &s);
    let ds = Dataset::open(&root).unwrap();
    let first = reconcile(&ds, "main", Some(1), &opts).unwrap();
    assert!(first.committed && !first.replayed);
    assert_eq!(first.operation, op);
}

/// A dataset upgraded from a pre-581 format-2 registry keeps its legacy protection nonces
/// on dedicated projection outputs without `projectionOnly`. Follow-on projecting reviews,
/// reconciliation, narrow-grant keyed replay, and supports resets (both retained and
/// cleared) must handle projected 9-triple `ev:Operation` lookalikes cleanly, marking
/// legacy outputs without genuine record history in `projectionOnly` (format 4) while
/// preserving their existing protection nonces unchanged and keeping genuine former-record
/// graphs protected.
#[test]
fn legacy_v4_dedicated_output_upgrade_review_supports_reset_and_replay() {
    check_legacy_output_replay(None);
}

#[test]
fn legacy_output_with_unrelated_receipt_ignores_projected_key_on_replay() {
    check_legacy_output_replay(Some(LegacyRecord::Source));
}

#[test]
fn legacy_replay_ignores_projected_use_of_another_policys_projection() {
    check_legacy_output_replay(Some(LegacyRecord::Projection));
}

enum LegacyRecord {
    Source,
    Projection,
}

fn check_legacy_output_replay(mixed_history: Option<LegacyRecord>) {
    use sparkles::managed::OwnerKind;

    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let mut s = settings(ConflictMode::Refuse);
    s.decisions.push(decisions2.into());
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "other".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported, ClaimKind::Observation],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into(), OTHER.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );

    if matches!(mixed_history, Some(LegacyRecord::Projection)) {
        s.supports.push("urn:ev:third-output".into());
    }

    // Seed a persistent dataset with a pre-581 format-2 `managed.json` whose dedicated
    // outputs (`OUTPUT`, `output2`) carry legacy version-4 protection nonces and no
    // `projectionOnly` metadata.
    let legacy_out_nonce = uuid::Uuid::new_v4();
    let legacy_out2_nonce = uuid::Uuid::new_v4();
    {
        let ds = Dataset::open(&root).unwrap();
        evidence::configure(&ds, &s, &Control::none()).unwrap();
    }
    {
        let managed_path = root.join("managed.json");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&managed_path).unwrap()).unwrap();
        assert_eq!(raw["format"], 4);
        raw["format"] = 2.into();
        raw["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("projectionOnly");
        raw["owners"]["evidence"]["protections"][OUTPUT] = legacy_out_nonce.to_string().into();
        raw["owners"]["evidence"]["protections"][output2] = legacy_out2_nonce.to_string().into();
        std::fs::write(&managed_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    }

    let ds = Dataset::open(&root).unwrap();
    let ev_entry = |ds: &Dataset| {
        ds.store()
            .managed_graphs()
            .unwrap()
            .unwrap()
            .owners
            .get(&OwnerKind::Evidence)
            .unwrap()
            .clone()
    };
    let e0 = ev_entry(&ds);
    assert_eq!(e0.protections[OUTPUT], legacy_out_nonce);
    assert_eq!(e0.protections[output2], legacy_out2_nonce);
    assert!(!e0.projection_only.contains(OUTPUT));
    assert!(!e0.projection_only.contains(output2));

    // 1. Project a 9-triple lookalike into legacy OUTPUT and verify a follow-on
    // projecting review and keyed reconciliation both succeed (not immutable-record-conflict).
    project_lookalike(
        &ds,
        "main",
        "v4-init",
        iri("urn:proj:v4-op"),
        legacy_out_nonce,
        None,
    );
    let follow = claim_of(&propose(
        &ds,
        &occurrence_with("v4-follow", "urn:cve:v4-follow", Polarity::Positive),
    ));
    let follow_res = review(
        &ds,
        &[decision("v4-follow", &follow, Disposition::Accepted, None)],
    )
    .unwrap();
    assert!(follow_res.committed);

    let mut keyed_opts = reviewer();
    keyed_opts.idempotency_key = Some("legacy-v4-reconcile-key".into());
    let first = reconcile(&ds, "main", Some(1), &keyed_opts).unwrap();
    assert!(first.committed && !first.replayed);

    // 2. Project the description and key of `first.operation` with a different digest into
    // legacy `output2` via `other`. Narrow-grant keyed replay on `main` (without read
    // grants on `supports2` / `output2`) must succeed without conflict or NotPermitted.
    project_lookalike(
        &ds,
        "other",
        "v4-shadow",
        first.operation.clone(),
        legacy_out2_nonce,
        keyed_opts.idempotency_key.as_deref(),
    );
    let ds = if matches!(mixed_history, Some(LegacyRecord::Projection)) {
        drop(ds);
        let mut swapped = s.clone();
        swapped.supports.retain(|g| g != "urn:ev:third-output");
        swapped.projections.push("urn:ev:third-output".into());
        swapped.supports.push(output2.into());
        swapped.projections.retain(|g| g != output2);
        let mut third = swapped.policies.remove("other").unwrap();
        third.output = "urn:ev:third-output".into();
        third.supports = output2.into();
        swapped.policies.insert("third".into(), third);
        legacy_settings(&root, &swapped);
        let ds = Dataset::open(&root).unwrap();
        let third = reconcile(&ds, "third", Some(1), &reviewer()).unwrap();
        assert!(third.committed);
        drop(ds);
        legacy_settings(&root, &s);
        let ds = legacy_projection_writer(&root, output2);
        project_legacy_triples(
            &ds,
            "other",
            vec![Triple::new(
                first.operation.clone(),
                iri("http://www.w3.org/ns/prov#used"),
                third.projection.clone(),
            )],
        );
        assert!(
            ds.ask(&format!(
                "ASK {{ GRAPH <{output2}> {{ {} <http://www.w3.org/ns/prov#used> {} }} }}",
                first.operation, third.projection
            ))
            .unwrap()
        );
        drop(ds);
        legacy_settings(&root, &s);
        Dataset::open(&root).unwrap()
    } else if mixed_history.is_some() {
        drop(ds);
        let mut swapped = s.clone();
        swapped.policies.remove("other");
        swapped.projections.retain(|g| g != output2);
        swapped.source_records.push(output2.into());
        legacy_settings(&root, &swapped);
        let ds = Dataset::open(&root).unwrap();
        let mut opts = ProposeOptions::new(iri("urn:principal:importer"));
        opts.source_graph = Some(iri(output2));
        let unrelated = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                revisions: vec![revision("unrelated")],
                ..Default::default()
            },
            &opts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        assert_ne!(unrelated.operation, first.operation);
        drop(ds);
        legacy_settings(&root, &s);
        Dataset::open(&root).unwrap()
    } else {
        ds
    };
    let main_only = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new(
            [SOURCES, CLAIMS, OTHER, DECISIONS, SUPPORTS, OUTPUT],
            &[],
        )),
        Graphs::Only(GraphRule::new([SUPPORTS, OUTPUT], &[])),
    ));
    let mut narrow_replay = keyed_opts.clone();
    narrow_replay.write = Some(main_only.clone());
    let head_before_replay = ds.store().head_commit().seq;
    let replayed1 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed1.committed && replayed1.replayed);
    assert_eq!(replayed1.commit, first.commit);
    assert_eq!(replayed1.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_before_replay);

    if mixed_history.is_some() {
        return;
    }

    // 3. Supports reset (retained, then cleared) on `supports2` marks legacy `output2`
    // as `projectionOnly` (format 4) while preserving its existing protection nonce,
    // keeping `SUPPORTS` intact and narrow-grant keyed replay on `main` returning `first`.
    evidence::detach(&ds, supports2).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let e1 = ev_entry(&ds);
    assert_eq!(e1.protections[output2], legacy_out2_nonce);
    assert!(e1.projection_only.contains(output2));
    assert_eq!(e1.protections[OUTPUT], legacy_out_nonce);
    assert!(e1.projection_only.contains(OUTPUT));
    let managed_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("managed.json")).unwrap()).unwrap();
    assert_eq!(managed_json["format"], 4);

    let head_after_s2_retain = ds.store().head_commit().seq;
    let replayed2 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only.clone()),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed2.committed && replayed2.replayed);
    assert_eq!(replayed2.commit, first.commit);
    assert_eq!(replayed2.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_after_s2_retain);

    evidence::detach(&ds, supports2).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{supports2}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    // Reopen the cleared state with the registry a pre-581 writer would leave.
    drop(ds);
    legacy_settings(&root, &s);
    let ds = Dataset::open(&root).unwrap();
    let head_after_s2_clear = ds.store().head_commit().seq;
    let replayed3 = evidence::reconcile_projection(
        &ds,
        &ds,
        "main",
        Some(1),
        &narrow_replay,
        Some(main_only),
        &Control::none(),
    )
    .unwrap();
    assert!(!replayed3.committed && replayed3.replayed);
    assert_eq!(replayed3.commit, first.commit);
    assert_eq!(replayed3.projection, first.projection);
    assert_eq!(ds.store().head_commit().seq, head_after_s2_clear);

    // 4. Supports reset (retained) on SUPPORTS preserves OUTPUT's nonce, keeps OUTPUT
    // in `projectionOnly`, and lets `reconcile_projection` empty OUTPUT cleanly.
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let e2 = ev_entry(&ds);
    assert_eq!(e2.protections[OUTPUT], legacy_out_nonce);
    assert!(e2.projection_only.contains(OUTPUT));

    let rec_retain = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec_retain.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "reconcile_projection after SUPPORTS reset (retained) must empty legacy OUTPUT"
    );

    // 5. Supports reset (cleared) on SUPPORTS after projecting another lookalike also
    // empties OUTPUT cleanly.
    project_lookalike(
        &ds,
        "main",
        "v4-clear",
        iri("urn:proj:v4-clear-op"),
        legacy_out_nonce,
        None,
    );
    evidence::detach(&ds, SUPPORTS).unwrap();
    ds.update(&format!(
        "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"
    ))
    .unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let rec_clear = reconcile(&ds, "main", Some(1), &reviewer()).unwrap();
    assert!(rec_clear.committed);
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{OUTPUT}> {{ ?s ?p ?o }} }}"))
            .unwrap(),
        "reconcile_projection after SUPPORTS reset (cleared) must empty OUTPUT"
    );
}

/// Regression for Neon r9 P2-1: on an upgraded pre-581 format-2 dataset with an active
/// matching policy (`matchingPolicies.*.output`), identical or unrelated `evidence::configure`
/// calls must mark the matching-policy output `projectionOnly` (format 4) WITHOUT rotating
/// its protection nonce, keeping `identity::resolve` and `identity::review` healthy.
#[test]
fn legacy_format2_matching_policy_output_preserves_nonce_across_configure() {
    use sparkles::evidence::{IdentityInput, IdentityRelation, MatchingPolicy};
    use sparkles::managed::OwnerKind;

    let identity = "urn:ev:identity";
    let maps = "urn:ev:maps";
    let map_out = "urn:ev:map";
    let decisions2 = "urn:ev:decisions2";
    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let mut s = settings(ConflictMode::Refuse);
    s.identity = vec![identity.into(), maps.into()];
    s.projections.push(map_out.into());
    s.matching_policies.insert(
        "people".into(),
        MatchingPolicy {
            version: 1,
            boundary: vec![identity.into(), SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            maps: maps.into(),
            output: map_out.into(),
        },
    );

    // Seed a persistent format-2 legacy dataset (no `projectionOnly`) and publish an
    // active resolution map revision under `people`.
    let legacy_map_nonce = uuid::Uuid::new_v4();
    {
        let ds = Dataset::open(&root).unwrap();
        evidence::configure(&ds, &s, &Control::none()).unwrap();
    }
    {
        let managed_path = root.join("managed.json");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&managed_path).unwrap()).unwrap();
        assert_eq!(raw["format"], 4);
        raw["format"] = 2.into();
        raw["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("projectionOnly");
        raw["owners"]["evidence"]["protections"][map_out] = legacy_map_nonce.to_string().into();
        std::fs::write(&managed_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    }

    let ds = Dataset::open(&root).unwrap();
    let propose_match = |occurrence: &str, a: &str, b: &str| {
        let mut po = ProposeOptions::new(iri("urn:principal:matcher"));
        po.identity_graph = Some(iri(identity));
        let p = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                identities: vec![IdentityInput {
                    version: RecordVersion::V1,
                    key: key("p"),
                    occurrence_key: key(occurrence),
                    entities: vec![iri(a), iri(b)],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: Some(iri(a)),
                    attributed_author: iri("urn:matcher"),
                    method: None,
                }],
                ..Default::default()
            },
            &po,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let r = p.records.iter().find(|r| r.key == "p").unwrap();
        (r.iri.clone(), r.digest)
    };
    let p1 = propose_match("m1", "urn:person:alice", "urn:person:alice-alias");
    let p2 = propose_match("m2", "urn:person:bob", "urn:person:bob-alias");

    let mut ro = reviewer();
    ro.if_head = Some(ds.store().head_commit().seq);
    let r1 = evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &[decision("d1", &p1, Disposition::Accepted, None)],
        &ro,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(r1.committed);
    assert!(r1.map_revision.is_some());

    let resolved_before = evidence::resolve_entities(
        &ds,
        &ds,
        "people",
        &[iri("urn:person:alice-alias")],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        resolved_before.entities[0].representative,
        iri("urn:person:alice")
    );

    // 1. Re-applying identical settings must keep `map_out`'s protection nonce unchanged
    // while upgrading it into `projectionOnly` (format 4).
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let entry_after_noop = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .clone();
    assert_eq!(entry_after_noop.protections[map_out], legacy_map_nonce);
    assert!(entry_after_noop.projection_only.contains(map_out));
    let managed_json: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("managed.json")).unwrap()).unwrap();
    assert_eq!(managed_json["format"], 4);

    let resolved_after_noop = evidence::resolve_entities(
        &ds,
        &ds,
        "people",
        &[iri("urn:person:alice-alias")],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        resolved_after_noop.entities[0].representative,
        iri("urn:person:alice")
    );

    // 2. An unrelated settings edit (adding another review policy) must also keep
    // `map_out`'s protection nonce unchanged and keep both resolve and follow-on matching
    // review healthy (never `projection-unavailable`).
    let mut edited = s.clone();
    edited.decisions.push(decisions2.into());
    edited.supports.push(supports2.into());
    edited.projections.push(output2.into());
    edited.policies.insert(
        "secondary".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: decisions2.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );
    evidence::configure(&ds, &edited, &Control::none()).unwrap();
    let entry_after_edit = ds
        .store()
        .managed_graphs()
        .unwrap()
        .unwrap()
        .owners
        .get(&OwnerKind::Evidence)
        .unwrap()
        .clone();
    assert_eq!(entry_after_edit.protections[map_out], legacy_map_nonce);
    assert!(entry_after_edit.projection_only.contains(map_out));

    let resolved_after_edit = evidence::resolve_entities(
        &ds,
        &ds,
        "people",
        &[iri("urn:person:alice-alias")],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        resolved_after_edit.entities[0].representative,
        iri("urn:person:alice")
    );

    let mut ro2 = reviewer();
    ro2.if_head = Some(ds.store().head_commit().seq);
    let r2 = evidence::review(
        None,
        &ds,
        &ds,
        "people",
        1,
        &[decision("d2", &p2, Disposition::Accepted, None)],
        &ro2,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(r2.committed);
    let resolved_bob = evidence::resolve_entities(
        &ds,
        &ds,
        "people",
        &[iri("urn:person:bob-alias")],
        all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(
        resolved_bob.entities[0].representative,
        iri("urn:person:bob")
    );
}

/// Regression for Neon r9 P2-2: on an upgraded pre-581 format-2 dataset, a same-version
/// rival output takeover (`main` v1 taking `second`'s undetached `output2`) AND a
/// subsequent detach/re-protect of challenger `main`'s `SUPPORTS` must preserve
/// `output2`'s protection nonce so `has_same_version_projection_drift` still refuses
/// `main` with `projection-unavailable` and preserves `second`'s accepted output.
#[test]
fn legacy_format2_review_output_preserves_rival_drift_on_takeover_and_detach() {
    use sparkles::managed::OwnerKind;

    let supports2 = "urn:ev:supports2";
    let output2 = "urn:ev:accepted2";

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let mut s = settings(ConflictMode::Refuse);
    s.supports.push(supports2.into());
    s.projections.push(output2.into());
    s.policies.insert(
        "second".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: Vec::new(),
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: supports2.into(),
            output: output2.into(),
        },
    );

    let legacy_out2_nonce = uuid::Uuid::new_v4();
    {
        let ds = Dataset::open(&root).unwrap();
        evidence::configure(&ds, &s, &Control::none()).unwrap();
    }
    {
        let managed_path = root.join("managed.json");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&managed_path).unwrap()).unwrap();
        assert_eq!(raw["format"], 4);
        raw["format"] = 2.into();
        raw["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("projectionOnly");
        raw["owners"]["evidence"]["protections"][output2] = legacy_out2_nonce.to_string().into();
        std::fs::write(&managed_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    }

    let ds = Dataset::open(&root).unwrap();
    let c_second = claim_of(&propose(
        &ds,
        &occurrence_with("s-second", "urn:cve:second", Polarity::Positive),
    ));
    let c_main = claim_of(&propose(
        &ds,
        &occurrence_with("s-main", "urn:cve:main", Polarity::Positive),
    ));
    let mut o_second = reviewer();
    o_second.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "second",
        1,
        &[decision("c-second", &c_second, Disposition::Accepted, None)],
        &o_second,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve:second> }} }}"
        ))
        .unwrap()
    );

    // 1. Same-version rival takeover: `main` (v1, no prior projection) takes `output2`
    // while `second` (v1) moves to `OUTPUT` and `supports2` stays continuously protected.
    let mut takeover = s.clone();
    takeover.policies.get_mut("main").unwrap().output = output2.into();
    takeover.policies.get_mut("second").unwrap().output = OUTPUT.into();
    evidence::configure(&ds, &takeover, &Control::none()).unwrap();

    let ev_entry = |ds: &Dataset| {
        ds.store()
            .managed_graphs()
            .unwrap()
            .unwrap()
            .owners
            .get(&OwnerKind::Evidence)
            .unwrap()
            .clone()
    };
    let e_after_takeover = ev_entry(&ds);
    assert_eq!(e_after_takeover.protections[output2], legacy_out2_nonce);
    assert!(e_after_takeover.projection_only.contains(output2));

    let head1 = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(review(
            &ds,
            &[decision("c-main", &c_main, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head1);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve:second> }} }}"
        ))
        .unwrap(),
        "second's undetached output must survive rival takeover"
    );

    // 2. Detaching and re-protecting challenger `main`'s `SUPPORTS` must NOT rotate
    // `output2`'s nonce or bypass `second`'s rival drift guard.
    evidence::detach(&ds, SUPPORTS).unwrap();
    evidence::configure(&ds, &takeover, &Control::none()).unwrap();
    assert_eq!(ev_entry(&ds).protections[output2], legacy_out2_nonce);

    let head2 = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(
        code(review(
            &ds,
            &[decision("c-main2", &c_main, Disposition::Accepted, None)]
        )),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head2);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve:second> }} }}"
        ))
        .unwrap(),
        "second's undetached output must survive challenger SUPPORTS detach"
    );

    // 3. Also test the direct detach path on a still-format-2 takeover registry (as if
    // the takeover happened before upgrade and `detach(SUPPORTS)` is the very first
    // mutation after upgrade): `detach_managed_checked` must mark `output2` in
    // `projectionOnly` without rotating `legacy_out2_nonce`.
    drop(ds);
    {
        let managed_path = root.join("managed.json");
        let mut raw: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&managed_path).unwrap()).unwrap();
        raw["format"] = 2.into();
        raw["owners"]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("projectionOnly");
        std::fs::write(&managed_path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();
    }
    let ds = Dataset::open(&root).unwrap();
    assert!(!ev_entry(&ds).projection_only.contains(output2));
    evidence::detach(&ds, SUPPORTS).unwrap();
    let e_after_direct_detach = ev_entry(&ds);
    assert_eq!(
        e_after_direct_detach.protections[output2],
        legacy_out2_nonce
    );
    assert!(e_after_direct_detach.projection_only.contains(output2));
    evidence::configure(&ds, &takeover, &Control::none()).unwrap();
    assert_eq!(ev_entry(&ds).protections[output2], legacy_out2_nonce);

    let head3 = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "projection-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head3);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{output2}> {{ <urn:release> <urn:affectedBy> <urn:cve:second> }} }}"
        ))
        .unwrap()
    );
}

/// A pre-581 format-2 dataset whose `supports` graph was already reset (detached, cleared,
/// and re-protected) before upgrade must still reconcile and empty its legacy output
/// cleanly after upgrade, preserving its nonce when promoted to `projectionOnly`.
#[test]
fn legacy_format2_pre_upgrade_supports_reset_reconciles_cleanly() {
    use sparkles::managed::OwnerKind;

    for configure_first in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ds");
        let s = settings(ConflictMode::Refuse);
        let ds = Dataset::open(&root).unwrap();
        evidence::configure(&ds, &s, &Control::none()).unwrap();
        let nonce = ds.store().managed_graphs().unwrap().unwrap().owners[&OwnerKind::Evidence]
            .protections[OUTPUT];
        let op = evidence::operation_iri(uuid::Uuid::new_v4());
        project_lookalike(&ds, "main", "pre-reset", op, nonce, None);
        assert_eq!(accepted(&ds).len(), 9);
        evidence::detach(&ds, SUPPORTS).unwrap();
        ds.update(&format!(
            "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ ?s ?p ?o }} }}"
        ))
        .unwrap();
        evidence::configure(&ds, &s, &Control::none()).unwrap();
        drop(ds);
        legacy_settings(&root, &s);

        let ds = Dataset::open(&root).unwrap();
        if configure_first {
            evidence::configure(&ds, &s, &Control::none()).unwrap();
            let registry = ds.store().managed_graphs().unwrap().unwrap();
            assert!(
                registry.owners[&OwnerKind::Evidence]
                    .projection_only
                    .contains(OUTPUT)
            );
            assert_eq!(
                registry.owners[&OwnerKind::Evidence].protections[OUTPUT],
                nonce
            );
        }
        assert!(
            reconcile(&ds, "main", Some(1), &reviewer())
                .unwrap()
                .committed
        );
        assert!(accepted(&ds).is_empty());
    }
}

#[test]
fn legacy_format2_projected_nonce_also_belongs_to_a_published_proposal() {
    check_legacy_dual_origin(DualOrigin::Nonce);
    check_legacy_dual_origin(DualOrigin::ExtraDigest);
}

#[test]
fn legacy_output_preserves_receipt_after_projected_generation_edge_is_withdrawn() {
    check_legacy_dual_origin(DualOrigin::WithdrawGeneration);
}

enum DualOrigin {
    Nonce,
    ExtraDigest,
    WithdrawGeneration,
}

fn check_legacy_dual_origin(case: DualOrigin) {
    use sparkles::managed::OwnerKind;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("ds");
    let s = settings(ConflictMode::Refuse);
    let ds = Dataset::open(&root).unwrap();
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let nonce = ds.store().managed_graphs().unwrap().unwrap().owners[&OwnerKind::Evidence]
        .protections[OUTPUT];
    let principal = iri("urn:principal:importer");
    let key = "dual-origin";
    let op = evidence::operation_iri(evidence::operation_id(ds.dataset_id(), &principal, key));
    let marker = Triple::new(
        op.clone(),
        iri(&format!("{EV}graphProtection")),
        oxrdf::Literal::new_simple_literal(nonce.to_string()),
    );
    let mut triples = vec![match case {
        DualOrigin::WithdrawGeneration => affected(),
        _ => marker,
    }];
    if matches!(case, DualOrigin::ExtraDigest) {
        triples.push(Triple::new(
            op.clone(),
            iri(&format!("{EV}payloadDigest")),
            oxrdf::Literal::new_simple_literal(sha("projected").to_string()),
        ));
    }
    let mut decisions = Vec::new();
    for (i, triple) in triples.into_iter().enumerate() {
        let tag = format!("marker-{i}");
        let input = EvidenceInput {
            revisions: vec![revision(&tag)],
            claims: vec![claim("c", Polarity::Positive, triple)],
            ..Default::default()
        };
        let c = claim_of(&propose(&ds, &input));
        decisions.push(decision(&tag, &c, Disposition::Accepted, None));
    }
    review(&ds, &decisions).unwrap();
    assert_eq!(accepted(&ds).len(), decisions.len());
    drop(ds);

    let initial_registry = std::fs::read(root.join("managed.json")).unwrap();
    let mut swapped = s.clone();
    swapped.source_records = vec![OUTPUT.into()];
    swapped.projections = vec![SOURCES.into()];
    let policy = swapped.policies.get_mut("main").unwrap();
    policy.output = SOURCES.into();
    policy.boundary = vec![OUTPUT.into(), CLAIMS.into(), OTHER.into()];
    legacy_settings(&root, &swapped);
    let ds = Dataset::open(&root).unwrap();
    let mut opts = ProposeOptions::new(principal);
    opts.idempotency_key = Some(key.into());
    let receipt = evidence::propose(
        None,
        &ds,
        &ds,
        &EvidenceInput {
            revisions: vec![revision("dual-origin")],
            ..Default::default()
        },
        &opts,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(receipt.operation, op);
    let record = &receipt.records[0].iri;
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{OUTPUT}> {{ {record} <{EV}availabilityReceipt> {op} }} }}"
        ))
        .unwrap()
    );
    drop(ds);
    legacy_settings(&root, &s);

    if matches!(case, DualOrigin::WithdrawGeneration) {
        // The pre-581 reviewer could project into a former record graph. Restore the
        // initial registry for this review so the newer history guard is skipped.
        std::fs::write(root.join("managed.json"), initial_registry).unwrap();
        let ds = Dataset::open(&root).unwrap();
        let generated_by = iri("http://www.w3.org/ns/prov#wasGeneratedBy");
        let edge = Triple::new(record.clone(), generated_by.clone(), op.clone());
        let claim = claim_of(&propose(
            &ds,
            &EvidenceInput {
                revisions: vec![revision("generation-edge")],
                claims: vec![claim("c", Polarity::Positive, edge)],
                ..Default::default()
            },
        ));
        review(
            &ds,
            &[decision("generation", &claim, Disposition::Accepted, None)],
        )
        .unwrap();
        drop(ds);
        legacy_settings(&root, &s);
        let ds = Dataset::open(&root).unwrap();
        let generation = format!("ASK {{ GRAPH <{OUTPUT}> {{ {record} {generated_by} {op} }} }}");
        assert!(ds.ask(&generation).unwrap());
        withdraw(&ds, &claim);
        assert!(!ds.ask(&generation).unwrap());
        assert!(
            ds.ask(&format!(
                "ASK {{ GRAPH <{OUTPUT}> {{ {record} <{EV}availabilityReceipt> {op} }} }}"
            ))
            .unwrap()
        );
    }
    let ds = Dataset::open(&root).unwrap();
    let history = accepted(&ds);
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(accepted(&ds), history);
    evidence::configure(&ds, &s, &Control::none()).unwrap();
    let registry = ds.store().managed_graphs().unwrap().unwrap();
    let entry = &registry.owners[&OwnerKind::Evidence];
    assert_eq!(entry.protections[OUTPUT], nonce);
    assert!(!entry.projection_only.contains(OUTPUT));
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(reconcile(&ds, "main", Some(1), &reviewer())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(accepted(&ds), history);
}
