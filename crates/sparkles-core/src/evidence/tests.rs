use super::*;
use crate::{Error, task::Control};
use oxrdf::{BlankNode, Literal, NamedNode, Term, Triple, vocab::xsd};
use std::collections::BTreeMap;
use uuid::Uuid;

const TEXT: &str = "Advisory: Café release 1.0 is affected.\n";
fn key(value: &str) -> RecordKey {
    RecordKey::new(value).unwrap()
}
fn node(value: &str) -> NamedNode {
    NamedNode::new(value).unwrap()
}
fn local(value: &str) -> RecordRef {
    RecordRef::Local(key(value))
}
fn digest(value: &[u8]) -> Sha256Digest {
    Sha256Digest::of_bytes(value, &Control::none()).unwrap()
}
fn method() -> MethodRef {
    MethodRef {
        method: node("urn:identity-conversion"),
        version: key("1"),
    }
}
fn timestamp(value: &str) -> Literal {
    Literal::new_typed_literal(value, xsd::DATE_TIME)
}

fn sample() -> EvidenceInput {
    EvidenceInput {
        revisions: vec![SourceRevisionInput {
            version: RecordVersion::V1,
            key: key("revision"),
            source: node("urn:source"),
            source_key: key("bulletin/1"),
            upstream_version: None,
            content: SourceContent::Retained {
                bytes: TEXT.as_bytes().to_vec(),
                expected_digest: None,
            },
            metadata: SourceMetadata::default(),
            times: SourceTimes {
                published_at: Some(timestamp("2026-01-01T12:00:00+02:00")),
                reported_available_at: None,
            },
            lineage: SourceLineage::default(),
        }],
        renditions: vec![RenditionInput {
            version: RecordVersion::V1,
            key: key("text"),
            revision: local("revision"),
            occurrence_key: key("text/1"),
            conversion: method(),
            media_type: "text/plain".into(),
            language: Some("en".into()),
            content: RenditionContent::NormalizedText(TEXT.into()),
            expected_digest: None,
        }],
        passages: vec![PassageInput {
            version: RecordVersion::V1,
            key: key("passage"),
            revision: local("revision"),
            rendition: local("text"),
            occurrence_key: key("passage/1"),
            start: 10,
            end: 14,
            text_digest: digest("Café".as_bytes()),
            quote: Some("Café".into()),
        }],
        references: vec![EvidenceRefInput {
            version: RecordVersion::V1,
            key: key("reference"),
            occurrence_key: key("reference/1"),
            target: EvidenceRefTarget::Passage {
                revision: local("revision"),
                rendition: local("text"),
                passage: local("passage"),
            },
        }],
        claims: vec![ClaimInput {
            version: RecordVersion::V1,
            key: key("claim"),
            occurrence_key: key("claim/1"),
            revision: local("revision"),
            proposition: Triple::new(
                node("urn:release"),
                node("urn:affectedBy"),
                node("urn:issue"),
            ),
            kind: ClaimKind::Reported,
            polarity: Polarity::Positive,
            attributed_author: node("urn:publisher"),
            scope: BTreeMap::from([("urn:configuration".into(), vec![node("urn:linux").into()])]),
            confidence: ConfidenceFacets::default(),
        }],
        links: vec![EvidenceLinkInput {
            version: RecordVersion::V1,
            key: key("link"),
            occurrence_key: key("link/1"),
            target: local("claim"),
            evidence: local("reference"),
            relation: EvidenceRelation::Supports,
            attributed_author: node("urn:importer"),
        }],
        assessments: vec![AssessmentInput {
            version: RecordVersion::V1,
            key: key("assessment"),
            occurrence_key: key("assessment/1"),
            target: local("claim"),
            conclusion: Literal::new_simple_literal("needs review").into(),
            method: method(),
            configuration_digest: digest(b"{}"),
            evaluated_at: timestamp("2026-01-01T12:05:00+02:00"),
            attributed_evaluator: node("urn:analyst"),
            confidence: None,
            scope: Scope::default(),
        }],
    }
}

