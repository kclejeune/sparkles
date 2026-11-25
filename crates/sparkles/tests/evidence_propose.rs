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
        validity: None,
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
        let (started, waiting) = std::sync::mpsc::channel();
        ds.store().set_failpoint(
            "holds-release-waiting",
            Some(Arc::new(move |_| {
                started.send(()).unwrap();
            })),
        );
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let r = evidence::release_hold(&ds, id).unwrap();
            ds.store().set_failpoint("holds-release-waiting", None);
            let _ = tx.send(());
            r
        });
        waiting
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
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

    #[test]
    fn a_crash_after_a_foreign_hold_reconciles_across_datasets_and_retries() {
        let dir = tempfile::tempdir().unwrap();
        let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let a = cat.create("a", &Default::default()).unwrap();
        let b = cat.create("b", &Default::default()).unwrap();
        configured(&a);
        let fact_b = with_fact(&b);
        let a_id = a.dataset_id();
        let mut o = options();
        o.idempotency_key = Some("cross-retry".into());
        let input = batch_for_fact(&b, b.dataset_id(), fact_b);
        b.store()
            .set_failpoint("evidence-held-foreign", Some(Arc::new(|_| panic!("crash"))));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evidence::propose(
                Some(&cat),
                &a,
                &a,
                &input,
                &o,
                &mut |_, _| all(),
                &Control::none(),
            )
        }));
        assert!(r.is_err(), "evidence-held-foreign was reached");
        drop(a);
        drop(b);
        drop(cat);
        // reopen the catalog: the cross-dataset hold in b is durable in holds.json format 2
        let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let a = cat.get("a").unwrap();
        let b = cat.get("b").unwrap();
        let holds = evidence::hold_details(&b).unwrap();
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].for_dataset, Some(a_id));
        assert!(!evidence::operation_recorded(&a, holds[0].hold.id, &Control::none()).unwrap());
        // standalone reconciliation (without a catalog) keeps cross-dataset holds
        assert!(
            evidence::reconcile_operation_holds(&b, 0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(evidence::holds(&b).unwrap().len(), 1);
        // within grace period, catalog reconciliation keeps it
        assert!(
            evidence::reconcile_operation_holds_in(Some(&cat), &b, evidence::DEFAULT_GRACE_MS)
                .unwrap()
                .is_empty()
        );
        // past grace period, catalog reconciliation finds it absent in a and releases it
        assert_eq!(
            evidence::reconcile_operation_holds_in(Some(&cat), &b, 0).unwrap(),
            vec![holds[0].hold.id]
        );
        assert!(evidence::holds(&b).unwrap().is_empty());
        // retry after a crash reuses the durable foreign hold and publishes cleanly
        b.store()
            .set_failpoint("evidence-held-foreign", Some(Arc::new(|_| panic!("crash"))));
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evidence::propose(
                Some(&cat),
                &a,
                &a,
                &input,
                &o,
                &mut |_, _| all(),
                &Control::none(),
            )
        }));
        assert!(r.is_err());
        b.store().set_failpoint("evidence-held-foreign", None);
        let orphan_id = evidence::holds(&b).unwrap()[0].id;
        let p = evidence::propose(
            Some(&cat),
            &a,
            &a,
            &input,
            &o,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        assert!(p.committed);
        assert_eq!(p.hold, Some(orphan_id));
        assert_eq!(evidence::holds(&b).unwrap().len(), 1);
        assert!(
            evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reconciliation_keeps_a_hold_when_a_new_branch_inherits_its_operation() {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        let fact = with_fact(&target);
        let proposal = evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let id = proposal.hold.unwrap();
        let fork_origin = origin.clone();
        target.store().set_failpoint(
            "evidence-reconcile-origin-opened",
            Some(Arc::new(move |_| {
                fork_origin
                    .create_branch("fork", &Default::default())
                    .unwrap();
                for graph in [SOURCES, CLAIMS] {
                    evidence::detach(&fork_origin, graph).unwrap();
                    fork_origin
                        .update(&format!("DROP GRAPH <{graph}>"))
                        .unwrap();
                }
            })),
        );
        assert!(
            evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0)
                .unwrap()
                .is_empty()
        );
        target
            .store()
            .set_failpoint("evidence-reconcile-origin-opened", None);
        assert!(evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
        assert_eq!(evidence::holds(&target).unwrap()[0].id, id);
    }

    #[test]
    fn cancellation_after_foreign_hold_leaves_a_reconcilable_hold() {
        let dir = tempfile::tempdir().unwrap();
        let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let a = cat.create("a", &Default::default()).unwrap();
        let b = cat.create("b", &Default::default()).unwrap();
        configured(&a);
        let fact_b = with_fact(&b);
        let control = Control::none();
        let cancel = control.cancel.clone();
        let head = a.store().head_commit().seq;
        a.store()
            .set_failpoint("evidence-held", Some(Arc::new(move |_| cancel.cancel())));
        let input = batch_for_fact(&b, b.dataset_id(), fact_b);
        assert_eq!(
            code(evidence::propose(
                Some(&cat),
                &a,
                &a,
                &input,
                &options(),
                &mut |_, _| all(),
                &control,
            )),
            "cancelled"
        );
        a.store().set_failpoint("evidence-held", None);
        assert_eq!(a.store().head_commit().seq, head);
        let holds = evidence::hold_details(&b).unwrap();
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].for_dataset, Some(a.dataset_id()));
        assert_eq!(
            evidence::reconcile_operation_holds_in(Some(&cat), &b, 0).unwrap(),
            vec![holds[0].hold.id]
        );
        assert!(evidence::holds(&b).unwrap().is_empty());
    }

    #[test]
    fn releasing_a_foreign_hold_before_publish_refuses_with_hold_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let a = cat.create("a", &Default::default()).unwrap();
        let b = cat.create("b", &Default::default()).unwrap();
        configured(&a);
        let fact_b = with_fact(&b);
        let b_release = b.clone();
        a.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| {
                let id = evidence::holds(&b_release).unwrap()[0].id;
                assert!(evidence::release_hold(&b_release, id).unwrap());
            })),
        );
        let head = a.store().head_commit().seq;
        let input = batch_for_fact(&b, b.dataset_id(), fact_b);
        assert_eq!(
            code(evidence::propose(
                Some(&cat),
                &a,
                &a,
                &input,
                &options(),
                &mut |_, _| all(),
                &Control::none(),
            )),
            "hold-conflict"
        );
        a.store().set_failpoint("evidence-held", None);
        assert_eq!(a.store().head_commit().seq, head);
    }
}

