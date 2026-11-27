//! Authenticated claim proposals (spec C25 §5 and §7): one batch publishes as one
//! commit of the evidence owner with server-owned receipts, first availability, record
//! digests and the operation marker, after the caller's grants, the references, the
//! idempotency key, the expected head and the caller's authority are checked.

use oxrdf::{Literal, NamedNode, Term, Triple};
use sparkles::Dataset;
use sparkles::access::{GraphAccess, GraphRule, Graphs, Protection, Rule, TripleRules};
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
            was_revision_of: None,
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
            was_revision_of: None,
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
    fn a_reconciliation_does_not_wait_for_a_busy_writer() {
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
            assert_eq!(released, 0, "the busy writer leaves the hold unconfirmed");
            drop(t);
        });
        assert_eq!(
            evidence::reconcile_operation_holds(&ds, 0).unwrap().len(),
            1
        );
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
        assert!(released.is_empty(), "the busy origin cannot be confirmed");
        drop(t);
    });
    assert_eq!(
        evidence::reconcile_operation_holds_in(Some(&cat), &b, 0).unwrap(),
        [orphan]
    );
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
            for dry_run in [false, true] {
                opts.dry_run = dry_run;
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
            }
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

#[test]
fn reconciliation_keeps_the_hold_of_a_recoverable_origin_branch_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let mut opts = options();
    opts.idempotency_key = Some("failed-origin-sync".into());
    let id;
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        configured(&target);
        origin.create_branch("work", &Default::default()).unwrap();
        let work = origin.branch("work").unwrap();
        let fact = with_fact(&target);
        id = evidence::operation_id(origin.dataset_id(), &opts.principal, "failed-origin-sync");
        let head = work.store().head_commit().seq;
        work.store().fail_next_vocab_sync();
        let error = evidence::propose(
            Some(&catalog),
            &origin,
            &work,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &opts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap_err();
        assert!(matches!(error, sparkles::Error::Io(_)), "{error:?}");
        assert_eq!(work.store().head_commit().seq, head);
        assert!(
            evidence::holds(&target)
                .unwrap()
                .iter()
                .any(|hold| hold.id == id)
        );
        assert!(!evidence::operation_recorded(&origin, id, &Control::none()).unwrap());

        let future =
            target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
        target.store().set_clock(Arc::new(move || future));
        assert!(
            evidence::propose(
                Some(&catalog),
                &target,
                &target,
                &batch(&target, fact),
                &options(),
                &mut |_, _| all(),
                &Control::none(),
            )
            .unwrap()
            .committed
        );
        assert!(
            evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0)
                .unwrap()
                .is_empty()
        );
        assert!(
            evidence::holds(&target)
                .unwrap()
                .iter()
                .any(|hold| hold.id == id)
        );
    }
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.get("origin").unwrap();
    let target = catalog.get("target").unwrap();
    assert!(evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    assert!(
        evidence::holds(&target)
            .unwrap()
            .iter()
            .any(|hold| hold.id == id)
    );
    assert!(
        evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn legacy_reconciliation_callbacks_only_see_local_holds() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let foreign = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap()
    .hold
    .unwrap();
    let local = uuid::Uuid::new_v4();
    evidence::acquire_hold(
        &target,
        local,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
    )
    .unwrap();

    let released = evidence::reconcile_holds(&target, 0, &mut |hold| {
        assert_eq!(hold.id, local);
        evidence::operation_recorded(&target, hold.id, &Control::none())
    })
    .unwrap();
    assert_eq!(released, [local]);
    assert_eq!(evidence::holds(&target).unwrap()[0].id, foreign);
}

#[test]
fn post_commit_reconciliation_shares_request_cancellation_across_targets() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let origin_fact = with_fact(&origin);
    let target_fact = with_fact(&target);
    let local = uuid::Uuid::new_v4();
    let foreign = uuid::Uuid::new_v4();
    evidence::acquire_hold(
        &origin,
        local,
        &[evidence::CommitRef {
            branch_id: origin.dataset_id(),
            seq: origin_fact,
        }],
    )
    .unwrap();
    evidence::acquire_hold_for(
        &target,
        foreign,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: target_fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    for ds in [&origin, &target] {
        let future = ds.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
        ds.store().set_clock(Arc::new(move || future));
    }

    let control = Control::none();
    let cancel = control.cancel.clone();
    origin.store().set_failpoint(
        "evidence-reconcile-origin-opened",
        Some(Arc::new(move |_| cancel.cancel())),
    );
    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), target_fact),
        &options(),
        &mut |_, _| all(),
        &control,
    )
    .unwrap();
    origin
        .store()
        .set_failpoint("evidence-reconcile-origin-opened", None);
    assert!(proposed.committed);
    assert!(matches!(control.check(), Err(sparkles::Error::Cancelled)));
    assert_eq!(
        evidence::holds(&origin)
            .unwrap()
            .iter()
            .map(|hold| hold.id)
            .collect::<Vec<_>>(),
        [local]
    );
    let held = evidence::holds(&target).unwrap();
    assert_eq!(held.len(), 2);
    assert!(held.iter().any(|hold| hold.id == foreign));
    assert!(held.iter().any(|hold| Some(hold.id) == proposed.hold));
}

#[test]
fn reconciliation_keeps_holds_when_the_original_registration_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let copied_root = dir.path().join("copy");
    let (origin_id, hold_id);
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        copy_dataset(origin.store().root().unwrap(), &copied_root);
        origin_id = origin.dataset_id();
        let fact = with_fact(&target);
        hold_id = evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap()
        .hold
        .unwrap();
    }

    let (catalog, _) = sparkles::Catalog::open_deferred(dir.path(), Default::default()).unwrap();
    assert!(catalog.is_opening("origin"));
    catalog.open_registered("target").unwrap();
    let target = catalog.get("target").unwrap();
    let copy = catalog
        .attach("copy", sparkles::catalog::Attach::Directory(copied_root))
        .unwrap();
    assert_eq!(copy.dataset_id(), origin_id);
    assert!(!evidence::operation_recorded(&copy, hold_id, &Control::none()).unwrap());
    let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();

    catalog.open_registered("origin").unwrap();
    let origin = catalog.get("origin").unwrap();
    assert_eq!(origin.dataset_id(), origin_id);
    assert!(evidence::operation_recorded(&origin, hold_id, &Control::none()).unwrap());
    assert!(
        released.is_empty(),
        "released a hold owned by a pending origin"
    );
    assert_eq!(evidence::holds(&target).unwrap()[0].id, hold_id);
}

#[test]
fn reconciliation_keeps_a_synced_receipt_after_unwind_and_process_exit() {
    const CHILD_ROOT: &str = "SPARKLES_EVIDENCE_WAL_UNWIND_ROOT";
    const KEY: &str = "synced-wal-unwind";
    let mut opts = options();
    opts.idempotency_key = Some(KEY.into());
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let catalog = sparkles::Catalog::open(root, Default::default()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        let fact = with_fact(&target);
        let head = origin.store().head_commit().seq;
        origin.store().set_failpoint(
            "evidence-wal-synced",
            Some(Arc::new(|_| panic!("unwind after WAL sync"))),
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            evidence::propose(
                Some(&catalog),
                &origin,
                &origin,
                &batch_for_fact(&target, target.dataset_id(), fact),
                &opts,
                &mut |_, _| all(),
                &Control::none(),
            )
        }));
        assert!(result.is_err(), "expected to unwind at the sync boundary");
        assert_eq!(origin.store().head_commit().seq, head);
        assert_eq!(evidence::holds(&target).unwrap().len(), 1);
        evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();
        // A process exit leaves the WAL intact. Orderly Store drop can trim its suffix.
        std::process::exit(0);
    }

    let dir = tempfile::tempdir().unwrap();
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "reconciliation_keeps_a_synced_receipt_after_unwind_and_process_exit",
            "--nocapture",
        ])
        .env(CHILD_ROOT, dir.path())
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.get("origin").unwrap();
    let target = catalog.get("target").unwrap();
    let id = evidence::operation_id(origin.dataset_id(), &opts.principal, KEY);
    assert!(evidence::operation_recorded(&origin, id, &Control::none()).unwrap());
    assert!(
        evidence::holds(&target)
            .unwrap()
            .iter()
            .any(|hold| hold.id == id)
    );
}

#[test]
fn reconciliation_keeps_foreign_hold_when_historical_read_exceeds_scan_bound() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let foreign_orphan = uuid::Uuid::from_u128(1);
    let local_orphan = uuid::Uuid::from_u128(2);
    evidence::acquire_hold_for(
        &target,
        foreign_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    evidence::acquire_hold(
        &target,
        local_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
    )
    .unwrap();
    origin.store().set_failpoint(
        "evidence-reconcile-historical-read",
        Some(Arc::new(|_| {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        })),
    );
    let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0);
    origin
        .store()
        .set_failpoint("evidence-reconcile-historical-read", None);
    let released =
        released.expect("internal scan bound must keep the foreign hold without aborting");
    assert_eq!(released, [local_orphan]);
    let held: Vec<_> = evidence::holds(&target)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(held, [foreign_orphan]);
}

#[test]
fn post_commit_reconciliation_cancels_during_historical_read() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let foreign_orphan = uuid::Uuid::from_u128(1);
    let local_orphan = uuid::Uuid::from_u128(2);
    evidence::acquire_hold_for(
        &target,
        foreign_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    evidence::acquire_hold(
        &target,
        local_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
    )
    .unwrap();
    let future = target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
    target.store().set_clock(Arc::new(move || future));

    let control = Control::none();
    let cancel = control.cancel.clone();
    origin.store().set_failpoint(
        "evidence-reconcile-historical-read",
        Some(Arc::new(move |_| cancel.cancel())),
    );
    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &control,
    )
    .unwrap();
    origin
        .store()
        .set_failpoint("evidence-reconcile-historical-read", None);
    assert!(proposed.committed);
    assert!(matches!(control.check(), Err(sparkles::Error::Cancelled)));
    let held: Vec<_> = evidence::holds(&target)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(held.len(), 3);
    assert!(held.contains(&foreign_orphan));
    assert!(held.contains(&local_orphan));
    assert!(held.contains(&proposed.hold.unwrap()));
}

#[test]
fn reconciliation_keeps_holds_when_an_expected_attachment_is_pending() {
    let dir = tempfile::tempdir().unwrap();
    let origin_loc = dir.path().join("origin-loc");
    let copied_root = dir.path().join("copy");
    let (origin_id, hold_id, orphan_id);
    {
        let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
        let origin = catalog
            .attach(
                "origin",
                sparkles::catalog::Attach::Directory(origin_loc.clone()),
            )
            .unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        copy_dataset(origin.store().root().unwrap(), &copied_root);
        origin_id = origin.dataset_id();
        let fact = with_fact(&target);
        hold_id = evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap()
        .hold
        .unwrap();
        orphan_id = uuid::Uuid::new_v4();
        evidence::acquire_hold_for(
            &target,
            orphan_id,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
            Some(origin_id),
        )
        .unwrap();
    }

    let catalog = sparkles::Catalog::open(dir.path(), Default::default()).unwrap();
    assert!(catalog.opening().is_empty());
    let target = catalog.get("target").unwrap();
    let copy = catalog
        .attach("copy", sparkles::catalog::Attach::Directory(copied_root))
        .unwrap();
    assert_eq!(copy.dataset_id(), origin_id);
    assert!(!evidence::operation_recorded(&copy, hold_id, &Control::none()).unwrap());

    catalog.expect("origin").unwrap();
    assert!(catalog.is_expected("origin"));
    let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();
    assert!(
        released.is_empty(),
        "released a hold while an expected startup attachment was still pending: {released:?}"
    );
    assert!(
        evidence::holds(&target)
            .unwrap()
            .iter()
            .any(|hold| hold.id == hold_id)
    );

    let origin = catalog
        .attach_expected("origin", sparkles::catalog::Attach::Directory(origin_loc))
        .unwrap();
    assert!(!catalog.is_expected("origin"));
    assert_eq!(origin.dataset_id(), origin_id);
    assert!(evidence::operation_recorded(&origin, hold_id, &Control::none()).unwrap());

    drop(copy);
    assert!(catalog.delete("copy").unwrap());
    let released = evidence::reconcile_operation_holds_in(Some(&catalog), &target, 0).unwrap();
    assert_eq!(released, [orphan_id]);
    assert_eq!(
        evidence::holds(&target)
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect::<Vec<_>>(),
        [hold_id]
    );
}

