//! Authenticated claim proposals (spec C25 §5 and §7): one batch publishes as one
//! commit of the evidence owner with server-owned receipts, first availability, record
//! digests and the operation marker, after the caller's grants, the references, the
//! idempotency key, the expected head and the caller's authority are checked.

use oxrdf::{Literal, NamedNode, Term, Triple};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs};
use sparkles::evidence::resolve::{GraphIdentity, StoredFactRef};
use sparkles::evidence::resolve::{RecordKind, RecordRequest, Resolution};
use sparkles::evidence::{
    self, AssessmentInput, AuthorityFence, ClaimInput, ClaimKind, EvidenceInput, EvidenceLinkInput,
    EvidenceRefInput, EvidenceRefTarget, EvidenceRelation, MethodRef, PassageInput, Polarity,
    ProposeOptions, Proposed, RecordKey, RecordRef, RecordVersion, RenditionContent,
    RenditionInput, Settings, Sha256Digest, SourceContent, SourceRevisionInput,
};
use sparkles::managed::OwnerKind;
use sparkles::task::Control;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const FACTS: &str = "urn:facts";
const TEXT: &str = "Release 1.2 is affected by CVE-1.";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn key(s: &str) -> RecordKey {
    RecordKey::new(s).unwrap()
}

fn local(s: &str) -> RecordRef {
    RecordRef::Local(key(s))
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
        Ok(v) => panic!("proposed: {v:?}"),
    }
}

/// A persistent dataset, whose history retains the commits stored facts name.
fn retained() -> (tempfile::TempDir, Dataset) {
    let dir = tempfile::tempdir().unwrap();
    let ds = Dataset::open(dir.path()).unwrap();
    (dir, ds)
}

fn settings() -> Settings {
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        ..Settings::default()
    }
}

fn configured(ds: &Dataset) {
    evidence::configure(ds, &settings(), &Control::none()).unwrap();
}

/// A dataset with one fact at a commit that later commits remove.
fn with_fact(ds: &Dataset) -> u64 {
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{FACTS}> {{ <urn:release> <urn:affectedBy> <urn:cve> }} }}"
    ))
    .unwrap();
    let commit = ds.store().head_commit().seq;
    ds.update(&format!(
        "DELETE DATA {{ GRAPH <{FACTS}> {{ <urn:release> <urn:affectedBy> <urn:cve> }} }}"
    ))
    .unwrap();
    commit
}

fn method(m: &str) -> MethodRef {
    MethodRef {
        method: iri(m),
        version: key("1"),
    }
}

fn proposition() -> Triple {
    Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:cve"))
}

/// A revision with its retained bytes, a rendition, a passage, a stored-fact and a
/// passage reference, a claim, a link and an assessment.
fn batch(ds: &Dataset, fact_commit: u64) -> EvidenceInput {
    let selected = &TEXT[0..11];
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("urn:bulletin"),
            source_key: key("bulletin"),
            upstream_version: None,
            content: SourceContent::Retained {
                bytes: TEXT.as_bytes().to_vec(),
                expected_digest: Some(sha(TEXT)),
            },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("text"),
            revision: local("rev"),
            occurrence_key: key("text"),
            conversion: method("urn:method:identity"),
            media_type: "text/plain".into(),
            language: Some("en".into()),
            content: RenditionContent::NormalizedText(TEXT.into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("span"),
            revision: local("rev"),
            rendition: local("text"),
            occurrence_key: key("span"),
            start: 0,
            end: 11,
            text_digest: sha(selected),
            quote: Some(selected.into()),
        }],
        references: vec![
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("fact"),
                occurrence_key: key("fact"),
                target: EvidenceRefTarget::StoredFact(StoredFactRef {
                    dataset: ds.dataset_id(),
                    branch: ds.dataset_id(),
                    commit: fact_commit,
                    graph: GraphIdentity::Named(iri(FACTS)),
                    expected: proposition(),
                    anchor: None,
                }),
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("passageRef"),
                occurrence_key: key("passageRef"),
                target: EvidenceRefTarget::Passage {
                    revision: local("rev"),
                    rendition: local("text"),
                    passage: local("span"),
                },
            },
        ],
        claims: vec![claim("claim", "c1", proposition())],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link"),
            occurrence_key: key("link"),
            target: local("claim"),
            evidence: local("passageRef"),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("urn:importer"),
        }],
        assessments: vec![AssessmentInput {
            version: RecordVersion::V1,
            key: key("assessment"),
            occurrence_key: key("a1"),
            target: local("claim"),
            conclusion: Term::NamedNode(iri("urn:conclusion:plausible")),
            method: method("urn:method:review"),
            configuration_digest: sha("config"),
            evaluated_at: Literal::new_typed_literal(
                "2026-01-01T10:00:00Z",
                iri("http://www.w3.org/2001/XMLSchema#dateTime"),
            ),
            attributed_evaluator: iri("urn:analyst"),
            confidence: None,
            scope: Default::default(),
            inputs: Vec::new(),
        }],
        identities: Vec::new(),
    }
}

