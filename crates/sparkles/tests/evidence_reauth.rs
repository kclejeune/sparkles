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
const MAPS: &str = "urn:ev:maps";
const OUTPUT: &str = "urn:ev:accepted";
const MAP_OUTPUT: &str = "urn:ev:identities-out";

fn iri(s: &str) -> NamedNode {
    NamedNode::new(s).unwrap()
}

fn memory_ds() -> Dataset {
    let ds = Dataset::memory();
    let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
    ds.store()
        .activate_memory_configuration_history(&prep)
        .unwrap();
    ds.store()
        .set_retention(sparkles_core::history::Retention {
            keep_commits: Some(256),
            ..Default::default()
        })
        .unwrap();
    ds
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

fn identity_settings() -> Settings {
    let mut s = settings();
    s.identity = vec![IDENTITY.into(), MAPS.into()];
    s.projections = vec![OUTPUT.into(), MAP_OUTPUT.into()];
    s.matching_policies.insert(
        "people".into(),
        sparkles::evidence::MatchingPolicy {
            version: 1,
            boundary: vec![IDENTITY.into()],
            decisions: DECISIONS.into(),
            maps: MAPS.into(),
            output: MAP_OUTPUT.into(),
        },
    );
    s
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
    let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
        let ds = memory_ds();
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
    let ds = memory_ds();
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
    let ds = memory_ds();
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

    // Refuses when the caller's graph write grants exclude the target graph.
    let mut restricted = reauth_options();
    restricted.write = Some(Arc::new(GraphAccess::graphs(
        sparkles::access::Graphs::All,
        sparkles::access::Graphs::Only(sparkles::access::GraphRule::new([SOURCES], &[])),
    )));
    assert!(matches!(
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &restricted, &Control::none()),
        Err(Error::NotPermitted(_))
    ));

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
    {
        let init = Dataset::open(dir.path()).unwrap();
        drop(init);
        let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        sparkles_core::migrate::activate_configuration_history(dir.path(), false, &prep).unwrap();
    }
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
    let ds = memory_ds();
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

#[test]
fn reauthenticate_requires_configuration_history_and_refuses_detached_forgeries() {
    // 1. Unactivated configuration history -> 409 configuration-history-inactive.
    {
        let ds = Dataset::memory();
        configure(&ds);
        let _p = propose(&ds, &small_batch("unact", "r-unact"), &propose_options()).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
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
            "configuration-history-inactive"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 2. Unretained/pruned publication commit -> 422 origin-not-reproduced (even with pinned digest).
    {
        let ds = Dataset::memory();
        let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        ds.store()
            .activate_memory_configuration_history(&prep)
            .unwrap();
        // Keep 0 past commits so commit 1 is dropped once commit 2 happens.
        configure(&ds);
        let p = propose(&ds, &small_batch("pruned", "r-pruned"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        let claim_digest = digest_of(&p, "claim");
        // Advance head with an unrelated write so commit 1 is no longer retained.
        ds.update("INSERT DATA { GRAPH <urn:other> { <urn:a> <urn:b> <urn:c> } }")
            .unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: claim.clone(),
            expected_digest: Some(claim_digest),
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

    // 3. Changed record content PLUS recomputed `ev:recordDigest` while detached -> refused!
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("recomp", "r-recomp"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        // Add an extra triple AND recompute and replace `ev:recordDigest` to match it.
        let extra_pred = iri("https://example.org/tampered");
        let extra_obj = Term::Literal(Literal::new_simple_literal("forged"));
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{c}> <{p}> \"forged\" }} }}",
            c = claim.as_str(),
            p = extra_pred.as_str(),
        ))
        .unwrap();
        // Read all current immutable triples of `claim` from `CLAIMS` and recompute digest.
        let snap = ds.store().snapshot();
        let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        let mut triples = Vec::new();
        let g_id = prep.lookup_iri(&snap, CLAIMS).unwrap().unwrap();
        let s_id = prep.lookup_iri(&snap, claim.as_str()).unwrap().unwrap();
        let excluded = [
            format!("{EV}availabilityReceipt"),
            "http://www.w3.org/ns/prov#wasGeneratedBy".to_string(),
            format!("{EV}graphProtection"),
            format!("{EV}recordDigest"),
        ];
        prep.scan(
            &snap,
            sparkles_core::index::Perm::Gspo,
            &[g_id.0, s_id.0],
            |q| {
                let p_term = prep.with_term(&snap, q[1], |t| Ok(t.cloned()))?.unwrap();
                let o_term = prep.with_term(&snap, q[2], |t| Ok(t.cloned()))?.unwrap();
                if let Term::NamedNode(pn) = p_term
                    && !excluded.iter().any(|e| e == pn.as_str())
                {
                    triples.push(Triple::new(claim.clone(), pn, o_term));
                }
                Ok(true)
            },
        )
        .unwrap();
        assert!(
            triples
                .iter()
                .any(|t| t.predicate == extra_pred && t.object == extra_obj)
        );
        let forged_digest = sparkles_core::evidence::write::record_digest(
            RecordKind::Claim,
            &triples,
            &Control::none(),
        )
        .unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{c}> ev:recordDigest ?d }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{c}> ev:recordDigest \"{fd}\" }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{c}> ev:recordDigest ?d }} }}",
            c = claim.as_str(),
            fd = forged_digest,
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
        assert_eq!(
            code(resolve_record(&ds, &ds, &claim, RecordKind::Claim)),
            "origin-not-reproduced"
        );
    }

    // 4. Planted record absent from publication commit claiming existing earlier receipt + matching nonce.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("legit", "r-legit"), &propose_options()).unwrap();
        let legit_op = p.operation.clone();
        // Read the original protection nonce on `legit_op`.
        let orig_nonce = {
            let snap = ds.store().snapshot();
            let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
            let mut reads = sparkles_core::evidence::write::Reads::new(&prep, &snap);
            match reads
                .single(legit_op.as_str(), &format!("{EV}graphProtection"), CLAIMS)
                .unwrap()
                .unwrap()
            {
                Term::Literal(l) => l.value().to_string(),
                other => panic!("unexpected {other:?}"),
            }
        };
        let planted = iri("urn:planted:absent-from-commit");
        let planted_digest = sparkles_core::evidence::write::record_digest(
            RecordKind::Claim,
            &[Triple::new(
                planted.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri(&format!("{EV}Claim")),
            )],
            &Control::none(),
        )
        .unwrap();
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
            c = planted.as_str(),
            op = legit_op.as_str(),
            n = orig_nonce,
            d = planted_digest,
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
            "origin-not-reproduced"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 5. Matching edits to BOTH `ev:firstAvailableAt` AND receipt `prov:endedAtTime` -> refused!
    {
        let ds = memory_ds();
        configure(&ds);
        let _p = propose(&ds, &rich_batch("ts-both"), &propose_options()).unwrap();
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#>
             DELETE {{ GRAPH <{SOURCES}> {{ ?r ev:firstAvailableAt ?t1 . ?op prov:endedAtTime ?t2 }} }}
             INSERT {{ GRAPH <{SOURCES}> {{
                 ?r ev:firstAvailableAt \"2020-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
                 ?op prov:endedAtTime \"2020-01-01T00:00:00Z\"^^<http://www.w3.org/2001/XMLSchema#dateTime> .
             }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ ?r ev:firstAvailableAt ?t1 ; ev:availabilityReceipt ?op . ?op prov:endedAtTime ?t2 }} }}"
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

    // 6. Detached write forging BOTH a new record AND a new receipt naming the detached commit itself -> refused!
    {
        let ds = memory_ds();
        configure(&ds);
        evidence::detach(&ds, CLAIMS).unwrap();
        let fake_op = iri("urn:uuid:11111111-2222-4333-8444-555555555555");
        let fake_claim = iri("urn:forged:detached-commit-claim");
        let fake_nonce = uuid::Uuid::new_v4();
        let fake_digest = sparkles_core::evidence::write::record_digest(
            RecordKind::Claim,
            &[Triple::new(
                fake_claim.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri(&format!("{EV}Claim")),
            )],
            &Control::none(),
        )
        .unwrap();
        let next_commit = ds.store().head_commit().seq + 1;
        ds.update(&format!(
            "PREFIX ev: <{EV}> PREFIX prov: <http://www.w3.org/ns/prov#> PREFIX xsd: <http://www.w3.org/2001/XMLSchema#>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{op}> a ev:Operation , prov:Activity ;
                     ev:datasetId \"{ds_id}\" ;
                     ev:branchId \"{ds_id}\" ;
                     ev:commit {seq} ;
                     prov:wasAssociatedWith <urn:x-sparkles:principal:attacker> ;
                     prov:endedAtTime \"2026-01-01T00:00:00Z\"^^xsd:dateTime ;
                     ev:payloadDigest \"{pd}\" ;
                     ev:graphProtection \"{n}\" .
                 <{c}> a ev:Claim ;
                     ev:availabilityReceipt <{op}> ;
                     prov:wasGeneratedBy <{op}> ;
                     ev:graphProtection \"{n}\" ;
                     ev:recordDigest \"{d}\" .
             }} }}",
            op = fake_op.as_str(),
            ds_id = ds.dataset_id(),
            seq = next_commit,
            pd = sha256(b"fake"),
            n = fake_nonce,
            c = fake_claim.as_str(),
            d = fake_digest,
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
            "origin-not-reproduced"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }
}