/// A batch whose stored-fact reference targets `(target_ds, branch_id, fact_commit)`.
fn batch_for_fact(target_ds: &Dataset, branch_id: uuid::Uuid, fact_commit: u64) -> EvidenceInput {
    let mut input = small("c1", proposition());
    input.references = vec![EvidenceRefInput {
        version: RecordVersion::V1,
        key: key("fact"),
        occurrence_key: key("fact"),
        target: EvidenceRefTarget::StoredFact(StoredFactRef {
            dataset: target_ds.dataset_id(),
            branch: branch_id,
            commit: fact_commit,
            graph: GraphIdentity::Named(iri(FACTS)),
            expected: proposition(),
            anchor: None,
        }),
    }];
    input
}

#[test]
fn foreign_holds_require_the_same_catalog_to_verify_both_datasets() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let registered = catalog.create("registered", &Default::default()).unwrap();
    let (_outside_dir, outside) = retained();
    configured(&outside);
    let fact = with_fact(&registered);

    assert_eq!(
        code(evidence::propose(
            Some(&catalog),
            &outside,
            &outside,
            &batch_for_fact(&registered, registered.dataset_id(), fact),
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )),
        "evidence-unavailable"
    );
    assert!(evidence::holds(&registered).unwrap().is_empty());

    let orphan = uuid::Uuid::new_v4();
    let local_fact = with_fact(&outside);
    evidence::acquire_hold_for(
        &outside,
        orphan,
        &[evidence::CommitRef {
            branch_id: outside.dataset_id(),
            seq: local_fact,
        }],
        Some(registered.dataset_id()),
    )
    .unwrap();
    assert!(
        evidence::reconcile_operation_holds_in(Some(&catalog), &outside, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(evidence::holds(&outside).unwrap()[0].id, orphan);
}

#[test]
fn cross_dataset_stored_fact_is_held_and_protected_from_retention() {
    let dir = tempfile::tempdir().unwrap();
    let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let a = cat.create("a", &Default::default()).unwrap();
    // b does not need evidence settings enabled to hold its commits
    let b = cat.create("b", &Default::default()).unwrap();
    configured(&a);
    let fact_b = with_fact(&b);
    let input = batch_for_fact(&b, b.dataset_id(), fact_b);
    // dry run holds nothing on b
    let mut dry = options();
    dry.dry_run = true;
    let preview = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &input,
        &dry,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(!preview.committed && preview.hold.is_none());
    assert!(evidence::holds(&b).unwrap().is_empty());
    // real proposal holds b's commit under the operation with for_dataset = a
    let mut o = options();
    o.idempotency_key = Some("k-cross".into());
    let p = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &input,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(p.committed);
    let hold_id = p.hold.expect("operation hold");
    assert!(evidence::holds(&a).unwrap().is_empty());
    let b_holds = evidence::hold_details(&b).unwrap();
    assert_eq!(b_holds.len(), 1);
    assert_eq!(b_holds[0].hold.id, hold_id);
    assert_eq!(b_holds[0].for_dataset, Some(a.dataset_id()));
    assert_eq!(
        b_holds[0].hold.commits,
        vec![evidence::CommitRef {
            branch_id: b.dataset_id(),
            seq: fact_b,
        }]
    );
    // replay returns the same hold id without writing
    let replayed = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &input,
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.hold, Some(hold_id));
    // ev:sourceHold is recorded on the operation in a
    assert!(
        a.ask(&format!(
            "ASK {{ GRAPH <{CLAIMS}> {{ <{}> <urn:x-sparkles:evidence:sourceHold> \"{hold_id}\" }} }}",
            p.operation.as_str()
        ))
        .unwrap()
    );
    b.store()
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(1),
            keep_age_ms: None,
            max_bytes: Some(1),
        })
        .unwrap();
    // aggressive compaction and retention on b keep fact_b readable and resolvable
    for i in 0..4 {
        b.update(&format!("INSERT DATA {{ <urn:n> <urn:v> {i} }}"))
            .unwrap();
        b.store().compact().unwrap();
    }
    b.store().history_tick().unwrap();
    let fact_ref = StoredFactRef {
        dataset: b.dataset_id(),
        branch: b.dataset_id(),
        commit: fact_b,
        graph: GraphIdentity::Named(iri(FACTS)),
        expected: proposition(),
        anchor: None,
    };
    Resolution::new(Control::none())
        .unwrap()
        .stored_fact("f", &cat, &fact_ref, |_, _| all())
        .unwrap();
    // catalog reconciliation keeps the live hold
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );
    // releasing the hold lets retention reclaim fact_b
    assert!(evidence::release_hold(&b, hold_id).unwrap());
    b.store().compact().unwrap();
    b.store().history_tick().unwrap();
    assert_eq!(
        code(Resolution::new(Control::none()).unwrap().stored_fact(
            "f",
            &cat,
            &fact_ref,
            |_, _| all()
        )),
        "evidence-unavailable"
    );
}