fn claim(alias: &str, occurrence: &str, proposition: Triple) -> ClaimInput {
    ClaimInput {
        version: RecordVersion::V1,
        key: key(alias),
        occurrence_key: key(occurrence),
        revision: local("rev"),
        proposition,
        kind: ClaimKind::Reported,
        polarity: Polarity::Positive,
        attributed_author: iri("urn:publisher"),
        scope: Default::default(),
        confidence: Default::default(),
    }
}

/// Just a revision and a claim of it.
fn small(occurrence: &str, proposition: Triple) -> EvidenceInput {
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("urn:bulletin"),
            source_key: key("bulletin"),
            upstream_version: None,
            content: SourceContent::MetadataOnly { digest: sha(TEXT) },
            metadata: Default::default(),
            times: Default::default(),
            lineage: Default::default(),
        }],
        claims: vec![claim("claim", occurrence, proposition)],
        ..Default::default()
    }
}

fn options() -> ProposeOptions {
    ProposeOptions::new(iri("urn:principal:alice"))
}

fn propose(ds: &Dataset, input: &EvidenceInput, o: &ProposeOptions) -> sparkles::Result<Proposed> {
    evidence::propose(None, ds, ds, input, o, &mut |_, _| all(), &Control::none())
}

fn record(
    ds: &Dataset,
    iri_: &NamedNode,
    kind: RecordKind,
) -> sparkles::Result<evidence::resolve::VerifiedRecord> {
    Resolution::new(Control::none()).unwrap().record(
        "r",
        ds,
        ds,
        all(),
        &RecordRequest {
            iri: iri_.clone(),
            kinds: vec![kind],
            content_digest: None,
        },
    )
}

fn iri_of(p: &Proposed, alias: &str) -> NamedNode {
    p.records
        .iter()
        .find(|r| r.key == alias)
        .unwrap()
        .iri
        .clone()
}

#[test]
fn a_batch_publishes_authenticated_records_in_one_commit() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let before = ds.store().head_commit().seq;
    let input = batch(&ds, fact);
    let p = propose(&ds, &input, &options()).unwrap();
    assert!(p.committed && !p.replayed);
    assert_eq!(p.commit, before + 1, "one commit");
    assert_eq!(ds.store().head_commit().seq, before + 1);
    assert_eq!(p.records.len(), 8);
    assert!(p.hold.is_some());
    // every record resolves as an authenticated profile record with its receipt
    for r in &p.records {
        let v = record(&ds, &r.iri, r.kind).unwrap();
        assert_eq!(v.graph, r.graph);
        assert_eq!(v.receipt.iri, p.operation);
        assert_eq!(v.receipt.commit, p.commit);
        assert_eq!(v.record_digest, Some(r.digest));
    }
    let span = record(&ds, &iri_of(&p, "span"), RecordKind::Passage).unwrap();
    assert_eq!(
        (span.span.as_ref().unwrap().start, span.span.unwrap().end),
        (0, 11)
    );
    // the source graph has the revision, rendition and passage; claims the rest
    let graph = |alias: &str| {
        p.records
            .iter()
            .find(|r| r.key == alias)
            .unwrap()
            .graph
            .as_str()
            .to_string()
    };
    assert_eq!(graph("rev"), SOURCES);
    assert_eq!(graph("span"), SOURCES);
    assert_eq!(graph("claim"), CLAIMS);
    assert_eq!(graph("fact"), CLAIMS);
    // first availability is the commit's own time, set by the server
    let rev = iri_of(&p, "rev");
    let r = ds
        .select(&format!(
            "SELECT ?t WHERE {{ GRAPH <{SOURCES}> {{ <{}> <urn:x-sparkles:evidence:firstAvailableAt> ?t }} }}",
            rev.as_str()
        ))
        .unwrap();
    assert_eq!(r.len(), 1);
    assert_eq!(
        r.iter().next().unwrap().get("t").unwrap().to_string(),
        format!(
            "\"{}\"^^<http://www.w3.org/2001/XMLSchema#dateTime>",
            p.first_available_at.as_deref().unwrap()
        )
    );
    // the proposition is described, never asserted
    assert!(
        !ds.ask("ASK { <urn:release> <urn:affectedBy> <urn:cve> }")
            .unwrap()
    );
    // the creation activity's agent is the authenticated principal
    assert!(ds.ask(&format!(
        "ASK {{ GRAPH <{CLAIMS}> {{ <{}> a <urn:x-sparkles:evidence:Operation> ; <http://www.w3.org/ns/prov#wasAssociatedWith> <urn:principal:alice> }} }}",
        p.operation.as_str()
    )).unwrap());
    // the referenced fact's commit is held under the operation's hold
    let holds = evidence::holds(&ds).unwrap();
    assert_eq!(holds.len(), 1);
    assert_eq!(Some(holds[0].id), p.hold);
    assert!(evidence::operation_recorded(&ds, holds[0].id, &Control::none()).unwrap());
}

