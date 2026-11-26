//! Administered re-authentication after a detach (spec C25, roadmap item 552).
//!
//! Detaching an evidence graph and protecting it again assigns a new protection nonce,
//! so records from the earlier protection answer `origin-not-reproduced` and their
//! identities refuse new proposals. `evidence::reauthenticate` checks candidate records
//! against their recorded digests, retained content, passage spans and publication
//! receipts, and atomically marks verified records under the current protection.

use oxrdf::{Literal, NamedNode, Term, Triple, vocab::xsd};
use sparkles::access::GraphAccess;
use sparkles::evidence::resolve::{RecordRequest, Reference, Resolution, Verified};
use sparkles::evidence::{
    AssessmentInput, BoundaryInput, ClaimInput, ClaimKind, Confidence, ConfidenceFacets,
    ConflictMode, DecisionInput, Disposition, EvidenceInput, EvidenceLinkInput, EvidenceRefInput,
    EvidenceRefTarget, EvidenceRelation, IdentityInput, IdentityRelation, JSON_LEXICAL_UTF8_V1,
    JSON_LEXICAL_UTF8_VERSION, MethodRef, NormalizationInput, NormalizationStatus, PassageInput,
    Polarity, Policy, ProposeOptions, ReauthenticateOptions, ReauthenticateRecord, RecordKey,
    RecordKind, RecordRef, RecordVersion, RenditionContent, RenditionInput, ReviewOptions, Scope,
    Settings, Sha256Digest, SourceContent, SourceLineage, SourceMetadata, SourceRevisionInput,
    SourceTimes, ValidityInput, WithdrawalInput,
};
use sparkles::managed::{ManagedError, ManagedErrorKind};
use sparkles::task::Control;
use sparkles::{Dataset, Error, evidence};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const EV: &str = "urn:x-sparkles:evidence:";
const SOURCES: &str = "urn:ev:sources";
const CLAIMS: &str = "urn:ev:claims";
const DECISIONS: &str = "urn:ev:decisions";
const SUPPORTS: &str = "urn:ev:supports";
const IDENTITY: &str = "urn:ev:identity";
const OUTPUT: &str = "urn:ev:accepted";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn key(s: &str) -> RecordKey {
    RecordKey::new(s).unwrap()
}

fn all() -> Option<Arc<GraphAccess>> {
    Some(Arc::new(GraphAccess::all()))
}

fn code<T: std::fmt::Debug>(r: sparkles::Result<T>) -> &'static str {
    match r.unwrap_err() {
        Error::Managed(m) => m.code,
        other => panic!("expected managed error, got {other:?}"),
    }
}

fn settings() -> Settings {
    let mut policies = BTreeMap::new();
    policies.insert(
        "advisories".into(),
        Policy {
            version: 1,
            admitted_kinds: vec![ClaimKind::Reported],
            required_scope: vec![],
            conflicts: ConflictMode::Refuse,
            boundary: vec![SOURCES.into(), CLAIMS.into()],
            decisions: DECISIONS.into(),
            supports: SUPPORTS.into(),
            output: OUTPUT.into(),
        },
    );
    Settings {
        enabled: true,
        source_records: vec![SOURCES.into()],
        proposals: vec![CLAIMS.into()],
        decisions: vec![DECISIONS.into()],
        supports: vec![SUPPORTS.into()],
        identity: vec![IDENTITY.into()],
        projections: vec![OUTPUT.into()],
        policies,
        ..Settings::default()
    }
}

fn configure(ds: &Dataset) {
    evidence::configure(ds, &settings(), &Control::none()).unwrap();
}

fn propose_options() -> ProposeOptions {
    ProposeOptions::new(iri("urn:x-sparkles:principal:importer"))
}

fn reauth_options() -> ReauthenticateOptions {
    ReauthenticateOptions::new(iri("urn:x-sparkles:principal:admin"))
}

fn proposition(tag: &str) -> Triple {
    Triple::new(
        iri(&format!("https://example.org/release-{tag}")),
        iri("https://example.org/affectedBy"),
        iri("https://example.org/cve-1"),
    )
}

fn sha256(bytes: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(bytes, &Control::none()).unwrap()
}