#[test]
fn replay_preserves_a_legacy_cross_dataset_receipt_without_a_hold() {
    use sparkles_core::evidence::write::{ProposalOptions, Stamp, plan};

    let ds = Dataset::memory();
    configured(&ds);
    let foreign = Dataset::memory();
    let input = batch_for_fact(&foreign, foreign.dataset_id(), 1);
    let control = Control::none();
    let mut opts = options();
    opts.idempotency_key = Some("legacy-cross-dataset".into());

    // Before cross-dataset holds, this operation recorded its references but no hold.
    let prepared = sparkles_core::evidence::prepare_input(&input, &control).unwrap();
    let plan = plan(
        &prepared,
        ds.dataset_id(),
        &settings(),
        &ProposalOptions {
            principal: &opts.principal,
            idempotency_key: opts.idempotency_key.as_deref(),
            source_graph: None,
            proposal_graph: None,
            identity_graph: None,
        },
        &control,
    )
    .unwrap();
    let store = ds.store();
    let writer = store
        .managed_writer(OwnerKind::Evidence, plan.operation)
        .unwrap();
    let mut txn = store.write();
    txn.set_managed(writer).unwrap();
    let (commit, time_ms) = txn.managed_stamp().unwrap();
    let quads = plan
        .quads(
            &Stamp {
                dataset: ds.dataset_id(),
                branch: store.branch_id(),
                commit,
                time_ms,
            },
            |graph| txn.managed().unwrap().protection(graph.as_str()),
            None,
            &vec![false; plan.records.len()],
        )
        .unwrap();
    let mut blank_nodes = Default::default();
    for quad in &quads {
        let ids = txn.encode_quad(quad, &mut blank_nodes).unwrap();
        txn.insert(ids).unwrap();
    }
    txn.commit().unwrap();

    // The catalog is unnecessary for replay, even if the target no longer exists.
    let replayed = propose(&ds, &input, &opts).unwrap();
    assert!(replayed.replayed);
    assert_eq!(replayed.commit, commit);
    assert_eq!(replayed.hold, None);
}

