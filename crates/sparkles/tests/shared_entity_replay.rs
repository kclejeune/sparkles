//! First private reference tranche: comparison bytes, not RDF/identity replay.
#[path = "../examples/shared_entity_replay/mod.rs"]
mod replay;

use replay::*;
use sparkles::task::Control;
use std::path::PathBuf;
use std::time::Instant;

fn fixture(path: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../testsuite/entity-representation/v1")
            .join(path),
    )
    .unwrap()
}
fn text(value: &str) -> String {
    value.to_owned()
}
fn decisions() -> Vec<String> {
    [
        "urn:example:d1",
        "urn:example:d2",
        "urn:example:d3",
        "urn:example:d4",
    ]
    .map(text)
    .into()
}
fn fixed_a12() -> Comparison {
    Comparison {
        profile_version: 1,
        scope: text("urn:example:a01:A12"),
        input_ids: [
            "https://example.org/A",
            "https://example.org/B",
            "urn:example:a01:A12:p1",
            "urn:example:a01:A12:p2",
            "urn:example:a01:A12:pair:manifest",
            "urn:example:a01:A12:unrelated",
            "urn:example:a01:mapping1",
            "urn:example:a01:normalization1",
            "urn:example:a01:ontology1",
            "urn:example:a01:r1",
            "urn:example:a01:simulatedSnapshot",
            "urn:example:d1",
            "urn:example:d2",
            "urn:example:d3",
            "urn:example:d4",
            "urn:x-sparkles:representation:policy:1",
        ]
        .map(text)
        .into(),
        assignments: vec![
            Assignment {
                entity: text("https://example.org/A"),
                status: Status::Unresolved,
                representative: None,
                decisions: decisions(),
            },
            Assignment {
                entity: text("https://example.org/B"),
                status: Status::Unresolved,
                representative: None,
                decisions: decisions(),
            },
            Assignment {
                entity: text("urn:example:a01:A12:unrelated"),
                status: Status::Resolved,
                representative: Some(text("urn:example:a01:A12:unrelated")),
                decisions: vec![],
            },
        ],
        conflicts: vec![Conflict {
            code: ConflictCode::Decision,
            entities: vec![text("https://example.org/A"), text("https://example.org/B")],
            decisions: decisions(),
        }],
        unknowns: vec![],
    }
}
fn encode(value: &Comparison) -> Result<Vec<u8>, Failure> {
    encode_comparison(value, OutputLimit(MAX_OUTPUT_BYTES), &Control::none())
}

#[test]
fn independently_fixed_scalar_vectors_preserve_exact_bytes() {
    let vectors: serde_json::Value =
        serde_json::from_slice(&fixture("expected/encoding-vectors.json")).unwrap();
    for vector in vectors.as_array().unwrap() {
        let value: String = vector["codepoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|code| {
                char::from_u32(u32::from_str_radix(code.as_str().unwrap(), 16).unwrap()).unwrap()
            })
            .collect();
        let bytes = encode_scalar(&value, OutputLimit(1024), &Control::none()).unwrap();
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, vector["encodedUtf8Hex"].as_str().unwrap());
    }
    assert_eq!(
        encode_scalar("\u{7f}\u{2029}", OutputLimit(8), &Control::none()).unwrap(),
        b"\"\x7f\xe2\x80\xa9\""
    );
}

#[test]
fn independently_fixed_full_comparison_is_exact_without_mutating_inputs() {
    let value = fixed_a12();
    let original = value.clone();
    let actual = encode(&value).unwrap();
    assert_eq!(actual, fixture("expected/a12-comparison.json"));
    assert_eq!(value, original);
    assert!(!actual.ends_with(b"\n"));
    assert!(!actual.windows(2).any(|bytes| bytes == b"\\/"));
}

#[test]
fn order_duplicates_and_same_component_conflict_causes_converge_to_fixed_bytes() {
    let mut value = fixed_a12();
    value.input_ids.reverse();
    value.input_ids.push(text("urn:example:d1"));
    value.assignments.reverse();
    value.assignments.push(value.assignments[0].clone());
    for row in &mut value.assignments {
        row.decisions.reverse();
        if !row.decisions.is_empty() {
            row.decisions.push(text("urn:example:d1"));
        }
    }
    value.conflicts = vec![
        Conflict {
            code: ConflictCode::Decision,
            entities: vec![
                text("https://example.org/B"),
                text("https://example.org/A"),
                text("https://example.org/B"),
            ],
            decisions: vec![text("urn:example:d4"), text("urn:example:d3")],
        },
        Conflict {
            code: ConflictCode::Decision,
            entities: vec![text("https://example.org/A"), text("https://example.org/B")],
            decisions: vec![
                text("urn:example:d2"),
                text("urn:example:d1"),
                text("urn:example:d1"),
            ],
        },
    ];
    assert_eq!(
        encode(&value).unwrap(),
        fixture("expected/a12-comparison.json")
    );
}