#[test]
fn post_commit_reconciliation_cancels_during_cold_changelog_index_read() {
    use sparkles::store::{ChangeLogReadEvent, StoreOptions, set_changelog_read_observer};
    use std::sync::atomic::AtomicUsize;

    struct ResetObserver;
    impl Drop for ResetObserver {
        fn drop(&mut self) {
            set_changelog_read_observer(None);
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let catalog_opts = StoreOptions {
        change_log_segment_bytes: 4096,
        ..Default::default()
    };
    let fact;
    let foreign_orphan = uuid::Uuid::from_u128(1);
    let local_orphan = uuid::Uuid::from_u128(2);
    {
        let catalog = sparkles::Catalog::open(dir.path(), catalog_opts.clone().into()).unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        fact = with_fact(&target);
        // Write enough distinct terms to produce a sealed .idx file larger than 128 KiB (>2 chunks).
        let mut triples = String::new();
        use std::fmt::Write as _;
        for i in 0..8_000 {
            writeln!(&mut triples, "<urn:big{i}> <urn:p> {i} .").unwrap();
        }
        origin
            .update(&format!("INSERT DATA {{ {triples} }}"))
            .unwrap();
        origin.store().flush_change_log().unwrap();
        origin
            .update("INSERT DATA { <urn:after> <urn:p> 1 . }")
            .unwrap();
        origin.store().flush_change_log().unwrap();

        let changes_dir = origin.store().root().unwrap().join("changes");
        let max_idx_len = std::fs::read_dir(&changes_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("idx"))
            .map(|e| e.metadata().unwrap().len())
            .max()
            .unwrap_or(0);
        assert!(
            max_idx_len > (128 << 10),
            "expected sealed cold index > 128 KiB, got {max_idx_len}"
        );

        evidence::acquire_hold_for(
            &target,
            foreign_orphan,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
            Some(origin.dataset_id()),
        )
        .unwrap();
        evidence::acquire_hold(
            &target,
            local_orphan,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
        )
        .unwrap();
    }

    // Reopen so every sealed segment index is cold (not in ChangeLog::inner.cache).
    let catalog = sparkles::Catalog::open(dir.path(), catalog_opts.into()).unwrap();
    let origin = catalog.get("origin").unwrap();
    let target = catalog.get("target").unwrap();
    let future = target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
    target.store().set_clock(Arc::new(move || future));

    let control = Control::none();
    let cancel = control.cancel.clone();
    let triggered = Arc::new(AtomicBool::new(false));
    let chunks_after = Arc::new(AtomicUsize::new(0));
    let decodes_after = Arc::new(AtomicUsize::new(0));
    let t = triggered.clone();
    let c_after = chunks_after.clone();
    let d_after = decodes_after.clone();
    let _reset = ResetObserver;
    set_changelog_read_observer(Some(Arc::new(move |event| match event {
        ChangeLogReadEvent::IndexReadChunk => {
            if !t.swap(true, Ordering::SeqCst) {
                cancel.cancel();
            } else {
                c_after.fetch_add(1, Ordering::SeqCst);
            }
        }
        ChangeLogReadEvent::IndexDecode if t.load(Ordering::SeqCst) => {
            d_after.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    })));

    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &control,
    )
    .unwrap();
    drop(_reset);

    assert!(proposed.committed);
    assert!(
        triggered.load(Ordering::SeqCst),
        "cold index read was not reached"
    );
    assert!(matches!(control.check(), Err(sparkles::Error::Cancelled)));
    assert_eq!(
        chunks_after.load(Ordering::SeqCst),
        0,
        "continued reading cold index chunks after cancellation"
    );
    assert_eq!(
        decodes_after.load(Ordering::SeqCst),
        0,
        "decoded cold index after cancellation"
    );
    let held: Vec<_> = evidence::holds(&target)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(held.len(), 3);
    assert!(held.contains(&foreign_orphan));
    assert!(held.contains(&local_orphan));
    assert!(held.contains(&proposed.hold.unwrap()));
}

#[test]
fn post_commit_reconciliation_cancels_during_large_changelog_record_read() {
    use sparkles::store::{ChangeLogReadEvent, StoreOptions, set_changelog_read_observer};
    use std::sync::atomic::AtomicUsize;

    struct ResetObserver;
    impl Drop for ResetObserver {
        fn drop(&mut self) {
            set_changelog_read_observer(None);
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(
        dir.path(),
        StoreOptions {
            change_log_segment_bytes: 4096,
            ..Default::default()
        }
        .into(),
    )
    .unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let foreign_orphan = uuid::Uuid::from_u128(1);
    let local_orphan = uuid::Uuid::from_u128(2);
    let op = evidence::operation_iri(foreign_orphan);
    let payload = "p".repeat(256 << 10);

    // Record a >256 KiB commit matching the foreign orphan's subject and rdf:type predicate,
    // then remove it so current_holds is false and historical_holds reads the large record.
    origin
        .update(&format!(
            "INSERT DATA {{ <{}> a <urn:not-an-operation> ; <urn:pad> \"{payload}\" . }}",
            op.as_str()
        ))
        .unwrap();
    origin.store().flush_change_log().unwrap();
    origin
        .update(&format!(
            "DELETE DATA {{ <{}> a <urn:not-an-operation> ; <urn:pad> \"{payload}\" . }}",
            op.as_str()
        ))
        .unwrap();
    origin.store().flush_change_log().unwrap();

    evidence::acquire_hold_for(
        &target,
        foreign_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    evidence::acquire_hold(
        &target,
        local_orphan,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
    )
    .unwrap();
    let future = target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
    target.store().set_clock(Arc::new(move || future));

    let control = Control::none();
    let cancel = control.cancel.clone();
    let triggered = Arc::new(AtomicBool::new(false));
    let chunks_after = Arc::new(AtomicUsize::new(0));
    let decodes_after = Arc::new(AtomicUsize::new(0));
    let t = triggered.clone();
    let c_after = chunks_after.clone();
    let d_after = decodes_after.clone();
    let _reset = ResetObserver;
    set_changelog_read_observer(Some(Arc::new(move |event| match event {
        ChangeLogReadEvent::RecordReadChunk => {
            if !t.swap(true, Ordering::SeqCst) {
                cancel.cancel();
            } else {
                c_after.fetch_add(1, Ordering::SeqCst);
            }
        }
        ChangeLogReadEvent::RecordDecode if t.load(Ordering::SeqCst) => {
            d_after.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    })));

    let proposed = evidence::propose(
        Some(&catalog),
        &origin,
        &origin,
        &batch_for_fact(&target, target.dataset_id(), fact),
        &options(),
        &mut |_, _| all(),
        &control,
    )
    .unwrap();
    drop(_reset);

    assert!(proposed.committed);
    assert!(
        triggered.load(Ordering::SeqCst),
        "large record read was not reached"
    );
    assert!(matches!(control.check(), Err(sparkles::Error::Cancelled)));
    assert_eq!(
        chunks_after.load(Ordering::SeqCst),
        0,
        "continued reading large record chunks after cancellation"
    );
    assert_eq!(
        decodes_after.load(Ordering::SeqCst),
        0,
        "decoded large record after cancellation"
    );
    let held: Vec<_> = evidence::holds(&target)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(held.len(), 3);
    assert!(held.contains(&foreign_orphan));
    assert!(held.contains(&local_orphan));
    assert!(held.contains(&proposed.hold.unwrap()));
}

#[test]
fn post_commit_reconciliation_stops_during_warm_candidate_traversal() {
    use sparkles::store::{ChangeLogReadEvent, StoreOptions, set_changelog_read_observer};
    use std::sync::atomic::AtomicUsize;
    use std::time::{Duration, Instant};

    struct ResetObserver;
    impl Drop for ResetObserver {
        fn drop(&mut self) {
            set_changelog_read_observer(None);
        }
    }

    for use_deadline in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let catalog = sparkles::Catalog::open(
            dir.path(),
            StoreOptions {
                // Keep all commits inside a single warm/live open segment (no cold .idx loads).
                change_log_segment_bytes: 64 << 20,
                ..Default::default()
            }
            .into(),
        )
        .unwrap();
        let origin = catalog.create("origin", &Default::default()).unwrap();
        let target = catalog.create("target", &Default::default()).unwrap();
        configured(&origin);
        let fact = with_fact(&target);
        let foreign_sparse = uuid::Uuid::from_u128(1);
        let foreign_missing = uuid::Uuid::from_u128(2);
        let local_orphan = uuid::Uuid::from_u128(3);
        let sparse_op = evidence::operation_iri(foreign_sparse);

        // Build a warm/live segment with frequent rdf:type commits and a sparse
        // subject posting for foreign_sparse (no postings for foreign_missing).
        for i in 0..40 {
            origin
                .update(&format!(
                    "INSERT DATA {{ <urn:noise:{i}> a <urn:Thing> . }}"
                ))
                .unwrap();
        }
        origin
            .update(&format!(
                "INSERT DATA {{ <{}> <urn:other> \"sparse\" . }}",
                sparse_op.as_str()
            ))
            .unwrap();
        origin.store().flush_change_log().unwrap();

        evidence::acquire_hold_for(
            &target,
            foreign_sparse,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
            Some(origin.dataset_id()),
        )
        .unwrap();
        evidence::acquire_hold_for(
            &target,
            foreign_missing,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
            Some(origin.dataset_id()),
        )
        .unwrap();
        evidence::acquire_hold(
            &target,
            local_orphan,
            &[evidence::CommitRef {
                branch_id: target.dataset_id(),
                seq: fact,
            }],
        )
        .unwrap();

        let future =
            target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
        target.store().set_clock(Arc::new(move || future));

        let deadline = use_deadline.then(|| Instant::now() + Duration::from_millis(300));
        let control = Control {
            deadline,
            ..Control::none()
        };
        let cancel = control.cancel.clone();
        let triggered = Arc::new(AtomicBool::new(false));
        let checks_after = Arc::new(AtomicUsize::new(0));
        let index_chunks = Arc::new(AtomicUsize::new(0));
        let record_reads = Arc::new(AtomicUsize::new(0));
        let t = triggered.clone();
        let c_after = checks_after.clone();
        let i_chunks = index_chunks.clone();
        let r_reads = record_reads.clone();
        let _reset = ResetObserver;
        set_changelog_read_observer(Some(Arc::new(move |event| match event {
            ChangeLogReadEvent::IndexReadChunk => {
                i_chunks.fetch_add(1, Ordering::SeqCst);
            }
            ChangeLogReadEvent::CandidateCheck => {
                if !t.swap(true, Ordering::SeqCst) {
                    if let Some(limit) = deadline {
                        while Instant::now() <= limit {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    } else {
                        cancel.cancel();
                    }
                } else {
                    c_after.fetch_add(1, Ordering::SeqCst);
                }
            }
            ChangeLogReadEvent::RecordReadChunk | ChangeLogReadEvent::RecordDecode
                if t.load(Ordering::SeqCst) =>
            {
                r_reads.fetch_add(1, Ordering::SeqCst);
            }
            _ => {}
        })));

        let proposed = evidence::propose(
            Some(&catalog),
            &origin,
            &origin,
            &batch_for_fact(&target, target.dataset_id(), fact),
            &options(),
            &mut |_, _| all(),
            &control,
        )
        .unwrap();
        drop(_reset);

        assert!(proposed.committed, "use_deadline={use_deadline}");
        assert!(
            triggered.load(Ordering::SeqCst),
            "use_deadline={use_deadline}: warm candidate traversal was not reached"
        );
        assert_eq!(
            index_chunks.load(Ordering::SeqCst),
            0,
            "use_deadline={use_deadline}: expected warm resident index without cold reads"
        );
        assert_eq!(
            checks_after.load(Ordering::SeqCst),
            0,
            "use_deadline={use_deadline}: continued candidate traversal after stop"
        );
        assert_eq!(
            record_reads.load(Ordering::SeqCst),
            0,
            "use_deadline={use_deadline}: read records after candidate traversal stop"
        );
        if use_deadline {
            assert!(matches!(control.check(), Err(sparkles::Error::Timeout)));
        } else {
            assert!(matches!(control.check(), Err(sparkles::Error::Cancelled)));
        }
        let held: Vec<_> = evidence::holds(&target)
            .unwrap()
            .into_iter()
            .map(|h| h.id)
            .collect();
        assert!(
            held.contains(&foreign_sparse),
            "use_deadline={use_deadline}: sparse hold must remain in place after stop"
        );
        assert!(
            held.contains(&local_orphan),
            "use_deadline={use_deadline}: unchecked local hold must remain in place"
        );
        assert!(held.contains(&proposed.hold.unwrap()));
    }

    // Confirm that a missing operation subject with frequent rdf:type short-circuits
    // cleanly on a warm segment without candidate traversal or record reads.
    let dir = tempfile::tempdir().unwrap();
    let catalog = sparkles::Catalog::open(
        dir.path(),
        StoreOptions {
            change_log_segment_bytes: 64 << 20,
            ..Default::default()
        }
        .into(),
    )
    .unwrap();
    let origin = catalog.create("origin", &Default::default()).unwrap();
    let target = catalog.create("target", &Default::default()).unwrap();
    configured(&origin);
    let fact = with_fact(&target);
    let foreign_missing = uuid::Uuid::from_u128(10);
    for i in 0..40 {
        origin
            .update(&format!(
                "INSERT DATA {{ <urn:noise:{i}> a <urn:Thing> . }}"
            ))
            .unwrap();
    }
    origin.store().flush_change_log().unwrap();
    evidence::acquire_hold_for(
        &target,
        foreign_missing,
        &[evidence::CommitRef {
            branch_id: target.dataset_id(),
            seq: fact,
        }],
        Some(origin.dataset_id()),
    )
    .unwrap();
    let future = target.store().head_commit().timestamp_ms + 2 * sparkles::holds::DEFAULT_GRACE_MS;
    target.store().set_clock(Arc::new(move || future));

    let candidate_checks = Arc::new(AtomicUsize::new(0));
    let record_reads = Arc::new(AtomicUsize::new(0));
    let c_checks = candidate_checks.clone();
    let r_reads = record_reads.clone();
    let _reset = ResetObserver;
    set_changelog_read_observer(Some(Arc::new(move |event| match event {
        ChangeLogReadEvent::CandidateCheck => {
            c_checks.fetch_add(1, Ordering::SeqCst);
        }
        ChangeLogReadEvent::RecordReadChunk | ChangeLogReadEvent::RecordDecode => {
            r_reads.fetch_add(1, Ordering::SeqCst);
        }
        _ => {}
    })));
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
    drop(_reset);

    assert!(proposed.committed);
    assert_eq!(
        candidate_checks.load(Ordering::SeqCst),
        0,
        "missing subject must short-circuit before candidate traversal"
    );
    assert_eq!(
        record_reads.load(Ordering::SeqCst),
        0,
        "missing subject must read zero changelog records"
    );
    let held: Vec<_> = evidence::holds(&target)
        .unwrap()
        .into_iter()
        .map(|h| h.id)
        .collect();
    assert_eq!(held, [proposed.hold.unwrap()]);
}

// ------------------------------------------------ item 551: existing-record selections ---

fn json_method() -> MethodRef {
    MethodRef {
        method: iri(evidence::JSON_LEXICAL_UTF8_V1),
        version: key(evidence::JSON_LEXICAL_UTF8_VERSION),
    }
}

#[test]
fn proposing_passages_against_an_existing_rendition_verifies_revision_and_text() {
    let ds = Dataset::memory();
    configured(&ds);
    let unicode_text = "naïve café 🦀 release 1.2 is affected.";
    let rev_input = SourceRevisionInput {
        was_revision_of: None,
        version: RecordVersion::V1,
        key: key("rev"),
        source: iri("urn:bulletin"),
        source_key: key("bulletin-unicode"),
        upstream_version: None,
        content: SourceContent::Retained {
            bytes: unicode_text.as_bytes().to_vec(),
            expected_digest: Some(sha(unicode_text)),
        },
        metadata: Default::default(),
        times: Default::default(),
        lineage: Default::default(),
    };
    let rend_input = RenditionInput {
        version: RecordVersion::V1,
        key: key("rend"),
        revision: local("rev"),
        occurrence_key: key("rend-text"),
        conversion: method("urn:method:identity"),
        media_type: "text/plain".into(),
        language: Some("en".into()),
        content: RenditionContent::NormalizedText(unicode_text.into()),
        expected_digest: None,
    };
    let first = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![rev_input.clone()],
            renditions: vec![rend_input.clone()],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let rev_iri = iri_of(&first, "rev");
    let rend_iri = iri_of(&first, "rend");

    // A second unrelated revision, to check mismatched revision refusal.
    let other_rev = propose(&ds, &small("other", proposition()), &options()).unwrap();
    let other_rev_iri = iri_of(&other_rev, "rev");

    // A metadata-only rendition and a retained-bytes rendition (no normalizedText).
    let no_text = propose(
        &ds,
        &EvidenceInput {
            renditions: vec![
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("meta"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    occurrence_key: key("rend-meta"),
                    conversion: method("urn:method:identity"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::MetadataOnly(sha(unicode_text)),
                    expected_digest: None,
                },
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("bytes"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    occurrence_key: key("rend-bytes"),
                    conversion: method("urn:method:identity"),
                    media_type: "application/octet-stream".into(),
                    language: None,
                    content: RenditionContent::RetainedBytes(unicode_text.as_bytes().to_vec()),
                    expected_digest: None,
                },
            ],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let meta_rend_iri = iri_of(&no_text, "meta");
    let bytes_rend_iri = iri_of(&no_text, "bytes");

    let cafe: String = unicode_text.chars().skip(6).take(4).collect();
    assert_eq!(cafe, "café");
    let crab: String = unicode_text.chars().skip(11).take(1).collect();
    assert_eq!(crab, "🦀");

    // Propose two passages against the existing rendition in one batch (one naming
    // existing rev, one re-sending the identical local rev).
    let mut o_idem = options();
    o_idem.idempotency_key = Some("passage-existing-rendition".into());
    let proposed = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![rev_input.clone()],
            passages: vec![
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p1"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    rendition: RecordRef::Existing(rend_iri.clone()),
                    occurrence_key: key("occ-cafe"),
                    start: 6,
                    end: 10,
                    text_digest: sha(&cafe),
                    quote: Some(cafe.clone()),
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p2"),
                    revision: local("rev"),
                    rendition: RecordRef::Existing(rend_iri.clone()),
                    occurrence_key: key("occ-crab"),
                    start: 11,
                    end: 12,
                    text_digest: sha(&crab),
                    quote: None,
                },
            ],
            ..Default::default()
        },
        &o_idem,
    )
    .unwrap();
    assert!(proposed.committed && !proposed.replayed);
    let p1_rec = proposed.records.iter().find(|r| r.key == "p1").unwrap();
    let p2_rec = proposed.records.iter().find(|r| r.key == "p2").unwrap();
    assert!(!p1_rec.existing && !p2_rec.existing);
    assert!(
        proposed
            .records
            .iter()
            .find(|r| r.key == "rev")
            .unwrap()
            .existing
    );
    // Both passages resolve through Resolution::record.
    let v1 = record(&ds, &p1_rec.iri, RecordKind::Passage).unwrap();
    assert_eq!(
        (v1.span.as_ref().unwrap().start, v1.span.unwrap().end),
        (6, 10)
    );
    let v2 = record(&ds, &p2_rec.iri, RecordKind::Passage).unwrap();
    assert_eq!(
        (v2.span.as_ref().unwrap().start, v2.span.unwrap().end),
        (11, 12)
    );

    // Digest & IRI stability: proposing the same passage with a local rendition on a
    // fresh dataset (same dataset_id cannot be forged, so dry-run on same dataset)
    // yields the exact same IRI and record_digest!
    let mut dry = options();
    dry.dry_run = true;
    let local_dry = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![rev_input.clone()],
            renditions: vec![rend_input.clone()],
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("p1"),
                revision: local("rev"),
                rendition: local("rend"),
                occurrence_key: key("occ-cafe"),
                start: 6,
                end: 10,
                text_digest: sha(&cafe),
                quote: Some(cafe.clone()),
            }],
            ..Default::default()
        },
        &dry,
    )
    .unwrap();
    let local_p1 = local_dry.records.iter().find(|r| r.key == "p1").unwrap();
    assert_eq!(local_p1.iri, p1_rec.iri);
    assert_eq!(local_p1.digest, p1_rec.digest);
    assert!(local_p1.existing);

    // Idempotency replay returns the exact first receipt without writing.
    let head = ds.store().head_commit().seq;
    let replayed = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![rev_input.clone()],
            passages: vec![
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p1"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    rendition: RecordRef::Existing(rend_iri.clone()),
                    occurrence_key: key("occ-cafe"),
                    start: 6,
                    end: 10,
                    text_digest: sha(&cafe),
                    quote: Some(cafe.clone()),
                },
                PassageInput {
                    version: RecordVersion::V1,
                    key: key("p2"),
                    revision: local("rev"),
                    rendition: RecordRef::Existing(rend_iri.clone()),
                    occurrence_key: key("occ-crab"),
                    start: 11,
                    end: 12,
                    text_digest: sha(&crab),
                    quote: None,
                },
            ],
            ..Default::default()
        },
        &o_idem,
    )
    .unwrap();
    assert!(replayed.replayed && !replayed.committed);
    assert_eq!(replayed.records, proposed.records);
    assert_eq!(ds.store().head_commit().seq, head);

    // Refusals: mismatched quote, mismatched text_digest, out-of-bounds offset,
    // mismatched revision, metadata-only/retained-bytes existing rendition, local
    // metadata-only rendition. None writes anything.
    let attempt = |p: PassageInput| {
        propose(
            &ds,
            &EvidenceInput {
                passages: vec![p],
                ..Default::default()
            },
            &options(),
        )
    };
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("bad-quote"),
            revision: RecordRef::Existing(rev_iri.clone()),
            rendition: RecordRef::Existing(rend_iri.clone()),
            occurrence_key: key("bad-quote"),
            start: 6,
            end: 10,
            text_digest: sha(&cafe),
            quote: Some("cafe".into()),
        })),
        "span-mismatch"
    );
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("bad-digest"),
            revision: RecordRef::Existing(rev_iri.clone()),
            rendition: RecordRef::Existing(rend_iri.clone()),
            occurrence_key: key("bad-digest"),
            start: 6,
            end: 10,
            text_digest: sha("wrong"),
            quote: Some(cafe.clone()),
        })),
        "span-mismatch"
    );
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("past-end"),
            revision: RecordRef::Existing(rev_iri.clone()),
            rendition: RecordRef::Existing(rend_iri.clone()),
            occurrence_key: key("past-end"),
            start: 6,
            end: 999,
            text_digest: sha(&cafe),
            quote: None,
        })),
        "span-mismatch"
    );
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("wrong-rev"),
            revision: RecordRef::Existing(other_rev_iri),
            rendition: RecordRef::Existing(rend_iri.clone()),
            occurrence_key: key("wrong-rev"),
            start: 6,
            end: 10,
            text_digest: sha(&cafe),
            quote: Some(cafe.clone()),
        })),
        "invalid"
    );
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("meta-rend"),
            revision: RecordRef::Existing(rev_iri.clone()),
            rendition: RecordRef::Existing(meta_rend_iri),
            occurrence_key: key("meta-rend"),
            start: 6,
            end: 10,
            text_digest: sha(&cafe),
            quote: Some(cafe.clone()),
        })),
        "evidence-unavailable"
    );
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("bytes-rend"),
            revision: RecordRef::Existing(rev_iri.clone()),
            rendition: RecordRef::Existing(bytes_rend_iri),
            occurrence_key: key("bytes-rend"),
            start: 6,
            end: 10,
            text_digest: sha(&cafe),
            quote: Some(cafe.clone()),
        })),
        "evidence-unavailable"
    );
    // Local metadata-only rendition cannot verify a passage either.
    assert_eq!(
        code(propose(
            &ds,
            &EvidenceInput {
                renditions: vec![RenditionInput {
                    version: RecordVersion::V1,
                    key: key("local-meta"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    occurrence_key: key("local-meta-occ"),
                    conversion: method("urn:method:identity"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::MetadataOnly(sha(unicode_text)),
                    expected_digest: None,
                }],
                passages: vec![PassageInput {
                    version: RecordVersion::V1,
                    key: key("local-meta-p"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    rendition: local("local-meta"),
                    occurrence_key: key("local-meta-p"),
                    start: 6,
                    end: 10,
                    text_digest: sha(&cafe),
                    quote: Some(cafe.clone()),
                }],
                ..Default::default()
            },
            &options(),
        )),
        "evidence-unavailable"
    );

    // Unauthorized caller gets identical evidence-unavailable as for a missing rendition.
    let limited = || {
        Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([CLAIMS], &[])),
            Graphs::All,
        )))
    };
    let missing_iri = iri("urn:uuid:00000000-0000-4000-8000-000000000000");
    let unauth_answer = |target_rend: NamedNode| {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                passages: vec![PassageInput {
                    version: RecordVersion::V1,
                    key: key("p-unauth"),
                    revision: RecordRef::Existing(rev_iri.clone()),
                    rendition: RecordRef::Existing(target_rend.clone()),
                    occurrence_key: key("p-unauth"),
                    start: 6,
                    end: 10,
                    text_digest: sha(&cafe),
                    quote: Some(cafe.clone()),
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| limited(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string()
                .replace(rev_iri.as_str(), "REV")
                .replace(target_rend.as_str(), "REND"),
        )
    };
    let hidden = unauth_answer(rend_iri.clone());
    let missing = unauth_answer(missing_iri);
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);
    assert_eq!(ds.store().head_commit().seq, head);

    // Detached and re-protected rendition answers origin-not-reproduced.
    evidence::detach(&ds, SOURCES).unwrap();
    configured(&ds);
    assert_eq!(
        code(attempt(PassageInput {
            version: RecordVersion::V1,
            key: key("detached-rend"),
            revision: RecordRef::Existing(rev_iri),
            rendition: RecordRef::Existing(rend_iri),
            occurrence_key: key("detached-rend"),
            start: 6,
            end: 10,
            text_digest: sha(&cafe),
            quote: Some(cafe),
        })),
        "origin-not-reproduced"
    );
}