/// A rich batch exercising source revisions with retained JSON bytes, renditions with
/// normalized text, passages with selectors and quotes, structured and passage evidence
/// references, claims with confidence and C27 validity, links, assessments and identity
/// proposals.
fn rich_batch(tag: &str) -> EvidenceInput {
    let json_text = r#"{"affected":["v1.0","v2.0"],"severity":"high"}"#;
    let json_bytes = json_text.as_bytes().to_vec();
    let text = "Advisory: release v1.0 is affected.".to_string();
    let quote = "release v1.0";
    let start = "Advisory: ".chars().count() as u64;
    let end = start + quote.chars().count() as u64;
    let text_digest = sha256(quote.as_bytes());
    let value_digest = sparkles_core::evidence::select_json_lexical_utf8(
        json_text,
        "/affected/0",
        &Control::none(),
    )
    .unwrap()
    .digest;
    let mut scope = Scope::new();
    scope.insert(
        "https://example.org/release".into(),
        vec![Term::NamedNode(iri("https://example.org/v1"))],
    );
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("https://example.org/bulletin"),
            source_key: key(&format!("bulletin-{tag}")),
            upstream_version: Some(key("1")),
            content: SourceContent::Retained {
                bytes: json_bytes,
                expected_digest: None,
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes::default(),
            lineage: SourceLineage::default(),
        }],
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("rend"),
            revision: RecordRef::Local(key("rev")),
            occurrence_key: key("text-1"),
            conversion: MethodRef {
                method: iri("https://example.org/extract"),
                version: key("1"),
            },
            media_type: "text/plain".into(),
            language: Some("en".into()),
            content: RenditionContent::NormalizedText(text),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("pass"),
            revision: RecordRef::Local(key("rev")),
            rendition: RecordRef::Local(key("rend")),
            occurrence_key: key("p1"),
            start,
            end,
            text_digest,
            quote: Some(quote.into()),
        }],
        references: vec![
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("pref"),
                occurrence_key: key("pref-1"),
                target: EvidenceRefTarget::Passage {
                    revision: RecordRef::Local(key("rev")),
                    rendition: RecordRef::Local(key("rend")),
                    passage: RecordRef::Local(key("pass")),
                },
            },
            EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("sref"),
                occurrence_key: key("sref-1"),
                target: EvidenceRefTarget::Structured {
                    revision: RecordRef::Local(key("rev")),
                    pointer: "/affected/0".into(),
                    method: MethodRef {
                        method: iri(JSON_LEXICAL_UTF8_V1),
                        version: key(JSON_LEXICAL_UTF8_VERSION),
                    },
                    value_digest,
                },
            },
        ],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key(&format!("claim-{tag}")),
            revision: RecordRef::Local(key("rev")),
            proposition: proposition(tag),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("https://example.org/publisher"),
            scope,
            confidence: ConfidenceFacets {
                source: Some(Confidence {
                    scale: iri("urn:x-sparkles:evidence:ZeroToOne"),
                    value: Literal::new_typed_literal("0.9", xsd::DECIMAL),
                    evaluator: iri("https://example.org/publisher"),
                    method: None,
                }),
                extraction: None,
                analyst: None,
            },
            validity: Some(ValidityInput {
                start: BoundaryInput::Known {
                    earliest: Literal::new_typed_literal("2026-01-01T00:00:00Z", xsd::DATE_TIME),
                    latest: Literal::new_typed_literal("2026-01-01T00:00:00Z", xsd::DATE_TIME),
                    precision: iri("urn:x-sparkles:valid-time:Second"),
                    source_value: "2026-01-01T00:00:00Z".into(),
                },
                end: BoundaryInput::Unbounded { source_value: None },
                normalization: NormalizationInput {
                    status: NormalizationStatus::Normalized,
                    method: iri("https://example.org/norm"),
                    method_version: "1".into(),
                    calendar: iri("urn:x-sparkles:valid-time:Gregorian"),
                    timezone_policy: None,
                    tzdb_version: None,
                    fold_choice: None,
                },
            }),
        }],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link"),
            occurrence_key: key("link-1"),
            target: RecordRef::Local(key("claim")),
            evidence: RecordRef::Local(key("pref")),
            relation: EvidenceRelation::Supports,
            attributed_author: iri("https://example.org/importer"),
        }],
        assessments: vec![AssessmentInput {
            version: RecordVersion::V1,
            key: key("assess"),
            occurrence_key: key("assess-1"),
            target: RecordRef::Local(key("claim")),
            conclusion: Term::NamedNode(iri("https://example.org/confirmed")),
            method: MethodRef {
                method: iri("https://example.org/triage"),
                version: key("1"),
            },
            configuration_digest: sha256(b"cfg"),
            evaluated_at: Literal::new_typed_literal("2026-01-02T00:00:00Z", xsd::DATE_TIME),
            attributed_evaluator: iri("https://example.org/analyst"),
            confidence: Some(Confidence {
                scale: iri("urn:x-sparkles:evidence:ZeroToOne"),
                value: Literal::new_typed_literal("0.85", xsd::DECIMAL),
                evaluator: iri("https://example.org/analyst"),
                method: None,
            }),
            scope: Scope::new(),
            inputs: vec![RecordRef::Local(key("claim"))],
        }],
        identities: vec![IdentityInput {
            version: RecordVersion::V1,
            key: key("id"),
            occurrence_key: key(&format!("id-{tag}")),
            entities: vec![iri("https://example.org/e1"), iri("https://example.org/e2")],
            relation: IdentityRelation::PossibleMatch,
            preferred: Some(iri("https://example.org/e1")),
            attributed_author: iri("https://example.org/matcher"),
            method: None,
        }],
    }
}

