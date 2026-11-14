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