#[test]
fn cross_dataset_holds_span_multiple_datasets_and_branches() {
    let dir = tempfile::tempdir().unwrap();
    let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let a = cat.create("a", &Default::default()).unwrap();
    let b = cat.create("b", &Default::default()).unwrap();
    let c = cat.create("c", &Default::default()).unwrap();
    configured(&a);
    let fact_a = with_fact(&a);
    b.create_branch("dev", &Default::default()).unwrap();
    let b_dev = b.branch("dev").unwrap();
    let fact_b = with_fact(&b_dev);
    let fact_c = with_fact(&c);
    let mut input = small("c1", proposition());
    input.references = vec![
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("fa"),
            occurrence_key: key("fa"),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: a.dataset_id(),
                branch: a.dataset_id(),
                commit: fact_a,
                graph: GraphIdentity::Named(iri(FACTS)),
                expected: proposition(),
                anchor: None,
            }),
        },
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("fb"),
            occurrence_key: key("fb"),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: b.dataset_id(),
                branch: b_dev.store().branch_id(),
                commit: fact_b,
                graph: GraphIdentity::Named(iri(FACTS)),
                expected: proposition(),
                anchor: None,
            }),
        },
        EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("fc"),
            occurrence_key: key("fc"),
            target: EvidenceRefTarget::StoredFact(StoredFactRef {
                dataset: c.dataset_id(),
                branch: c.dataset_id(),
                commit: fact_c,
                graph: GraphIdentity::Named(iri(FACTS)),
                expected: proposition(),
                anchor: None,
            }),
        },
    ];
    let p = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &input,
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let id = p.hold.unwrap();
    assert_eq!(evidence::holds(&a).unwrap()[0].id, id);
    assert_eq!(evidence::hold_details(&a).unwrap()[0].for_dataset, None);
    assert_eq!(evidence::holds(&b).unwrap()[0].id, id);
    assert_eq!(
        evidence::hold_details(&b).unwrap()[0].for_dataset,
        Some(a.dataset_id())
    );
    assert_eq!(evidence::holds(&c).unwrap()[0].id, id);
    assert_eq!(
        evidence::hold_details(&c).unwrap()[0].for_dataset,
        Some(a.dataset_id())
    );
    // deleting the held branch in b is refused without release_holds
    drop(b_dev);
    assert_eq!(
        code(b.delete_branch_with(
            "dev",
            &sparkles::branch::DeleteOptions {
                force: true,
                release_holds: false,
                ..Default::default()
            }
        )),
        "branch-held"
    );
}

#[test]
fn cross_dataset_reconciliation_tracks_rename_and_unmerged_branch_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let a = cat.create("a", &Default::default()).unwrap();
    let b = cat.create("b", &Default::default()).unwrap();
    configured(&a);
    let fact_b = with_fact(&b);
    // propose on a branch of a
    a.create_branch("work", &Default::default()).unwrap();
    let a_work = a.branch("work").unwrap();
    let p_branch = evidence::propose(
        Some(&cat),
        &a,
        &a_work,
        &batch_for_fact(&b, b.dataset_id(), fact_b),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let hold_branch = p_branch.hold.unwrap();
    // propose on main of a
    let mut main_batch = batch_for_fact(&b, b.dataset_id(), fact_b);
    main_batch.claims[0].occurrence_key = key("c2");
    let p_main = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &main_batch,
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let hold_main = p_main.hold.unwrap();
    assert_eq!(evidence::holds(&b).unwrap().len(), 2);
    // both live holds survive reconciliation
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );
    // deleting the unmerged branch of a orphans only the branch's hold
    drop(a_work);
    a.delete_branch_with(
        "work",
        &sparkles::branch::DeleteOptions {
            force: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0).unwrap(),
        vec![hold_branch]
    );
    assert_eq!(evidence::holds(&b).unwrap().len(), 1);
    assert_eq!(evidence::holds(&b).unwrap()[0].id, hold_main);
    // renaming a keeps the hold because UUID is unchanged
    drop(a);
    let a2 = cat.rename("a", "a_renamed").unwrap();
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );
    // A missing origin could be temporarily closed or unregistered.
    drop(a2);
    assert!(cat.delete("a_renamed").unwrap());
    let _replacement = cat.create("a", &Default::default()).unwrap();
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(evidence::holds(&b).unwrap()[0].id, hold_main);
    assert!(evidence::release_hold(&b, hold_main).unwrap());
}