fn small_batch(tag: &str, rev_key: &str) -> EvidenceInput {
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("https://example.org/bulletin"),
            source_key: key(rev_key),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha256(rev_key.as_bytes()),
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes::default(),
            lineage: SourceLineage::default(),
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key(tag),
            revision: RecordRef::Local(key("rev")),
            proposition: proposition(tag),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: iri("https://example.org/publisher"),
            scope: Scope::new(),
            confidence: ConfidenceFacets::default(),
            validity: None,
        }],
        ..EvidenceInput::default()
    }
}

fn propose(
    ds: &Dataset,
    input: &EvidenceInput,
    options: &ProposeOptions,
) -> sparkles::Result<evidence::Proposed> {
    evidence::propose(
        None,
        ds,
        ds,
        input,
        options,
        &mut |_, _| all(),
        &Control::none(),
    )
}

fn resolve_record(
    ds: &Dataset,
    logical: &Dataset,
    record_iri: &NamedNode,
    kind: RecordKind,
) -> sparkles::Result<sparkles_core::evidence::resolve::VerifiedRecord> {
    let mut r = Resolution::new(Control::none())?;
    let mut out = r.all(
        None,
        ds,
        logical,
        &[(
            "r".into(),
            Reference::Record(RecordRequest {
                iri: record_iri.clone(),
                kinds: vec![kind],
                content_digest: None,
            }),
        )],
        &mut |_, _| all(),
    )?;
    match out.remove(0) {
        Verified::Record(v) => Ok(v),
        Verified::Fact(_) => unreachable!(),
    }
}

fn iri_of(p: &evidence::Proposed, k: &str) -> NamedNode {
    p.records.iter().find(|r| r.key == k).unwrap().iri.clone()
}

fn digest_of(p: &evidence::Proposed, k: &str) -> Sha256Digest {
    p.records.iter().find(|r| r.key == k).unwrap().digest
}