#[test]
fn a_dry_run_writes_nothing_and_holds_nothing() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let head = ds.store().head_commit().seq;
    let mut o = options();
    o.dry_run = true;
    let p = propose(&ds, &batch(&ds, fact), &o).unwrap();
    assert!(!p.committed && p.preview.is_some());
    assert_eq!(p.commit, head);
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(evidence::holds(&ds).unwrap().is_empty());
}

#[test]
fn a_repeated_key_returns_the_first_receipt_and_a_changed_payload_conflicts() {
    let ds = Dataset::memory();
    configured(&ds);
    let mut o = options();
    o.idempotency_key = Some("k1".into());
    let first = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(first.committed);
    let head = ds.store().head_commit().seq;
    let again = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(again.replayed && !again.committed);
    assert_eq!(again.commit, first.commit);
    assert_eq!(again.operation, first.operation);
    assert_eq!(again.first_available_at, first.first_available_at);
    assert_eq!(again.records, first.records);
    assert_eq!(
        ds.store().head_commit().seq,
        head,
        "a replay writes nothing"
    );
    let changed = Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:other"));
    assert_eq!(
        code(propose(&ds, &small("c1", changed), &o)),
        "idempotency-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    // the key is the principal's own
    let mut bob = ProposeOptions::new(iri("urn:principal:bob"));
    bob.idempotency_key = Some("k1".into());
    let other = propose(&ds, &small("c1", proposition()), &bob).unwrap();
    assert!(other.committed);
    assert_ne!(other.operation, first.operation);
}

#[test]
fn a_stale_head_is_refused_without_a_write() {
    let ds = Dataset::memory();
    configured(&ds);
    let mut o = options();
    o.if_head = Some(ds.store().head_commit().seq);
    ds.update("INSERT DATA { <urn:x> <urn:y> <urn:z> }")
        .unwrap();
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &o)),
        "head-changed"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    o.if_head = Some(head);
    assert!(
        propose(&ds, &small("c1", proposition()), &o)
            .unwrap()
            .committed
    );
}