#[test]
fn referencing_an_existing_passage_verifies_its_revision_and_rendition() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);
    let first = propose(&ds, &batch(&ds, fact), &options()).unwrap();
    let rev1 = iri_of(&first, "rev");
    let rend1 = iri_of(&first, "text");
    let pass1 = iri_of(&first, "span");
    let claim1 = iri_of(&first, "claim");

    // Publish a second revision and rendition for mismatch tests.
    let second_text = "Second document text for mismatch tests.";
    let second = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                was_revision_of: None,
                version: RecordVersion::V1,
                key: key("rev2"),
                source: iri("urn:bulletin-2"),
                source_key: key("bulletin-2"),
                upstream_version: None,
                content: SourceContent::Retained {
                    bytes: second_text.as_bytes().to_vec(),
                    expected_digest: Some(sha(second_text)),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            renditions: vec![RenditionInput {
                version: RecordVersion::V1,
                key: key("rend2"),
                revision: local("rev2"),
                occurrence_key: key("rend2-occ"),
                conversion: method("urn:method:identity"),
                media_type: "text/plain".into(),
                language: None,
                content: RenditionContent::NormalizedText(second_text.into()),
                expected_digest: None,
            }],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let rev2 = iri_of(&second, "rev2");
    let rend2 = iri_of(&second, "rend2");

    // Propose a reference to the existing passage + a new claim and supporting link.
    // A caller with write grants ONLY on CLAIMS and read grants on SOURCES succeeds!
    let mut o_claims_only = options();
    o_claims_only.write = Some(Arc::new(GraphAccess::graphs(
        Graphs::All,
        Graphs::Only(GraphRule::new([CLAIMS], &[])),
    )));
    let proposed = propose(
        &ds,
        &EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("existingPassRef"),
                occurrence_key: key("passageRef"),
                target: EvidenceRefTarget::Passage {
                    revision: RecordRef::Existing(rev1.clone()),
                    rendition: RecordRef::Existing(rend1.clone()),
                    passage: RecordRef::Existing(pass1.clone()),
                },
            }],
            claims: vec![ClaimInput {
                version: RecordVersion::V1,
                key: key("c2"),
                occurrence_key: key("c2"),
                revision: RecordRef::Existing(rev1.clone()),
                proposition: proposition(),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("urn:publisher"),
                scope: Default::default(),
                confidence: Default::default(),
                validity: None,
            }],
            links: vec![EvidenceLinkInput {
                version: RecordVersion::V1,
                key: key("l2"),
                occurrence_key: key("l2"),
                target: local("c2"),
                evidence: local("existingPassRef"),
                relation: EvidenceRelation::Supports,
                attributed_author: iri("urn:importer"),
            }],
            ..Default::default()
        },
        &o_claims_only,
    )
    .unwrap();
    assert!(proposed.committed);
    // Digest & IRI stability: occurrence_key "passageRef" matches the first batch's
    // local passage reference, so it is marked existing with identical IRI and digest.
    let first_ref = first
        .records
        .iter()
        .find(|r| r.key == "passageRef")
        .unwrap();
    let second_ref = proposed
        .records
        .iter()
        .find(|r| r.key == "existingPassRef")
        .unwrap();
    assert_eq!(second_ref.iri, first_ref.iri);
    assert_eq!(second_ref.digest, first_ref.digest);
    assert!(second_ref.existing);

    // Refusals: mismatched revision, mismatched rendition, wrong kind, missing passage.
    let head = ds.store().head_commit().seq;
    let ref_attempt = |r: NamedNode, rn: NamedNode, p: NamedNode| {
        propose(
            &ds,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("r"),
                    occurrence_key: key("r-occ"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(r),
                        rendition: RecordRef::Existing(rn),
                        passage: RecordRef::Existing(p),
                    },
                }],
                ..Default::default()
            },
            &options(),
        )
    };
    assert_eq!(
        code(ref_attempt(rev2.clone(), rend1.clone(), pass1.clone())),
        "invalid"
    );
    assert_eq!(
        code(ref_attempt(rev1.clone(), rend2.clone(), pass1.clone())),
        "invalid"
    );
    assert_eq!(
        code(ref_attempt(rev1.clone(), rend1.clone(), claim1)),
        "immutable-record-conflict"
    );
    let missing_iri = iri("urn:uuid:00000000-0000-4000-8000-000000000000");
    assert_eq!(
        code(ref_attempt(
            rev1.clone(),
            rend1.clone(),
            missing_iri.clone()
        )),
        "evidence-unavailable"
    );

    // Unauthorized caller gets identical evidence-unavailable as for a missing passage.
    let limited = || {
        Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([CLAIMS], &[])),
            Graphs::All,
        )))
    };
    let hidden_answer = |p: NamedNode| {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("r"),
                    occurrence_key: key("r-unauth"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(rev1.clone()),
                        rendition: RecordRef::Existing(rend1.clone()),
                        passage: RecordRef::Existing(p.clone()),
                    },
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| limited(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string()
                .replace(rev1.as_str(), "REV")
                .replace(rend1.as_str(), "REND")
                .replace(p.as_str(), "PASS"),
        )
    };
    let hidden = hidden_answer(pass1.clone());
    let missing = hidden_answer(missing_iri);
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);
    assert_eq!(ds.store().head_commit().seq, head);

    // Branch isolation: a passage from main resolves on a child branch; a passage
    // created on a branch does not resolve on main.
    ds.create_branch("dev", &Default::default()).unwrap();
    let dev = ds.branch("dev").unwrap();
    let dev_pass = evidence::propose(
        None,
        &ds,
        &dev,
        &EvidenceInput {
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("dev-span"),
                revision: RecordRef::Existing(rev1.clone()),
                rendition: RecordRef::Existing(rend1.clone()),
                occurrence_key: key("dev-span"),
                start: 12,
                end: 14,
                text_digest: sha("is"),
                quote: Some("is".into()),
            }],
            ..Default::default()
        },
        &options(),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let dev_pass_iri = iri_of(&dev_pass, "dev-span");
    assert_eq!(
        code(ref_attempt(
            rev1.clone(),
            rend1.clone(),
            dev_pass_iri.clone()
        )),
        "evidence-unavailable"
    );
    assert!(
        evidence::propose(
            None,
            &ds,
            &dev,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("dev-ref"),
                    occurrence_key: key("dev-ref"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(rev1.clone()),
                        rendition: RecordRef::Existing(rend1.clone()),
                        passage: RecordRef::Existing(dev_pass_iri),
                    },
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap()
        .committed
    );

    // Detached and re-protected source graph invalidates the passage.
    evidence::detach(&ds, SOURCES).unwrap();
    configured(&ds);
    assert_eq!(
        code(ref_attempt(rev1, rend1, pass1)),
        "origin-not-reproduced"
    );
}