#[test]
fn cross_dataset_reconciliation_keeps_holds_when_catalog_has_unavailable_dataset() {
    let dir = tempfile::tempdir().unwrap();
    let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let a = cat.create("a", &Default::default()).unwrap();
    let b = cat.create("b", &Default::default()).unwrap();
    configured(&a);
    let fact_b = with_fact(&b);
    let p = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &batch_for_fact(&b, b.dataset_id(), fact_b),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let hold_id = p.hold.unwrap();
    drop(a);
    drop(b);
    drop(cat);
    // damage dataset a so it is refused when the catalog reopens
    std::fs::write(
        dir.path().join("databases").join("a").join("dataset.json"),
        b"not json",
    )
    .unwrap();
    let (cat, _) = sparkles::Catalog::open_deferred(dir.path(), Default::default()).unwrap();
    cat.open_registered("b").unwrap();
    let b = cat.get("b").unwrap();
    assert!(cat.is_opening("a"));
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );

    assert!(cat.open_registered("a").is_err());
    assert!(cat.refusal("a").is_some());
    assert!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(evidence::holds(&b).unwrap()[0].id, hold_id);
}

#[test]
fn foreign_release_waits_for_publication_and_reconcile_does_not_block_on_writer() {
    let dir = tempfile::tempdir().unwrap();
    let cat = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let a = cat.create("a", &Default::default()).unwrap();
    let b = cat.create("b", &Default::default()).unwrap();
    configured(&a);
    let fact_b = with_fact(&b);
    let fence = Arc::new(ReleaseDuringPublication {
        ds: b.clone(),
        released_early: Default::default(),
        release: Default::default(),
    });
    let mut o = options();
    o.authority = Some(fence.clone());
    let p = evidence::propose(
        Some(&cat),
        &a,
        &a,
        &batch_for_fact(&b, b.dataset_id(), fact_b),
        &o,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    assert!(p.committed);
    assert!(
        !fence.released_early.load(Ordering::SeqCst),
        "foreign release finished before publication"
    );
    let handle = fence.release.lock().unwrap().take().unwrap();
    assert!(handle.join().unwrap());
    // reconciliation on b finishes even while a's writer lock is held
    let orphan = uuid::Uuid::new_v4();
    evidence::acquire_hold_for(
        &b,
        orphan,
        &[evidence::CommitRef {
            branch_id: b.dataset_id(),
            seq: fact_b,
        }],
        Some(a.dataset_id()),
    )
    .unwrap();
    let t = a.store().write();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            tx.send(evidence::reconcile_operation_holds_in(Some(&cat), &b, 0))
                .unwrap();
        });
        let released = rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .expect("cross-dataset reconciliation finished while a's writer was held")
            .unwrap();
        assert_eq!(released, vec![orphan]);
        drop(t);
    });
}

fn copy_dataset(source: &std::path::Path, destination: &std::path::Path) {
    std::fs::create_dir(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let to = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dataset(&entry.path(), &to);
        } else {
            std::fs::copy(entry.path(), to).unwrap();
        }
    }
}

#[test]
fn proposal_rejects_copied_main_and_branch_handles() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let copied_root = dir.path().join("copy");
    copy_dataset(origin.store().root().unwrap(), &copied_root);
    let copy = Dataset::open(copied_root).unwrap();
    assert_eq!(copy.dataset_id(), origin.dataset_id());
    copy.create_branch("work", &Default::default()).unwrap();

    let input = batch_for_fact(&target, target.dataset_id(), fact);
    for logical in [&copy, &copy.branch("work").unwrap()] {
        let head = logical.store().head_commit().seq;
        assert_eq!(
            code(evidence::propose(
                Some(&catalog),
                &origin,
                logical,
                &input,
                &options(),
                &mut |_, _| all(),
                &Control::none(),
            )),
            "invalid"
        );
        assert_eq!(logical.store().head_commit().seq, head);
    }
    assert!(evidence::holds(&target).unwrap().is_empty());
}