#[test]
fn equal_content_is_not_written_again_and_other_content_conflicts() {
    let ds = Dataset::memory();
    configured(&ds);
    let first = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    // another operation of the same principal with the same identities and content
    let second = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    assert!(second.committed);
    assert_ne!(second.operation, first.operation);
    assert!(second.records.iter().all(|r| r.existing));
    assert_eq!(
        second.records,
        first
            .records
            .iter()
            .cloned()
            .map(|mut r| {
                r.existing = true;
                r
            })
            .collect::<Vec<_>>()
    );
    // the record keeps its first receipt
    let claim = iri_of(&first, "claim");
    assert_eq!(
        record(&ds, &claim, RecordKind::Claim).unwrap().receipt.iri,
        first.operation
    );
    // the same identity with another proposition
    let head = ds.store().head_commit().seq;
    let changed = Triple::new(iri("urn:release"), iri("urn:affectedBy"), iri("urn:other"));
    assert_eq!(
        code(propose(&ds, &small("c1", changed), &options())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

#[test]
fn input_and_profile_refusals_have_their_codes() {
    let ds = Dataset::memory();
    // no profile
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &options())),
        "unsupported-profile"
    );
    configured(&ds);
    // an empty batch and an unknown alias are invalid
    assert_eq!(
        code(propose(&ds, &EvidenceInput::default(), &options())),
        "invalid"
    );
    let mut bad = small("c1", proposition());
    bad.claims[0].revision = local("nothing");
    assert_eq!(code(propose(&ds, &bad, &options())), "invalid");
    // a graph the settings do not give the role
    let mut o = options();
    o.proposal_graph = Some(iri("urn:elsewhere"));
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &o)),
        "invalid"
    );
    // a blank node in a proposition
    let blank = Triple::new(oxrdf::BlankNode::default(), iri("urn:p"), iri("urn:o"));
    assert_eq!(
        code(propose(&ds, &small("c1", blank), &options())),
        "invalid"
    );
    // a reference to a record that does not exist
    let mut missing = small("c1", proposition());
    missing.claims[0].revision =
        RecordRef::Existing(iri("urn:uuid:00000000-0000-4000-8000-000000000000"));
    missing.revisions.clear();
    assert_eq!(
        code(propose(&ds, &missing, &options())),
        "evidence-unavailable"
    );
}

#[test]
fn a_caller_without_update_grants_on_a_written_graph_is_refused() {
    let ds = Dataset::memory();
    configured(&ds);
    let head = ds.store().head_commit().seq;
    let mut o = options();
    // may write the claims graph but not the source graph
    o.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([CLAIMS], &[])),
    )));
    let e = propose(&ds, &small("c1", proposition()), &o).unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    assert_eq!(ds.store().head_commit().seq, head);
    // a batch of the claims graph alone passes
    let first = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let mut only_claim = small("c2", proposition());
    only_claim.revisions.clear();
    only_claim.claims[0].revision = RecordRef::Existing(iri_of(&first, "rev"));
    assert!(propose(&ds, &only_claim, &o).unwrap().committed);
}

#[test]
fn an_unreadable_record_answers_as_a_missing_one() {
    let ds = Dataset::memory();
    configured(&ds);
    let first = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let hidden = iri_of(&first, "rev");
    // a caller that cannot read the source graph
    let limited = || {
        Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([CLAIMS], &[])),
            Graphs::Only(GraphRule::new([CLAIMS], &[])),
        )))
    };
    let answer = |revision: NamedNode| {
        let mut input = small("c9", proposition());
        input.revisions.clear();
        input.claims[0].revision = RecordRef::Existing(revision);
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &input,
            &options(),
            &mut |_, _| limited(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string()
                .replace(hidden.as_str(), "IRI")
                .replace("urn:uuid:00000000-0000-4000-8000-000000000000", "IRI"),
        )
    };
    let unreadable = answer(hidden.clone());
    let missing = answer(iri("urn:uuid:00000000-0000-4000-8000-000000000000"));
    assert_eq!(unreadable.0, "evidence-unavailable");
    assert_eq!(unreadable, missing);
}

struct Revocable(Arc<AtomicBool>);

impl AuthorityFence for Revocable {
    fn enter(&self) -> sparkles::Result<Box<dyn std::any::Any>> {
        if self.0.load(Ordering::SeqCst) {
            return Err(sparkles::managed::ManagedError::error(
                sparkles::managed::ManagedErrorKind::AuthorityChanged,
                "the credential was revoked",
            ));
        }
        Ok(Box::new(()))
    }
}