#[test]
fn structured_references_verify_against_existing_and_non_json_revisions() {
    let (_dir, ds) = retained();
    let fact = with_fact(&ds);
    configured(&ds);

    let json_doc = r#"{"a/b":{"m~n":[null,1.000,"café"]},"affected":["pkg-1","pkg-2"]}"#;
    let plain_text = "Release 1.2 is not JSON.";
    let binary_bytes: Vec<u8> = vec![0x89, 0x50, 0x4e, 0x47, 0xff, 0x00, 0xfe];
    let dup_json = r#"{"x":1,"\u0078":2}"#;

    let seed = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("json-rev"),
                    source: iri("urn:advisory:json"),
                    source_key: key("adv-json"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: json_doc.as_bytes().to_vec(),
                        expected_digest: Some(sha(json_doc)),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("text-rev"),
                    source: iri("urn:advisory:text"),
                    source_key: key("adv-text"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: plain_text.as_bytes().to_vec(),
                        expected_digest: Some(sha(plain_text)),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("bin-rev"),
                    source: iri("urn:advisory:bin"),
                    source_key: key("adv-bin"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: binary_bytes.clone(),
                        expected_digest: Some(
                            Sha256Digest::of_bytes(&binary_bytes, &Control::none()).unwrap(),
                        ),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("dup-rev"),
                    source: iri("urn:advisory:dup"),
                    source_key: key("adv-dup"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: dup_json.as_bytes().to_vec(),
                        expected_digest: Some(sha(dup_json)),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("meta-rev"),
                    source: iri("urn:advisory:meta"),
                    source_key: key("adv-meta"),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha(json_doc),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
            ],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let json_rev = iri_of(&seed, "json-rev");
    let text_rev = iri_of(&seed, "text-rev");
    let bin_rev = iri_of(&seed, "bin-rev");
    let dup_rev = iri_of(&seed, "dup-rev");
    let meta_rev = iri_of(&seed, "meta-rev");

    // Multiple structured references against the existing JSON revision + a stored fact
    // in one batch, with idempotency key.
    let mut o_idem = options();
    o_idem.idempotency_key = Some("structured-existing".into());
    let valid_batch = EvidenceInput {
        references: vec![
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-num"),
                occurrence_key: key("s-num"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(json_rev.clone()),
                    pointer: "/a~1b/m~0n/1".into(),
                    method: json_method(),
                    value_digest: sha("1.000"),
                },
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-str"),
                occurrence_key: key("s-str"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(json_rev.clone()),
                    pointer: "/a~1b/m~0n/2".into(),
                    method: json_method(),
                    value_digest: sha("\"café\""),
                },
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-root"),
                occurrence_key: key("s-root"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(json_rev.clone()),
                    pointer: "".into(),
                    method: json_method(),
                    value_digest: sha(json_doc),
                },
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("fact-alongside"),
                occurrence_key: key("fact-alongside"),
                target: EvidenceRefTarget::StoredFact(StoredFactRef {
                    dataset: ds.dataset_id(),
                    branch: ds.dataset_id(),
                    commit: fact,
                    graph: GraphIdentity::Named(iri(FACTS)),
                    expected: proposition(),
                    anchor: None,
                }),
            },
        ],
        ..Default::default()
    };
    let proposed = propose(&ds, &valid_batch, &o_idem).unwrap();
    assert!(proposed.committed && !proposed.replayed);
    assert!(proposed.hold.is_some());
    assert_eq!(evidence::holds(&ds).unwrap().len(), 1);

    // Digest & IRI stability: local revision produces the exact same IRI and digest.
    let mut dry = options();
    dry.dry_run = true;
    let local_dry = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                was_revision_of: None,
                version: RecordVersion::V1,
                key: key("json-rev"),
                source: iri("urn:advisory:json"),
                source_key: key("adv-json"),
                upstream_version: None,
                content: SourceContent::Retained {
                    bytes: json_doc.as_bytes().to_vec(),
                    expected_digest: Some(sha(json_doc)),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-num"),
                occurrence_key: key("s-num"),
                target: EvidenceRefTarget::Structured {
                    revision: local("json-rev"),
                    pointer: "/a~1b/m~0n/1".into(),
                    method: json_method(),
                    value_digest: sha("1.000"),
                },
            }],
            ..Default::default()
        },
        &dry,
    )
    .unwrap();
    let existing_s = proposed.records.iter().find(|r| r.key == "s-num").unwrap();
    let local_s = local_dry.records.iter().find(|r| r.key == "s-num").unwrap();
    assert_eq!(local_s.iri, existing_s.iri);
    assert_eq!(local_s.digest, existing_s.digest);
    assert!(local_s.existing);

    // Idempotent replay returns first receipt without committing.
    let head = ds.store().head_commit().seq;
    let replayed = propose(&ds, &valid_batch, &o_idem).unwrap();
    assert!(replayed.replayed && !replayed.committed);
    assert_eq!(replayed.records, proposed.records);
    assert_eq!(ds.store().head_commit().seq, head);

    // Refusals: wrong value_digest (lexical 1 vs 1.000), missing path, duplicate keys,
    // existing plain-text non-JSON, existing binary non-UTF-8, local plain-text non-JSON,
    // existing metadata-only, local metadata-only, unsupported method.
    let structured_attempt = |rev: RecordRef, pointer: &str, m: MethodRef, d: Sha256Digest| {
        propose(
            &ds,
            &EvidenceInput {
                references: vec![
                    EvidenceRefInput {
                        version: RecordVersion::V1,
                        key: key("sr"),
                        occurrence_key: key("sr-occ"),
                        target: EvidenceRefTarget::Structured {
                            revision: rev,
                            pointer: pointer.into(),
                            method: m,
                            value_digest: d,
                        },
                    },
                    // Include a stored fact to verify failed selections acquire no new hold.
                    EvidenceRefInput {
                        version: RecordVersion::V1,
                        key: key("f"),
                        occurrence_key: key("f-no-hold"),
                        target: EvidenceRefTarget::StoredFact(StoredFactRef {
                            dataset: ds.dataset_id(),
                            branch: ds.dataset_id(),
                            commit: fact,
                            graph: GraphIdentity::Named(iri(FACTS)),
                            expected: proposition(),
                            anchor: None,
                        }),
                    },
                ],
                ..Default::default()
            },
            &options(),
        )
    };
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(json_rev.clone()),
            "/a~1b/m~0n/1",
            json_method(),
            sha("1"),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(json_rev.clone()),
            "/missing/path",
            json_method(),
            sha("1.000"),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(dup_rev),
            "/x",
            json_method(),
            sha("1"),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(text_rev),
            "",
            json_method(),
            sha(plain_text),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(bin_rev),
            "",
            json_method(),
            Sha256Digest::of_bytes(&binary_bytes, &Control::none()).unwrap(),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(meta_rev),
            "/a~1b/m~0n/1",
            json_method(),
            sha("1.000"),
        )),
        "evidence-unavailable"
    );
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(json_rev.clone()),
            "/a~1b/m~0n/1",
            method("urn:example:unsupported-norm"),
            sha("1.000"),
        )),
        "incomplete-evidence"
    );
    // Local non-JSON revision also returns span-mismatch; local metadata-only returns
    // evidence-unavailable.
    assert_eq!(
        code(propose(
            &ds,
            &EvidenceInput {
                revisions: vec![SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("local-text"),
                    source: iri("urn:local:text"),
                    source_key: key("local-text"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: plain_text.as_bytes().to_vec(),
                        expected_digest: None,
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                }],
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("local-sr"),
                    occurrence_key: key("local-sr"),
                    target: EvidenceRefTarget::Structured {
                        revision: local("local-text"),
                        pointer: "".into(),
                        method: json_method(),
                        value_digest: sha(plain_text),
                    },
                }],
                ..Default::default()
            },
            &options(),
        )),
        "span-mismatch"
    );
    assert_eq!(
        code(propose(
            &ds,
            &EvidenceInput {
                revisions: vec![SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("local-meta"),
                    source: iri("urn:local:meta"),
                    source_key: key("local-meta"),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha(json_doc),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                }],
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("local-meta-sr"),
                    occurrence_key: key("local-meta-sr"),
                    target: EvidenceRefTarget::Structured {
                        revision: local("local-meta"),
                        pointer: "/a~1b/m~0n/1".into(),
                        method: json_method(),
                        value_digest: sha("1.000"),
                    },
                }],
                ..Default::default()
            },
            &options(),
        )),
        "evidence-unavailable"
    );
    // No refusal acquired a hold or advanced the head.
    assert_eq!(evidence::holds(&ds).unwrap().len(), 1);
    assert_eq!(ds.store().head_commit().seq, head);

    // Unauthorized caller gets identical evidence-unavailable as for a missing revision.
    let limited = || {
        Some(Arc::new(GraphAccess::graphs(
            Graphs::Only(GraphRule::new([CLAIMS], &[])),
            Graphs::All,
        )))
    };
    let missing_iri = iri("urn:uuid:00000000-0000-4000-8000-000000000000");
    let hidden_answer = |r: NamedNode| {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("sr"),
                    occurrence_key: key("sr-unauth"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(r.clone()),
                        pointer: "/a~1b/m~0n/1".into(),
                        method: json_method(),
                        value_digest: sha("1.000"),
                    },
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| limited(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(r.as_str(), "REV"),
        )
    };
    let hidden = hidden_answer(json_rev.clone());
    let missing = hidden_answer(missing_iri);
    assert_eq!(hidden.0, "evidence-unavailable");
    assert_eq!(hidden, missing);

    // Large (~4 MiB) existing retained JSON revision verifies cleanly within the 64 MiB
    // allowance, including after compaction into the base vocabulary.
    let big_val = "a".repeat(4 * 1024 * 1024 - 32);
    let big_json = format!(r#"{{"tail":"ok","blob":"{big_val}"}}"#);
    let big_seed = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                was_revision_of: None,
                version: RecordVersion::V1,
                key: key("big-rev"),
                source: iri("urn:advisory:big"),
                source_key: key("adv-big"),
                upstream_version: None,
                content: SourceContent::Retained {
                    bytes: big_json.as_bytes().to_vec(),
                    expected_digest: None,
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let big_rev = iri_of(&big_seed, "big-rev");
    ds.store().compact().unwrap();
    let big_ref = propose(
        &ds,
        &EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("big-tail"),
                occurrence_key: key("big-tail"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(big_rev),
                    pointer: "/tail".into(),
                    method: json_method(),
                    value_digest: sha("\"ok\""),
                },
            }],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    assert!(big_ref.committed);

    // Detached and re-protected source graph refuses with origin-not-reproduced.
    evidence::detach(&ds, SOURCES).unwrap();
    configured(&ds);
    assert_eq!(
        code(structured_attempt(
            RecordRef::Existing(json_rev),
            "/a~1b/m~0n/1",
            json_method(),
            sha("1.000"),
        )),
        "origin-not-reproduced"
    );
}