#[test]
fn local_sources_unicode_passages_and_assessments_prepare_without_publication() {
    let input = sample();
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(
        prepared.content()["revision"].digest,
        digest(TEXT.as_bytes())
    );
    assert!(prepared.content()["revision"].retained);
    assert_eq!(prepared.content()["text"].digest, digest(TEXT.as_bytes()));
    assert!(prepared.requirements().is_empty());
    let published = prepared.normalized_times()["revision"]
        .published_at
        .as_ref()
        .unwrap();
    assert_eq!(published.original.value(), "2026-01-01T12:00:00+02:00");
    assert_eq!(published.utc.value(), "2026-01-01T10:00:00Z");
    assert_eq!(
        prepared.assessment_times()["assessment"].utc.value(),
        "2026-01-01T10:05:00Z"
    );
    assert_eq!(
        prepared.input().claims[0].proposition,
        input.claims[0].proposition
    );
    assert_eq!(
        prepared.body_digest(),
        prepare_input(&input, &Control::none())
            .unwrap()
            .body_digest()
    );
}

#[test]
fn reported_availability_does_not_become_publication_or_local_availability() {
    let input = sample();
    let original = prepare_input(&input, &Control::none()).unwrap();
    let mut changed = input.clone();
    changed.revisions[0].times.reported_available_at =
        changed.revisions[0].times.published_at.take();
    let changed = prepare_input(&changed, &Control::none()).unwrap();
    assert!(
        changed.normalized_times()["revision"]
            .published_at
            .is_none()
    );
    assert!(
        changed.normalized_times()["revision"]
            .reported_available_at
            .is_some()
    );
    assert_ne!(changed.body_digest(), original.body_digest());
}

#[test]
fn content_digests_and_metadata_only_sources_keep_their_distinction() {
    let input = sample();
    let original = prepare_input(&input, &Control::none())
        .unwrap()
        .body_digest();
    let mut mismatch = input.clone();
    if let SourceContent::Retained {
        expected_digest, ..
    } = &mut mismatch.revisions[0].content
    {
        *expected_digest = Some(digest(b"different"));
    }
    assert!(prepare_input(&mismatch, &Control::none()).is_err());
    let mut metadata = input;
    metadata.revisions[0].content = SourceContent::MetadataOnly {
        digest: digest(TEXT.as_bytes()),
    };
    let prepared = prepare_input(&metadata, &Control::none()).unwrap();
    assert!(!prepared.content()["revision"].retained);
    assert_ne!(prepared.body_digest(), original);
}

#[test]
fn passage_validation_uses_exact_code_points_quote_and_selected_bytes() {
    for (start, end) in [(10, 15), (10, 10), (10, 1000)] {
        let mut input = sample();
        input.passages[0].start = start;
        input.passages[0].end = end;
        assert!(
            prepare_input(&input, &Control::none()).is_err(),
            "{start}..{end}"
        );
    }
    let mut input = sample();
    input.passages[0].quote = Some("Cafe".into());
    assert!(prepare_input(&input, &Control::none()).is_err());
    input.passages[0].quote = None;
    assert!(prepare_input(&input, &Control::none()).is_ok());
    input.passages[0].text_digest = digest(b"wrong");
    assert!(prepare_input(&input, &Control::none()).is_err());
}

#[test]
fn batch_aliases_and_record_kinds_cannot_be_substituted() {
    let mut input = sample();
    input.claims[0].key = key("revision");
    assert!(prepare_input(&input, &Control::none()).is_err());
    let mut input = sample();
    input.claims[0].revision = local("text");
    assert!(prepare_input(&input, &Control::none()).is_err());
    input.claims[0].revision = local("absent");
    assert!(prepare_input(&input, &Control::none()).is_err());
}

#[test]
fn existing_records_stay_explicit_resolution_requirements() {
    let mut input = sample();
    input.claims[0].revision = RecordRef::Existing(node("urn:existing-revision"));
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(
        prepared.requirements(),
        &[ResolutionRequirement::ExistingRecord {
            record: node("urn:existing-revision"),
            expected_kinds: vec![RecordKind::SourceRevision]
        }]
    );
}