#[test]
fn reauthenticate_refuses_preexisting_triples_on_audit_subject() {
    let ds = memory_ds();
    configure(&ds);
    let p = propose(
        &ds,
        &small_batch("audit-pre", "r-audit-pre"),
        &propose_options(),
    )
    .unwrap();
    let claim = iri_of(&p, "claim");
    let receipt = p.operation.clone();

    // Precompute the predictable audit operation IRI for key "admin-key-1".
    let admin_principal = reauth_options().principal;
    let op_id = sparkles_core::evidence::write::operation_id(
        ds.dataset_id(),
        &admin_principal,
        "admin-key-1",
    );
    let audit_iri = sparkles_core::evidence::write::operation_iri(op_id);

    // Detach CLAIMS and plant an untyped `prov:wasInformedBy` triple on `audit_iri`.
    evidence::detach(&ds, CLAIMS).unwrap();
    ds.update(&format!(
        "PREFIX prov: <http://www.w3.org/ns/prov#>
         INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{a}> prov:wasInformedBy <{r}> }} }}",
        a = audit_iri.as_str(),
        r = receipt.as_str(),
    ))
    .unwrap();
    configure(&ds);

    let mut opts = reauth_options();
    opts.idempotency_key = Some("admin-key-1".into());
    let head = ds.store().head_commit().seq;
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &opts,
            &Control::none(),
        )),
        "immutable-record-conflict"
    );
    assert_eq!(ds.store().head_commit().seq, head);
    // The untyped audit subject gained no `ev:Operation` type or protection marker, and
    // `claim` remains unauthenticated.
    assert_eq!(
        code(resolve_record(&ds, &ds, &claim, RecordKind::Claim)),
        "origin-not-reproduced"
    );
}