#[test]
fn detach_and_reprotect_then_reauthenticate_restores_resolution_and_proposals() {
    let ds = Dataset::memory();
    configure(&ds);

    let batch = rich_batch("1");
    let proposed = propose(&ds, &batch, &propose_options()).unwrap();
    let rev_iri = iri_of(&proposed, "rev");
    let rend_iri = iri_of(&proposed, "rend");
    let pass_iri = iri_of(&proposed, "pass");
    let claim_iri = iri_of(&proposed, "claim");
    let assess_iri = iri_of(&proposed, "assess");
    let id_iri = iri_of(&proposed, "id");

    // Baseline resolution before detach preserves exact original receipt.
    let before_claim = resolve_record(&ds, &ds, &claim_iri, RecordKind::Claim).unwrap();
    let before_pass = resolve_record(&ds, &ds, &pass_iri, RecordKind::Passage).unwrap();
    assert_eq!(before_claim.receipt.iri, proposed.operation);
    assert_eq!(before_claim.receipt.commit, proposed.commit);
    assert!(before_pass.span.is_some());

    // Detach all three record graphs and protect them again under new nonces.
    evidence::detach(&ds, SOURCES).unwrap();
    evidence::detach(&ds, CLAIMS).unwrap();
    evidence::detach(&ds, IDENTITY).unwrap();
    configure(&ds);

    // Every earlier record now answers `origin-not-reproduced`, and re-proposing the
    // same identities refuses with `immutable-record-conflict`.
    for (r, k) in [
        (&rev_iri, RecordKind::SourceRevision),
        (&rend_iri, RecordKind::Rendition),
        (&pass_iri, RecordKind::Passage),
        (&claim_iri, RecordKind::Claim),
        (&assess_iri, RecordKind::Assessment),
        (&id_iri, RecordKind::IdentityProposal),
    ] {
        assert_eq!(
            code(resolve_record(&ds, &ds, r, k)),
            "origin-not-reproduced"
        );
    }
    assert_eq!(
        code(propose(&ds, &batch, &propose_options())),
        "immutable-record-conflict"
    );

    // Re-authenticate each protected record graph.
    let mut opts = reauth_options();
    opts.idempotency_key = Some("reauth-sources-1".into());
    let re_src =
        evidence::reauthenticate(&ds, &ds, &iri(SOURCES), &opts, &Control::none()).unwrap();
    assert!(re_src.committed);
    assert!(!re_src.replayed);
    assert_eq!(re_src.records.len(), 3);
    assert!(re_src.records.iter().all(|r| !r.already_authenticated));
    assert!(
        re_src
            .records
            .iter()
            .all(|r| r.receipt == proposed.operation)
    );

    // Replaying the same idempotency key returns the first receipt without writing.
    let head_after_src = ds.store().head_commit().seq;
    let replayed =
        evidence::reauthenticate(&ds, &ds, &iri(SOURCES), &opts, &Control::none()).unwrap();
    assert!(!replayed.committed);
    assert!(replayed.replayed);
    assert_eq!(replayed.operation, re_src.operation);
    assert_eq!(replayed.commit, re_src.commit);
    assert_eq!(replayed.records, re_src.records);
    assert_eq!(ds.store().head_commit().seq, head_after_src);

    // Reusing the same idempotency key with a different payload conflicts.
    let mut bad_replay = opts.clone();
    bad_replay.records = Some(vec![ReauthenticateRecord {
        iri: rev_iri.clone(),
        expected_digest: None,
    }]);
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(SOURCES),
            &bad_replay,
            &Control::none(),
        )),
        "idempotency-conflict"
    );

    // Re-authenticate CLAIMS and IDENTITY.
    let re_claims =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
            .unwrap();
    assert!(re_claims.committed);
    assert_eq!(re_claims.records.len(), 5);

    let re_id = evidence::reauthenticate(
        &ds,
        &ds,
        &iri(IDENTITY),
        &reauth_options(),
        &Control::none(),
    )
    .unwrap();
    assert!(re_id.committed);
    assert_eq!(re_id.records.len(), 1);

    // Keyless repeat is an idempotent no-op that creates no commit.
    let head_before_noop = ds.store().head_commit().seq;
    let noop =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
            .unwrap();
    assert!(!noop.committed);
    assert!(!noop.replayed);
    assert_eq!(ds.store().head_commit().seq, head_before_noop);
    assert!(noop.records.iter().all(|r| r.already_authenticated));

    // Resolution succeeds for every record and preserves original receipts and spans.
    let after_claim = resolve_record(&ds, &ds, &claim_iri, RecordKind::Claim).unwrap();
    assert_eq!(after_claim.receipt, before_claim.receipt);
    assert_eq!(after_claim.record_digest, before_claim.record_digest);
    let after_pass = resolve_record(&ds, &ds, &pass_iri, RecordKind::Passage).unwrap();
    assert_eq!(after_pass.receipt, before_pass.receipt);
    assert_eq!(after_pass.span, before_pass.span);

    // Re-proposing the exact same identities now succeeds as `existing: true`.
    let reproposed = propose(&ds, &batch, &propose_options()).unwrap();
    assert!(reproposed.records.iter().all(|r| r.existing));

    // A new proposal referencing the re-authenticated source revision and claim succeeds!
    let followup = EvidenceInput {
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim2"),
            occurrence_key: key("claim-followup"),
            revision: RecordRef::Existing(rev_iri.clone()),
            proposition: proposition("followup"),
            kind: ClaimKind::Observation,
            polarity: Polarity::Positive,
            attributed_author: iri("https://example.org/analyst"),
            scope: Scope::new(),
            confidence: ConfidenceFacets::default(),
            validity: None,
        }],
        assessments: vec![AssessmentInput {
            version: RecordVersion::V1,
            key: key("a2"),
            occurrence_key: key("a2"),
            target: RecordRef::Existing(claim_iri.clone()),
            conclusion: Term::NamedNode(iri("https://example.org/verified")),
            method: MethodRef {
                method: iri("https://example.org/triage"),
                version: key("1"),
            },
            configuration_digest: sha256(b"cfg2"),
            evaluated_at: Literal::new_typed_literal("2026-01-03T00:00:00Z", xsd::DATE_TIME),
            attributed_evaluator: iri("https://example.org/analyst"),
            confidence: None,
            scope: Scope::new(),
            inputs: vec![RecordRef::Existing(claim_iri.clone())],
        }],
        ..EvidenceInput::default()
    };
    let next = propose(&ds, &followup, &propose_options()).unwrap();
    assert!(next.committed);
    assert!(resolve_record(&ds, &ds, &iri_of(&next, "claim2"), RecordKind::Claim).is_ok());

    // A second detach + re-protect cycle also re-authenticates cleanly.
    evidence::detach(&ds, CLAIMS).unwrap();
    configure(&ds);
    assert_eq!(
        code(resolve_record(&ds, &ds, &claim_iri, RecordKind::Claim)),
        "origin-not-reproduced"
    );
    let re2 = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
        .unwrap();
    assert!(re2.committed);
    assert!(resolve_record(&ds, &ds, &claim_iri, RecordKind::Claim).is_ok());
}