#[test]
fn codes_and_unknown_rows_have_fixed_order_and_duplicate_semantics() {
    let mut value = fixed_a12();
    for code in [ConflictCode::Kind, ConflictCode::Distinct] {
        let mut row = value.conflicts[0].clone();
        row.code = code;
        value.conflicts.push(row);
    }
    value.unknowns = vec![
        Unknown {
            record: text("urn:example:a01:A12:p2"),
            code: UnknownCode::UnavailableInput,
        },
        Unknown {
            record: text("urn:example:a01:A12:p1"),
            code: UnknownCode::PossibleMatch,
        },
        Unknown {
            record: text("urn:example:a01:A12:p2"),
            code: UnknownCode::UnavailableInput,
        },
    ];
    let result: serde_json::Value = serde_json::from_slice(&encode(&value).unwrap()).unwrap();
    assert_eq!(
        result["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["code"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["decision-conflict", "distinct-conflict", "kind-conflict"]
    );
    assert_eq!(
        result["unknowns"],
        serde_json::json!([
            {"record":"urn:example:a01:A12:p1","code":"possible-match"},
            {"record":"urn:example:a01:A12:p2","code":"unavailable-input"}
        ])
    );
}

#[test]
fn malformed_comparison_frames_refuse_instead_of_dropping_information() {
    let mut value = fixed_a12();
    value.profile_version = 2;
    assert_eq!(encode(&value).unwrap_err().code(), "unsupported-profile");
    let mut value = fixed_a12();
    value.scope = text("relative scope");
    assert_eq!(encode(&value), Err(Failure::InvalidDomainRecord));
    let mut value = fixed_a12();
    value.input_ids.retain(|id| id != "urn:example:d1");
    assert_eq!(encode(&value), Err(Failure::UnresolvedReference));
    let mut value = fixed_a12();
    let mut contradictory = value.assignments[0].clone();
    contradictory.decisions.pop();
    value.assignments.push(contradictory);
    assert_eq!(encode(&value), Err(Failure::InvalidDomainRecord));
    let mut value = fixed_a12();
    value.assignments[0].representative = Some(text("https://example.org/A"));
    assert_eq!(encode(&value), Err(Failure::InvalidDomainRecord));
    let mut value = fixed_a12();
    value.conflicts.clear();
    assert_eq!(encode(&value), Err(Failure::InvalidDomainRecord));
    let mut value = fixed_a12();
    value.conflicts[0].entities.clear();
    assert_eq!(encode(&value), Err(Failure::InvalidDomainRecord));
    let mut value = fixed_a12();
    value.input_ids.push(text("urn:example:missing"));
    value.assignments[2].representative = Some(text("urn:example:missing"));
    assert_eq!(encode(&value), Err(Failure::UnresolvedReference));
}

#[test]
fn lower_output_caps_and_existing_controls_refuse_without_an_artifact() {
    let value = fixed_a12();
    let expected = fixture("expected/a12-comparison.json");
    assert_eq!(
        encode_comparison(&value, OutputLimit(expected.len()), &Control::none()).unwrap(),
        expected
    );
    assert_eq!(
        encode_comparison(&value, OutputLimit(expected.len() - 1), &Control::none()),
        Err(Failure::ReplayLimit)
    );
    assert_eq!(
        encode_comparison(&value, OutputLimit(MAX_OUTPUT_BYTES + 1), &Control::none()),
        Err(Failure::ReplayLimit)
    );
    assert_eq!(
        encode_scalar("x", OutputLimit(0), &Control::none()),
        Err(Failure::ReplayLimit)
    );
    let cancelled = Control::none();
    cancelled.cancel.cancel();
    assert_eq!(
        encode_comparison(&value, OutputLimit(MAX_OUTPUT_BYTES), &cancelled),
        Err(Failure::ReplayCancelled)
    );
    let elapsed = Control {
        deadline: Some(Instant::now()),
        ..Control::none()
    };
    assert_eq!(
        encode_comparison(&value, OutputLimit(MAX_OUTPUT_BYTES), &elapsed),
        Err(Failure::ReplayCancelled)
    );
}

#[test]
fn iri_sets_keep_unicode_spelling_and_utf8_byte_order() {
    let mut value = fixed_a12();
    for id in ["urn:example:😀", "urn:example:é", "urn:example:e\u{301}"] {
        value.input_ids.push(text(id));
        value.unknowns.push(Unknown {
            record: text(id),
            code: UnknownCode::PossibleMatch,
        });
    }
    let result: serde_json::Value = serde_json::from_slice(&encode(&value).unwrap()).unwrap();
    assert_eq!(
        result["unknowns"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["record"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["urn:example:e\u{301}", "urn:example:é", "urn:example:😀"]
    );
}

#[test]
fn rdf_replay_computes_a12_without_authored_assignments() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let common = fixture("valid/common.ttl");
    let resolution = fixture("valid/resolution.ttl");
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let mut graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&resolution, oxrdfio::RdfFormat::Turtle),
            (policy.as_bytes(), oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    let authored: Vec<_> = graph
        .records
        .keys()
        .filter(|id| {
            graph.has_type(id, &format!("{SR}Assignment"))
                || graph.has_type(id, &format!("{SR}Conflict"))
        })
        .cloned()
        .collect();
    for id in authored {
        graph.records.remove(&id);
    }
    let result = resolve(&graph, "urn:example:a01:A12:pair", &operation).unwrap();
    assert_eq!(
        encode_comparison(&result.comparison, OutputLimit(1 << 20), &control).unwrap(),
        fixture("expected/a12-comparison.json")
    );
}

fn with_resolution_graph(extra: &[u8], run: impl FnOnce(&Graph, &Operation<'_>)) {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let common = fixture("valid/common.ttl");
    let source = fixture("valid/resolution.ttl");
    let relations = fixture("valid/relations.ttl");
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let mut graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&source, oxrdfio::RdfFormat::Turtle),
            (&relations, oxrdfio::RdfFormat::Turtle),
            (policy.as_bytes(), oxrdfio::RdfFormat::Turtle),
            (extra, oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    let authored: Vec<_> = graph
        .records
        .keys()
        .filter(|id| {
            graph.has_type(id, &format!("{SR}Assignment"))
                || graph.has_type(id, &format!("{SR}Conflict"))
        })
        .cloned()
        .collect();
    for id in authored {
        graph.records.remove(&id);
    }
    run(&graph, &operation);
}
fn assignment<'a>(computed: &'a Comparison, local: &str) -> &'a Assignment {
    let id = format!("urn:example:a01:{local}");
    computed
        .assignments
        .iter()
        .find(|row| row.entity == id)
        .unwrap()
}

#[test]
fn rdf_replay_binary_nary_and_unrelated_components_match_fixed_a3() {
    with_resolution_graph(&[], |graph, operation| {
        for (view, unresolved) in [
            ("chain", &["A", "B", "C"][..]),
            ("naryPair", &["A", "B"][..]),
            ("naryOnly", &[][..]),
        ] {
            let result = resolve(graph, &format!("urn:example:a01:A3:{view}"), operation).unwrap();
            for member in ["A", "B", "C", "unrelated"] {
                let row = assignment(&result.comparison, &format!("A3:{member}"));
                assert_eq!(
                    row.status,
                    if unresolved.contains(&member) {
                        Status::Unresolved
                    } else {
                        Status::Resolved
                    }
                );
                if row.status == Status::Resolved {
                    assert_eq!(row.representative.as_deref(), Some(row.entity.as_str()));
                }
            }
            assert_eq!(
                result.comparison.conflicts.len(),
                usize::from(!unresolved.is_empty())
            );
        }
    });
}

#[test]
fn rdf_replay_account_person_and_contradicted_nary_have_fixed_components() {
    with_resolution_graph(&[], |graph, operation| {
        let account = resolve(graph, "urn:example:a01:A2:view", operation).unwrap();
        assert_eq!(account.comparison.assignments.len(), 2);
        assert!(
            account
                .comparison
                .assignments
                .iter()
                .all(|r| r.status == Status::Unresolved)
        );
        assert_eq!(account.comparison.conflicts[0].code, ConflictCode::Kind);
        let separation = resolve(graph, "urn:example:a01:A12:singletons", operation).unwrap();
        assert_eq!(separation.comparison.conflicts.len(), 3);
        assert!(
            separation
                .comparison
                .conflicts
                .iter()
                .all(|c| c.entities.len() == 1
                    && c.decisions.len() == 2
                    && c.code == ConflictCode::Decision)
        );
        assert_eq!(
            assignment(&separation.comparison, "A12:unrelated").status,
            Status::Resolved
        );
    });
}

#[test]
fn rdf_replay_merge_surviving_path_and_split_keep_historic_inputs() {
    with_resolution_graph(&[], |graph, operation| {
        let original = graph.records.clone();
        for (view, representatives) in [
            ("merged", [Some("A"), Some("A"), Some("A")]),
            ("stillConnected", [None, None, None]),
            ("split", [Some("A"), Some("B"), Some("B")]),
        ] {
            let result = resolve(graph, &format!("urn:example:a01:A4:{view}"), operation).unwrap();
            for (member, expected) in ["A", "B", "C"].into_iter().zip(representatives) {
                let row = assignment(&result.comparison, &format!("A4:{member}"));
                assert_eq!(
                    row.representative,
                    expected.map(|v| format!("urn:example:a01:A4:{v}"))
                );
            }
        }
        assert_eq!(graph.records, original);
        let again = resolve(graph, "urn:example:a01:A4:merged", operation).unwrap();
        assert_eq!(
            assignment(&again.comparison, "A4:C")
                .representative
                .as_deref(),
            Some("urn:example:a01:A4:A")
        );
    });
}

#[test]
fn complete_record_redelivery_is_idempotent_and_conflicts_are_atomic() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let first = b"<urn:record> <urn:value> \"old\" .";
    let second = b"<urn:record> <urn:value> \"changed\" .\n<urn:new> <urn:value> \"new\" .";
    let mut registry =
        Graph::load_documents(&[(first, oxrdfio::RdfFormat::NTriples)], &operation).unwrap();
    let original = registry.records.clone();
    let used = operation.used();
    let duplicate =
        Graph::load_documents(&[(first, oxrdfio::RdfFormat::NTriples)], &operation).unwrap();
    registry.merge_delivery(duplicate, &operation).unwrap();
    assert_eq!(registry.records, original);
    assert_eq!(operation.used(), used);
    let conflict =
        Graph::load_documents(&[(second, oxrdfio::RdfFormat::NTriples)], &operation).unwrap();
    assert!(matches!(
        registry.merge_delivery(conflict, &operation),
        Err(Failure::ImmutableRecordConflict)
    ));
    assert_eq!(registry.records, original);
    assert_eq!(operation.used(), used);
    drop(registry);
    assert_eq!(operation.used(), 0);
}