#[test]
fn multi_detach_partial_reconciliation_works_in_any_order_and_rejects_unrelated_nonce() {
    // Helper that creates two claims under N1, detaches to N2, selectively reconciles c1
    // (leaving c2 at N1 and receipt at N2), then detaches again to N3.
    let setup_two_detaches = || {
        let ds = memory_ds();
        configure(&ds);
        let batch = EvidenceInput {
            revisions: vec![SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("rev"),
                source: iri("https://example.org/bulletin"),
                source_key: key("md-rev"),
                upstream_version: None,
                content: SourceContent::MetadataOnly {
                    digest: sha256(b"md-rev"),
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
                    proposition: proposition("md1"),
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
                    proposition: proposition("md2"),
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
        let p = propose(&ds, &batch, &propose_options()).unwrap();
        let c1 = iri_of(&p, "c1");
        let c2 = iri_of(&p, "c2");
        // Detach #1 -> N2, selectively reconcile ONLY c1.
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let mut sel1 = reauth_options();
        sel1.records = Some(vec![ReauthenticateRecord {
            iri: c1.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &sel1, &Control::none())
                .unwrap()
                .committed
        );
        // Detach #2 -> N3. Now c1 & OP carry N2, while c2 still carries N1.
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        (ds, c1, c2)
    };

    // (a) Whole-graph reconciliation under N3 succeeds for both c1 and c2.
    {
        let (ds, c1, c2) = setup_two_detaches();
        let res =
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(res.committed);
        assert_eq!(res.records.len(), 2);
        assert!(resolve_record(&ds, &ds, &c1, RecordKind::Claim).is_ok());
        assert!(resolve_record(&ds, &ds, &c2, RecordKind::Claim).is_ok());
    }

    // (b) Selective c2 first, then c1 under N3.
    {
        let (ds, c1, c2) = setup_two_detaches();
        let mut o2 = reauth_options();
        o2.records = Some(vec![ReauthenticateRecord {
            iri: c2.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &o2, &Control::none())
                .unwrap()
                .committed
        );
        let mut o1 = reauth_options();
        o1.records = Some(vec![ReauthenticateRecord {
            iri: c1.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &o1, &Control::none())
                .unwrap()
                .committed
        );
        assert!(resolve_record(&ds, &ds, &c1, RecordKind::Claim).is_ok());
        assert!(resolve_record(&ds, &ds, &c2, RecordKind::Claim).is_ok());
    }

    // (c) Selective c1 first, then c2 under N3.
    {
        let (ds, c1, c2) = setup_two_detaches();
        let mut o1 = reauth_options();
        o1.records = Some(vec![ReauthenticateRecord {
            iri: c1.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &o1, &Control::none())
                .unwrap()
                .committed
        );
        let mut o2 = reauth_options();
        o2.records = Some(vec![ReauthenticateRecord {
            iri: c2.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &o2, &Control::none())
                .unwrap()
                .committed
        );
        assert!(resolve_record(&ds, &ds, &c1, RecordKind::Claim).is_ok());
        assert!(resolve_record(&ds, &ds, &c2, RecordKind::Claim).is_ok());
    }

    // (d) Overwriting c2's protection nonce with an unrelated random UUID during detach
    //     is refused even after c1 is reconciled under N3.
    {
        let (ds, c1, c2) = setup_two_detaches();
        evidence::detach(&ds, CLAIMS).unwrap();
        let random_nonce = uuid::Uuid::new_v4();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{c2}> ev:graphProtection ?n }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{c2}> ev:graphProtection \"{rn}\" }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{c2}> ev:graphProtection ?n }} }}",
            c2 = c2.as_str(),
            rn = random_nonce,
        ))
        .unwrap();
        configure(&ds);
        let mut o1 = reauth_options();
        o1.records = Some(vec![ReauthenticateRecord {
            iri: c1.clone(),
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &o1, &Control::none())
                .unwrap()
                .committed
        );
        let mut o2 = reauth_options();
        o2.records = Some(vec![ReauthenticateRecord {
            iri: c2.clone(),
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &o2,
                &Control::none(),
            )),
            "origin-not-reproduced"
        );
    }
}

#[test]
fn reauthenticate_handles_reifier_anchored_blank_node_expected_triple() {
    let ds = memory_ds();
    configure(&ds);

    // Assert a blank-node fact anchored by a stable RDF 1.2 reifier in a data graph.
    let data_graph = iri("urn:data:facts");
    let reifier = iri("urn:reifier:blank-fact-1");
    ds.update(&format!(
        "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
         INSERT DATA {{
             GRAPH <{g}> {{
                 _:stored_node <https://example.org/label> \"alpha\" .
                 <{r}> rdf:reifies <<( _:stored_node <https://example.org/label> \"alpha\" )>> .
             }}
         }}",
        g = data_graph.as_str(),
        r = reifier.as_str(),
    ))
    .unwrap();
    let fact_commit = ds.store().head_commit().seq;

    // Propose a stored-fact `ev:EvidenceRef` whose input blank-node label differs from
    // whatever internal blank-node ID the store allocates on `encode_quad`.
    let caller_triple = Triple::new(
        oxrdf::BlankNode::new("caller_distinct_label_xyz").unwrap(),
        iri("https://example.org/label"),
        Literal::new_simple_literal("alpha"),
    );
    let batch = EvidenceInput {
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("bref"),
            occurrence_key: key("bref-1"),
            target: EvidenceRefTarget::StoredFact(sparkles_core::evidence::StoredFactRef {
                dataset: ds.dataset_id(),
                branch: ds.dataset_id(),
                commit: fact_commit,
                graph: sparkles_core::evidence::GraphIdentity::Named(data_graph),
                expected: caller_triple,
                anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(reifier)),
            }),
        }],
        ..EvidenceInput::default()
    };
    let p = propose(&ds, &batch, &propose_options()).unwrap();
    let bref = iri_of(&p, "bref");

    // Detach, re-protect, and re-authenticate CLAIMS -> must succeed cleanly!
    evidence::detach(&ds, CLAIMS).unwrap();
    configure(&ds);
    let res = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
        .unwrap();
    assert!(res.committed);
    assert_eq!(res.records.len(), 1);
    assert_eq!(res.records[0].iri, bref);
    assert!(resolve_record(&ds, &ds, &bref, RecordKind::EvidenceRef).is_ok());

    // Legacy pre-fix digest fallback: simulate a record published under the old scheme
    // where `ev:recordDigest` hashed a caller blank-node label that differs from the
    // canonical `b0` label (while `ev:expectedTriple` was stored with a store bnode ID).
    {
        let ds2 = memory_ds();
        configure(&ds2);
        let data_g = iri("urn:data:legacy");
        let reif = iri("urn:reifier:legacy-1");
        ds2.update(&format!(
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             INSERT DATA {{ GRAPH <{g}> {{
                 _:s <https://example.org/p> \"v\" .
                 <{r}> rdf:reifies <<( _:s <https://example.org/p> \"v\" )>> .
             }} }}",
            g = data_g.as_str(),
            r = reif.as_str(),
        ))
        .unwrap();
        let fc = ds2.store().head_commit().seq;
        // Propose under current canonical digest, then publish an identical protected
        // record with a legacy raw-label digest (simulated as a pre-fix build commit).
        let legacy_ref = iri("urn:uuid:aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee");
        let legacy_op_id = uuid::Uuid::new_v4();
        let legacy_op = sparkles_core::evidence::write::operation_iri(legacy_op_id);
        let cap = ds2
            .store()
            .managed_writer(sparkles::managed::OwnerKind::Evidence, legacy_op_id)
            .unwrap();
        let nonce = cap.protection(CLAIMS).unwrap();
        let mut t = ds2
            .store()
            .try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            )
            .unwrap();
        t.set_managed(cap).unwrap();
        let (seq, time_ms) = t.managed_stamp().unwrap();
        let gn = oxrdf::GraphName::NamedNode(iri(CLAIMS));
        let expected_triple = Triple::new(
            oxrdf::BlankNode::new("legacy_caller_label_999").unwrap(),
            iri("https://example.org/p"),
            Literal::new_simple_literal("v"),
        );
        // Legacy digest differs from canonical digest because it hashed raw label.
        let legacy_digest = sha256(b"legacy-raw-label-digest");
        let ts = sparkles_core::evidence::write::date_time(time_ms);
        let quads = vec![
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri(&format!("{EV}Operation")),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri("http://www.w3.org/ns/prov#Activity"),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri("http://www.w3.org/ns/prov#wasAssociatedWith"),
                iri("urn:x-sparkles:principal:importer"),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri("http://www.w3.org/ns/prov#endedAtTime"),
                ts,
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds2.dataset_id().to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(ds2.dataset_id().to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri(&format!("{EV}commit")),
                Literal::new_typed_literal(seq.to_string(), xsd::INTEGER),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri(&format!("{EV}payloadDigest")),
                Literal::new_simple_literal(sha256(b"legacy-payload").to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_op.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(nonce.to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri(&format!("{EV}EvidenceRef")),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds2.dataset_id().to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(ds2.dataset_id().to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}commit")),
                Literal::new_typed_literal(fc.to_string(), xsd::INTEGER),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}graphKind")),
                iri(&format!("{EV}NamedGraph")),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}graph")),
                data_g,
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}reifier")),
                reif,
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}expectedTriple")),
                Term::Triple(Box::new(expected_triple)),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}availabilityReceipt")),
                legacy_op.clone(),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri("http://www.w3.org/ns/prov#wasGeneratedBy"),
                legacy_op,
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}graphProtection")),
                Literal::new_simple_literal(nonce.to_string()),
                gn.clone(),
            ),
            oxrdf::Quad::new(
                legacy_ref.clone(),
                iri(&format!("{EV}recordDigest")),
                Literal::new_simple_literal(legacy_digest.to_string()),
                gn,
            ),
        ];
        let mut labels = std::collections::HashMap::new();
        for q in &quads {
            let ids = t.encode_quad(q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();

        // Untampered legacy record succeeds via its retained publication view.
        evidence::detach(&ds2, CLAIMS).unwrap();
        configure(&ds2);
        let res2 = evidence::reauthenticate(
            &ds2,
            &ds2,
            &iri(CLAIMS),
            &reauth_options(),
            &Control::none(),
        )
        .unwrap();
        assert!(res2.committed);
        assert!(resolve_record(&ds2, &ds2, &legacy_ref, RecordKind::EvidenceRef).is_ok());

        // Tampering with the legacy record while detached still refuses!
        evidence::detach(&ds2, CLAIMS).unwrap();
        ds2.update(&format!(
            "INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{r}> <https://example.org/tampered> \"1\" }} }}",
            r = legacy_ref.as_str(),
        ))
        .unwrap();
        configure(&ds2);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds2,
                &ds2,
                &iri(CLAIMS),
                &reauth_options(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }
}

#[test]
fn reauthenticate_handles_multi_mib_retained_bytes_and_json_structured_reference() {
    let ds = memory_ds();
    configure(&ds);

    // 2 MiB binary retained payload (~2.67 MiB base64 -> would exceed 64 MiB by >5x under
    // the old `128x` decode charge!) plus a 1.2 MiB retained JSON revision with a
    // structured `ev:EvidenceRef` pointing into it.
    let bin_bytes: Vec<u8> = (0..(2 * 1024 * 1024)).map(|i| (i % 251) as u8).collect();
    let padding = "x".repeat(1200 * 1024);
    let json_text = format!(r#"{{"target":"needle","pad":"{padding}"}}"#);
    let val_digest =
        sparkles_core::evidence::select_json_lexical_utf8(&json_text, "/target", &Control::none())
            .unwrap()
            .digest;

    let batch = EvidenceInput {
        revisions: vec![
            SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("binrev"),
                source: iri("https://example.org/bin"),
                source_key: key("bin-2mib"),
                upstream_version: None,
                content: SourceContent::Retained {
                    bytes: bin_bytes,
                    expected_digest: None,
                },
                metadata: SourceMetadata::default(),
                times: SourceTimes::default(),
                lineage: SourceLineage::default(),
            },
            SourceRevisionInput {
                version: RecordVersion::V1,
                key: key("jsonrev"),
                source: iri("https://example.org/json"),
                source_key: key("json-1mib"),
                upstream_version: None,
                content: SourceContent::Retained {
                    bytes: json_text.into_bytes(),
                    expected_digest: None,
                },
                metadata: SourceMetadata::default(),
                times: SourceTimes::default(),
                lineage: SourceLineage::default(),
            },
        ],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("sref"),
            occurrence_key: key("sref-big"),
            target: EvidenceRefTarget::Structured {
                revision: RecordRef::Local(key("jsonrev")),
                pointer: "/target".into(),
                method: MethodRef {
                    method: iri(JSON_LEXICAL_UTF8_V1),
                    version: key(JSON_LEXICAL_UTF8_VERSION),
                },
                value_digest: val_digest,
            },
        }],
        ..EvidenceInput::default()
    };
    let p = propose(&ds, &batch, &propose_options()).unwrap();
    let binrev = iri_of(&p, "binrev");
    let sref = iri_of(&p, "sref");

    evidence::detach(&ds, SOURCES).unwrap();
    evidence::detach(&ds, CLAIMS).unwrap();
    configure(&ds);

    let re_src =
        evidence::reauthenticate(&ds, &ds, &iri(SOURCES), &reauth_options(), &Control::none())
            .unwrap();
    assert!(re_src.committed);
    assert_eq!(re_src.records.len(), 2);
    let re_claims =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
            .unwrap();
    assert!(re_claims.committed);
    assert_eq!(re_claims.records.len(), 1);
    assert!(resolve_record(&ds, &ds, &binrev, RecordKind::SourceRevision).is_ok());
    assert!(resolve_record(&ds, &ds, &sref, RecordKind::EvidenceRef).is_ok());
}

#[test]
fn reauthenticate_keyed_whole_graph_replay_and_cross_graph_idempotency() {
    let ds = memory_ds();
    configure(&ds);
    let p1 = propose(&ds, &small_batch("m1", "rm1"), &propose_options()).unwrap();
    let c1 = iri_of(&p1, "claim");

    // Detach & re-protect CLAIMS, then propose a second claim `c2` under the NEW nonce
    // so CLAIMS is mixed: `c1` stale, `c2` already authenticated.
    evidence::detach(&ds, CLAIMS).unwrap();
    configure(&ds);
    let mut p2_opts = propose_options();
    p2_opts.idempotency_key = Some("prop-key-shared".into());
    let p2 = propose(&ds, &small_batch("m2", "rm2"), &p2_opts).unwrap();
    let c2 = iri_of(&p2, "claim");

    // Keyed whole-graph reconciliation on mixed graph returns both records with exact flags.
    let head_before = ds.store().head_commit().seq;
    let mut opts = reauth_options();
    opts.idempotency_key = Some("whole-mixed-1".into());
    opts.if_head = Some(head_before);
    let first = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts, &Control::none()).unwrap();
    assert!(first.committed);
    assert!(!first.replayed);
    assert_eq!(first.records.len(), 2);
    assert!(
        first
            .records
            .iter()
            .any(|r| r.iri == c1 && !r.already_authenticated)
    );
    assert!(
        first
            .records
            .iter()
            .any(|r| r.iri == c2 && r.already_authenticated)
    );

    // Replaying with the original (now stale!) `ifHead` must STILL replay identically
    // (replay runs before `ifHead` AND preserves alreadyAuthenticated records).
    let replayed =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts, &Control::none()).unwrap();
    assert!(!replayed.committed);
    assert!(replayed.replayed);
    assert_eq!(replayed.operation, first.operation);
    assert_eq!(replayed.commit, first.commit);
    assert_eq!(replayed.records, first.records);

    // Cross-graph key reuse: reusing "whole-mixed-1" on SOURCES must return
    // `409 idempotency-conflict`, NOT publish a second operation under the same IRI.
    let mut cross = reauth_options();
    cross.idempotency_key = Some("whole-mixed-1".into());
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(SOURCES),
            &cross,
            &Control::none(),
        )),
        "idempotency-conflict"
    );

    // Cross-operation key reuse: reusing the `propose` key "prop-key-shared" for
    // `reauthenticate` on SOURCES (same principal) must return `idempotency-conflict`
    // and leave the original proposal replay intact.
    let mut cross_op = ReauthenticateOptions::new(propose_options().principal);
    cross_op.idempotency_key = Some("prop-key-shared".into());
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(SOURCES),
            &cross_op,
            &Control::none(),
        )),
        "idempotency-conflict"
    );
    let prop_replay = propose(&ds, &small_batch("m2", "rm2"), &p2_opts).unwrap();
    assert!(prop_replay.replayed);
    assert_eq!(prop_replay.operation, p2.operation);

    // Keyed whole-graph reconciliation on an all-current graph returns all records on
    // both initial call and replay.
    let mut all_cur = reauth_options();
    all_cur.idempotency_key = Some("whole-all-current".into());
    let first_cur =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &all_cur, &Control::none()).unwrap();
    assert!(first_cur.committed);
    assert_eq!(first_cur.records.len(), 2);
    assert!(first_cur.records.iter().all(|r| r.already_authenticated));
    let replay_cur =
        evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &all_cur, &Control::none()).unwrap();
    assert!(replay_cur.replayed);
    assert_eq!(replay_cur.records, first_cur.records);
}