#[test]
fn duplicate_catalog_identities_refuse_proposals_and_keep_ambiguous_orphans() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let input = batch_for_fact(&target, target.dataset_id(), fact);
    let copied_target = dir.path().join("target-copy");
    copy_dataset(target.store().root().unwrap(), &copied_target);
    let duplicate = catalog
        .attach(
            "a-copy",
            sparkles::catalog::Attach::Directory(copied_target),
        )
        .unwrap();
    assert_eq!(target.dataset_id(), duplicate.dataset_id());
    assert_eq!(
        code(evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &input,
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )),
        "evidence-unavailable"
    );
    assert!(evidence::holds(&target).unwrap().is_empty());
    assert!(evidence::holds(&duplicate).unwrap().is_empty());
    assert!(catalog.delete("a-copy").unwrap());
    drop(duplicate);

    let copied_origin = dir.path().join("origin-copy");
    copy_dataset(origin.store().root().unwrap(), &copied_origin);
    let duplicate = catalog
        .attach(
            "a-copy",
            sparkles::catalog::Attach::Directory(copied_origin),
        )
        .unwrap();
    assert_eq!(origin.dataset_id(), duplicate.dataset_id());
    let id = uuid::Uuid::new_v4();
    evidence::acquire_hold_for(
        &target,
        id,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    assert!(
        evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0)
            .unwrap()
            .is_empty()
    );
    assert_eq!(evidence::holds(&target).unwrap()[0].id, id);
    assert!(catalog.delete("a-copy").unwrap());
    assert_eq!(
        evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap(),
        [id]
    );
}

#[test]
fn mixed_persistence_refuses_before_any_hold_or_receipt() {
    let dir = tempfile::tempdir().unwrap();
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let persistent = catalog.create("disk", &Default::default()).unwrap();
        let memory = catalog
            .create(
                "memory",
                &sparkles::catalog::CreateDataset {
                    kind: sparkles::catalog::DatasetKind::Memory,
                    geo: None,
                },
            )
            .unwrap();
        configured(&persistent);
        configured(&memory);
        for (origin, target) in [(&persistent, &memory), (&memory, &persistent)] {
            target.update("INSERT DATA { GRAPH <urn:facts> { <urn:release> <urn:affectedBy> <urn:cve> } }").unwrap();
            let fact = target.store().head_commit().seq;
            let input = batch_for_fact(target, target.dataset_id(), fact);
            let head = origin.store().head_commit().seq;
            let mut opts = options();
            opts.idempotency_key = Some("mixed".into());
            assert_eq!(
                code(evidence::propose(
                    Some(&catalog),
                    origin,
                    origin,
                    &input,
                    &opts,
                    &mut |_, _| all(),
                    &Control::none(),
                )),
                "evidence-unavailable"
            );
            assert_eq!(origin.store().head_commit().seq, head);
            assert!(evidence::holds(target).unwrap().is_empty());
        }
    }
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let persistent = catalog.get("disk").unwrap();
    assert!(evidence::holds(&persistent).unwrap().is_empty());
    let op = evidence::operation_id(persistent.dataset_id(), &options().principal, "mixed");
    assert!(!evidence::operation_recorded(&persistent, op, &Control::none()).unwrap());
}

#[test]
fn public_hold_types_still_support_struct_literals_and_patterns() {
    let id = uuid::Uuid::nil();
    let evidence::SourceHold {
        id,
        owner,
        created_ms,
        commits,
    } = evidence::SourceHold {
        id,
        owner: OwnerKind::Evidence,
        created_ms: 0,
        commits: vec![evidence::CommitRef {
            branch_id: id,
            seq: 1,
        }],
    };
    use evidence::propose::HeldCommit;
    let HeldCommit { branch, commit } = HeldCommit {
        branch: id,
        commit: 1,
    };
    use sparkles_core::evidence::write::RecordedOperation;
    let RecordedOperation {
        authenticated,
        payload_digest,
        branch: recorded_branch,
        commit: recorded_commit,
        ended_at,
    } = RecordedOperation {
        authenticated: true,
        payload_digest: None,
        branch: Some(branch),
        commit: Some(commit),
        ended_at: None,
    };
    assert!(authenticated);
    assert_eq!(owner, OwnerKind::Evidence);
    assert_eq!(created_ms, 0);
    assert_eq!(branch, id);
    assert_eq!(recorded_branch, Some(branch));
    assert_eq!(recorded_commit, Some(commits[0].seq));
    assert!(payload_digest.is_none() && ended_at.is_none());
}