#[test]
fn rdf_supersession_cycle_refuses_the_fixed_a12_case() {
    let extra = b"<urn:example:d1> <http://www.w3.org/ns/prov#wasRevisionOf> <urn:example:d2> .\n<urn:example:d2> <http://www.w3.org/ns/prov#wasRevisionOf> <urn:example:d1> .";
    with_resolution_graph(extra, |graph, operation| {
        assert!(matches!(
            resolve(graph, "urn:example:a01:A12:pair", operation),
            Err(Failure::DecisionCycle)
        ));
    });
}

#[test]
fn rdf_limits_and_cancellation_refuse_without_partial_owned_output() {
    let control = Control::none();
    let limits = Limits {
        entities: 0,
        ..Limits::default()
    };
    let operation = Operation::new(&control, limits).unwrap();
    let data = b"<urn:entity> a <urn:x-sparkles:representation:Entity> .";
    assert!(matches!(
        Graph::load_documents(&[(data, oxrdfio::RdfFormat::Turtle)], &operation),
        Err(Failure::ReplayLimit)
    ));
    assert_eq!(operation.used(), 0);
    with_resolution_graph(&[], |graph, operation| {
        let used = operation.used();
        operation.control.cancel.cancel();
        assert!(matches!(
            resolve(graph, "urn:example:a01:A12:pair", operation),
            Err(Failure::ReplayCancelled)
        ));
        assert_eq!(operation.used(), used);
    });
}

#[test]
fn rdf_unknown_fields_and_scope_reuse_refuse_without_label_diagnostics() {
    let extra = b"<urn:example:a01:A3:A> <urn:x-sparkles:representation:unknownControl> \"hidden private label\" .";
    with_resolution_graph(extra, |graph, operation| {
        let error = resolve(graph, "urn:example:a01:A3:chain", operation)
            .err()
            .unwrap();
        assert_eq!(error, Failure::UnknownControlField);
        assert_eq!(error.code(), "unknown-control-field");
    });
    with_resolution_graph(&[], |graph, operation| {
        assert!(matches!(
            resolve_in_scope(
                graph,
                "urn:example:a01:A12:pair",
                "urn:example:a01:A15:public",
                operation
            ),
            Err(Failure::ScopeMismatch)
        ));
    });
}