#[test]
fn reauthenticate_rejects_noncanonical_uuids_and_subnode_large_predicates() {
    // 1. Uppercase UUID on record -> refuses without writing or leaving duplicate nonce.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("uc-rec", "r-uc-rec"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{c}> ev:graphProtection ?n }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{c}> ev:graphProtection ?u }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{c}> ev:graphProtection ?n . BIND(UCASE(?n) AS ?u) }} }}",
            c = claim.as_str(),
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
            "origin-not-reproduced"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 2. Uppercase UUID on receipt -> refuses without writing.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("uc-op", "r-uc-op"), &propose_options()).unwrap();
        let op = p.operation.clone();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{op}> ev:graphProtection ?n }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{op}> ev:graphProtection ?u }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{op}> ev:graphProtection ?n . BIND(UCASE(?n) AS ?u) }} }}",
            op = op.as_str(),
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
            "origin-not-reproduced"
        );
        assert_eq!(ds.store().head_commit().seq, head);
    }

    // 3. Adding `ev:retainedBytes` on a fragment sub-node of a revision -> refused!
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("sub-large"), &propose_options()).unwrap();
        let rev = iri_of(&p, "rev");
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{SOURCES}> {{ <{r}#selector> ev:retainedBytes \"arbitrary\" }} }}",
            r = rev.as_str(),
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
}

#[test]
fn reauthenticate_checks_passage_and_structured_dependencies_and_duplicates() {
    // 1. Removed rendition -> reconciling passage refuses with `evidence-unavailable`.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("no-rend"), &propose_options()).unwrap();
        let rend = iri_of(&p, "rend");
        let pass = iri_of(&p, "pass");
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "DELETE WHERE {{ GRAPH <{SOURCES}> {{ <{r}> ?p ?o }} }}",
            r = rend.as_str(),
        ))
        .unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: pass,
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &opts,
                &Control::none(),
            )),
            "evidence-unavailable"
        );
    }

    // 2. Removed `ev:normalizedText` on rendition -> reconciling passage refuses `evidence-unavailable`.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("no-text"), &propose_options()).unwrap();
        let rend = iri_of(&p, "rend");
        let pass = iri_of(&p, "pass");
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}> DELETE WHERE {{ GRAPH <{SOURCES}> {{ <{r}> ev:normalizedText ?t }} }}",
            r = rend.as_str(),
        ))
        .unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: pass,
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &opts,
                &Control::none(),
            )),
            "evidence-unavailable"
        );
    }

    // 3. Duplicate rendition in another record graph -> reconciling passage refuses `immutable-record-conflict`.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("dup-rend"), &propose_options()).unwrap();
        let rend = iri_of(&p, "rend");
        let pass = iri_of(&p, "pass");
        evidence::detach(&ds, SOURCES).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{ <{r}> a ev:Rendition }} }}",
            r = rend.as_str(),
        ))
        .unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: pass,
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(SOURCES),
                &opts,
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 4. Multiple `ev:retainedBytes` on JSON revision -> reconciling structured ref refuses `immutable-record-conflict`.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("multi-bytes"), &propose_options()).unwrap();
        let rev = iri_of(&p, "rev");
        let sref = iri_of(&p, "sref");
        evidence::detach(&ds, SOURCES).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{SOURCES}> {{ <{r}> ev:retainedBytes \"e30=\"^^<http://www.w3.org/2001/XMLSchema#base64Binary> }} }}",
            r = rev.as_str(),
        ))
        .unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.records = Some(vec![ReauthenticateRecord {
            iri: sref,
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
            "immutable-record-conflict"
        );
    }

    // 5. Tampered passage quote (`oa:exact`) and selector (`oa:end`) -> refused.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &rich_batch("bad-quote"), &propose_options()).unwrap();
        let pass = iri_of(&p, "pass");
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "PREFIX oa: <http://www.w3.org/ns/oa#>
             DELETE {{ GRAPH <{SOURCES}> {{ <{p}> oa:exact ?q }} }}
             INSERT {{ GRAPH <{SOURCES}> {{ <{p}> oa:exact \"different quote\" }} }}
             WHERE {{ GRAPH <{SOURCES}> {{ <{p}> oa:exact ?q }} }}",
            p = pass.as_str(),
        ))
        .unwrap();
        configure(&ds);
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
    }

    // 6. Cross-graph duplicate candidate record -> refused.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(
            &ds,
            &small_batch("dup-cand", "r-dup-cand"),
            &propose_options(),
        )
        .unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, SOURCES).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{SOURCES}> {{ <{c}> <urn:extra> \"1\" }} }}",
            c = claim.as_str(),
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
}

#[test]
fn partial_reauth_blocks_unauthenticated_replay_in_review_withdraw_and_identity_review() {
    // 1. Multi-decision `review`: selectively re-authenticating only one decision (or one
    //    support) must prevent replaying the review idempotency key.
    {
        let ds = memory_ds();
        configure(&ds);
        let p1 = propose(&ds, &small_batch("rv1", "rrv1"), &propose_options()).unwrap();
        let p2 = propose(&ds, &small_batch("rv2", "rrv2"), &propose_options()).unwrap();
        let c1 = iri_of(&p1, "claim");
        let c2 = iri_of(&p2, "claim");
        let d1_dig = digest_of(&p1, "claim");
        let d2_dig = digest_of(&p2, "claim");

        let decisions = vec![
            DecisionInput {
                key: "d1".into(),
                target: c1,
                target_digest: d1_dig,
                disposition: Disposition::Accepted,
                supersedes: None,
            },
            DecisionInput {
                key: "d2".into(),
                target: c2,
                target_digest: d2_dig,
                disposition: Disposition::Accepted,
                supersedes: None,
            },
        ];
        let mut ropts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
        ropts.if_head = Some(ds.store().head_commit().seq);
        ropts.idempotency_key = Some("review-multi-1".into());
        let rev = evidence::review(
            None,
            &ds,
            &ds,
            "advisories",
            1,
            &decisions,
            &ropts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let dec1 = rev.decisions[0].iri.clone();
        let sup1 = rev.decisions[0].support.clone().unwrap();

        // Detach DECISIONS, re-protect, selectively reconcile ONLY dec1 (leaving dec2 unvouched).
        evidence::detach(&ds, DECISIONS).unwrap();
        configure(&ds);
        let mut sel = reauth_options();
        sel.records = Some(vec![ReauthenticateRecord {
            iri: dec1,
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(DECISIONS), &sel, &Control::none())
                .unwrap()
                .committed
        );
        // Replaying the review idempotency key MUST refuse with `immutable-record-conflict`!
        assert_eq!(
            code(evidence::review(
                None,
                &ds,
                &ds,
                "advisories",
                1,
                &decisions,
                &ropts,
                &mut |_, _| all(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );

        // Now reconcile all of DECISIONS, then detach SUPPORTS and reconcile ONLY sup1.
        assert!(
            evidence::reauthenticate(
                &ds,
                &ds,
                &iri(DECISIONS),
                &reauth_options(),
                &Control::none()
            )
            .unwrap()
            .committed
        );
        evidence::detach(&ds, SUPPORTS).unwrap();
        configure(&ds);
        let mut sel_s = reauth_options();
        sel_s.records = Some(vec![ReauthenticateRecord {
            iri: sup1,
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(SUPPORTS), &sel_s, &Control::none())
                .unwrap()
                .committed
        );
        assert_eq!(
            code(evidence::review(
                None,
                &ds,
                &ds,
                "advisories",
                1,
                &decisions,
                &ropts,
                &mut |_, _| all(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 2. Multi-withdrawal `withdraw`: selectively re-authenticating only w1 must block
    //    replaying the withdrawal idempotency key.
    {
        let ds = memory_ds();
        configure(&ds);
        let p1 = propose(&ds, &small_batch("wd1", "rwd1"), &propose_options()).unwrap();
        let p2 = propose(&ds, &small_batch("wd2", "rwd2"), &propose_options()).unwrap();
        let withdrawals = vec![
            WithdrawalInput {
                key: "w1".into(),
                target: iri_of(&p1, "claim"),
                target_digest: digest_of(&p1, "claim"),
            },
            WithdrawalInput {
                key: "w2".into(),
                target: iri_of(&p2, "claim"),
                target_digest: digest_of(&p2, "claim"),
            },
        ];
        let mut wopts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
        wopts.idempotency_key = Some("withdraw-multi-1".into());
        let w = evidence::withdraw(
            &ds,
            &ds,
            &withdrawals,
            &wopts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let w1_iri = w.withdrawals[0].iri.clone().unwrap();

        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        // Re-authenticate both target claims AND w1 (leaving ONLY w2 unvouched) so
        // `withdraw` passes target resolution, enters its replay path with an
        // authenticated operation receipt, and must refuse because w2 is unvouched.
        let mut sel = reauth_options();
        sel.records = Some(vec![
            ReauthenticateRecord {
                iri: iri_of(&p1, "claim"),
                expected_digest: None,
            },
            ReauthenticateRecord {
                iri: iri_of(&p2, "claim"),
                expected_digest: None,
            },
            ReauthenticateRecord {
                iri: w1_iri,
                expected_digest: None,
            },
        ]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &sel, &Control::none())
                .unwrap()
                .committed
        );
        assert_eq!(
            code(evidence::withdraw(
                &ds,
                &ds,
                &withdrawals,
                &wopts,
                &mut |_, _| all(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 3. Multi-decision `identity::review`: selectively re-authenticating only d1 must
    //    block replaying the identity review idempotency key.
    {
        let ds = memory_ds();
        evidence::configure(&ds, &identity_settings(), &Control::none()).unwrap();
        let batch = EvidenceInput {
            identities: vec![
                IdentityInput {
                    version: RecordVersion::V1,
                    key: key("id1"),
                    occurrence_key: key("id1"),
                    entities: vec![iri("https://example.org/p1"), iri("https://example.org/p2")],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: Some(iri("https://example.org/p1")),
                    attributed_author: iri("https://example.org/matcher"),
                    method: None,
                },
                IdentityInput {
                    version: RecordVersion::V1,
                    key: key("id2"),
                    occurrence_key: key("id2"),
                    entities: vec![iri("https://example.org/p3"), iri("https://example.org/p4")],
                    relation: IdentityRelation::PossibleMatch,
                    preferred: Some(iri("https://example.org/p3")),
                    attributed_author: iri("https://example.org/matcher"),
                    method: None,
                },
            ],
            ..EvidenceInput::default()
        };
        let mut popts = propose_options();
        popts.identity_graph = Some(iri(IDENTITY));
        let p = propose(&ds, &batch, &popts).unwrap();
        let decisions = vec![
            DecisionInput {
                key: "d1".into(),
                target: iri_of(&p, "id1"),
                target_digest: digest_of(&p, "id1"),
                disposition: Disposition::Accepted,
                supersedes: None,
            },
            DecisionInput {
                key: "d2".into(),
                target: iri_of(&p, "id2"),
                target_digest: digest_of(&p, "id2"),
                disposition: Disposition::Accepted,
                supersedes: None,
            },
        ];
        let mut ropts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
        ropts.if_head = Some(ds.store().head_commit().seq);
        ropts.idempotency_key = Some("id-review-multi-1".into());
        let rev = evidence::review(
            None,
            &ds,
            &ds,
            "people",
            1,
            &decisions,
            &ropts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let dec1 = rev.decisions[0].iri.clone();

        evidence::detach(&ds, DECISIONS).unwrap();
        evidence::configure(&ds, &identity_settings(), &Control::none()).unwrap();
        let mut sel = reauth_options();
        sel.records = Some(vec![ReauthenticateRecord {
            iri: dec1,
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(DECISIONS), &sel, &Control::none())
                .unwrap()
                .committed
        );
        assert_eq!(
            code(evidence::review(
                None,
                &ds,
                &ds,
                "people",
                1,
                &decisions,
                &ropts,
                &mut |_, _| all(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }
}

#[test]
fn reauthenticate_counts_direct_and_fragment_triples_against_max_record_triples() {
    let ds = memory_ds();
    configure(&ds);
    let p = propose(&ds, &small_batch("max-t", "r-max-t"), &propose_options()).unwrap();
    let claim = iri_of(&p, "claim");

    // Count existing direct triples on `claim` (including server-owned triples like
    // `ev:availabilityReceipt`, `prov:wasGeneratedBy`, `ev:graphProtection`, `ev:recordDigest`).
    let direct_count = {
        let snap = ds.store().snapshot();
        let prep = sparkles_core::access::Preparation::new(Control::none(), 64 << 20).unwrap();
        let mut reads = sparkles_core::evidence::write::Reads::new(&prep, &snap);
        reads.subject_triples(claim.as_str(), CLAIMS, 1000).unwrap()
    };
    // Insert fragment triples so that `direct_count + fragment_count == MAX_RECORD_TRIPLES + 1`.
    // Previously `MAX_RECORD_TRIPLES.saturating_sub(immutable_triples.len())` forgot the 4
    // excluded server-owned direct triples and let this slip through.
    let to_add = evidence::MAX_RECORD_TRIPLES - direct_count + 1;
    evidence::detach(&ds, CLAIMS).unwrap();
    {
        let mut t = ds.store().write();
        let mut labels = std::collections::HashMap::new();
        let sub = iri(&format!("{}#sourceConfidence", claim.as_str()));
        let pred = iri("https://example.org/pad");
        let gn = oxrdf::GraphName::NamedNode(iri(CLAIMS));
        for i in 0..to_add {
            let q = oxrdf::Quad::new(
                sub.clone(),
                pred.clone(),
                Literal::new_simple_literal(format!("v{i}")),
                gn.clone(),
            );
            let ids = t.encode_quad(&q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();
    }
    configure(&ds);
    assert_eq!(
        code(evidence::reauthenticate(
            &ds,
            &ds,
            &iri(CLAIMS),
            &reauth_options(),
            &Control::none(),
        )),
        "incomplete-evidence"
    );
}

#[test]
fn reauthenticate_refuses_duplicate_isomorphic_blank_expected_triple_in_both_digest_schemes() {
    // 1. Canonical digest scheme: adding a second distinct blank-node `ev:expectedTriple`
    //    with the same structure while detached must refuse atomically.
    {
        let ds = memory_ds();
        configure(&ds);
        let data_graph = iri("urn:data:dup-bnode");
        let reifier = iri("urn:reifier:dup-bnode-1");
        ds.update(&format!(
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             INSERT DATA {{ GRAPH <{g}> {{
                 _:x <https://example.org/label> \"alpha\" .
                 <{r}> rdf:reifies <<( _:x <https://example.org/label> \"alpha\" )>> .
             }} }}",
            g = data_graph.as_str(),
            r = reifier.as_str(),
        ))
        .unwrap();
        let fact_commit = ds.store().head_commit().seq;
        let caller_triple = Triple::new(
            oxrdf::BlankNode::new("caller_a").unwrap(),
            iri("https://example.org/label"),
            Literal::new_simple_literal("alpha"),
        );
        let batch = EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("bref"),
                occurrence_key: key("bref-dup-1"),
                target: EvidenceRefTarget::StoredFact(sparkles_core::evidence::StoredFactRef {
                    dataset: ds.dataset_id(),
                    branch: ds.dataset_id(),
                    commit: fact_commit,
                    graph: sparkles_core::evidence::GraphIdentity::Named(data_graph),
                    expected: caller_triple,
                    anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(reifier)),
                }),
            }],
            ..EvidenceInput::default()
        };
        let p = propose(&ds, &batch, &propose_options()).unwrap();
        let bref = iri_of(&p, "bref");

        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{b}> ev:expectedTriple <<( _:second_bnode <https://example.org/label> \"alpha\" )>> .
             }} }}",
            b = bref.as_str(),
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

    // 2. Legacy digest scheme: adding a second distinct isomorphic blank-node
    //    `ev:expectedTriple` while detached must also refuse atomically.
    {
        let ds = memory_ds();
        configure(&ds);
        let data_g = iri("urn:data:legacy-dup");
        let reif = iri("urn:reifier:legacy-dup-1");
        ds.update(&format!(
            "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             INSERT DATA {{ GRAPH <{g}> {{
                 _:s <https://example.org/p> \"v\" .
                 <{r}> rdf:reifies <<( _:s <https://example.org/p> \"v\" )>> .
             }} }}",
            g = data_g.as_str(),
            r = reif.as_str(),
        ))
        .unwrap();
        let fc = ds.store().head_commit().seq;
        let expected_triple = Triple::new(
            oxrdf::BlankNode::new("legacy_caller_dup").unwrap(),
            iri("https://example.org/p"),
            Literal::new_simple_literal("v"),
        );
        let batch = EvidenceInput {
            references: vec![EvidenceRefInput {
                version: RecordVersion::V1,
                key: key("bref"),
                occurrence_key: key("bref-legacy-dup"),
                target: EvidenceRefTarget::StoredFact(sparkles_core::evidence::StoredFactRef {
                    dataset: ds.dataset_id(),
                    branch: ds.dataset_id(),
                    commit: fc,
                    graph: sparkles_core::evidence::GraphIdentity::Named(data_g.clone()),
                    expected: expected_triple.clone(),
                    anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(reif.clone())),
                }),
            }],
            ..EvidenceInput::default()
        };
        let popts = propose_options();
        let prepared = sparkles_core::evidence::prepare_input(&batch, &Control::none()).unwrap();
        let plan = sparkles_core::evidence::write::plan(
            &prepared,
            ds.dataset_id(),
            &settings(),
            &sparkles_core::evidence::write::ProposalOptions {
                principal: &popts.principal,
                idempotency_key: None,
                source_graph: None,
                proposal_graph: None,
                identity_graph: None,
            },
            &Control::none(),
        )
        .unwrap();
        let mut legacy_plan = plan.clone();
        let legacy_triples = vec![
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
                iri(&format!("{EV}EvidenceRef")),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}datasetId")),
                Literal::new_simple_literal(ds.dataset_id().to_string()),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}branchId")),
                Literal::new_simple_literal(ds.dataset_id().to_string()),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}commit")),
                Literal::new_typed_literal(fc.to_string(), xsd::INTEGER),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}expectedTriple")),
                oxrdf::Term::Triple(Box::new(expected_triple)),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}graphKind")),
                iri(&format!("{EV}NamedGraph")),
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}graph")),
                data_g,
            ),
            Triple::new(
                legacy_plan.records[0].iri.clone(),
                iri(&format!("{EV}reifier")),
                reif,
            ),
        ];
        let legacy_digest = sparkles_core::evidence::write::legacy_record_digest(
            RecordKind::EvidenceRef,
            &legacy_triples,
            &Control::none(),
        )
        .unwrap();
        assert_ne!(legacy_digest, legacy_plan.records[0].digest);
        legacy_plan.records[0].digest = legacy_digest;

        let cap = ds
            .store()
            .managed_writer(
                sparkles::managed::OwnerKind::Evidence,
                legacy_plan.operation,
            )
            .unwrap();
        let mut t = ds
            .store()
            .try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            )
            .unwrap();
        t.set_managed(cap).unwrap();
        let (seq, time_ms) = t.managed_stamp().unwrap();
        let quads = {
            let cap = t.managed().unwrap();
            legacy_plan
                .quads(
                    &sparkles_core::evidence::write::Stamp {
                        dataset: ds.dataset_id(),
                        branch: ds.dataset_id(),
                        commit: seq,
                        time_ms,
                    },
                    |g| cap.protection(g.as_str()),
                    None,
                    &[false],
                )
                .unwrap()
        };
        let mut labels = std::collections::HashMap::new();
        for q in &quads {
            let ids = t.encode_quad(q, &mut labels).unwrap();
            t.insert(ids).unwrap();
        }
        t.commit().unwrap();

        let bref = legacy_plan.records[0].iri.clone();
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{b}> ev:expectedTriple <<( _:extra_legacy <https://example.org/p> \"v\" )>> .
             }} }}",
            b = bref.as_str(),
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

    // 3. Planting a bare blank node directly as `ev:expectedTriple` object -> refused.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("bare-b", "r-bare-b"), &propose_options()).unwrap();
        let claim = iri_of(&p, "claim");
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{c}> ev:expectedTriple _:bare .
             }} }}",
            c = claim.as_str(),
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
}

#[test]
fn reauthenticate_refuses_detached_writes_planted_on_receipt_operation() {
    // 1. Selective sibling reconciliation: attacker plants a forged `ev:Claim` with
    //    `ev:availabilityReceipt <op1>` directly onto `<op1>` while detached, then admin
    //    selectively reconciles untouched sibling `c1`. Must refuse with
    //    `immutable-record-conflict` without moving head or authenticating `<op1>`.
    {
        let ds = memory_ds();
        configure(&ds);
        let p = propose(&ds, &small_batch("rcpt-forged", "r-rf"), &propose_options()).unwrap();
        let c1 = iri_of(&p, "claim");
        let op1 = p.operation.clone();

        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             PREFIX prov: <http://www.w3.org/ns/prov#>
             PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{op}> a ev:Claim ;
                     ev:availabilityReceipt <{op}> ;
                     prov:wasGeneratedBy <{op}> ;
                     ev:recordDigest \"{d}\" ;
                     rdf:reifies <<( <urn:forged:s> <urn:forged:p> \"evil\" )>> ;
                     ev:claimKind ev:Reported ;
                     ev:polarity ev:Positive ;
                     prov:wasAttributedTo <https://example.org/evil> .
             }} }}",
            op = op1.as_str(),
            d = sha256(b"forged-claim-on-receipt"),
        ))
        .unwrap();
        configure(&ds);
        let head = ds.store().head_commit().seq;
        let mut sel = reauth_options();
        sel.records = Some(vec![ReauthenticateRecord {
            iri: c1,
            expected_digest: None,
        }]);
        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &sel,
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
        assert_eq!(ds.store().head_commit().seq, head);
        assert_eq!(
            code(resolve_record(&ds, &ds, &op1, RecordKind::Claim)),
            "origin-not-reproduced"
        );
    }

    // 2. Whole-graph mode: planting `ev:operationKind ev:Reauthentication` or tampering
    //    `ev:idempotencyKey` on a legitimate receipt while detached must refuse.
    {
        let ds = memory_ds();
        configure(&ds);
        let mut popts = propose_options();
        popts.idempotency_key = Some("rcpt-tamper-key".into());
        let p = propose(&ds, &small_batch("rcpt-tamp", "r-rt"), &popts).unwrap();
        let op1 = p.operation.clone();

        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             INSERT DATA {{ GRAPH <{CLAIMS}> {{
                 <{op}> ev:operationKind ev:Reauthentication ;
                     ev:reauthenticatedGraph <{CLAIMS}> .
             }} }}",
            op = op1.as_str(),
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

    // 3. Whole-graph mode: deleting `ev:idempotencyKey` from a receipt while detached
    //    must refuse with `immutable-record-conflict`.
    {
        let ds = memory_ds();
        configure(&ds);
        let mut popts = propose_options();
        popts.idempotency_key = Some("rcpt-del-key".into());
        let p = propose(&ds, &small_batch("rcpt-del", "r-rd"), &popts).unwrap();
        let op1 = p.operation.clone();

        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX ev: <{EV}>
             DELETE DATA {{ GRAPH <{CLAIMS}> {{
                 <{op}> ev:idempotencyKey \"rcpt-del-key\" .
             }} }}",
            op = op1.as_str(),
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
}

#[test]
fn legacy_blank_node_proposal_supports_keyed_replay_and_equal_record_reuse_after_upgrade() {
    let ds = memory_ds();
    configure(&ds);
    let data_g = iri("urn:data:legacy-upgrade");
    let reif = iri("urn:reifier:legacy-upgrade-1");
    ds.update(&format!(
        "PREFIX rdf: <http://www.w3.org/1999/02/22-rdf-syntax-ns#>
         INSERT DATA {{ GRAPH <{g}> {{
             _:s <https://example.org/p> \"v\" .
             <{r}> rdf:reifies <<( _:s <https://example.org/p> \"v\" )>> .
         }} }}",
        g = data_g.as_str(),
        r = reif.as_str(),
    ))
    .unwrap();
    let fc = ds.store().head_commit().seq;
    let caller_triple = Triple::new(
        oxrdf::BlankNode::new("pre_upgrade_caller_label").unwrap(),
        iri("https://example.org/p"),
        Literal::new_simple_literal("v"),
    );
    let batch = EvidenceInput {
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("bref"),
            occurrence_key: key("bref-upgrade-1"),
            target: EvidenceRefTarget::StoredFact(sparkles_core::evidence::StoredFactRef {
                dataset: ds.dataset_id(),
                branch: ds.dataset_id(),
                commit: fc,
                graph: sparkles_core::evidence::GraphIdentity::Named(data_g.clone()),
                expected: caller_triple.clone(),
                anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(reif.clone())),
            }),
        }],
        ..EvidenceInput::default()
    };

    // Publish the genuine old-format keyed proposal receipt & record whose `ev:recordDigest`
    // was computed by the pre-upgrade raw-label `legacy_record_digest`.
    let mut popts = propose_options();
    popts.idempotency_key = Some("legacy-bnode-key-1".into());
    let prepared = sparkles_core::evidence::prepare_input(&batch, &Control::none()).unwrap();
    let mut legacy_plan = sparkles_core::evidence::write::plan(
        &prepared,
        ds.dataset_id(),
        &settings(),
        &sparkles_core::evidence::write::ProposalOptions {
            principal: &popts.principal,
            idempotency_key: popts.idempotency_key.as_deref(),
            source_graph: None,
            proposal_graph: None,
            identity_graph: None,
        },
        &Control::none(),
    )
    .unwrap();
    let bref_iri = legacy_plan.records[0].iri.clone();
    let legacy_triples = vec![
        Triple::new(
            bref_iri.clone(),
            iri("http://www.w3.org/1999/02/22-rdf-syntax-ns#type"),
            iri(&format!("{EV}EvidenceRef")),
        ),
        Triple::new(
            bref_iri.clone(),
            iri(&format!("{EV}datasetId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()),
        ),
        Triple::new(
            bref_iri.clone(),
            iri(&format!("{EV}branchId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()),
        ),
        Triple::new(
            bref_iri.clone(),
            iri(&format!("{EV}commit")),
            Literal::new_typed_literal(fc.to_string(), xsd::INTEGER),
        ),
        Triple::new(
            bref_iri.clone(),
            iri(&format!("{EV}expectedTriple")),
            oxrdf::Term::Triple(Box::new(caller_triple)),
        ),
        Triple::new(
            bref_iri.clone(),
            iri(&format!("{EV}graphKind")),
            iri(&format!("{EV}NamedGraph")),
        ),
        Triple::new(bref_iri.clone(), iri(&format!("{EV}graph")), data_g.clone()),
        Triple::new(bref_iri.clone(), iri(&format!("{EV}reifier")), reif.clone()),
    ];
    let legacy_digest = sparkles_core::evidence::write::legacy_record_digest(
        RecordKind::EvidenceRef,
        &legacy_triples,
        &Control::none(),
    )
    .unwrap();
    assert_ne!(legacy_digest, legacy_plan.records[0].digest);
    legacy_plan.records[0].digest = legacy_digest;

    let cap = ds
        .store()
        .managed_writer(
            sparkles::managed::OwnerKind::Evidence,
            legacy_plan.operation,
        )
        .unwrap();
    let mut t = ds
        .store()
        .try_write_with(
            sparkles_core::commit::CommitKind::Transaction,
            Default::default(),
        )
        .unwrap();
    t.set_managed(cap).unwrap();
    let (seq, time_ms) = t.managed_stamp().unwrap();
    let quads = {
        let cap = t.managed().unwrap();
        legacy_plan
            .quads(
                &sparkles_core::evidence::write::Stamp {
                    dataset: ds.dataset_id(),
                    branch: ds.dataset_id(),
                    commit: seq,
                    time_ms,
                },
                |g| cap.protection(g.as_str()),
                Some(legacy_plan.operation),
                &[false],
            )
            .unwrap()
    };
    let mut labels = std::collections::HashMap::new();
    for q in &quads {
        let ids = t.encode_quad(q, &mut labels).unwrap();
        t.insert(ids).unwrap();
    }
    t.commit().unwrap();

    // 1. Keyed replay BEFORE any detach succeeds and returns the recorded legacy digest.
    let replayed = propose(&ds, &batch, &popts).unwrap();
    assert!(replayed.replayed);
    assert!(!replayed.committed);
    assert_eq!(replayed.records[0].iri, bref_iri);
    assert_eq!(replayed.records[0].digest, legacy_digest);

    // 2. Equal-record reuse via a new proposal (different key) succeeds and returns the
    //    recorded legacy digest with `existing: true`.
    let mut reuse_opts = propose_options();
    reuse_opts.idempotency_key = Some("legacy-bnode-reuse-1".into());
    let reused = propose(&ds, &batch, &reuse_opts).unwrap();
    assert!(!reused.replayed);
    assert!(reused.records[0].existing);
    assert_eq!(reused.records[0].iri, bref_iri);
    assert_eq!(reused.records[0].digest, legacy_digest);

    // 3. Different blank-node label that does not reproduce the legacy digest is refused.
    let mut other_batch = batch.clone();
    other_batch.references[0].target =
        EvidenceRefTarget::StoredFact(sparkles_core::evidence::StoredFactRef {
            dataset: ds.dataset_id(),
            branch: ds.dataset_id(),
            commit: fc,
            graph: sparkles_core::evidence::GraphIdentity::Named(data_g),
            expected: Triple::new(
                oxrdf::BlankNode::new("different_caller_label").unwrap(),
                iri("https://example.org/p"),
                Literal::new_simple_literal("v"),
            ),
            anchor: Some(sparkles_core::evidence::BlankAnchor::Reifier(reif)),
        });
    assert_eq!(
        code(propose(&ds, &other_batch, &propose_options())),
        "immutable-record-conflict"
    );

    // 4. After detach, re-protect, and re-authentication (which preserves `legacy_digest`),
    //    both keyed replay and equal-record reuse continue to succeed with `legacy_digest`.
    evidence::detach(&ds, CLAIMS).unwrap();
    configure(&ds);
    let rec = evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
        .unwrap();
    assert!(rec.committed);
    assert_eq!(rec.records[0].digest, legacy_digest);

    let replayed_after = propose(&ds, &batch, &popts).unwrap();
    assert!(replayed_after.replayed);
    assert_eq!(replayed_after.records[0].digest, legacy_digest);

    let mut reuse_after_opts = propose_options();
    reuse_after_opts.idempotency_key = Some("legacy-bnode-reuse-2".into());
    let reused_after = propose(&ds, &batch, &reuse_after_opts).unwrap();
    assert!(reused_after.records[0].existing);
    assert_eq!(reused_after.records[0].digest, legacy_digest);
}

#[test]
fn reauthenticate_resolves_non_main_parent_record_and_audit_at_fork_boundary_and_rejects_unrelated_branch()
 {
    // 1. Parent branch `b1` (forked from `main`) publishes a claim at `seq`, then `CLAIMS`
    //    is detached and re-protected while `b1` stays at `seq`. Child `b2` forks from `b1`
    //    at `Head` (`from = (b1, seq)`), and nested `b3` forks from `b2` at `Head`.
    //    Re-authenticating on `b2` and `b3` must resolve `b1`'s original publication view!
    {
        let ds = memory_ds();
        configure(&ds);
        ds.create_branch("b1", &Default::default()).unwrap();
        let b1 = ds.branch("b1").unwrap();
        let p = evidence::propose(
            None,
            &ds,
            &b1,
            &small_batch("b1-claim", "r-b1"),
            &propose_options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let claim = iri_of(&p, "claim");

        // Detach and re-protect while b1 is STILL at the publication commit `seq`.
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);

        // Fork b2 from b1 at Head (exact publication boundary), and b3 from b2 at Head.
        ds.create_branch(
            "b2",
            &sparkles::branch::BranchOptions {
                from: "b1".into(),
                ..Default::default()
            },
        )
        .unwrap();
        ds.create_branch(
            "b3",
            &sparkles::branch::BranchOptions {
                from: "b2".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let b2 = ds.branch("b2").unwrap();
        let b3 = ds.branch("b3").unwrap();

        let res2 =
            evidence::reauthenticate(&ds, &b2, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(res2.committed);
        assert_eq!(res2.records.len(), 1);
        assert_eq!(res2.records[0].iri, claim);
        assert!(resolve_record(&ds, &b2, &claim, RecordKind::Claim).is_ok());

        let res3 =
            evidence::reauthenticate(&ds, &b3, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(res3.committed);
        assert_eq!(res3.records.len(), 1);
        assert_eq!(res3.records[0].iri, claim);
        assert!(resolve_record(&ds, &b3, &claim, RecordKind::Claim).is_ok());
    }

    // 2. Parent-authored reconciliation audit inherited at the exact fork boundary after
    //    a subsequent detach/re-protect.
    {
        let ds = memory_ds();
        configure(&ds);
        ds.create_branch("b1", &Default::default()).unwrap();
        let b1 = ds.branch("b1").unwrap();
        let p = evidence::propose(
            None,
            &ds,
            &b1,
            &small_batch("b1-audit", "r-b1-a"),
            &propose_options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let claim = iri_of(&p, "claim");

        // Detach #1 -> N2, reconcile on b1 (creates audit commit A on b1 under N2).
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let r1 =
            evidence::reauthenticate(&ds, &b1, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(r1.committed);

        // Detach #2 -> N3 while b1 is STILL at audit commit A, then fork b2 from b1 at Head.
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        ds.create_branch(
            "b2",
            &sparkles::branch::BranchOptions {
                from: "b1".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let b2 = ds.branch("b2").unwrap();

        // Re-authenticating on b2 under N3 must trace b1's audit commit A at the exact fork
        // boundary and succeed!
        let r2 =
            evidence::reauthenticate(&ds, &b2, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(r2.committed);
        assert_eq!(r2.records[0].iri, claim);
        assert!(resolve_record(&ds, &b2, &claim, RecordKind::Claim).is_ok());
    }

    // 3. Same-sequence commit on an unrelated branch must be refused as
    //    `origin-not-reproduced` (ancestry is proven from the branch table, never a
    //    `(timestamp_ms, quads)` heuristic).
    {
        let ds = memory_ds();
        configure(&ds);
        ds.create_branch("branch-a", &Default::default()).unwrap();
        ds.create_branch("branch-b", &Default::default()).unwrap();
        let ba = ds.branch("branch-a").unwrap();
        let bb = ds.branch("branch-b").unwrap();

        // Publish a genuine claim on branch-a at commit seq S.
        let pa = evidence::propose(
            None,
            &ds,
            &ba,
            &small_batch("same-seq", "r-same-seq"),
            &propose_options(),
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let claim_a = iri_of(&pa, "claim");
        let op_a = pa.operation.clone();

        // Copy the exact triples of `claim_a` and `op_a` onto `branch-b` while detached at
        // the SAME commit sequence number `S` (so `branch-b` has a commit at `S`, and
        // `branch-a` has a protected commit at `S`, yet `branch-a` is NOT an ancestor of
        // `branch-b`!).
        let copied_quads: Vec<oxrdf::Quad> = {
            let mut out = Vec::new();
            let snap = ba.store().snapshot();
            let prep = sparkles_core::access::Preparation::new(Control::none(), 16 << 20).unwrap();
            let mut reads = sparkles_core::evidence::write::Reads::new(&prep, &snap);
            for subj in [claim_a.as_str(), op_a.as_str()] {
                let (objs, _) = reads
                    .objects_up_to(
                        subj,
                        "http://www.w3.org/1999/02/22-rdf-syntax-ns#type",
                        CLAIMS,
                        10,
                    )
                    .unwrap();
                assert!(!objs.is_empty());
            }
            // Extract all quads of CLAIMS from ba using a SPARQL CONSTRUCT query.
            let res = ba
                .query(&format!(
                    "CONSTRUCT {{ ?s ?p ?o }} WHERE {{ GRAPH <{CLAIMS}> {{ ?s ?p ?o }} }}"
                ))
                .unwrap();
            for tr in res.triples() {
                out.push(oxrdf::Quad::new(
                    tr.subject.clone(),
                    tr.predicate.clone(),
                    tr.object.clone(),
                    oxrdf::GraphName::NamedNode(iri(CLAIMS)),
                ));
            }
            out
        };
        evidence::detach(&ds, CLAIMS).unwrap();
        {
            let mut t = bb.store().write();
            let mut labels = std::collections::HashMap::new();
            for q in &copied_quads {
                let ids = t.encode_quad(q, &mut labels).unwrap();
                t.insert(ids).unwrap();
            }
            t.commit().unwrap();
        }
        assert_eq!(
            bb.store().head_commit().seq,
            ba.store().head_commit().seq,
            "branch-b and branch-a must be at the exact same commit sequence"
        );
        configure(&ds);

        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &bb,
                &iri(CLAIMS),
                &reauth_options(),
                &Control::none(),
            )),
            "origin-not-reproduced"
        );
    }
}

#[test]
fn review_replay_refuses_missing_sibling_support_after_detach_and_allows_non_projected() {
    // 1. Multi-support keyed review: delete `sup2` while `SUPPORTS` is detached, then
    //    re-protect and selectively reconcile `sup1` (moving the shared receipt to
    //    `current_nonce`). Replay MUST refuse with `immutable-record-conflict`, NOT
    //    silently return `support: None` for `d2`!
    {
        let ds = memory_ds();
        configure(&ds);
        let p1 = propose(&ds, &small_batch("ms1", "rms1"), &propose_options()).unwrap();
        let p2 = propose(&ds, &small_batch("ms2", "rms2"), &propose_options()).unwrap();
        let decisions = vec![
            DecisionInput {
                key: "d1".into(),
                target: iri_of(&p1, "claim"),
                target_digest: digest_of(&p1, "claim"),
                disposition: Disposition::Accepted,
                supersedes: None,
            },
            DecisionInput {
                key: "d2".into(),
                target: iri_of(&p2, "claim"),
                target_digest: digest_of(&p2, "claim"),
                disposition: Disposition::Accepted,
                supersedes: None,
            },
        ];
        let mut ropts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
        ropts.if_head = Some(ds.store().head_commit().seq);
        ropts.idempotency_key = Some("review-missing-sup-1".into());
        let rev = evidence::review(
            None,
            &ds,
            &ds,
            "advisories",
            1,
            &decisions,
            &ropts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let sup1 = rev.decisions[0].support.clone().unwrap();
        let sup2 = rev.decisions[1].support.clone().unwrap();

        evidence::detach(&ds, SUPPORTS).unwrap();
        ds.update(&format!(
            "DELETE WHERE {{ GRAPH <{SUPPORTS}> {{ <{s2}> ?p ?o }} }}",
            s2 = sup2.as_str(),
        ))
        .unwrap();
        configure(&ds);

        let mut sel = reauth_options();
        sel.records = Some(vec![ReauthenticateRecord {
            iri: sup1,
            expected_digest: None,
        }]);
        assert!(
            evidence::reauthenticate(&ds, &ds, &iri(SUPPORTS), &sel, &Control::none())
                .unwrap()
                .committed
        );
        assert_eq!(
            code(evidence::review(
                None,
                &ds,
                &ds,
                "advisories",
                1,
                &decisions,
                &ropts,
                &mut |_, _| all(),
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 2. Keyed review mixing an accepted projected claim and a rejected claim (no support
    //    originally produced) still replays cleanly with `support: None` for the rejected one.
    {
        let ds = memory_ds();
        configure(&ds);
        let p1 = propose(&ds, &small_batch("mix1", "rmix1"), &propose_options()).unwrap();
        let p2 = propose(&ds, &small_batch("mix2", "rmix2"), &propose_options()).unwrap();
        let decisions = vec![
            DecisionInput {
                key: "d1".into(),
                target: iri_of(&p1, "claim"),
                target_digest: digest_of(&p1, "claim"),
                disposition: Disposition::Accepted,
                supersedes: None,
            },
            DecisionInput {
                key: "d2".into(),
                target: iri_of(&p2, "claim"),
                target_digest: digest_of(&p2, "claim"),
                disposition: Disposition::Rejected,
                supersedes: None,
            },
        ];
        let mut ropts = ReviewOptions::new(iri("urn:x-sparkles:principal:reviewer"));
        ropts.if_head = Some(ds.store().head_commit().seq);
        ropts.idempotency_key = Some("review-mixed-proj-1".into());
        let first = evidence::review(
            None,
            &ds,
            &ds,
            "advisories",
            1,
            &decisions,
            &ropts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        let replay = evidence::review(
            None,
            &ds,
            &ds,
            "advisories",
            1,
            &decisions,
            &ropts,
            &mut |_, _| all(),
            &Control::none(),
        )
        .unwrap();
        assert!(replay.replayed);
        assert_eq!(replay.decisions[0].support, first.decisions[0].support);
        assert!(replay.decisions[0].support.is_some());
        assert_eq!(replay.decisions[1].support, None);
    }
}

#[test]
fn reauthenticate_replay_and_lineage_enforce_full_audit_identity() {
    // 1. Replay refuses an audit operation that also has untyped triples in another
    //    record graph.
    {
        let ds = memory_ds();
        configure(&ds);
        let _p = propose(&ds, &small_batch("aud-cg", "r-acg"), &propose_options()).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.idempotency_key = Some("audit-cross-graph-1".into());
        let first =
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts, &Control::none()).unwrap();
        assert!(first.committed);

        // Detach SOURCES (leaving CLAIMS's protection intact!) and plant an untyped
        // triple on `first.operation` in SOURCES.
        evidence::detach(&ds, SOURCES).unwrap();
        ds.update(&format!(
            "INSERT DATA {{ GRAPH <{SOURCES}> {{ <{op}> <urn:extra> \"1\" }} }}",
            op = first.operation.as_str(),
        ))
        .unwrap();
        configure(&ds);

        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &opts,
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 2. Replay refuses an audit operation with a missing `ev:branchId` (does not fall
    //    back to the requested branch).
    {
        let ds = memory_ds();
        configure(&ds);
        let _p = propose(&ds, &small_batch("aud-br", "r-abr"), &propose_options()).unwrap();
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let mut opts = reauth_options();
        opts.idempotency_key = Some("audit-branch-1".into());
        let first =
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &opts, &Control::none()).unwrap();
        assert!(first.committed);

        // Remove `ev:branchId` from the audit operation using a managed write under the
        // current protection so the operation still carries `current_nonce`.
        let cap = ds
            .store()
            .managed_writer(sparkles::managed::OwnerKind::Evidence, uuid::Uuid::new_v4())
            .unwrap();
        let mut t = ds
            .store()
            .try_write_with(
                sparkles_core::commit::CommitKind::Transaction,
                Default::default(),
            )
            .unwrap();
        t.set_managed(cap).unwrap();
        let q = oxrdf::Quad::new(
            first.operation.clone(),
            iri(&format!("{EV}branchId")),
            Literal::new_simple_literal(ds.dataset_id().to_string()),
            oxrdf::GraphName::NamedNode(iri(CLAIMS)),
        );
        let mut labels = std::collections::HashMap::new();
        let ids = t.encode_quad(&q, &mut labels).unwrap();
        t.delete(ids).unwrap();
        t.commit().unwrap();

        assert_eq!(
            code(evidence::reauthenticate(
                &ds,
                &ds,
                &iri(CLAIMS),
                &opts,
                &Control::none(),
            )),
            "immutable-record-conflict"
        );
    }

    // 3. Multi-detach lineage refuses an audit operation whose `prov:wasAssociatedWith`
    //    was altered while detached.
    {
        let ds = memory_ds();
        configure(&ds);
        let _p = propose(&ds, &small_batch("aud-lin", "r-alin"), &propose_options()).unwrap();
        // Detach #1 -> N2, reconcile under N2.
        evidence::detach(&ds, CLAIMS).unwrap();
        configure(&ds);
        let r1 =
            evidence::reauthenticate(&ds, &ds, &iri(CLAIMS), &reauth_options(), &Control::none())
                .unwrap();
        assert!(r1.committed);

        // Detach #2 -> alter `prov:wasAssociatedWith` on the N2 audit op, then re-protect N3.
        evidence::detach(&ds, CLAIMS).unwrap();
        ds.update(&format!(
            "PREFIX prov: <http://www.w3.org/ns/prov#>
             DELETE {{ GRAPH <{CLAIMS}> {{ <{op}> prov:wasAssociatedWith ?a }} }}
             INSERT {{ GRAPH <{CLAIMS}> {{ <{op}> prov:wasAssociatedWith <urn:x-sparkles:principal:tampered> }} }}
             WHERE {{ GRAPH <{CLAIMS}> {{ <{op}> prov:wasAssociatedWith ?a }} }}",
            op = r1.operation.as_str(),
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
            "origin-not-reproduced"
        );
    }
}