#[test]
fn reauthenticate_rejects_tampered_records_atomically() {
    // 1. Tampered claim triple (extra triple added while detached)
    {
        let ds = Dataset::memory();
        configure(&ds);
        let p = propose(&ds, &rich_batch("t1"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{}> <https://example.org/extra> \"tampered\" }} }}",
            claim.as_str()
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 2. Tampered fragment sub-node (confidence value edited while detached)
    {
        let ds = Dataset::memory();
        configure(&ds);
        let p = propose(&ds, &rich_batch("t2"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{}#sourceConfidence> ev:value ?v }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{}#sourceConfidence> ev:value \"0.1\"^^<http://www.w3.org/2001/XMLSchema#decimal> }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{}#sourceConfidence> ev:value ?v }} }}",
            claim.as_str(),
            claim.as_str(),
            claim.as_str(),
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 3. Tampered ev:retainedBytes (bytes changed without changing ev:contentDigest)
    {
        let ds = Dataset::memory();
        configure(&ds);
        let _p = propose(&ds, &rich_batch("t3"), &propose_options()).unwrap();
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{SOURCES}> {{ ?r ev:retainedBytes ?b }} }}
             INSERT {{ GRAPH <{SOURCES}> {{ ?r ev:retainedBytes \"dGFtcGVyZWQ=\"^^<http://www.w3.org/2001/XMLSchema#base64Binary> }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ ?r ev:retainedBytes ?b }} }}"
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 4. Tampered ev:normalizedText (text changed without changing ev:contentDigest)
    {
        let ds = Dataset::memory();
        configure(&ds);
        let _p = propose(&ds, &rich_batch("t4"), &propose_options()).unwrap();
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{SOURCES}> {{ ?r ev:normalizedText ?t }} }}
             INSERT {{ GRAPH <{SOURCES}> {{ ?r ev:normalizedText \"tampered text\" }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ ?r ev:normalizedText ?t }} }}"
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 5. Tampered ev:firstAvailableAt (backdated while detached)
    {
        let ds = Dataset::memory();
        configure(&ds);
        let _p = propose(&ds, &rich_batch("t5"), &propose_options()).unwrap();
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{SOURCES}> {{ ?r ev:firstAvailableAt ?t }} }}
             INSERT {{ GRAPH <{SOURCES}> {{ ?r ev:firstAvailableAt \"2020-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ ?r ev:firstAvailableAt ?t }} }}"
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 6. Caller-supplied expected_digest mismatch -> target-changed
    {
        let ds = Dataset::memory();
        configure(&ds);
        let p = propose(&ds, &small_batch("c1", "r1"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: claim,
            expected_digest: Some(sha256(b"wrong-digest")),
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &opts,
                &Control::none(),
            )),
            "target-changed"
        );
    }

    // 7. Planted record pointing to a future predictable operation cannot be vouched
    {
        let ds = Dataset::memory();
        configure(&ds);
        let mut future_opts = propose_options();
        future_opts.idempotency_key = Some("predictable-future".into());
        let dry = {
            let mut d = future_opts.clone();
            d.dry_run = true;
            propose(&ds, &small_batch("fut", "rfut"), &d).unwrap()
        };
        let future_op = dry.operation.clone();
        let planted_claim = iri("urn:planted:claim");
        let planted_triples = vec![Triple::new(
            planted_claim.clone(),
            iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            iri(&format!("{EV}Claim")),
        )];
        let planted_digest = sparkles_core::evidence::write::record_digest(
            RecordKind::Claim,
            &planted_triples,
            &Control::none(),
        )
        .unwrap();
        let fake_nonce = uuid::Uuid::new_v4();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{c}> a ev:Claim ;
                     ev:availabilityReceipt <{op}> ;
                     prov:wasGeneratedBy <{op}> ;
                     ev:graphProtection \"{n}\" ;
                     ev:recordDigest \"{d}\" .
             }} }}",
            c = planted_claim.as_str(),
            op = future_op.as_str(),
            n = fake_nonce,
            d = planted_digest,
        ))
        .unwrap();
        configure(&ds);
        // Now the legitimate future operation publishes under the current protection!
        propose(&ds, &small_batch("fut", "rfut"), &future_opts).unwrap();
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: planted_claim.clone(),
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &opts,
                &Control::none(),
            )),
            "origin-not-reproduced"
        );
    }
}

#[test]
fn selective_reauthentication_and_partial_operation_replay_safety() {
    let ds = Dataset::memory();
    configure(&ds);

    let batch = EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("rev"),
            source: iri("https://example.org/bulletin"),
            source_key: key("multi-rev"),
            upstream_version: None,
            content: SourceContent::MetadataOnly {
                digest: sha256(b"multi-rev"),
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes::default(),
            lineage: SourceLineage::default(),
        }],
        claims: vec![
            ClaimInput {
                version: RecordVersion::V1,
                key: key("c1"),
                occurrence_key: key("c1"),
                revision: RecordRef::Local(key("rev")),
                proposition: proposition("c1"),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("https://example.org/pub"),
                scope: Scope::new(),
                confidence: ConfidenceFacets::default(),
                validity: None,
            },
            ClaimInput {
                version: RecordVersion::V1,
                key: key("c2"),
                occurrence_key: key("c2"),
                revision: RecordRef::Local(key("rev")),
                proposition: proposition("c2"),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("https://example.org/pub"),
                scope: Scope::new(),
                confidence: ConfidenceFacets::default(),
                validity: None,
            },
            ClaimInput {
                version: RecordVersion::V1,
                key: key("c3"),
                occurrence_key: key("c3"),
                revision: RecordRef::Local(key("rev")),
                proposition: proposition("c3"),
                kind: ClaimKind::Reported,
                polarity: Polarity::Positive,
                attributed_author: iri("https://example.org/pub"),
                scope: Scope::new(),
                confidence: ConfidenceFacets::default(),
                validity: None,
            },
        ],
        ..EvidenceInput::default()
    };
    let mut popts = propose_options();
    popts.idempotency_key = Some("multi-batch-key".into());
    let p = propose(&ds, &batch, &popts).unwrap();
    let c1 = iri_of(&p, "c1");
    let c2 = iri_of(&p, "c2");
    let c3 = iri_of(&p, "c3");
    let c1_digest = digest_of(&p, "c1");
    let c3_digest = digest_of(&p, "c3");

    // Detach CLAIMS, tamper ONLY with c2, and protect CLAIMS again.
    evidence::detach(&ds, CLAIMS).unwrap();
    ds.update(&format!(
        "INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{}> <https://example.org/bad> \"1\" }} }}",
        c2.as_str()
    ))
    .unwrap();
    configure(&ds);

    // Whole-graph reconciliation refuses because c2 is tampered.
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &reauth_options(),
            &Control::none(),
        )),
        "immutable-record-conflict"
    );

    // Selective reconciliation of c1 with pinned expected_digest succeeds.
    let mut opts1 = reauth_options();
    opts1.records = Some(vec![ReauthenticateRecord {
        iri: c1.clone(),
        expected_digest: Some(c1_digest),
    }]);
    let r1 = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts1, &Control::none()).unwrap();
    assert!(r1.committed);
    assert_eq!(r1.records.len(), 1);

    // c1 now resolves cleanly; tampered c2 and untouched c3 still refuse.
    assert!(resolve_record(&ds, &ds, &c1, RecordKind::Claim).is_ok());
    assert_eq!(
        code(resolve_record(&ds, &ds, &c2, RecordKind::Claim)),
        "origin-not-reproduced"
    );
    assert_eq!(
        code(resolve_record(&ds, &ds, &c3, RecordKind::Claim)),
        "origin-not-reproduced"
    );

    // Replaying the original proposal's idempotency key MUST NOT succeed while c2 is
    // still unauthenticated.
    assert_eq!(
        code(propose(&ds, &batch, &popts)),
        "immutable-record-conflict"
    );

    // A second selective reconciliation can still vouch for untampered c3 from the
    // same original operation (whose nonce was already renewed in r1 with
    // `prov:wasInformedBy`).
    let mut opts3 = reauth_options();
    opts3.records = Some(vec![ReauthenticateRecord {
        iri: c3.clone(),
        expected_digest: Some(c3_digest),
    }]);
    let r3 = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts3, &Control::none()).unwrap();
    assert!(r3.committed);
    assert!(resolve_record(&ds, &ds, &c3, RecordKind::Claim).is_ok());
    assert_eq!(
        code(resolve_record(&ds, &ds, &c2, RecordKind::Claim)),
        "origin-not-reproduced"
    );
}