#[test]
fn publication_rechecks_foreign_registration_after_acquisition() {
    for duplicate in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog
            .attach(
                "target",
                sparkles::catalog::Attach::Directory(dir.path().join("target")),
            )
            .unwrap();
        configured(&origin);
        let fact = with_fact(&target);
        let copied_root = dir.path().join("copy");
        copy_dataset(target.store().root().unwrap(), &copied_root);
        let change = catalog.clone();
        origin.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| {
                if duplicate {
                    change
                        .attach(
                            "a-copy",
                            sparkles::catalog::Attach::Directory(copied_root.clone()),
                        )
                        .unwrap();
                } else {
                    assert!(change.delete("target").unwrap());
                }
            })),
        );
        let head = origin.store().head_commit().seq;
        assert_eq!(
            code(evidence::propose(
                Some(&catalog),
                &origin,
                &origin,
                &batch_for_fact(&target, target.dataset_id(), fact),
                &options(),
                &mut |_, _| all(),
                &Control::none(),
            )),
            "evidence-unavailable"
        );
        origin.store().set_failpoint("evidence-held", None);
        assert_eq!(origin.store().head_commit().seq, head);
        assert_eq!(evidence::holds(&target).unwrap().len(), 1);
    }
}

#[cfg(unix)]
#[test]
fn uncertain_release_refuses_a_foreign_receipt_and_reopens_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options();
    opts.idempotency_key = Some("uncertain-release".into());
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        let fact = with_fact(&target);
        let root = target.store().root().unwrap().to_owned();
        let aside = dir.path().join("aside");
        let root_for_sync = root.clone();
        let aside_for_sync = aside.clone();
        target.store().set_failpoint(
            "holds-replaced",
            Some(Arc::new(move |_| {
                std::fs::rename(&root_for_sync, &aside_for_sync).unwrap();
            })),
        );
        let release = target.clone();
        origin.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| {
                let id = evidence::holds(&release).unwrap()[0].id;
                let error = evidence::release_hold(&release, id).unwrap_err();
                assert!(matches!(error, sparkles::Error::Io(_)));
                std::fs::rename(&aside, &root).unwrap();
            })),
        );
        let error = evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &opts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "holds-unavailable");
        assert_eq!(
            error.to_string(),
            "a referenced dataset's holds are unavailable"
        );
        origin.store().set_failpoint("evidence-held", None);
        target.store().set_failpoint("holds-replaced", None);
    }
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.get("origin").unwrap();
    let target = catalog.get("target").unwrap();
    let id = evidence::operation_id(origin.dataset_id(), &opts.principal, "uncertain-release");
    assert!(!evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    assert!(evidence::holds(&target).unwrap().is_empty());
}

#[test]
fn reconciliation_releases_only_the_losing_payloads_disjoint_hold() {
    let dir = tempfile::tempdir().unwrap();
    let winning_name;
    let id;
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let b = catalog.create("b", &Default::default()).unwrap();
        let c = catalog.create("c", &Default::default()).unwrap();
        configured(&origin);
        let inputs = [
            batch_for_fact(&b, b.dataset_id(), with_fact(&b)),
            batch_for_fact(&c, c.dataset_id(), with_fact(&c)),
        ];
        let both_acquired = Arc::new(std::sync::Barrier::new(2));
        origin.store().set_failpoint(
            "evidence-held",
            Some(Arc::new(move |_| {
                both_acquired.wait();
            })),
        );
        let mut opts = options();
        opts.idempotency_key = Some("race".into());
        let attempt = |input: &EvidenceInput| {
            evidence::propose(
                Some(&catalog),
                &origin,
                &origin,
                input,
                &opts,
                &mut |_, _| all(),
                &Control::none(),
            )
        };
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| attempt(&inputs[0]));
            let second = attempt(&inputs[1]);
            (first.join().unwrap(), second)
        });
        origin.store().set_failpoint("evidence-held", None);
        winning_name = if first.is_ok() { "b" } else { "c" };
        let (winner, loser) = if first.is_ok() {
            (first, second)
        } else {
            (second, first)
        };
        let winning = winner.unwrap();
        assert!(winning.committed);
        id = winning.hold.unwrap();
        assert_eq!(code(loser), "idempotency-conflict");
        for ds in [&b, &c] {
            assert_eq!(
                evidence::holds(ds).unwrap()[0].id,
                id,
                "both acquisitions completed"
            );
        }
    }
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    for name in ["b", "c"] {
        let target = catalog.get(name).unwrap();
        let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();
        if name == winning_name {
            assert!(released.is_empty());
            assert_eq!(evidence::holds(&target).unwrap()[0].id, id);
        } else {
            assert_eq!(released, [id]);
            assert!(evidence::holds(&target).unwrap().is_empty());
        }
    }
}