#[test]
fn a_revoked_authority_refuses_the_commit() {
    let ds = Dataset::memory();
    configured(&ds);
    let revoked = Arc::new(AtomicBool::new(false));
    let mut o = options();
    o.authority = Some(Arc::new(Revocable(revoked.clone())));
    assert!(
        propose(&ds, &small("c1", proposition()), &o)
            .unwrap()
            .committed
    );
    let head = ds.store().head_commit().seq;
    revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        code(propose(&ds, &small("c2", proposition()), &o)),
        "authority-changed"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

#[test]
fn records_written_while_a_graph_was_detached_never_authenticate() {
    let ds = Dataset::memory();
    configured(&ds);
    let p = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let claim = iri_of(&p, "claim");
    assert!(record(&ds, &claim, RecordKind::Claim).is_ok());
    evidence::detach(&ds, CLAIMS).unwrap();
    // while detached the graph holds no authenticated record
    assert_eq!(
        code(record(&ds, &claim, RecordKind::Claim)),
        "evidence-unavailable"
    );
    // a forged copy of a typed record and its marker, written while detached
    ds.update(&format!(
        "PREFIX ev: <urn:x-sparkles:evidence:>
         INSERT {{ GRAPH <{CLAIMS}> {{ <urn:forged> a ev:Claim ; ev:availabilityReceipt ?op . }} }}
         WHERE {{ GRAPH <{CLAIMS}> {{ <{}> ev:availabilityReceipt ?op }} }}",
        claim.as_str()
    ))
    .unwrap();
    configured(&ds);
    // the new protection has a new nonce: neither the forged copy nor the records of
    // the earlier protection authenticate
    assert_eq!(
        code(record(&ds, &iri("urn:forged"), RecordKind::Claim)),
        "origin-not-reproduced"
    );
    assert_eq!(
        code(record(&ds, &claim, RecordKind::Claim)),
        "origin-not-reproduced"
    );
    // and a typed write does not shadow them
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &options())),
        "immutable-record-conflict"
    );
    let fresh = propose(&ds, &small("c2", proposition()), &options()).unwrap();
    assert!(record(&ds, &iri_of(&fresh, "claim"), RecordKind::Claim).is_ok());
}

#[test]
fn a_typed_write_on_a_branch_authenticates_there() {
    let ds = Dataset::memory();
    configured(&ds);
    ds.create_branch("dev", &Default::default()).unwrap();
    let dev = ds.branch("dev").unwrap();
    let p = evidence::propose(
        None,
        &ds,
        &dev,
        &small("c1", proposition()),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert_eq!(p.branch, dev.store().branch_id());
    let v = Resolution::new(Control::none())
        .unwrap()
        .record(
            "r",
            &ds,
            &dev,
            all(),
            &RecordRequest {
                iri: iri_of(&p, "claim"),
                kinds: vec![],
                content_digest: None,
            },
        )
        .unwrap();
    assert_eq!(v.receipt.branch, dev.store().branch_id());
    assert_eq!(
        code(record(&ds, &iri_of(&p, "claim"), RecordKind::Claim)),
        "evidence-unavailable"
    );
}

#[test]
fn a_planted_record_cannot_borrow_a_later_operations_marker() {
    let ds = Dataset::memory();
    configured(&ds);
    let mut o = options();
    o.idempotency_key = Some("future".into());
    // the operation IRI of a key is predictable
    let dry = {
        let mut d = o.clone();
        d.dry_run = true;
        propose(&ds, &small("c1", proposition()), &d).unwrap()
    };
    let future = dry.operation.clone();
    evidence::detach(&ds, CLAIMS).unwrap();
    ds.update(&format!(
        "PREFIX ev: <urn:x-sparkles:evidence:>
         INSERT DATA {{ GRAPH <{CLAIMS}> {{ <urn:planted> a ev:Claim ; ev:availabilityReceipt <{}> . }} }}",
        future.as_str()
    ))
    .unwrap();
    configured(&ds);
    // the later typed write describes that operation with the current nonce
    let p = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert_eq!(p.operation, future);
    assert!(record(&ds, &iri_of(&p, "claim"), RecordKind::Claim).is_ok());
    assert_eq!(
        code(record(&ds, &iri("urn:planted"), RecordKind::Claim)),
        "origin-not-reproduced"
    );
}

#[test]
fn a_record_reference_is_checked_even_when_a_batch_key_spells_its_iri() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let missing = "urn:uuid:00000000-0000-4000-8000-000000000000";
    let mut input = small("c1", proposition());
    input.revisions.clear();
    input.claims[0].revision = RecordRef::Existing(iri(missing));
    input.references = vec![EvidenceRefInput {
        version: RecordVersion::V1,
        key: key(missing),
        occurrence_key: key("fact"),
        target: EvidenceRefTarget::StoredFact(StoredFactRef {
            dataset: ds.dataset_id(),
            branch: ds.dataset_id(),
            commit: fact,
            graph: GraphIdentity::Named(iri(FACTS)),
            expected: proposition(),
            anchor: None,
        }),
    }];
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(propose(&ds, &input, &options())),
        "evidence-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    // one existing record used as two kinds is checked against each
    let p = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let rev = iri_of(&p, "rev");
    let mut twice = small("c2", proposition());
    twice.revisions.clear();
    twice.claims[0].revision = RecordRef::Existing(rev.clone());
    twice.assessments = vec![AssessmentInput {
        version: RecordVersion::V1,
        key: key("assessment"),
        occurrence_key: key("a1"),
        target: RecordRef::Existing(rev),
        conclusion: Term::NamedNode(iri("urn:conclusion:plausible")),
        method: method("urn:method:review"),
        configuration_digest: sha("config"),
        evaluated_at: Literal::new_typed_literal(
            "2026-01-01T10:00:00Z",
            iri("http://www.w3.org/2001/XMLSchema#dateTime"),
        ),
        attributed_evaluator: iri("urn:analyst"),
        confidence: None,
        scope: Default::default(),
        inputs: Vec::new(),
    }];
    assert_eq!(
        code(propose(&ds, &twice, &options())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, p.commit);
}

#[test]
fn a_key_used_for_records_in_other_graphs_conflicts() {
    let ds = Dataset::memory();
    configured(&ds);
    let mut o = options();
    o.idempotency_key = Some("k".into());
    let mut revisions = small("c1", proposition());
    revisions.claims.clear();
    let first = propose(&ds, &revisions, &o).unwrap();
    assert!(first.records.iter().all(|r| r.graph.as_str() == SOURCES));
    let mut claims = small("c1", proposition());
    claims.revisions.clear();
    claims.claims[0].revision = RecordRef::Existing(iri_of(&first, "rev"));
    let head = ds.store().head_commit().seq;
    assert_eq!(code(propose(&ds, &claims, &o)), "idempotency-conflict");
    assert_eq!(ds.store().head_commit().seq, head);
}

#[test]
fn a_replay_keeps_the_existing_flags_of_the_first_answer() {
    let ds = Dataset::memory();
    configured(&ds);
    propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let mut o = options();
    o.idempotency_key = Some("again".into());
    let second = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(second.records.iter().all(|r| r.existing));
    let replayed = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.records, second.records);
}