#[test]
fn reauthenticate_refuses_detached_projection_stale_head_and_supports_dry_run_and_branches() {
    let ds = Dataset::memory();
    configure(&ds);
    let p = propose(&ds, &small_batch("c1", "r1"), &propose_options()).unwrap();
    let claim = iri_of(&p, "claim");

    // Refuses projection graph.
    assert!(matches!(
        evidence::reauthenticate(&ds, &ds, &iri(OUTPUT), &reauth_options(), &Control::none()),
        Err(Error::Invalid(_))
    ));

    // Refuses while graph is detached.
    evidence::detach(&ds, CLAIMS).unwrap();
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &reauth_options(),
            &Control::none(),
        )),
        "unsupported-profile"
    );
    configure(&ds);

    // Refuses stale if_head.
    let head = ds.store().head_commit().seq;
    let mut stale = reauth_options();
    stale.if_head = Some(head.saturating_sub(1));
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &stale,
            &Control::none(),
        )),
        "head-changed"
    );

    // Dry run previews changes without committing or authenticating.
    let mut dry = reauth_options();
    dry.dry_run = true;
    dry.if_head = Some(head);
    let previewed =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &dry, &Control::none()).unwrap();
    assert!(!previewed.committed);
    assert!(previewed.preview.is_some());
    assert_eq!(ds.store().head_commit().seq, head);
    assert_eq!(
        code(resolve_record(&ds, &ds, &claim, RecordKind::Claim)),
        "origin-not-reproduced"
    );

    // Branch support: reconcile on branch `dev` without changing `main`.
    ds.create_branch("dev", &Default::default()).unwrap();
    let dev = ds.branch("dev").unwrap();
    let on_dev =
        evidence::reauthenticate(&ds, &dev, &iri(CLAIMS), &reauth_options(), &Control::none())
            .unwrap();
    assert!(on_dev.committed);
    assert_eq!(on_dev.branch, dev.store().branch_id());
    assert!(resolve_record(&ds, &dev, &claim, RecordKind::Claim).is_ok());
    assert_eq!(
        code(resolve_record(&ds, &ds, &claim, RecordKind::Claim)),
        "origin-not-reproduced"
    );
}