#[test]
fn passage_identity_and_retained_text_requirements_are_never_claimed_verified() {
    let mut input = sample();
    input.passages[0].revision = RecordRef::Existing(node("urn:other-revision"));
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert!(
        prepared
            .requirements()
            .iter()
            .any(|r| matches!(r, ResolutionRequirement::PassageIdentity { .. }))
    );
    let mut input = sample();
    if let EvidenceRefTarget::Passage { revision, .. } = &mut input.references[0].target {
        *revision = RecordRef::Existing(node("urn:other-revision"));
    }
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert!(
        prepared
            .requirements()
            .iter()
            .any(|r| matches!(r, ResolutionRequirement::PassageReferenceIdentity { .. }))
    );
    let mut input = sample();
    input.renditions[0].content = RenditionContent::MetadataOnly(digest(TEXT.as_bytes()));
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(
        prepared.requirements(),
        &[ResolutionRequirement::RetainedPassage {
            passage_key: "passage".into()
        }]
    );
}

fn stored_ref() -> EvidenceRefTarget {
    EvidenceRefTarget::StoredFact(StoredFactRef {
        dataset: Uuid::parse_str("12345678-1234-4234-8234-123456789abc").unwrap(),
        branch: Uuid::parse_str("abcdef01-2345-4678-8123-abcdef012345").unwrap(),
        commit: 7,
        graph: GraphIdentity::Default,
        expected: Triple::new(node("urn:s"), node("urn:p"), node("urn:o")),
        anchor: None,
    })
}

#[test]
fn stored_fact_coordinates_and_named_default_term_remain_unresolved_and_distinct() {
    let mut input = sample();
    input.references[0].target = stored_ref();
    let first = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(
        first.requirements(),
        &[ResolutionRequirement::StoredFact {
            reference_key: "reference".into()
        }]
    );
    let first = first.body_digest();
    if let EvidenceRefTarget::StoredFact(fact) = &mut input.references[0].target {
        fact.graph = GraphIdentity::Named(node("urn:x-sparkles:evidence:DefaultGraph"));
    }
    let named = prepare_input(&input, &Control::none())
        .unwrap()
        .body_digest();
    assert_ne!(first, named);
    if let EvidenceRefTarget::StoredFact(fact) = &mut input.references[0].target {
        fact.commit += 1;
    }
    let next_commit = prepare_input(&input, &Control::none())
        .unwrap()
        .body_digest();
    assert_ne!(named, next_commit);
    if let EvidenceRefTarget::StoredFact(fact) = &mut input.references[0].target {
        fact.branch = Uuid::parse_str("abcdef01-2345-4678-8123-abcdef012346").unwrap();
    }
    assert_ne!(
        next_commit,
        prepare_input(&input, &Control::none())
            .unwrap()
            .body_digest()
    );
}

#[test]
fn blank_fact_labels_require_an_anchor_but_preparation_does_not_verify_it() {
    let mut input = sample();
    input.references[0].target = stored_ref();
    if let EvidenceRefTarget::StoredFact(fact) = &mut input.references[0].target {
        fact.expected.subject = BlankNode::new("local").unwrap().into();
    }
    assert!(prepare_input(&input, &Control::none()).is_err());
    if let EvidenceRefTarget::StoredFact(fact) = &mut input.references[0].target {
        fact.anchor = Some(BlankAnchor::Reifier(node("urn:declared-anchor")));
    }
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(prepared.requirements().len(), 1);
}