#[test]
fn a_published_operation_keeps_its_hold_after_its_graph_is_protected_again() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let p = propose(&ds, &batch(&ds, fact), &options()).unwrap();
    let hold = p.hold.unwrap();
    evidence::detach(&ds, CLAIMS).unwrap();
    configured(&ds);
    assert!(
        evidence::reconcile_operation_holds(&ds, 0)
            .unwrap()
            .is_empty()
    );
    assert!(evidence::holds(&ds).unwrap().iter().any(|h| h.id == hold));
}

/// Releases the operation's hold from another thread while the commit is between its
/// hold check and its publication, and records whether the release finished first.
struct ReleaseDuringPublication {
    ds: Dataset,
    released_early: Arc<AtomicBool>,
    release: std::sync::Mutex<Option<std::thread::JoinHandle<bool>>>,
}

impl AuthorityFence for ReleaseDuringPublication {
    fn enter(&self) -> sparkles::Result<Box<dyn std::any::Any>> {
        let id = evidence::holds(&self.ds).unwrap()[0].id;
        let ds = self.ds.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let r = evidence::release_hold(&ds, id).unwrap();
            let _ = tx.send(());
            r
        });
        if rx
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_ok()
        {
            self.released_early.store(true, Ordering::SeqCst);
        }
        *self.release.lock().unwrap() = Some(handle);
        Ok(Box::new(()))
    }
}

#[test]
fn a_release_waits_for_a_publication_that_passed_its_hold_check() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let fence = Arc::new(ReleaseDuringPublication {
        ds: ds.clone(),
        released_early: Default::default(),
        release: Default::default(),
    });
    let mut o = options();
    o.authority = Some(fence.clone());
    let p = propose(&ds, &batch(&ds, fact), &o).unwrap();
    assert!(p.committed);
    assert!(
        !fence.released_early.load(Ordering::SeqCst),
        "the release finished before the publication"
    );
    let handle = fence.release.lock().unwrap().take().unwrap();
    assert!(
        handle.join().unwrap(),
        "the release runs once the commit published"
    );
}

const ORDINARY: &str = "urn:source-graph";

fn ordinary() -> evidence::OrdinaryChanges {
    evidence::OrdinaryChanges {
        delete: Vec::new(),
        insert: vec![oxrdf::Quad::new(
            iri("urn:doc"),
            iri("urn:x-sparkles:text"),
            Literal::new_simple_literal(TEXT),
            iri(ORDINARY),
        )],
    }
}

