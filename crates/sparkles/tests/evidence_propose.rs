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