#[test]
fn propose_masked_and_metadata_only_selections_match_missing_and_deduplicate_passages() {
    let (_dir, ds) = retained();
    configured(&ds);

    let json_doc = r#"{"score":1.000}"#;
    let seed = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev1"),
                    source: iri("urn:doc:1"),
                    source_key: key("doc-1"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: json_doc.as_bytes().to_vec(),
                        expected_digest: None,
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev2"),
                    source: iri("urn:doc:2"),
                    source_key: key("doc-2"),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha(json_doc),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
            ],
            renditions: vec![
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend1"),
                    revision: local("rev1"),
                    occurrence_key: key("rend1-occ"),
                    conversion: method("urn:method:identity"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::NormalizedText(TEXT.into()),
                    expected_digest: None,
                },
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend-meta"),
                    revision: local("rev1"),
                    occurrence_key: key("rend-meta-occ"),
                    conversion: method("urn:method:meta"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::MetadataOnly(sha(TEXT)),
                    expected_digest: None,
                },
            ],
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("pass1"),
                revision: local("rev1"),
                rendition: local("rend1"),
                occurrence_key: key("pass1-occ"),
                start: 0,
                end: 11,
                text_digest: sha("Release 1.2"),
                quote: Some("Release 1.2".into()),
            }],
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    let rev1 = iri_of(&seed, "rev1");
    let rev2 = iri_of(&seed, "rev2");
    let rend1 = iri_of(&seed, "rend1");
    let rend_meta = iri_of(&seed, "rend-meta");
    let pass1 = iri_of(&seed, "pass1");
    let missing_iri = iri("urn:uuid:00000000-0000-4000-8000-000000000000");

    let hide_pred = |pred: &str| {
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(vec![pred.to_string()]),
            classes: None,
            subclasses: false,
            graphs: None,
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        let rules = TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Default::default(),
            limits: Default::default(),
        };
        Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        )))
    };

    // 1. Proposed passage against existing rendition: absent vs metadata-only vs masked
    //    ev:normalizedText / prov:wasDerivedFrom — including with a MISMATCHED revision.
    let prop_passage_err =
        |target_rend: &NamedNode, supplied_rev: &NamedNode, access: Option<Arc<GraphAccess>>| {
            let e = evidence::propose(
                None,
                &ds,
                &ds,
                &EvidenceInput {
                    passages: vec![PassageInput {
                        version: RecordVersion::V1,
                        key: key("p-check"),
                        revision: RecordRef::Existing(supplied_rev.clone()),
                        rendition: RecordRef::Existing(target_rend.clone()),
                        occurrence_key: key("p-check"),
                        start: 0,
                        end: 11,
                        text_digest: sha("Release 1.2"),
                        quote: Some("Release 1.2".into()),
                    }],
                    ..Default::default()
                },
                &options(),
                &mut |_, _| access.clone(),
                &Control::none(),
            )
            .unwrap_err();
            (
                e.code().to_string(),
                e.to_string().replace(target_rend.as_str(), "TARGET"),
            )
        };
    let absent_rend = prop_passage_err(&missing_iri, &rev2, all());
    assert_eq!(absent_rend.0, "evidence-unavailable");
    assert_eq!(absent_rend.1, "evidence reference `TARGET` is unavailable");
    for rev in [&rev1, &rev2] {
        assert_eq!(prop_passage_err(&rend_meta, rev, all()), absent_rend);
        assert_eq!(
            prop_passage_err(
                &rend1,
                rev,
                hide_pred("urn:x-sparkles:evidence:normalizedText")
            ),
            absent_rend
        );
        assert_eq!(
            prop_passage_err(
                &rend1,
                rev,
                hide_pred("http://www.w3.org/ns/prov#wasDerivedFrom")
            ),
            absent_rend
        );
    }

    // 2. Existing passage reference: absent vs masked links/selectors — including with
    //    MISMATCHED revision or rendition.
    let prop_pass_ref_err = |target_pass: &NamedNode,
                             supplied_rev: &NamedNode,
                             supplied_rend: &NamedNode,
                             access: Option<Arc<GraphAccess>>| {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("r-check"),
                    occurrence_key: key("r-check"),
                    target: EvidenceRefTarget::Passage {
                        revision: RecordRef::Existing(supplied_rev.clone()),
                        rendition: RecordRef::Existing(supplied_rend.clone()),
                        passage: RecordRef::Existing(target_pass.clone()),
                    },
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| access.clone(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target_pass.as_str(), "TARGET"),
        )
    };
    let absent_pass = prop_pass_ref_err(&missing_iri, &rev2, &rend_meta, all());
    assert_eq!(absent_pass.0, "evidence-unavailable");
    assert_eq!(absent_pass.1, "evidence reference `TARGET` is unavailable");
    for (rev, rend) in [(&rev1, &rend1), (&rev2, &rend1), (&rev1, &rend_meta)] {
        for pred in [
            "http://www.w3.org/ns/oa#hasSource",
            "http://www.w3.org/ns/oa#hasSelector",
            "http://www.w3.org/ns/oa#start",
            "http://www.w3.org/ns/oa#end",
            "urn:x-sparkles:evidence:textDigest",
            "urn:x-sparkles:evidence:sourceRevision",
            "urn:x-sparkles:evidence:normalizedText",
            "http://www.w3.org/ns/prov#wasDerivedFrom",
        ] {
            assert_eq!(
                prop_pass_ref_err(&pass1, rev, rend, hide_pred(pred)),
                absent_pass,
                "masking {pred} should match absent passage"
            );
        }
    }

    // 3. Structured reference: absent vs metadata-only vs masked retainedBytes/contentDigest.
    let prop_struct_err = |target_rev: &NamedNode, access: Option<Arc<GraphAccess>>| {
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            &EvidenceInput {
                references: vec![EvidenceRefInput {
                    version: RecordVersion::V1,
                    key: key("s-check"),
                    occurrence_key: key("s-check"),
                    target: EvidenceRefTarget::Structured {
                        revision: RecordRef::Existing(target_rev.clone()),
                        pointer: "/score".into(),
                        method: MethodRef {
                            method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                            version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                        },
                        value_digest: sha("1.000"),
                    },
                }],
                ..Default::default()
            },
            &options(),
            &mut |_, _| access.clone(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target_rev.as_str(), "TARGET"),
        )
    };
    let absent_rev = prop_struct_err(&missing_iri, all());
    assert_eq!(absent_rev.0, "evidence-unavailable");
    assert_eq!(absent_rev.1, "evidence reference `TARGET` is unavailable");
    assert_eq!(prop_struct_err(&rev2, all()), absent_rev);
    assert_eq!(
        prop_struct_err(&rev1, hide_pred("urn:x-sparkles:evidence:retainedBytes")),
        absent_rev
    );
    assert_eq!(
        prop_struct_err(&rev1, hide_pred("urn:x-sparkles:evidence:contentDigest")),
        absent_rev
    );

    // 4. Batch with many distinct occurrence keys referencing the same existing passage
    //    deduplicates passage scans and commits cleanly.
    let repeated_refs: Vec<EvidenceRefInput> = (0..25)
        .map(|i| EvidenceRefInput {
            version: RecordVersion::V1,
            key: key(&format!("rep-{i}")),
            occurrence_key: key(&format!("rep-occ-{i}")),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Existing(rev1.clone()),
                rendition: RecordRef::Existing(rend1.clone()),
                passage: RecordRef::Existing(pass1.clone()),
            },
        })
        .collect();
    let batch = propose(
        &ds,
        &EvidenceInput {
            references: repeated_refs,
            ..Default::default()
        },
        &options(),
    )
    .unwrap();
    assert!(batch.committed);
    assert_eq!(batch.records.len(), 25);
}