#[test]
fn reconciliation_preserves_a_hold_that_a_historical_fork_inherits() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let id = proposed.hold.unwrap();
    for graph in [SOURCES, CLAIMS] {
        evidence::detach(&origin, graph).unwrap();
        origin.update(&format!("DROP GRAPH <{graph}>")).unwrap();
    }
    assert!(!evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    let orphan = uuid::Uuid::new_v4();
    evidence::acquire_hold_for(
        &target,
        orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    let (forking, wait_for_fork) = std::sync::mpsc::channel();
    let confirmed = Arc::new(AtomicBool::new(false));
    let did_confirm = confirmed.clone();
    let checked_origin = origin.clone();
    target.store().set_failpoint(
        "holds-reconcile-confirmed",
        Some(Arc::new(move |_| {
            assert!(checked_origin.store().pin_listed_branch_ids(&[]).is_none());
            did_confirm.store(true, Ordering::SeqCst);
            forking.send(()).unwrap();
        })),
    );
    std::thread::scope(|scope| {
        let fork = scope.spawn(move || {
            wait_for_fork
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            origin
                .create_branch(
                    "fork",
                    &sparkles::branch::BranchOptions {
                        at: sparkles::history::At::Commit(proposed.commit),
                        ..Default::default()
                    },
                )
                .unwrap();
            origin
        });
        assert_eq!(
            evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap(),
            [orphan]
        );
        let origin = fork.join().unwrap();
        assert!(evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    });
    target
        .store()
        .set_failpoint("holds-reconcile-confirmed", None);
    assert!(confirmed.load(Ordering::SeqCst));
    assert_eq!(evidence::holds(&target).unwrap()[0].id, id);
}

#[test]
fn reconciliation_keeps_a_hold_while_a_memory_fork_has_an_unlisted_base() {
    let catalog = sparkles::Catalog::memory(Default::default());
    let origin = catalog
        .attach("origin", sparkles::catalog::Attach::Memory)
        .unwrap();
    let target = catalog
        .attach("target", sparkles::catalog::Attach::Memory)
        .unwrap();
    configured(&origin);
    target
        .update("INSERT DATA { GRAPH <urn:facts> { <urn:release> <urn:affectedBy> <urn:cve> } }")
        .unwrap();
    let fact = target.store().head_commit().seq;
    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let id = proposed.hold.unwrap();
    origin
        .store()
        .set_retention(sparkles::history::Retention {
            keep_commits: Some(32),
            keep_age_ms: None,
            max_bytes: None,
        })
        .unwrap();
    for graph in [SOURCES, CLAIMS] {
        evidence::detach(&origin, graph).unwrap();
        origin.update(&format!("DROP GRAPH <{graph}>")).unwrap();
    }
    let (captured, capture) = std::sync::mpsc::sync_channel(0);
    let (finish, done) = std::sync::mpsc::channel();
    let done = std::sync::Mutex::new(done);
    origin.store().set_failpoint(
        "memory-ordinal-reserved",
        Some(Arc::new(move |_| {
            captured.send(()).unwrap();
            done.lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(30))
                .unwrap();
        })),
    );
    std::thread::scope(|scope| {
        let fork = scope.spawn(|| {
            origin
                .create_branch(
                    "fork",
                    &sparkles::branch::BranchOptions {
                        at: sparkles::history::At::Commit(proposed.commit),
                        ..Default::default()
                    },
                )
                .unwrap()
        });
        capture
            .recv_timeout(std::time::Duration::from_secs(30))
            .unwrap();
        let retained = origin
            .store()
            .set_retention(sparkles::history::Retention {
                keep_commits: Some(1),
                keep_age_ms: None,
                max_bytes: None,
            })
            .unwrap();
        assert!(
            !retained
                .reconstructable
                .iter()
                .any(|(lo, hi)| (*lo..=*hi).contains(&proposed.commit))
        );
        let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();
        finish.send(()).unwrap();
        fork.join().unwrap();
        assert!(released.is_empty());
    });
    origin
        .store()
        .set_failpoint("memory-ordinal-reserved", None);
    assert!(evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    assert_eq!(evidence::holds(&target).unwrap()[0].id, id);
}