#[test]
fn structured_pointer_syntax_and_normalization_are_separate_from_verification() {
    let mut input = sample();
    input.references[0].target = EvidenceRefTarget::Structured {
        revision: local("revision"),
        pointer: "/a~1b/m~0n".into(),
        method: method(),
        value_digest: digest(b"value"),
    };
    assert_eq!(
        prepare_input(&input, &Control::none())
            .unwrap()
            .requirements(),
        &[ResolutionRequirement::StructuredValue {
            reference_key: "reference".into()
        }]
    );
    for pointer in ["not/a/pointer", "/bad~2escape", "/bad~"] {
        if let EvidenceRefTarget::Structured { pointer: field, .. } =
            &mut input.references[0].target
        {
            *field = pointer.into();
        }
        assert!(
            prepare_input(&input, &Control::none()).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn open_scope_cannot_inject_profile_or_authenticated_creation_fields() {
    for predicate in [
        "urn:x-sparkles:evidence:firstAvailableAt",
        "http://www.w3.org/ns/prov#wasGeneratedBy",
        "http://www.w3.org/ns/prov#wasInvalidatedBy",
        "http://www.w3.org/1999/02/22-rdf-syntax-ns#reifies",
    ] {
        let mut input = sample();
        input.claims[0]
            .scope
            .insert(predicate.into(), vec![node("urn:forged").into()]);
        assert!(
            prepare_input(&input, &Control::none()).is_err(),
            "{predicate}"
        );
    }
}

#[test]
fn duplicate_propositions_keep_occurrences_and_precise_qualified_values() {
    let mut input = sample();
    let mut second = input.claims[0].clone();
    second.key = key("claim-again");
    second.occurrence_key = key("claim/2");
    input.claims.push(second);
    input.claims[0].confidence.analyst = Some(Confidence {
        scale: node("urn:x-sparkles:evidence:ZeroToOne"),
        value: Literal::new_typed_literal("0.75000000000000000001", xsd::DECIMAL),
        evaluator: node("urn:analyst"),
        method: None,
    });
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(prepared.input().claims.len(), 2);
    let first = prepared.body_digest();
    input.claims[0].confidence.analyst.as_mut().unwrap().value =
        Literal::new_typed_literal("0.75", xsd::DECIMAL);
    assert_ne!(
        first,
        prepare_input(&input, &Control::none())
            .unwrap()
            .body_digest()
    );
}

#[test]
fn source_times_require_explicit_offsets_without_inventing_midnight_or_utc() {
    for value in ["2026-01-01T10:00:00", "2026-02-30T10:00:00Z", "not a date"] {
        let mut input = sample();
        input.revisions[0].times.published_at = Some(timestamp(value));
        assert!(prepare_input(&input, &Control::none()).is_err(), "{value}");
    }
}

#[test]
fn batch_limits_and_cancellation_fail_before_any_runtime_resolution() {
    let mut input = sample();
    for index in 1..=MAX_CLAIMS {
        let mut claim = input.claims[0].clone();
        claim.key = key(&format!("claim-{index}"));
        claim.occurrence_key = key(&format!("claim/{index}"));
        input.claims.push(claim);
    }
    assert!(prepare_input(&input, &Control::none()).is_err());
    input.claims.pop();
    assert!(prepare_input(&input, &Control::none()).is_ok());
    let mut input = sample();
    for index in 1..=MAX_LINKS {
        let mut link = input.links[0].clone();
        link.key = key(&format!("link-{index}"));
        link.occurrence_key = key(&format!("link/{index}"));
        input.links.push(link);
    }
    assert!(prepare_input(&input, &Control::none()).is_err());
    input.links.pop();
    assert!(prepare_input(&input, &Control::none()).is_ok());
    let mut input = sample();
    input.revisions[0].content = SourceContent::Retained {
        bytes: vec![0; RecordKey::MAX_BYTES],
        expected_digest: None,
    };
    assert!(matches!(
        prepare_input(&input, &Control::none()),
        Err(Error::BudgetExceeded(_))
    ));
    let control = Control::none();
    control.cancel.cancel();
    assert!(matches!(
        prepare_input(&sample(), &control),
        Err(Error::Cancelled)
    ));
}

#[test]
fn scope_object_order_is_canonical_but_values_remain_exact_rdf_terms() {
    let mut first = sample();
    first.claims[0].scope.insert(
        "urn:z".into(),
        vec![Literal::new_simple_literal("urn:last").into()],
    );
    let mut second = sample();
    let scope = &mut second.claims[0].scope;
    let original = std::mem::take(scope);
    scope.insert(
        "urn:z".into(),
        vec![Literal::new_simple_literal("urn:last").into()],
    );
    scope.extend(original);
    assert_eq!(
        prepare_input(&first, &Control::none())
            .unwrap()
            .body_digest(),
        prepare_input(&second, &Control::none())
            .unwrap()
            .body_digest()
    );
    second.claims[0]
        .scope
        .insert("urn:z".into(), vec![Term::NamedNode(node("urn:last"))]);
    assert_ne!(
        prepare_input(&first, &Control::none())
            .unwrap()
            .body_digest(),
        prepare_input(&second, &Control::none())
            .unwrap()
            .body_digest()
    );
}

#[test]
fn locators_and_reported_rights_are_bound_as_data_without_fetch_or_authority() {
    let input = sample();
    let original = prepare_input(&input, &Control::none())
        .unwrap()
        .body_digest();
    let mut metadata = input.clone();
    metadata.revisions[0]
        .metadata
        .locators
        .push(node("https://example.org/source"));
    let located = prepare_input(&metadata, &Control::none())
        .unwrap()
        .body_digest();
    assert_ne!(original, located);
    metadata.revisions[0]
        .metadata
        .reported_rights
        .push(Literal::new_simple_literal("reported redistribution terms").into());
    assert_ne!(
        located,
        prepare_input(&metadata, &Control::none())
            .unwrap()
            .body_digest()
    );
    assert!(
        prepare_input(&metadata, &Control::none())
            .unwrap()
            .requirements()
            .is_empty()
    );
}

#[test]
fn local_lookup_keeps_the_record_index_and_kind_across_multiple_candidates() {
    let mut input = sample();
    let mut rendition = input.renditions[0].clone();
    rendition.key = key("second-text");
    rendition.occurrence_key = key("text/2");
    rendition.content = RenditionContent::NormalizedText("ZZ".into());
    input.renditions.push(rendition);
    let mut passage = input.passages[0].clone();
    passage.key = key("second-passage");
    passage.occurrence_key = key("passage/2");
    passage.rendition = local("second-text");
    passage.start = 0;
    passage.end = 1;
    passage.quote = Some("Z".into());
    passage.text_digest = digest(b"Z");
    input.passages.push(passage);
    input.references[0].target = EvidenceRefTarget::Passage {
        revision: local("revision"),
        rendition: local("second-text"),
        passage: local("second-passage"),
    };
    assert!(
        prepare_input(&input, &Control::none())
            .unwrap()
            .requirements()
            .is_empty()
    );
}

#[test]
fn known_json_encoding_verifies_only_exact_local_retained_values() {
    let mut input = sample();
    input.revisions[0].content = SourceContent::Retained {
        bytes: br#"{"a/b":{"m~n":1.00000000000000000000001}}"#.to_vec(),
        expected_digest: None,
    };
    input.references[0].target = EvidenceRefTarget::Structured {
        revision: local("revision"),
        pointer: "/a~1b/m~0n".into(),
        method: MethodRef {
            method: node(JSON_LEXICAL_UTF8_V1),
            version: key(JSON_LEXICAL_UTF8_VERSION),
        },
        value_digest: digest(b"1.00000000000000000000001"),
    };
    assert!(
        prepare_input(&input, &Control::none())
            .unwrap()
            .requirements()
            .is_empty()
    );
    let mut changed = input.clone();
    if let EvidenceRefTarget::Structured { value_digest, .. } = &mut changed.references[0].target {
        *value_digest = digest(b"1");
    }
    assert!(prepare_input(&changed, &Control::none()).is_err());
    let mut changed = input.clone();
    if let EvidenceRefTarget::Structured { method, .. } = &mut changed.references[0].target {
        method.version = key("2");
    }
    assert!(matches!(
        prepare_input(&changed, &Control::none())
            .unwrap()
            .requirements(),
        [ResolutionRequirement::StructuredValue { .. }]
    ));
    let mut changed = input.clone();
    changed.revisions[0].content = SourceContent::MetadataOnly {
        digest: digest(b"not retained"),
    };
    assert!(matches!(
        prepare_input(&changed, &Control::none())
            .unwrap()
            .requirements(),
        [ResolutionRequirement::StructuredValue { .. }]
    ));
    let mut changed = input.clone();
    if let EvidenceRefTarget::Structured { revision, .. } = &mut changed.references[0].target {
        *revision = RecordRef::Existing(node("urn:existing-revision"));
    }
    assert!(
        prepare_input(&changed, &Control::none())
            .unwrap()
            .requirements()
            .iter()
            .any(|requirement| matches!(
                requirement,
                ResolutionRequirement::StructuredValue { .. }
            ))
    );
    input.revisions[0].content = SourceContent::Retained {
        bytes: vec![0xff],
        expected_digest: None,
    };
    assert!(prepare_input(&input, &Control::none()).is_err());
}

#[test]
fn unresolved_passage_coordinates_always_have_explicit_identity_requirements() {
    let mut input = sample();
    input.references[0].target = EvidenceRefTarget::Passage {
        revision: local("revision"),
        rendition: local("text"),
        passage: RecordRef::Existing(node("urn:passage-from-another-revision")),
    };
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert!(
        prepared
            .requirements()
            .contains(&ResolutionRequirement::PassageReferenceIdentity {
                reference_key: "reference".into(),
            })
    );
}

#[test]
fn unresolved_rendition_coordinates_always_have_explicit_identity_requirements() {
    let mut input = sample();
    input.references[0].target = EvidenceRefTarget::Passage {
        revision: local("revision"),
        rendition: RecordRef::Existing(node("urn:rendition-from-another-revision")),
        passage: local("passage"),
    };
    input.passages[0].rendition = RecordRef::Existing(node("urn:rendition-from-another-revision"));
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert!(
        prepared
            .requirements()
            .contains(&ResolutionRequirement::PassageIdentity {
                passage_key: "passage".into(),
            })
    );
}

#[test]
fn many_passages_visit_each_rendition_character_at_most_once() {
    let mut input = sample();
    let text = "é🚀".repeat(2048);
    input.renditions[0].content = RenditionContent::NormalizedText(text.clone());
    let template = input.passages[0].clone();
    input.passages = (0..128)
        .map(|index| {
            let mut passage = template.clone();
            passage.key = key(&if index == 0 {
                "passage".into()
            } else {
                format!("passage/{index}")
            });
            passage.occurrence_key = key(&format!("selected/{index}"));
            passage.start = 4096 - 32 + index % 32;
            passage.end = passage.start + 1;
            let quote = if passage.start.is_multiple_of(2) {
                "é"
            } else {
                "🚀"
            };
            passage.quote = Some(quote.into());
            passage.text_digest = digest(quote.as_bytes());
            passage
        })
        .collect();
    super::prepare::PASSAGE_CHARACTER_VISITS.with(|visits| visits.set(0));
    assert!(
        prepare_input(&input, &Control::none())
            .unwrap()
            .requirements()
            .is_empty()
    );
    let visits = super::prepare::PASSAGE_CHARACTER_VISITS.with(std::cell::Cell::get);
    assert!(
        visits <= text.chars().count(),
        "visited {visits} characters for one 4096-code-point rendition"
    );
}

#[test]
fn overdeep_native_claims_and_stored_facts_return_an_error_before_cloning() {
    const CHILD: &str = "SPARKLES_C25_DEEP_NATIVE_CHILD";
    if let Ok(kind) = std::env::var(CHILD) {
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || {
                let mut term: Term = Literal::new_simple_literal("leaf").into();
                for _ in 0..40_000 {
                    term = Triple::new(
                        node("urn:nested-subject"),
                        node("urn:nested-predicate"),
                        term,
                    )
                    .into();
                }
                let expected = Triple::new(node("urn:s"), node("urn:p"), term);
                let mut input = sample();
                if kind == "claim" {
                    input.claims[0].proposition = expected;
                } else {
                    input.references[0].target = EvidenceRefTarget::StoredFact(StoredFactRef {
                        dataset: Uuid::nil(),
                        branch: Uuid::nil(),
                        commit: 1,
                        graph: GraphIdentity::Default,
                        expected,
                        anchor: None,
                    });
                }
                let refused = matches!(
                    prepare_input(&input, &Control::none()),
                    Err(Error::Invalid(_))
                );
                // Native RDF structures deliberately exceed their safe drop depth. The
                // isolated test process owns them until exit rather than recursively dropping.
                std::mem::forget(input);
                assert!(refused);
            })
            .unwrap()
            .join()
            .unwrap();
        return;
    }
    for kind in ["claim", "fact"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "evidence::tests::overdeep_native_claims_and_stored_facts_return_an_error_before_cloning", "--nocapture"])
            .env(CHILD, kind).output().unwrap();
        assert!(
            output.status.success(),
            "{kind} input aborted: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn grouped_selectors_preserve_input_order_overlap_duplicates_and_unicode_eof() {
    let mut input = sample();
    input.renditions[0].content = RenditionContent::NormalizedText("aé🚀z".into());
    let template = input.passages[0].clone();
    input.passages = [(2, 4, "🚀z"), (0, 2, "aé"), (1, 3, "é🚀"), (2, 4, "🚀z")]
        .into_iter()
        .enumerate()
        .map(|(index, (start, end, quote))| {
            let mut passage = template.clone();
            passage.key = key(&if index == 0 {
                "passage".into()
            } else {
                format!("passage/{index}")
            });
            passage.occurrence_key = key(&format!("selected/{index}"));
            passage.start = start;
            passage.end = end;
            passage.quote = Some(quote.into());
            passage.text_digest = digest(quote.as_bytes());
            passage
        })
        .collect();
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert_eq!(prepared.input().passages[0].start, 2);
    assert_eq!(prepared.input().passages[1].start, 0);
    assert!(prepared.requirements().is_empty());
    let mut changed = input.clone();
    changed.passages[3].text_digest = digest(b"incorrect duplicate-range digest");
    assert!(prepare_input(&changed, &Control::none()).is_err());
    let mut changed = input.clone();
    changed.passages[0].end = 5;
    assert!(prepare_input(&changed, &Control::none()).is_err());
    input.renditions[0].content = RenditionContent::NormalizedText(String::new());
    assert!(prepare_input(&input, &Control::none()).is_err());
}

#[test]
fn many_structured_references_validate_each_retained_revision_once() {
    let mut input = sample();
    input.revisions[0].content = SourceContent::Retained {
        bytes: br#"{"a/b":{"m~n":[0,1]}}"#.to_vec(),
        expected_digest: None,
    };
    let mut second = input.revisions[0].clone();
    second.key = key("revision-two");
    second.source_key = key("source/two");
    second.content = SourceContent::Retained {
        bytes: br#"{"a/b":{"m~n":[2,3]}}"#.to_vec(),
        expected_digest: None,
    };
    input.revisions.push(second);
    let template = input.references[0].clone();
    input.references = (0..64)
        .map(|index| {
            let mut reference = template.clone();
            reference.key = key(&if index == 0 {
                "reference".into()
            } else {
                format!("reference/{index}")
            });
            reference.occurrence_key = key(&format!("reference/{index}"));
            reference.target = EvidenceRefTarget::Structured {
                revision: local(if index < 32 {
                    "revision"
                } else {
                    "revision-two"
                }),
                pointer: format!("/a~1b/m~0n/{}", index % 2),
                method: MethodRef {
                    method: node(JSON_LEXICAL_UTF8_V1),
                    version: key(JSON_LEXICAL_UTF8_VERSION),
                },
                value_digest: digest(
                    (index % 2 + if index < 32 { 0 } else { 2 })
                        .to_string()
                        .as_bytes(),
                ),
            };
            reference
        })
        .collect();
    super::structured::JSON_REVISION_VALIDATIONS.with(|count| count.set(0));
    let prepared = prepare_input(&input, &Control::none()).unwrap();
    assert!(prepared.requirements().is_empty());
    assert_eq!(
        super::structured::JSON_REVISION_VALIDATIONS.with(std::cell::Cell::get),
        2
    );
    assert_eq!(prepared.input().references.len(), 64);
    if let EvidenceRefTarget::Structured { value_digest, .. } = &mut input.references[63].target {
        *value_digest = digest(b"a changed digest on one duplicated selection");
    }
    assert!(prepare_input(&input, &Control::none()).is_err());
}