#[test]
fn propose_mixed_batches_preserve_masked_vs_missing_parity_and_input_order() {
    let (_dir, ds) = retained();
    const SOURCES_TWO: &str = "urn:ev:sources-two";
    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES.into(), SOURCES_TWO.into()],
        proposals: vec![CLAIMS.into()],
        ..Default::default()
    };
    evidence::configure(&ds, &settings, &Control::none()).unwrap();
    let opts_in = |sg: &str| {
        let mut o = options();
        o.source_graph = Some(iri(sg));
        o
    };

    let json_doc = r#"{"score":1.000}"#;
    let seed1 = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev1"),
                    source: iri("urn:doc:mix-1"),
                    source_key: key("doc-mix-1"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: json_doc.as_bytes().to_vec(),
                        expected_digest: None,
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev-meta"),
                    source: iri("urn:doc:mix-meta"),
                    source_key: key("doc-mix-meta"),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha(json_doc),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
            ],
            renditions: vec![RenditionInput {
                version: RecordVersion::V1,
                key: key("rend1"),
                revision: local("rev1"),
                occurrence_key: key("rend1-occ"),
                conversion: method("urn:method:identity"),
                media_type: "text/plain".into(),
                language: None,
                content: RenditionContent::NormalizedText(TEXT.into()),
                expected_digest: None,
            }],
            ..Default::default()
        },
        &opts_in(SOURCES),
    )
    .unwrap();
    let rev1 = iri_of(&seed1, "rev1");
    let rev_meta = iri_of(&seed1, "rev-meta");
    let rend1 = iri_of(&seed1, "rend1");

    // Seed a second rendition, metadata-only rendition, and passage in SOURCES_TWO so
    // graph-scoped triple masks can mask target B while leaving rend1 readable.
    let seed2 = propose(
        &ds,
        &EvidenceInput {
            renditions: vec![
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend2"),
                    revision: RecordRef::Existing(rev1.clone()),
                    occurrence_key: key("rend2-occ"),
                    conversion: method("urn:method:identity-2"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::NormalizedText(TEXT.into()),
                    expected_digest: None,
                },
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend2-meta"),
                    revision: RecordRef::Existing(rev1.clone()),
                    occurrence_key: key("rend2-meta-occ"),
                    conversion: method("urn:method:meta-2"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::MetadataOnly(sha(TEXT)),
                    expected_digest: None,
                },
            ],
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("pass2"),
                revision: RecordRef::Existing(rev1.clone()),
                rendition: local("rend2"),
                occurrence_key: key("pass2-occ"),
                start: 0,
                end: 11,
                text_digest: sha("Release 1.2"),
                quote: Some("Release 1.2".into()),
            }],
            ..Default::default()
        },
        &opts_in(SOURCES_TWO),
    )
    .unwrap();
    let rend2 = iri_of(&seed2, "rend2");
    let rend2_meta = iri_of(&seed2, "rend2-meta");
    let pass2 = iri_of(&seed2, "pass2");
    let missing_b = iri("urn:uuid:00000000-0000-4000-8000-0000000000b1");
    let missing_c = iri("urn:uuid:00000000-0000-4000-8000-0000000000c2");

    let hide_pred = |pred: &str, graph_scope: Option<&str>| {
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(vec![pred.to_string()]),
            classes: None,
            subclasses: false,
            graphs: graph_scope.map(|g| Graphs::Only(GraphRule::new([g], &[]))),
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        let rules = TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Default::default(),
            limits: Default::default(),
        };
        Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        )))
    };

    let run_err = |input: &EvidenceInput, target: &NamedNode, access: Option<Arc<GraphAccess>>| {
        let mut o = options();
        if !input.revisions.is_empty() || !input.renditions.is_empty() || !input.passages.is_empty()
        {
            o.source_graph = Some(iri(SOURCES));
        }
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            input,
            &o,
            &mut |_, _| access.clone(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target.as_str(), "TARGET"),
        )
    };

    // 1. Mixed batch: failing proposed passage on readable rend1 (mismatched readable
    //    revision rev_meta) + supported structured reference to revision B.
    //    Absent B, metadata-only B, masked ev:retainedBytes and masked ev:contentDigest
    //    must all answer identically with `evidence-unavailable` naming B (never 400).
    let mixed_passage_and_struct = |target_rev: &NamedNode| EvidenceInput {
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("p-bad-rev"),
            revision: RecordRef::Existing(rev_meta.clone()),
            rendition: RecordRef::Existing(rend1.clone()),
            occurrence_key: key("p-bad-rev"),
            start: 0,
            end: 11,
            text_digest: sha("Release 1.2"),
            quote: Some("Release 1.2".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("s-target"),
            occurrence_key: key("s-target"),
            target: EvidenceRefTarget::Structured {
                revision: RecordRef::Existing(target_rev.clone()),
                pointer: "/score".into(),
                method: MethodRef {
                    method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                    version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                },
                value_digest: sha("1.000"),
            },
        }],
        ..Default::default()
    };
    let absent_struct = run_err(&mixed_passage_and_struct(&missing_b), &missing_b, all());
    assert_eq!(absent_struct.0, "evidence-unavailable");
    assert_eq!(
        absent_struct.1,
        "evidence reference `TARGET` is unavailable"
    );
    assert_eq!(
        run_err(&mixed_passage_and_struct(&rev_meta), &rev_meta, all()),
        absent_struct
    );
    assert_eq!(
        run_err(
            &mixed_passage_and_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:retainedBytes", None),
        ),
        absent_struct
    );
    assert_eq!(
        run_err(
            &mixed_passage_and_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:contentDigest", None),
        ),
        absent_struct
    );

    // 2. Mixed batch: structured reference to B followed by a reference to missing C.
    //    Masked and metadata-only B must fail on B (never advance to missing C).
    let struct_before_missing_c = |target_rev: &NamedNode| EvidenceInput {
        references: vec![
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-first"),
                occurrence_key: key("s-first"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(target_rev.clone()),
                    pointer: "/score".into(),
                    method: MethodRef {
                        method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                        version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: sha("1.000"),
                },
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("s-second"),
                occurrence_key: key("s-second"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Existing(missing_c.clone()),
                    pointer: "/score".into(),
                    method: MethodRef {
                        method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                        version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest: sha("1.000"),
                },
            },
        ],
        ..Default::default()
    };
    let absent_before_c = run_err(&struct_before_missing_c(&missing_b), &missing_b, all());
    assert_eq!(absent_before_c, absent_struct);
    assert_eq!(
        run_err(&struct_before_missing_c(&rev_meta), &rev_meta, all()),
        absent_before_c
    );
    assert_eq!(
        run_err(
            &struct_before_missing_c(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:retainedBytes", None),
        ),
        absent_before_c
    );

    // 3. Mixed batch: failing proposed passage on readable rend1 + proposed passage on
    //    second rendition R2 (absent vs metadata-only vs graph-scoped masked predicates).
    let mixed_two_renditions = |target_rend: &NamedNode| EvidenceInput {
        passages: vec![
            PassageInput {
                version: RecordVersion::V1,
                key: key("p-bad-1"),
                revision: RecordRef::Existing(rev_meta.clone()),
                rendition: RecordRef::Existing(rend1.clone()),
                occurrence_key: key("p-bad-1"),
                start: 0,
                end: 11,
                text_digest: sha("Release 1.2"),
                quote: Some("Release 1.2".into()),
            },
            PassageInput {
                version: RecordVersion::V1,
                key: key("p-target-2"),
                revision: RecordRef::Existing(rev1.clone()),
                rendition: RecordRef::Existing(target_rend.clone()),
                occurrence_key: key("p-target-2"),
                start: 0,
                end: 11,
                text_digest: sha("Release 1.2"),
                quote: Some("Release 1.2".into()),
            },
        ],
        ..Default::default()
    };
    let absent_rend2 = run_err(&mixed_two_renditions(&missing_b), &missing_b, all());
    assert_eq!(absent_rend2, absent_struct);
    assert_eq!(
        run_err(&mixed_two_renditions(&rend2_meta), &rend2_meta, all()),
        absent_rend2
    );
    assert_eq!(
        run_err(
            &mixed_two_renditions(&rend2),
            &rend2,
            hide_pred("urn:x-sparkles:evidence:normalizedText", Some(SOURCES_TWO)),
        ),
        absent_rend2
    );
    assert_eq!(
        run_err(
            &mixed_two_renditions(&rend2),
            &rend2,
            hide_pred(
                "http://www.w3.org/ns/prov#wasDerivedFrom",
                Some(SOURCES_TWO)
            ),
        ),
        absent_rend2
    );

    // 4. Mixed batch: failing proposed passage on readable rend1 + passage reference to
    //    pass2 (absent vs masked ev:sourceRevision vs masked rendition prov:wasDerivedFrom).
    let mixed_passage_and_ref = |target_pass: &NamedNode| EvidenceInput {
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("p-bad-3"),
            revision: RecordRef::Existing(rev_meta.clone()),
            rendition: RecordRef::Existing(rend1.clone()),
            occurrence_key: key("p-bad-3"),
            start: 0,
            end: 11,
            text_digest: sha("Release 1.2"),
            quote: Some("Release 1.2".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("r-target-2"),
            occurrence_key: key("r-target-2"),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Existing(rev1.clone()),
                rendition: RecordRef::Existing(rend2.clone()),
                passage: RecordRef::Existing(target_pass.clone()),
            },
        }],
        ..Default::default()
    };
    let absent_pass2 = run_err(&mixed_passage_and_ref(&missing_b), &missing_b, all());
    assert_eq!(absent_pass2, absent_struct);
    assert_eq!(
        run_err(
            &mixed_passage_and_ref(&pass2),
            &pass2,
            hide_pred("urn:x-sparkles:evidence:sourceRevision", Some(SOURCES_TWO)),
        ),
        absent_pass2
    );
    assert_eq!(
        run_err(
            &mixed_passage_and_ref(&pass2),
            &pass2,
            hide_pred(
                "http://www.w3.org/ns/prov#wasDerivedFrom",
                Some(SOURCES_TWO)
            ),
        ),
        absent_pass2
    );

    // 5. Input-only checks run before store lookups so unsupported normalization methods
    //    and mismatched local passages answer identically regardless of target store state.
    let unsup_for = |target_rev: &NamedNode| EvidenceInput {
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("s-unsup"),
            occurrence_key: key("s-unsup"),
            target: EvidenceRefTarget::Structured {
                revision: RecordRef::Existing(target_rev.clone()),
                pointer: "/score".into(),
                method: method("urn:example:unsupported-norm"),
                value_digest: sha("1.000"),
            },
        }],
        ..Default::default()
    };
    let unsup_present = run_err(&unsup_for(&rev1), &rev1, all());
    assert_eq!(unsup_present.0, "incomplete-evidence");
    assert_eq!(
        run_err(&unsup_for(&missing_b), &missing_b, all()),
        unsup_present
    );
    assert_eq!(
        run_err(&unsup_for(&rev_meta), &rev_meta, all()),
        unsup_present
    );
    assert_eq!(
        run_err(
            &unsup_for(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:retainedBytes", None),
        ),
        unsup_present
    );

    let local_mismatch_with_struct = |target_rev: &NamedNode| EvidenceInput {
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("local-rend"),
            revision: RecordRef::Existing(rev1.clone()),
            occurrence_key: key("local-rend-occ"),
            conversion: method("urn:method:local"),
            media_type: "text/plain".into(),
            language: None,
            content: RenditionContent::NormalizedText(TEXT.into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("local-p-bad"),
            revision: RecordRef::Existing(rev_meta.clone()),
            rendition: local("local-rend"),
            occurrence_key: key("local-p-bad"),
            start: 0,
            end: 11,
            text_digest: sha("Release 1.2"),
            quote: Some("Release 1.2".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("s-with-local-bad"),
            occurrence_key: key("s-with-local-bad"),
            target: EvidenceRefTarget::Structured {
                revision: RecordRef::Existing(target_rev.clone()),
                pointer: "/score".into(),
                method: MethodRef {
                    method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                    version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                },
                value_digest: sha("1.000"),
            },
        }],
        ..Default::default()
    };
    let local_bad_absent = run_err(&local_mismatch_with_struct(&missing_b), &missing_b, all());
    assert_eq!(local_bad_absent.0, "invalid");
    assert_eq!(
        run_err(
            &local_mismatch_with_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:retainedBytes", None),
        ),
        local_bad_absent
    );
}