fn has_ordinary(ds: &Dataset) -> bool {
    ds.ask(&format!("ASK {{ GRAPH <{ORDINARY}> {{ ?s ?p ?o }} }}"))
        .unwrap()
}

#[test]
fn ordinary_changes_publish_in_the_batchs_commit() {
    let ds = Dataset::memory();
    configured(&ds);
    let before = ds.store().head_commit().seq;
    let mut o = options();
    o.ordinary = Some(ordinary());
    o.idempotency_key = Some("k".into());
    let p = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(p.committed);
    assert_eq!(
        p.commit,
        before + 1,
        "the source and the batch share one commit"
    );
    assert!(has_ordinary(&ds));
    // a replay writes nothing again, the ordinary changes included
    ds.update(&format!("DROP GRAPH <{ORDINARY}>")).unwrap();
    let head = ds.store().head_commit().seq;
    let again = propose(&ds, &small("c1", proposition()), &o).unwrap();
    assert!(again.replayed);
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(!has_ordinary(&ds));
}

#[test]
fn a_refused_batch_leaves_its_ordinary_changes_unwritten() {
    let ds = Dataset::memory();
    configured(&ds);
    let head = ds.store().head_commit().seq;
    // refused at publication by the authority fence
    let mut o = options();
    o.ordinary = Some(ordinary());
    o.authority = Some(Arc::new(Revocable(Arc::new(AtomicBool::new(true)))));
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &o)),
        "authority-changed"
    );
    // refused by a stale head
    let mut o = options();
    o.ordinary = Some(ordinary());
    o.if_head = Some(head + 7);
    assert_eq!(
        code(propose(&ds, &small("c1", proposition()), &o)),
        "head-changed"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    assert!(!has_ordinary(&ds));
    assert!(
        !ds.ask(&format!("ASK {{ GRAPH <{CLAIMS}> {{ ?s ?p ?o }} }}"))
            .unwrap()
    );
}

#[test]
fn ordinary_changes_cannot_write_a_managed_graph() {
    let ds = Dataset::memory();
    configured(&ds);
    let head = ds.store().head_commit().seq;
    for g in [SOURCES, CLAIMS] {
        let mut o = options();
        o.ordinary = Some(evidence::OrdinaryChanges {
            delete: Vec::new(),
            insert: vec![oxrdf::Quad::new(
                iri("urn:x"),
                iri("urn:y"),
                iri("urn:z"),
                iri(g),
            )],
        });
        assert_eq!(
            code(propose(&ds, &small("c1", proposition()), &o)),
            "profile-managed-graph"
        );
    }
    // the default graph is not a source graph
    let mut o = options();
    o.ordinary = Some(evidence::OrdinaryChanges {
        delete: Vec::new(),
        insert: vec![oxrdf::Quad::new(
            iri("urn:x"),
            iri("urn:y"),
            iri("urn:z"),
            oxrdf::GraphName::DefaultGraph,
        )],
    });
    assert!(propose(&ds, &small("c1", proposition()), &o).is_err());
    // grants decide the ordinary graph too
    let mut o = options();
    o.ordinary = Some(ordinary());
    o.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([SOURCES, CLAIMS], &[])),
    )));
    let e = propose(&ds, &small("c1", proposition()), &o).unwrap_err();
    assert!(matches!(e, sparkles::Error::NotPermitted(_)), "{e:?}");
    assert_eq!(ds.store().head_commit().seq, head);
}

#[cfg(feature = "failpoints")]
mod crashes {
    use super::*;

    fn persistent(dir: &std::path::Path) -> Dataset {
        Dataset::open(dir).unwrap()
    }