#[test]
fn reauthenticate_survives_persistent_reopen_and_respects_authority_fence() {
    struct ToggleFence(Arc<AtomicBool>);
    impl evidence::AuthorityFence for ToggleFence {
        fn enter(&self) -> sparkles::Result<Box<dyn std::any::Any>> {
            if !self.0.load(Ordering::SeqCst) {
                return Err(ManagedError::error(
                    ManagedErrorKind::AuthorityChanged,
                    "revoked before commit",
                ));
            }
            Ok(Box::new(()))
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let claim_iri;
    let rev_iri;
    {
        let ds = Dataset::open(dir.path()).unwrap();
        configure(&ds);
        let p = propose(&ds, &small_batch("persist", "rev-p"), &propose_options()).unwrap();
        claim_iri = iri_of(&p, "claim");
        rev_iri = iri_of(&p, "rev");
        evidence::detach(&ds, CLAIMS).unwrap();
        evidence::detach(&ds, SOURCES).unwrap();
        configure(&ds);

        // Authority fence refusal writes nothing.
        let allowed = Arc::new(AtomicBool::new(false));
        let mut fenced = reauth_options();
        fenced.authority = Some(Arc::new(ToggleFence(allowed.clone())));
        let head = ds.store().head_commit().seq;
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &fenced,
                &Control::none(),
            )),
            "authority-changed"
        );
        assert_eq!(ds.store().head_commit().seq, head);

        // Once authority is valid, re-authentication commits durably.
        allowed.store(true, Ordering::SeqCst);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(SOURCES), &fenced, &Control::none())
                .unwrap()
                .committed
        );
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &fenced, &Control::none())
                .unwrap()
                .committed
        );
    }

    // Reopen from disk and verify re-authenticated records still resolve and accept
    // new proposals.
    let reopened = Dataset::open(dir.path()).unwrap();
    assert!(resolve_record(&reopened, &reopened, &rev_iri, RecordKind::SourceRevision).is_ok());
    assert!(resolve_record(&reopened, &reopened, &claim_iri, RecordKind::Claim).is_ok());
    let reproposed = propose(
        &reopened,
        &small_batch("persist", "rev-p"),
        &propose_options(),
    )
    .unwrap();
    assert!(reproposed.records.iter().all(|r| r.existing));
}