#[test]
fn propose_same_iri_selection_precedes_incompatible_kind_constraint() {
    let (_dir, ds) = retained();
    const SOURCES_TWO: &str = "urn:ev:sources-two";
    let settings = Settings {
        enabled: true,
        source_records: vec![SOURCES.into(), SOURCES_TWO.into()],
        proposals: vec![CLAIMS.into()],
        ..Default::default()
    };
    evidence::configure(&ds, &settings, &Control::none()).unwrap();
    let opts_in = |sg: &str| {
        let mut o = options();
        o.source_graph = Some(iri(sg));
        o
    };

    let json_doc = r#"{"score":1.000}"#;
    let seed_other = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![SourceRevisionInput {
                was_revision_of: None,
                version: RecordVersion::V1,
                key: key("rev-other"),
                source: iri("urn:doc:same-other"),
                source_key: key("doc-same-other"),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha(json_doc),
                },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
            }],
            ..Default::default()
        },
        &opts_in(SOURCES),
    )
    .unwrap();
    let rev_other = iri_of(&seed_other, "rev-other");

    let seed1 = propose(
        &ds,
        &EvidenceInput {
            revisions: vec![
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev1"),
                    source: iri("urn:doc:same-1"),
                    source_key: key("doc-same-1"),
                    upstream_version: None,
                    content: SourceContent::Retained {
                        bytes: json_doc.as_bytes().to_vec(),
                        expected_digest: None,
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
                SourceRevisionInput {
                    was_revision_of: None,
                    version: RecordVersion::V1,
                    key: key("rev-meta"),
                    source: iri("urn:doc:same-meta"),
                    source_key: key("doc-same-meta"),
                    upstream_version: None,
                    content: SourceContent::MetadataOnly {
                        digest: sha(json_doc),
                    },
                    metadata: Default::default(),
                    times: Default::default(),
                    lineage: Default::default(),
                },
            ],
            ..Default::default()
        },
        &opts_in(SOURCES_TWO),
    )
    .unwrap();
    let rev1 = iri_of(&seed1, "rev1");
    let rev_meta = iri_of(&seed1, "rev-meta");

    let seed2 = propose(
        &ds,
        &EvidenceInput {
            renditions: vec![
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend2"),
                    revision: RecordRef::Existing(rev1.clone()),
                    occurrence_key: key("rend2-occ"),
                    conversion: method("urn:method:same-2"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::NormalizedText(TEXT.into()),
                    expected_digest: None,
                },
                RenditionInput {
                    version: RecordVersion::V1,
                    key: key("rend2-meta"),
                    revision: RecordRef::Existing(rev1.clone()),
                    occurrence_key: key("rend2-meta-occ"),
                    conversion: method("urn:method:same-meta-2"),
                    media_type: "text/plain".into(),
                    language: None,
                    content: RenditionContent::MetadataOnly(sha(TEXT)),
                    expected_digest: None,
                },
            ],
            passages: vec![PassageInput {
                version: RecordVersion::V1,
                key: key("pass2"),
                revision: RecordRef::Existing(rev1.clone()),
                rendition: local("rend2"),
                occurrence_key: key("pass2-occ"),
                start: 0,
                end: 11,
                text_digest: sha("Release 1.2"),
                quote: Some("Release 1.2".into()),
            }],
            ..Default::default()
        },
        &opts_in(SOURCES_TWO),
    )
    .unwrap();
    let rend2 = iri_of(&seed2, "rend2");
    let rend2_meta = iri_of(&seed2, "rend2-meta");
    let pass2 = iri_of(&seed2, "pass2");
    let missing_b = iri("urn:uuid:00000000-0000-4000-8000-0000000000d9");

    let deny_sources_two = Some(Arc::new(GraphAccess::with_triples(
        Graphs::Only(GraphRule::new([SOURCES, CLAIMS], &[])),
        Graphs::All,
        TripleRules {
            rules: Vec::new(),
            caller: Default::default(),
            limits: Default::default(),
        },
    )));

    let hide_preds = |preds: &[&str]| {
        let protection = Protection {
            name: "hidden".into(),
            predicates: Some(preds.iter().map(|p| (*p).to_string()).collect()),
            classes: None,
            subclasses: false,
            graphs: Some(Graphs::Only(GraphRule::new([SOURCES_TWO], &[]))),
            pattern: None,
            prefixes: Default::default(),
            hide_inferences: false,
        };
        let rules = TripleRules {
            rules: vec![Rule {
                protection: Arc::new(protection),
                read: Graphs::none(),
                write: Graphs::none(),
            }],
            caller: Default::default(),
            limits: Default::default(),
        };
        Some(Arc::new(GraphAccess::with_triples(
            Graphs::All,
            Graphs::All,
            rules,
        )))
    };
    let hide_pred = |pred: &str| hide_preds(&[pred]);

    let run_err = |input: &EvidenceInput, target: &NamedNode, access: Option<Arc<GraphAccess>>| {
        let mut o = options();
        if !input.revisions.is_empty() || !input.renditions.is_empty() || !input.passages.is_empty()
        {
            o.source_graph = Some(iri(SOURCES));
        }
        let e = evidence::propose(
            None,
            &ds,
            &ds,
            input,
            &o,
            &mut |_, _| access.clone(),
            &Control::none(),
        )
        .unwrap_err();
        (
            e.code().to_string(),
            e.to_string().replace(target.as_str(), "TARGET"),
        )
    };

    // 1. Same IRI named as both `revision` (SourceRevision) and `rendition` (Rendition)
    //    of a nonempty proposed passage: absent, graph-denied, rdf:type-masked,
    //    metadata-only, ev:normalizedText-masked, prov:wasDerivedFrom-masked, and
    //    receipt-masked targets all answer identically with `evidence-unavailable`
    //    naming the target; a fully readable rendition preserves
    //    `immutable-record-conflict`.
    let same_iri_passage = |target: &NamedNode| EvidenceInput {
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("p-same"),
            revision: RecordRef::Existing(target.clone()),
            rendition: RecordRef::Existing(target.clone()),
            occurrence_key: key("p-same"),
            start: 0,
            end: 11,
            text_digest: sha("Release 1.2"),
            quote: Some("Release 1.2".into()),
        }],
        ..Default::default()
    };
    let absent = run_err(&same_iri_passage(&missing_b), &missing_b, all());
    assert_eq!(absent.0, "evidence-unavailable");
    assert_eq!(absent.1, "evidence reference `TARGET` is unavailable");
    assert_eq!(
        run_err(&same_iri_passage(&rend2), &rend2, deny_sources_two.clone()),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
        ),
        absent
    );
    assert_eq!(
        run_err(&same_iri_passage(&rend2_meta), &rend2_meta, all()),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("urn:x-sparkles:evidence:normalizedText"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("http://www.w3.org/ns/prov#wasDerivedFrom"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("urn:x-sparkles:evidence:availabilityReceipt"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_preds(&[
                "urn:x-sparkles:evidence:availabilityReceipt",
                "urn:x-sparkles:evidence:normalizedText",
            ]),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("urn:x-sparkles:evidence:graphProtection"),
        ),
        absent
    );
    let readable_rend = run_err(&same_iri_passage(&rend2), &rend2, all());
    assert_eq!(readable_rend.0, "immutable-record-conflict");
    assert!(readable_rend.1.contains("another kind"));

    // 2. Same IRI named as an earlier `rendition` (Rendition) and a later structured
    //    reference's `revision` (SourceRevision): unavailable parity for absent,
    //    graph-denied, metadata-only, masked ev:retainedBytes, masked ev:contentDigest,
    //    and masked receipt predicates; `immutable-record-conflict` when fully readable.
    let same_iri_struct = |target: &NamedNode| EvidenceInput {
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("p-wrong-rend"),
            revision: RecordRef::Existing(rev_other.clone()),
            rendition: RecordRef::Existing(target.clone()),
            occurrence_key: key("p-wrong-rend"),
            start: 0,
            end: 11,
            text_digest: sha("Release 1.2"),
            quote: Some("Release 1.2".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("s-same"),
            occurrence_key: key("s-same"),
            target: EvidenceRefTarget::Structured {
                revision: RecordRef::Existing(target.clone()),
                pointer: "/score".into(),
                method: MethodRef {
                    method: iri(sparkles::evidence::JSON_LEXICAL_UTF8_V1),
                    version: key(sparkles::evidence::JSON_LEXICAL_UTF8_VERSION),
                },
                value_digest: sha("1.000"),
            },
        }],
        ..Default::default()
    };
    assert_eq!(
        run_err(&same_iri_struct(&missing_b), &missing_b, all()),
        absent
    );
    assert_eq!(
        run_err(&same_iri_struct(&rev1), &rev1, deny_sources_two.clone()),
        absent
    );
    assert_eq!(
        run_err(&same_iri_struct(&rev_meta), &rev_meta, all()),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:retainedBytes"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:contentDigest"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:availabilityReceipt"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_struct(&rev1),
            &rev1,
            hide_preds(&[
                "urn:x-sparkles:evidence:availabilityReceipt",
                "urn:x-sparkles:evidence:retainedBytes",
            ]),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_struct(&rev1),
            &rev1,
            hide_pred("urn:x-sparkles:evidence:graphProtection"),
        ),
        absent
    );
    let readable_rev = run_err(&same_iri_struct(&rev1), &rev1, all());
    assert_eq!(readable_rev.0, "immutable-record-conflict");
    assert!(readable_rev.1.contains("another kind"));

    // 3. Same IRI named in all three roles (`revision`, `rendition`, `passage`) of an
    //    existing-passage reference: absent, graph-denied, masked ev:sourceRevision,
    //    masked rendition ev:normalizedText, masked rendition prov:wasDerivedFrom, and
    //    masked receipt predicates answer identically with `evidence-unavailable`; a
    //    fully readable verified passage preserves `immutable-record-conflict`.
    let same_iri_pass_ref = |target: &NamedNode| EvidenceInput {
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("r-same"),
            occurrence_key: key("r-same"),
            target: EvidenceRefTarget::Passage {
                revision: RecordRef::Existing(target.clone()),
                rendition: RecordRef::Existing(target.clone()),
                passage: RecordRef::Existing(target.clone()),
            },
        }],
        ..Default::default()
    };
    assert_eq!(
        run_err(&same_iri_pass_ref(&missing_b), &missing_b, all()),
        absent
    );
    assert_eq!(
        run_err(&same_iri_pass_ref(&pass2), &pass2, deny_sources_two),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_pred("urn:x-sparkles:evidence:sourceRevision"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_pred("urn:x-sparkles:evidence:normalizedText"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_pred("http://www.w3.org/ns/prov#wasDerivedFrom"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_pred("urn:x-sparkles:evidence:availabilityReceipt"),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_preds(&[
                "urn:x-sparkles:evidence:availabilityReceipt",
                "urn:x-sparkles:evidence:sourceRevision",
            ]),
        ),
        absent
    );
    assert_eq!(
        run_err(
            &same_iri_pass_ref(&pass2),
            &pass2,
            hide_pred("urn:x-sparkles:evidence:graphProtection"),
        ),
        absent
    );
    let readable_pass = run_err(&same_iri_pass_ref(&pass2), &pass2, all());
    assert_eq!(readable_pass.0, "immutable-record-conflict");
    assert!(readable_pass.1.contains("another kind"));

    // 4. Detaching and re-protecting `SOURCES_TWO` still produces
    //    `origin-not-reproduced` for an unmasked caller, while masking
    //    `ev:availabilityReceipt` keeps answering `evidence-unavailable`.
    evidence::detach(&ds, SOURCES_TWO).unwrap();
    evidence::configure(&ds, &settings, &Control::none()).unwrap();
    assert_eq!(
        run_err(&same_iri_passage(&rend2), &rend2, all()).0,
        "origin-not-reproduced"
    );
    assert_eq!(
        run_err(&same_iri_struct(&rev1), &rev1, all()).0,
        "origin-not-reproduced"
    );
    assert_eq!(
        run_err(&same_iri_pass_ref(&pass2), &pass2, all()).0,
        "origin-not-reproduced"
    );
    assert_eq!(
        run_err(
            &same_iri_passage(&rend2),
            &rend2,
            hide_pred("urn:x-sparkles:evidence:availabilityReceipt"),
        ),
        absent
    );
}

#[test]
fn source_revision_was_revision_of_authenticates_and_refuses_invalid_predecessors() {
    let ds = Dataset::memory();
    configured(&ds);

    // First revision without predecessor.
    let first_input = small("c1", proposition());
    let first = propose(&ds, &first_input, &options()).unwrap();
    let rev1 = iri_of(&first, "rev");
    let claim1 = iri_of(&first, "claim");

    // Second revision links to the existing authenticated predecessor.
    let mut second_input = small("c2", proposition());
    second_input.revisions[0].content = SourceContent::MetadataOnly {
        digest: sha("Release 1.2 updated text."),
    };
    second_input.revisions[0].was_revision_of = Some(RecordRef::Existing(rev1.clone()));
    let second = propose(&ds, &second_input, &options()).unwrap();
    let rev2 = iri_of(&second, "rev");
    assert_ne!(rev2, rev1);
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SOURCES}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            rev2.as_str(),
            rev1.as_str()
        ))
        .unwrap()
    );
    let verified = record(&ds, &rev2, RecordKind::SourceRevision).unwrap();
    assert_eq!(verified.record_digest, Some(second.records[0].digest));

    // Re-proposing rev1 with a predecessor conflicts because prov:wasRevisionOf changes its
    // immutable record digest.
    let mut mutate_first = first_input.clone();
    mutate_first.revisions[0].was_revision_of = Some(RecordRef::Existing(rev2.clone()));
    assert_eq!(
        code(propose(&ds, &mutate_first, &options())),
        "immutable-record-conflict"
    );

    // Batch-local predecessor link writes both revisions and prov:wasRevisionOf in one commit.
    let mut local_chain = EvidenceInput {
        revisions: vec![
            SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("ra"),
                source: iri("urn:chain"),
                source_key: key("chain"),
                upstream_version: None,
                content: SourceContent::MetadataOnly { digest: sha("a") },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
                was_revision_of: None,
            },
            SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rb"),
                source: iri("urn:chain"),
                source_key: key("chain"),
                upstream_version: None,
                content: SourceContent::MetadataOnly { digest: sha("b") },
                metadata: Default::default(),
                times: Default::default(),
                lineage: Default::default(),
                was_revision_of: Some(local("ra")),
            },
        ],
        ..Default::default()
    };
    let chained = propose(&ds, &local_chain, &options()).unwrap();
    let ra = iri_of(&chained, "ra");
    let rb = iri_of(&chained, "rb");
    assert!(
        ds.ask(&format!(
            "ASK {{ GRAPH <{SOURCES}> {{ <{}> <http://www.w3.org/ns/prov#wasRevisionOf> <{}> }} }}",
            rb.as_str(),
            ra.as_str()
        ))
        .unwrap()
    );
    assert!(record(&ds, &ra, RecordKind::SourceRevision).is_ok());
    assert!(record(&ds, &rb, RecordKind::SourceRevision).is_ok());

    // Missing predecessor refuses as evidence-unavailable and writes nothing.
    let head = ds.store().head_commit().seq;
    let mut missing = small("c3", proposition());
    missing.revisions[0].content = SourceContent::MetadataOnly { digest: sha("v3") };
    missing.revisions[0].was_revision_of = Some(RecordRef::Existing(iri(
        "urn:uuid:00000000-0000-4000-8000-000000000000",
    )));
    assert_eq!(
        code(propose(&ds, &missing, &options())),
        "evidence-unavailable"
    );
    assert_eq!(ds.store().head_commit().seq, head);

    // Unauthorized caller (cannot read source graph) gets the same non-disclosing
    // evidence-unavailable error as a missing predecessor.
    let deny_sources = Arc::new(GraphAccess::graphs(
        Graphs::Only(GraphRule::new([CLAIMS], &[])),
        Graphs::All,
    ));
    let mut hidden_input = small("c3", proposition());
    hidden_input.revisions[0].content = SourceContent::MetadataOnly { digest: sha("v3") };
    hidden_input.revisions[0].was_revision_of = Some(RecordRef::Existing(rev1.clone()));
    let hidden = evidence::propose(
        None,
        &ds,
        &ds,
        &hidden_input,
        &options(),
        &mut |_, _| Some(deny_sources.clone()),
        &Control::none(),
    );
    assert_eq!(code(hidden), "evidence-unavailable");
    assert_eq!(ds.store().head_commit().seq, head);

    // Wrong-kind predecessor (a Claim instead of a SourceRevision) is refused.
    let mut wrong_kind = small("c3", proposition());
    wrong_kind.revisions[0].content = SourceContent::MetadataOnly { digest: sha("v3") };
    wrong_kind.revisions[0].was_revision_of = Some(RecordRef::Existing(claim1));
    assert_eq!(
        code(propose(&ds, &wrong_kind, &options())),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);

    // Existing-IRI self-link is refused before resolution.
    let mut self_iri = small("c1", proposition());
    self_iri.revisions[0].was_revision_of = Some(RecordRef::Existing(rev1.clone()));
    assert!(matches!(
        propose(&ds, &self_iri, &options()),
        Err(sparkles::Error::Invalid(_))
    ));
    assert_eq!(ds.store().head_commit().seq, head);

    // Local self-link is refused.
    local_chain.revisions[0].was_revision_of = Some(local("ra"));
    assert!(matches!(
        propose(&ds, &local_chain, &options()),
        Err(sparkles::Error::Invalid(_))
    ));
    assert_eq!(ds.store().head_commit().seq, head);

    // An unauthenticated predecessor planted while the source graph was detached is refused.
    evidence::detach(&ds, SOURCES).unwrap();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{SOURCES}> {{ <urn:fake-rev> a <urn:x-sparkles:evidence:SourceRevision> ; <http://www.w3.org/ns/prov#specializationOf> <urn:bulletin> . }} }}"
    ))
    .unwrap();
    configured(&ds);
    let head = ds.store().head_commit().seq;
    let mut planted = small("c4", proposition());
    planted.revisions[0].content = SourceContent::MetadataOnly { digest: sha("v4") };
    planted.revisions[0].was_revision_of = Some(RecordRef::Existing(iri("urn:fake-rev")));
    assert_eq!(
        code(propose(&ds, &planted, &options())),
        "origin-not-reproduced"
    );
    assert_eq!(ds.store().head_commit().seq, head);
}