    /// Propose with a crash at `failpoint`, then reopen.
    fn crash_at(dir: &std::path::Path, failpoint: &'static str, o: &ProposeOptions) -> Dataset {
        let ds = persistent(dir);
        let fact = with_fact(&ds);
        configured(&ds);
        ds.store()
            .set_failpoint(failpoint, Some(Arc::new(|_| panic!("crash"))));
        let input = batch(&ds, fact);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| propose(&ds, &input, o)));
        assert!(r.is_err(), "{failpoint} was reached");
        drop(ds);
        persistent(dir)
    }

    #[test]
    fn a_crash_after_the_hold_leaves_an_orphan_that_reconciles_and_a_retry_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = options();
        o.idempotency_key = Some("retry".into());
        let ds = crash_at(dir.path(), "evidence-held", &o);
        let holds = evidence::holds(&ds).unwrap();
        assert_eq!(holds.len(), 1, "the hold is durable before the commit");
        assert!(!evidence::operation_recorded(&ds, holds[0].id, &Control::none()).unwrap());
        // within the grace period it stays
        assert!(
            evidence::reconcile_operation_holds(&ds, evidence::DEFAULT_GRACE_MS)
                .unwrap()
                .is_empty()
        );
        let released = evidence::reconcile_operation_holds(&ds, 0).unwrap();
        assert_eq!(
            released,
            vec![holds[0].id],
            "the orphan is released after its grace"
        );
        assert!(evidence::holds(&ds).unwrap().is_empty());
    }

    #[test]
    fn a_reconciliation_takes_no_writer_while_it_holds_off_publications() {
        let dir = tempfile::tempdir().unwrap();
        let ds = crash_at(dir.path(), "evidence-held", &options());
        ds.create_branch("dev", &Default::default()).unwrap();
        assert_eq!(evidence::holds(&ds).unwrap().len(), 1);
        // a commit that holds the writer, as one on its way to its hold check does
        let t = ds.store().write();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                tx.send(evidence::reconcile_operation_holds(&ds, 0).map(|r| r.len()))
                    .unwrap();
            });
            let released = rx
                .recv_timeout(std::time::Duration::from_secs(20))
                .expect("the reconciliation finished while the writer was held")
                .unwrap();
            assert_eq!(released, 1);
            drop(t);
        });
    }

    #[test]
    fn a_retry_after_a_crash_reuses_the_hold() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = options();
        o.idempotency_key = Some("retry".into());
        let ds = crash_at(dir.path(), "evidence-held", &o);
        let holds = evidence::holds(&ds).unwrap();
        let fact = holds[0].commits[0].seq;
        let p = propose(&ds, &batch(&ds, fact), &o).unwrap();
        assert!(p.committed);
        assert_eq!(p.hold, Some(holds[0].id));
        assert_eq!(evidence::holds(&ds).unwrap().len(), 1);
        assert!(
            evidence::reconcile_operation_holds(&ds, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_crash_after_the_commit_keeps_the_hold_and_the_records() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = options();
        o.idempotency_key = Some("done".into());
        let ds = crash_at(dir.path(), "evidence-committed", &o);
        let holds = evidence::holds(&ds).unwrap();
        assert_eq!(holds.len(), 1);
        assert!(evidence::operation_recorded(&ds, holds[0].id, &Control::none()).unwrap());
        assert!(
            evidence::reconcile_operation_holds(&ds, 0)
                .unwrap()
                .is_empty()
        );
        let p = propose(&ds, &batch(&ds, holds[0].commits[0].seq), &o).unwrap();
        assert!(p.replayed);
        assert!(record(&ds, &iri_of(&p, "claim"), RecordKind::Claim).is_ok());
    }

    #[test]
    fn a_head_that_moves_after_the_hold_refuses_and_leaves_a_reconcilable_hold() {
        let (_dir, ds) = retained();
        let fact = with_fact(&ds);
        configured(&ds);
        let mut o = options();
        o.if_head = Some(ds.store().head_commit().seq);
        let mover = ds.clone();
        ds.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| {
                mover
                    .update("INSERT DATA { <urn:x> <urn:y> <urn:z> }")
                    .unwrap();
            })),
        );
        assert_eq!(code(propose(&ds, &batch(&ds, fact), &o)), "head-changed");
        ds.store().set_failpoint("evidence-held", None);
        let holds = evidence::holds(&ds).unwrap();
        assert_eq!(holds.len(), 1);
        assert_eq!(
            evidence::reconcile_operation_holds(&ds, 0).unwrap(),
            vec![holds[0].id]
        );
    }

    #[test]
    fn a_revocation_between_preparation_and_commit_refuses() {
        let (_dir, ds) = retained();
        let fact = with_fact(&ds);
        configured(&ds);
        let revoked = Arc::new(AtomicBool::new(false));
        let mut o = options();
        o.authority = Some(Arc::new(Revocable(revoked.clone())));
        let flag = revoked.clone();
        ds.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| flag.store(true, Ordering::SeqCst))),
        );
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(propose(&ds, &batch(&ds, fact), &o)),
            "authority-changed"
        );
        assert_eq!(ds.store().head_commit().seq, head);
        let _ = OwnerKind::Evidence;
    }
}