#[test]
fn reauthenticate_handles_withdrawals_and_preserves_projection_unavailable() {
    let ds = Dataset::memory();
    configure(&ds);
    let p = propose(
        &ds,
        &small_batch("rev-claim", "rev-src"),
        &propose_options(),
    )
    .unwrap();
    let claim = iri_of(&p, "claim");
    let claim_digest = digest_of(&p, "claim");

    // Accept the claim into the projection.
    let mut ropts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
    ropts.if_head = Some(ds.store().head_commit().seq);
    evidence::review(
        None,
        &ds,
        &ds,
        "advisories",
        1,
        &[DecisionInput {
            key: "d1".into(),
            target: claim.clone(),
            target_digest: claim_digest,
            disposition: Disposition::Accepted,
            supersedes: None,
        }],
        &ropts,
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();

    // Withdraw the claim (writes an `ev:Withdrawal` in CLAIMS).
    let w = evidence::withdraw(
        &ds,
        &ds,
        &[WithdrawalInput {
            key: "w1".into(),
            target: claim.clone(),
            target_digest: claim_digest,
        }],
        &ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer")),
        &mut |_, _| all(),
        &Control::none(),
    )
    .unwrap();
    let withdrawal_iri = w.withdrawals[0].iri.clone().unwrap();

    // Detach SUPPORTS and re-protect: even after re-authenticating SUPPORTS, the
    // projection record still carries the old `supportsProtection` nonce, so reviews
    // under the policy must still answer `projection-unavailable` until reconciled.
    evidence::detach(&ds, SUPPORTS).unwrap();
    configure(&ds);
    let re_sup = evidence::reauthenticate(
        &ds,
        &ds,
        &iri(SUPPORTS),
        &reauth_options(),
        &Control::none(),
    )
    .unwrap();
    assert!(re_sup.committed);
    let p2 = propose(&ds, &small_batch("c-after", "r-after"), &propose_options()).unwrap();
    let mut ropts2 = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
    ropts2.if_head = Some(ds.store().head_commit().seq);
    assert_eq!(
        code(evidence::review(
            None,
            &ds,
            &ds,
            "advisories",
            1,
            &[DecisionInput {
                key: "d2".into(),
                target: iri_of(&p2, "claim"),
                target_digest: digest_of(&p2, "claim"),
                disposition: Disposition::Accepted,
                supersedes: None,
            }],
            &ropts2,
            &mut |_, _| all(),
            &Control::none(),
        )),
        "projection-unavailable"
    );

    // Detach CLAIMS, tamper with withdrawal's `prov:invalidatedAtTime`, and verify
    // re-authentication refuses cleanly.
    evidence::detach(&ds, CLAIMS).unwrap();
    ds.update(&format!(
        "PREFIX prov: <http://www.w3.org/ns/prov#>
         DELETE {{ GRAPH <{CLAIMS}> {{ <{w}> prov:invalidatedAtTime ?t }} }}
         INSERT {{ GRAPH <{CLAIMS}> {{ <{w}> prov:invalidatedAtTime \"2020-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> }} }}
         WHERE {{ GRAPH <{CLAIMS}> {{ <{w}> prov:invalidatedAtTime ?t }} }}",
        w = withdrawal_iri.as_str(),
    ))
    .unwrap();
    configure(&ds);
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &reauth_options(),
            &Control::none(),
        )),
        "immutable-record-conflict"
    );
}