#[test]
fn if_head_refuses_stale_head_before_resolving_newly_masked_predecessor() {
    use sparkles::access::{Caller, Limits, Protection, Rule, TripleRules};

    let ds = Dataset::memory();
    configured(&ds);

    let p1 = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let rev1 = iri_of(&p1, "rev");
    let expected_head = ds.store().head_commit().seq;

    // A triple-level read protection hides subjects typed as <urn:test:HiddenPred>.
    let hide_pred = Arc::new(Protection {
        name: "hide_pred".into(),
        predicates: None,
        classes: Some(vec!["urn:test:HiddenPred".into()]),
        subclasses: false,
        graphs: None,
        pattern: None,
        prefixes: Default::default(),
        hide_inferences: false,
    });
    let masked_read = Arc::new(GraphAccess::with_triples(
        Graphs::All,
        Graphs::All,
        TripleRules {
            rules: vec![Rule {
                protection: hide_pred,
                read: Graphs::none(),
                write: Graphs::All,
            }],
            caller: Caller::default(),
            limits: Limits::default(),
        },
    ));

    // Advance the head by tagging rev1 with the protected class in an ordinary graph.
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <urn:tags> {{ <{}> a <urn:test:HiddenPred> }} }}",
        rev1.as_str()
    ))
    .unwrap();
    let moved_head = ds.store().head_commit().seq;
    assert_ne!(moved_head, expected_head);

    let mut input = small("c2", proposition());
    input.revisions[0].content = SourceContent::MetadataOnly { digest: sha("v2") };
    input.revisions[0].was_revision_of = Some(RecordRef::Existing(rev1));

    // With stale if_head, propose must report head-changed rather than evidence-unavailable
    // from resolving against the newer masked snapshot.
    let mut stale = options();
    stale.if_head = Some(expected_head);
    let stale_res = evidence::propose(
        None,
        &ds,
        &ds,
        &input,
        &stale,
        &mut |_, _| Some(masked_read.clone()),
        &Control::none(),
    );
    assert_eq!(code(stale_res), "head-changed");
    assert_eq!(ds.store().head_commit().seq, moved_head);

    // At the current head (where rev1 is genuinely masked), propose still returns
    // evidence-unavailable.
    let mut current = options();
    current.if_head = Some(moved_head);
    let current_res = evidence::propose(
        None,
        &ds,
        &ds,
        &input,
        &current,
        &mut |_, _| Some(masked_read.clone()),
        &Control::none(),
    );
    assert_eq!(code(current_res), "evidence-unavailable");
    assert_eq!(ds.store().head_commit().seq, moved_head);
}

#[test]
fn record_charged_covers_verified_result_and_skips_unauthenticated_receipt_decode() {
    let ds = Dataset::memory();
    configured(&ds);

    let p = propose(&ds, &small("c1", proposition()), &options()).unwrap();
    let rev = iri_of(&p, "rev");

    // 1. `record_charged` holds memory covering `iri`, `graph`, and `receipt.iri` until
    //    its returned reservation is dropped.
    let mut res = Resolution::new(Control::none()).unwrap();
    let (warm, warm_held) = res
        .record_charged(
            "k",
            &ds,
            &ds,
            Some(Arc::new(GraphAccess::all())),
            &rev,
            &[RecordKind::SourceRevision],
            None,
        )
        .unwrap();
    drop(warm);
    drop(warm_held);
    let base_used = res.preparation().limit() - res.preparation().available_bytes();

    let (verified, held) = res
        .record_charged(
            "k",
            &ds,
            &ds,
            Some(Arc::new(GraphAccess::all())),
            &rev,
            &[RecordKind::SourceRevision],
            None,
        )
        .unwrap();
    let expected_min_bytes = verified.iri.as_str().len() as u64
        + verified.graph.as_str().len() as u64
        + verified.receipt.iri.as_str().len() as u64;
    let used_with_verified = res.preparation().limit() - res.preparation().available_bytes();
    let one_record = used_with_verified.saturating_sub(base_used);
    assert!(
        one_record >= expected_min_bytes,
        "VerifiedRecord reservation must cover variable-sized strings ({expected_min_bytes} B): {base_used} -> {used_with_verified}"
    );
    drop(verified);
    drop(held);
    let used_after_drop = res.preparation().limit() - res.preparation().available_bytes();
    assert_eq!(
        used_after_drop, base_used,
        "dropping VerifiedRecord reservation must restore baseline usage"
    );

    // 2. Under a tight cap where decoding a single ~80 B IRI (~11 KiB) and settings
    //    decoding (~64 KiB) fit comfortably, holding many live verified results exhausts
    //    the allowance with `incomplete-evidence`, and dropping them allows reuse.
    let tight_cap = base_used + 96 * 1024;
    let mut tight = Resolution::with_memory(Control::none(), tight_cap).unwrap();
    let mut live = Vec::new();
    let mut overflow = None;
    for _ in 0..400 {
        match tight.record_charged(
            "k",
            &ds,
            &ds,
            Some(Arc::new(GraphAccess::all())),
            &rev,
            &[RecordKind::SourceRevision],
            None,
        ) {
            Ok(pair) => live.push(pair),
            Err(e) => {
                overflow = Some(e);
                break;
            }
        }
    }
    assert!(
        !live.is_empty(),
        "at least one verified record must fit in cap={tight_cap}"
    );
    assert_eq!(
        code::<()>(Err(
            overflow.expect("retaining verified records must exhaust cap")
        )),
        "incomplete-evidence"
    );
    live.clear();
    tight
        .record_charged(
            "k",
            &ds,
            &ds,
            Some(Arc::new(GraphAccess::all())),
            &rev,
            &[RecordKind::SourceRevision],
            None,
        )
        .unwrap();

    // 3. An unauthenticated record pointing to a 10 KiB receipt IRI (whose `decode_term`
    //    needs > 1.2 MiB) is rejected as `origin-not-reproduced` under a 256 KiB cap
    //    because the nonce check runs before decoding or memoizing the receipt IRI.
    evidence::detach(&ds, SOURCES).unwrap();
    let huge_receipt = format!("urn:test:huge-receipt:{}", "r".repeat(10_000));
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{SOURCES}> {{ \
           <urn:planted-huge-receipt> a <urn:x-sparkles:evidence:SourceRevision> ; \
             <urn:x-sparkles:evidence:availabilityReceipt> <{huge_receipt}> . \
           <{huge_receipt}> a <urn:x-sparkles:evidence:Operation> ; \
             <urn:x-sparkles:evidence:datasetId> \"{}\" ; \
             <urn:x-sparkles:evidence:branchId> \"{}\" ; \
             <urn:x-sparkles:evidence:commit> 1 . \
         }} }}",
        ds.dataset_id(),
        ds.store().branch_id(),
    ))
    .unwrap();
    configured(&ds);

    let mut capped = Resolution::with_memory(Control::none(), 256 * 1024).unwrap();
    let err = capped.record_charged(
        "k",
        &ds,
        &ds,
        Some(Arc::new(GraphAccess::all())),
        &iri("urn:planted-huge-receipt"),
        &[RecordKind::SourceRevision],
        None,
    );
    assert_eq!(
        code(err.map(|(v, _)| v)),
        "origin-not-reproduced",
        "nonce check must reject planted record before decoding the 10 KiB receipt IRI"
    );
}
