//! Private offline replay against independently fixed input and semantic oracles.
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

fn with_evolution_graph(run: impl FnOnce(&mut Graph, &Operation<'_>)) {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let docs: Vec<_> = [
        "valid/common.ttl",
        "valid/support-lineage.ttl",
        "valid/evolution.ttl",
        "valid/episode-observations.ttl",
    ]
    .map(fixture)
    .into();
    let supplied: Vec<_> = docs
        .iter()
        .map(|d| (d.as_slice(), oxrdfio::RdfFormat::Turtle))
        .collect();
    let mut graph = Graph::load_documents(&supplied, &operation).unwrap();
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let delivery = Graph::load_documents(
        &[(policy.as_bytes(), oxrdfio::RdfFormat::Turtle)],
        &operation,
    )
    .unwrap();
    graph.merge_delivery(delivery, &operation).unwrap();
    let authored: Vec<_> = graph
        .records
        .keys()
        .filter(|id| {
            ["Projection", "Assignment", "Conflict"]
                .iter()
                .any(|kind| graph.has_type(id, &format!("{SR}{kind}")))
        })
        .cloned()
        .collect();
    for id in authored {
        graph.records.remove(&id);
    }
    run(&mut graph, &operation);
}
fn ex(local: &str) -> String {
    format!("urn:example:a01:{local}")
}
fn candidates() -> Vec<ObservationCandidate> {
    let declared: serde_json::Value =
        serde_json::from_slice(&fixture("expected/a10-selection-inputs.json")).unwrap();
    declared["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| ObservationCandidate {
            revision: ex(c["sourceRevision"].as_str().unwrap()),
            observation: ex(c["observation"].as_str().unwrap()),
            relation_claim: ex(c["relationClaim"].as_str().unwrap()),
            value_claim: ex(c["valueClaim"].as_str().unwrap()),
        })
        .collect()
}
#[test]
fn a5_admission_is_computed_from_decisions_after_projection_rows_are_removed() {
    with_evolution_graph(|graph, operation| {
        let oracle: serde_json::Value =
            serde_json::from_slice(&fixture("expected/semantic-oracle.json")).unwrap();
        let expected = &oracle["cases"][4]["outcome"];
        for (view, key) in [
            ("both", "bothOccurrences"),
            ("afterFirst", "afterFirstWithdrawal"),
            ("afterFinal", "afterFinalWithdrawal"),
        ] {
            let selected = admitted_occurrences(
                graph,
                &ex(&format!("A5:{view}:view")),
                &ex("A5:simulatedClaimAdmission:1"),
                1,
                operation,
            )
            .unwrap();
            let Selection::Known(actual) = selected else {
                panic!("fixed policy must select");
            };
            let fixed: Vec<_> = expected[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| ex(&format!("A5:{}", id.as_str().unwrap())))
                .collect();
            assert_eq!(actual.value, fixed);
        }
        assert!(graph.has_type(&ex("A5:claim1"), &format!("{EV}Claim")));
        assert!(graph.has_type(&ex("A5:claim2"), &format!("{EV}Claim")));
        assert_eq!(
            admitted_occurrences(
                graph,
                &ex("A5:both:view"),
                "urn:unknown:policy",
                1,
                operation
            )
            .unwrap_err(),
            Failure::UnsupportedProfile
        );
    });
}
#[test]
fn a10_selects_terminal_revision_without_summing_or_reading_projection_rows() {
    with_evolution_graph(|graph, operation| {
        let oracle: serde_json::Value =
            serde_json::from_slice(&fixture("expected/a10-selection-inputs.json")).unwrap();
        for state in oracle["states"].as_array().unwrap() {
            let Selection::Known(actual) = select_observation(
                graph,
                &ex(state["view"].as_str().unwrap()),
                &ex("A10:selectionProfile1"),
                &candidates(),
                operation,
            )
            .unwrap() else {
                panic!("fixed series must select");
            };
            assert_eq!(
                actual.revision,
                ex(state["expected"]["sourceRevision"].as_str().unwrap())
            );
            assert_eq!(
                actual.observation,
                ex(state["expected"]["observation"].as_str().unwrap())
            );
            assert_eq!(
                actual.count,
                state["expected"]["count"].as_u64().unwrap().to_string()
            );
            assert_eq!(actual.episode, ex("A10:episode"));
            assert_eq!(
                actual.denominator_status,
                Some(format!("{SR}UnknownDenominator"))
            );
            assert_eq!(actual.geometry_version, ex("geometry1"));
        }
    });
}
#[test]
fn revision_unknowns_are_explicit_and_never_resolved_by_count_or_order() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    assert_eq!(
        terminal_revision(&[], &[], &operation).unwrap(),
        Selection::Unknown("NoSelectedCandidate")
    );
    let ids = vec![ex("one"), ex("two")];
    assert_eq!(
        terminal_revision(&ids, &[], &operation).unwrap(),
        Selection::Unknown("ConflictingTerminalRevisions")
    );
    assert_eq!(
        terminal_revision(
            &ids,
            &[
                (ids[0].clone(), ids[1].clone()),
                (ids[1].clone(), ids[0].clone())
            ],
            &operation
        )
        .unwrap(),
        Selection::Unknown("RevisionCycle")
    );
    assert_eq!(
        terminal_revision(&ids, &[(ids[1].clone(), ids[0].clone())], &operation).unwrap(),
        Selection::Known(ids[1].as_str())
    );
}
#[test]
fn a10_unknown_selection_rule_and_incompatible_series_stay_unknown() {
    with_evolution_graph(|graph, operation| {
        let id = ex("A10:selectionProfile1");
        let predicate = ex("application:revisionSelectionRule");
        let old = graph.records[&id].clone();
        graph
            .records
            .get_mut(&id)
            .unwrap()
            .retain(|s| s.predicate != predicate);
        graph.records.get_mut(&id).unwrap().insert(Statement {
            graph: "DEFAULT".into(),
            predicate,
            object: oxrdf::NamedNode::new("urn:unknown:rule").unwrap().into(),
        });
        assert!(matches!(
            select_observation(graph, &ex("A10:currentView"), &id, &candidates(), operation)
                .unwrap(),
            Selection::Unknown("UnknownSelectionRule")
        ));
        graph.records.insert(id.clone(), old);
        let observation = ex("A10:obs2");
        let predicate = format!("{SR}geometryVersion");
        graph
            .records
            .get_mut(&observation)
            .unwrap()
            .retain(|s| s.predicate != predicate);
        graph
            .records
            .get_mut(&observation)
            .unwrap()
            .insert(Statement {
                graph: "DEFAULT".into(),
                predicate,
                object: oxrdf::NamedNode::new(ex("geometry2")).unwrap().into(),
            });
        assert!(matches!(
            select_observation(graph, &ex("A10:currentView"), &id, &candidates(), operation)
                .unwrap(),
            Selection::Unknown("IncompatibleObservationSeries")
        ));
    });
}
#[test]
fn a6_lineage_and_a7_availability_use_explicit_declarations() {
    with_evolution_graph(|graph, operation| {
        let occurrences =
            ["bulletinClaim", "mirrorClaim", "translationClaim"].map(|id| ex(&format!("A6:{id}")));
        let Selection::Known(groups) = source_lineages(graph, &occurrences, operation).unwrap()
        else {
            panic!("fixed lineage");
        };
        assert_eq!(groups.value.len(), 1);
        assert_eq!(groups.value[&ex("bulletinFamily")], occurrences);
        assert_eq!(
            simulated_available(graph, &ex("late"), "2026-02-01T00:00:00Z", operation).unwrap(),
            Selection::Known(false)
        );
        assert_eq!(
            simulated_available(graph, &ex("late"), "2026-03-01T00:00:00Z", operation).unwrap(),
            Selection::Known(true)
        );
        assert_eq!(
            simulated_available(graph, &ex("r1"), "2026-03-01T00:00:00Z", operation).unwrap(),
            Selection::Unknown("UnknownAvailability")
        );
    });
}
#[test]
fn a8_mapping_migration_retains_source_identity_and_old_views() {
    with_evolution_graph(|graph, operation| {
        let old = graph.records[&ex("A8:view1")].clone();
        let revision = graph.records[&ex("r1")].clone();
        assert_eq!(
            mapping_migration(
                graph,
                &ex("A8:view1"),
                &ex("A8:view2"),
                &ex("A8:migration"),
                operation
            )
            .unwrap(),
            Selection::Known(true)
        );
        assert_eq!(graph.records[&ex("A8:view1")], old);
        assert_eq!(graph.records[&ex("r1")], revision);
    });
}
#[test]
fn retained_source_digests_and_selected_values_match_all_pinned_source_records() {
    with_evolution_graph(|graph, operation| {
        let declared: serde_json::Value =
            serde_json::from_slice(&fixture("expected/source-selections.json")).unwrap();
        let mut retained = RetainedSources::default();
        for source in declared.as_array().unwrap() {
            let bytes = fixture(source["path"].as_str().unwrap());
            retained
                .insert(
                    graph,
                    source["revision"].as_str().unwrap(),
                    bytes.as_slice(),
                    operation,
                )
                .unwrap();
            for selection in source["selections"].as_array().unwrap() {
                retained
                    .verify_reference(graph, selection["reference"].as_str().unwrap(), operation)
                    .unwrap();
            }
        }
        retained
            .verify_selected(graph, &ex("A10:currentView:manifest"), operation)
            .unwrap();
        assert_eq!(
            retained.insert(
                graph,
                &ex("r1"),
                fixture("raw/tampered-r1.json").as_slice(),
                operation
            ),
            Err(Failure::SourceDigestMismatch)
        );
        let empty = RetainedSources::default();
        assert_eq!(
            empty.verify_selected(graph, &ex("A10:currentView:manifest"), operation),
            Err(Failure::IncompleteInput)
        );
    });
}
#[test]
fn existing_software_bytes_map_only_explicit_family_version_bindings() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testsuite/software-inventory/v1/raw/synthetic-before.Cargo.lock");
    let bytes = std::fs::read(path).unwrap();
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let bindings = [SoftwareBinding {
        name: text("sparkles-fixture-only-leaf"),
        version: text("1.3.0"),
        source: Some(text(
            "registry+https://github.com/rust-lang/crates.io-index",
        )),
        family_id: ex("offline:leaf"),
        version_id: ex("offline:leaf:1.3.0"),
    }];
    let Selection::Known(actual) = software_inventory(&bytes, 1, &bindings, &operation).unwrap()
    else {
        panic!("fixed binding");
    };
    assert_eq!(
        actual.packages,
        vec![(ex("offline:leaf"), ex("offline:leaf:1.3.0"))]
    );
    assert_eq!(
        actual.deployed_exposure,
        Selection::Unknown("NoDeploymentEvidence")
    );
    assert!(actual.source_digest.starts_with("sha256:"));
    assert!(matches!(
        software_inventory(&bytes, 2, &bindings, &operation).unwrap(),
        Selection::Unknown("UnsupportedMapping")
    ));
}

#[test]
fn rdf_permutations_duplicate_triples_and_batched_complete_records_keep_exact_bytes() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let docs = [fixture("valid/common.ttl"), fixture("valid/resolution.ttl")];
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let mut subjects = std::collections::BTreeMap::<String, Vec<String>>::new();
    for bytes in docs.iter().map(Vec::as_slice).chain([policy.as_bytes()]) {
        for quad in oxrdfio::RdfParser::from_format(oxrdfio::RdfFormat::Turtle).for_slice(bytes) {
            let quad = quad.unwrap();
            subjects
                .entry(quad.subject.to_string())
                .or_default()
                .push(format!("{quad} .\n"));
        }
    }
    let mut graph = Graph::default();
    for (_, mut triples) in subjects.into_iter().rev() {
        triples.reverse();
        let bytes = triples.concat().repeat(2);
        let delivery = Graph::load_documents(
            &[(bytes.as_bytes(), oxrdfio::RdfFormat::NQuads)],
            &operation,
        )
        .unwrap();
        graph.merge_delivery(delivery, &operation).unwrap();
    }
    let computed = resolve(&graph, &ex("A12:pair"), &operation).unwrap();
    assert_eq!(
        encode_resolved(&computed.comparison, &operation).unwrap(),
        fixture("expected/a12-comparison.json")
    );
}
#[test]
fn graph_coordinates_are_content_and_blank_labels_cannot_be_durable_ids() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let default = b"<urn:record> <urn:value> \"value\" .\n";
    let named = b"<urn:record> <urn:value> \"value\" <urn:graph> .\n";
    let mut graph =
        Graph::load_documents(&[(default, oxrdfio::RdfFormat::NQuads)], &operation).unwrap();
    let same_id =
        Graph::load_documents(&[(named, oxrdfio::RdfFormat::NQuads)], &operation).unwrap();
    assert_eq!(
        graph.merge_delivery(same_id, &operation),
        Err(Failure::ImmutableRecordConflict)
    );
    for bytes in [
        b"_:blank <urn:value> <urn:object> .".as_slice(),
        b"<urn:record> <urn:value> _:blank .".as_slice(),
    ] {
        assert!(matches!(
            Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::Turtle)], &operation),
            Err(Failure::InvalidDomainRecord)
        ));
    }
}
#[test]
fn all_record_caps_refuse_plus_one_and_accept_the_supported_sibling() {
    for kind in [
        "Entity",
        "Description",
        "Claim",
        "ReviewDecision",
        "Relation",
    ] {
        let control = Control::none();
        let mut limits = Limits::default();
        match kind {
            "Entity" => limits.entities = 1,
            "Description" => limits.descriptions = 1,
            "Claim" => limits.claims = 1,
            "ReviewDecision" => limits.decisions = 1,
            _ => limits.relations = 1,
        }
        let operation = Operation::new(&control, limits).unwrap();
        let namespace = if matches!(kind, "Claim" | "ReviewDecision") {
            EV
        } else {
            SR
        };
        let one = format!("<urn:one> a <{namespace}{kind}> .\n");
        let two = format!("{one}<urn:two> a <{namespace}{kind}> .\n");
        assert!(
            Graph::load_documents(&[(one.as_bytes(), oxrdfio::RdfFormat::Turtle)], &operation)
                .is_ok()
        );
        assert!(matches!(
            Graph::load_documents(&[(two.as_bytes(), oxrdfio::RdfFormat::Turtle)], &operation),
            Err(Failure::ReplayLimit)
        ));
        assert_eq!(operation.used(), 0);
    }
}
#[test]
fn byte_memory_source_caps_deadline_and_owned_result_lifetimes_are_enforced() {
    let data = b"<urn:record> <urn:value> \"value\" .\n";
    let control = Control::none();
    let operation = Operation::new(
        &control,
        Limits {
            decoded: data.len() - 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        Graph::load_documents(&[(data, oxrdfio::RdfFormat::NTriples)], &operation),
        Err(Failure::ReplayLimit)
    ));
    assert_eq!(operation.used(), 0);
    let operation = Operation::new(
        &control,
        Limits {
            memory: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(matches!(
        Graph::load_documents(&[(data, oxrdfio::RdfFormat::NTriples)], &operation),
        Err(Failure::ReplayLimit)
    ));
    assert_eq!(operation.used(), 0);
    let elapsed = Control {
        deadline: Some(Instant::now()),
        ..Control::none()
    };
    assert!(matches!(
        Operation::new(&elapsed, Limits::default()),
        Err(Failure::ReplayCancelled)
    ));
    with_evolution_graph(|graph, operation| {
        let baseline = operation.used();
        let admitted = admitted_occurrences(
            graph,
            &ex("A5:both:view"),
            &ex("A5:simulatedClaimAdmission:1"),
            1,
            operation,
        )
        .unwrap();
        assert!(operation.used() > baseline);
        drop(admitted);
        assert_eq!(operation.used(), baseline);
        let control = Control::none();
        let small = Operation::new(
            &control,
            Limits {
                source: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let mut retained = RetainedSources::default();
        assert_eq!(
            retained.insert(graph, &ex("r1"), fixture("raw/r1.json").as_slice(), &small),
            Err(Failure::ReplayLimit)
        );
        assert_eq!(small.used(), 0);
    });
}
#[test]
fn selected_value_digest_tampering_refuses_without_overwriting_retained_source() {
    with_evolution_graph(|graph, operation| {
        let mut retained = RetainedSources::default();
        retained
            .insert(
                graph,
                &ex("r1"),
                fixture("raw/r1.json").as_slice(),
                operation,
            )
            .unwrap();
        let reference = ex("A5:claim1:ref");
        retained
            .verify_reference(graph, &reference, operation)
            .unwrap();
        let original = graph.records[&reference].clone();
        let predicate = format!("{EV}valueDigest");
        graph
            .records
            .get_mut(&reference)
            .unwrap()
            .retain(|s| s.predicate != predicate);
        graph
            .records
            .get_mut(&reference)
            .unwrap()
            .insert(Statement {
                graph: "DEFAULT".into(),
                predicate,
                object: oxrdf::Literal::new_simple_literal(format!("sha256:{}", "0".repeat(64)))
                    .into(),
            });
        assert_eq!(
            retained.verify_reference(graph, &reference, operation),
            Err(Failure::SourceDigestMismatch)
        );
        graph.records.insert(reference.clone(), original);
        retained
            .verify_reference(graph, &reference, operation)
            .unwrap();
    });
}
#[test]
fn a11_successors_keep_old_grouping_and_immutable_geography_records() {
    with_evolution_graph(|graph, operation| {
        let policy = sparkles::shared_entity::profile(1).unwrap().policy;
        let policy = Graph::load_documents(
            &[(policy.as_bytes(), oxrdfio::RdfFormat::Turtle)],
            operation,
        )
        .unwrap();
        graph.merge_delivery(policy, operation).unwrap();
        let old = resolve(graph, &ex("A11:oldGrouping"), operation).unwrap();
        let split = resolve(graph, &ex("A11:splitGrouping"), operation).unwrap();
        assert_eq!(old.comparison.assignments.len(), 3);
        assert!(
            old.comparison
                .assignments
                .iter()
                .all(|r| r.representative == Some(ex("A11:episode1")))
        );
        assert!(
            split
                .comparison
                .assignments
                .iter()
                .all(|r| r.representative.as_ref() == Some(&r.entity))
        );
        let old_bytes = encode_resolved(&old.comparison, operation).unwrap();
        assert_eq!(
            encode_resolved(
                &resolve(graph, &ex("A11:oldGrouping"), operation)
                    .unwrap()
                    .comparison,
                operation
            )
            .unwrap(),
            old_bytes
        );
        let geography = graph.records[&ex("A11:place1")].clone();
        let changed = fixture("valid/geometry-successor.ttl");
        let delivery = Graph::load_documents(
            &[(changed.as_slice(), oxrdfio::RdfFormat::Turtle)],
            operation,
        )
        .unwrap();
        assert_eq!(
            graph.merge_delivery(delivery, operation),
            Err(Failure::ImmutableRecordConflict)
        );
        assert_eq!(graph.records[&ex("A11:place1")], geography);
        assert_eq!(
            graph
                .one_iri(&ex("A11:place2"), &format!("{SR}geometryVersion"))
                .unwrap(),
            ex("geometry2")
        );
    });
}

fn supplied_view(
    scope: &str,
    view: &str,
    entities: &[String],
    operation: &Operation<'_>,
) -> Vec<u8> {
    let manifest = format!("{view}:manifest");
    let mut ids = entities.to_vec();
    ids.extend([
        ex("ontology1"),
        ex("mapping1"),
        ex("normalization1"),
        ex("simulatedSnapshot"),
        format!("{SR}policy:1"),
    ]);
    ids.sort();
    let payload = serde_json::json!({"profileVersion":1,"inputIds":ids,"requiredExtensions":[],"extensionIds":[]});
    // Fixed profile property order, independent of semantic answer computation.
    let payload = format!(
        "{{\"profileVersion\":1,\"inputIds\":{},\"requiredExtensions\":[],\"extensionIds\":[]}}",
        serde_json::to_string(&payload["inputIds"]).unwrap()
    );
    let digest =
        sparkles_core::evidence::Sha256Digest::of_bytes(payload.as_bytes(), operation.control)
            .unwrap();
    let ids = ids
        .iter()
        .map(|id| format!("<{id}>"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("PREFIX sr: <{SR}>\n<{view}> a sr:ResolutionView ; sr:profileVersion 1 ; sr:domainScope <{scope}> ; sr:matchingPolicy <{SR}policy:1> ; sr:ontologyVersion <{}> ; sr:mappingVersion <{}> ; sr:normalizationVersion <{}> ; sr:recordedView <{}> ; sr:inputManifest <{manifest}> .\n<{manifest}> a sr:InputManifest ; sr:profileVersion 1 ; sr:inputId {ids} ; sr:contentDigest \"{digest}\" .",ex("ontology1"),ex("mapping1"),ex("normalization1"),ex("simulatedSnapshot")).into_bytes()
}
#[test]
fn a15_public_replay_starts_from_separate_complete_scope_and_has_no_hidden_labels() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let common = fixture("valid/public-common.ttl");
    let public = fixture("valid/public-boundary.ttl");
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let view = ex("A15:publicView");
    let scope = ex("A15:public");
    let declaration = supplied_view(
        &scope,
        &view,
        &[ex("A15:publicEntity"), ex("public")],
        &operation,
    );
    let graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&public, oxrdfio::RdfFormat::Turtle),
            (policy.as_bytes(), oxrdfio::RdfFormat::Turtle),
            (&declaration, oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    let result = resolve_in_scope(&graph, &view, &scope, &operation).unwrap();
    assert_eq!(result.comparison.assignments.len(), 1);
    assert_eq!(
        result.comparison.assignments[0].entity,
        ex("A15:publicEntity")
    );
    let bytes = encode_resolved(&result.comparison, &operation).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("hidden"));
    assert!(!text.contains("private"));
    assert_eq!(
        resolve_in_scope(&graph, &view, &ex("A15:private"), &operation)
            .err()
            .unwrap()
            .code(),
        "scope-mismatch"
    );
}
#[test]
fn a1_alias_bindings_preserve_scope_and_do_not_merge_same_name_organizations() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let docs = [
        fixture("valid/common.ttl"),
        fixture("valid/descriptions.ttl"),
        fixture("valid/identifiers.ttl"),
    ];
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let view = ex("A1:offlineView");
    let declaration = supplied_view(
        &ex("A1"),
        &view,
        &[ex("A1:org1"), ex("A1:org2"), ex("r1")],
        &operation,
    );
    let mut input: Vec<_> = docs
        .iter()
        .map(|d| (d.as_slice(), oxrdfio::RdfFormat::Turtle))
        .collect();
    input.push((policy.as_bytes(), oxrdfio::RdfFormat::Turtle));
    input.push((&declaration, oxrdfio::RdfFormat::Turtle));
    let graph = Graph::load_documents(&input, &operation).unwrap();
    let result = resolve(&graph, &view, &operation).unwrap();
    assert!(
        result
            .comparison
            .assignments
            .iter()
            .all(|r| r.representative.as_ref() == Some(&r.entity))
    );
    assert_eq!(
        graph
            .one_iri(&ex("A1:alias1:binding"), &format!("{SR}entity"))
            .unwrap(),
        ex("A1:org1")
    );
    assert_eq!(
        graph
            .one_iri(&ex("A1:alias2:binding"), &format!("{SR}entity"))
            .unwrap(),
        ex("A1:org1")
    );
    assert_ne!(
        graph
            .one_iri(&ex("A1:alias1"), &format!("{SR}realm"))
            .unwrap(),
        graph
            .one_iri(&ex("A1:sameName"), &format!("{SR}realm"))
            .unwrap()
    );
}
#[test]
fn replay_never_asserts_same_as_or_described_business_triples() {
    with_resolution_graph(&[], |graph, operation| {
        let original = graph.records.clone();
        resolve(graph, &ex("A2:view"), operation).unwrap();
        assert_eq!(graph.records, original);
        assert!(
            !graph
                .records
                .values()
                .flatten()
                .any(|s| s.predicate == "http://www.w3.org/2002/07/owl#sameAs"
                    || s.predicate == format!("{SR}ControlsAccount"))
        );
    });
}
#[test]
fn local_cli_requires_explicit_inputs_and_returns_only_completed_comparison() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../");
    let mut args = vec![text("--view"), ex("A12:pair"), text("--scope"), ex("A12")];
    for path in [
        "testsuite/entity-representation/v1/valid/common.ttl",
        "testsuite/entity-representation/v1/valid/resolution.ttl",
        "crates/sparkles/resources/entity-representation/v1/policies/default.ttl",
    ] {
        args.extend([text("--rdf"), format!("ttl={}", root.join(path).display())]);
    }
    args.extend([
        text("--source"),
        format!(
            "{}={}",
            ex("r1"),
            root.join("testsuite/entity-representation/v1/raw/r1.json")
                .display()
        ),
    ]);
    assert_eq!(
        run_cli(&args).unwrap(),
        fixture("expected/a12-comparison.json")
    );
    let mut cancelled = args.clone();
    cancelled.extend([text("--deadline-ms"), text("0")]);
    assert_eq!(run_cli(&cancelled), Err(Failure::ReplayCancelled));
    let mut unknown = args;
    unknown.extend([text("--unknown"), text("hidden label")]);
    assert_eq!(run_cli(&unknown), Err(Failure::UnknownControlField));
    assert_eq!(run_cli(&[]), Err(Failure::IncompleteInput));
}

#[test]
fn regression_decoded_cap_covers_all_deliveries() {
    let control = Control::none();
    let bytes = b"<urn:record> <urn:value> \"value\" .\n";
    let operation = Operation::new(
        &control,
        Limits {
            decoded: bytes.len() * 2 - 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let graph =
        Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::NTriples)], &operation).unwrap();
    assert!(matches!(
        Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::NTriples)], &operation),
        Err(Failure::ReplayLimit)
    ));
    drop(graph);
    assert_eq!(operation.used(), 0);
}
#[test]
fn regression_record_cap_refuses_before_parsing_trailing_payload() {
    let control = Control::none();
    let operation = Operation::new(
        &control,
        Limits {
            entities: 0,
            ..Limits::default()
        },
    )
    .unwrap();
    let bytes = b"<urn:entity> a <urn:x-sparkles:representation:Entity> .\n this is malformed RDF";
    assert!(matches!(
        Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::Turtle)], &operation),
        Err(Failure::ReplayLimit)
    ));
    assert_eq!(operation.used(), 0);
}
#[test]
fn regression_missing_selected_observation_binding_refuses() {
    with_evolution_graph(|graph, operation| {
        let mut missing = candidates();
        missing.pop();
        assert!(matches!(
            select_observation(
                graph,
                &ex("A10:currentView"),
                &ex("A10:selectionProfile1"),
                &missing,
                operation
            ),
            Err(Failure::IncompleteInput)
        ));
        assert!(matches!(
            select_observation(
                graph,
                &ex("A10:oldView"),
                &ex("A10:selectionProfile1"),
                &missing,
                operation
            )
            .unwrap(),
            Selection::Known(_)
        ));
    });
}
#[test]
fn regression_missing_selected_withdrawal_refuses_instead_of_readmitting_support() {
    with_evolution_graph(|graph, operation| {
        graph.records.remove(&ex("A5:withdraw1"));
        assert!(matches!(
            admitted_occurrences(
                graph,
                &ex("A5:afterFirst:view"),
                &ex("A5:simulatedClaimAdmission:1"),
                1,
                operation
            ),
            Err(Failure::IncompleteInput)
        ));
    });
}
#[test]
fn regression_dangling_domain_record_reference_refuses() {
    let extra=b"PREFIX sr: <urn:x-sparkles:representation:>\n<urn:test:identification> a sr:Identification ; sr:profileVersion 1 ; sr:domainScope <urn:example:a01:A2> ; sr:identifier <urn:missing:identifier> ; sr:entity <urn:example:a01:A2:person> ; sr:claim <urn:example:a01:A2:control:claim> .";
    with_resolution_graph(extra, |graph, operation| {
        assert_eq!(
            graph.validate_control("urn:test:identification", "Identification", operation),
            Err(Failure::UnresolvedReference)
        );
    });
}
#[test]
fn regression_noncanonical_utc_spellings_never_compare_as_earlier_instants() {
    with_evolution_graph(|graph, operation| {
        let revision = ex("late");
        let predicate = format!("{EV}firstAvailableAt");
        graph
            .records
            .get_mut(&revision)
            .unwrap()
            .retain(|s| s.predicate != predicate);
        graph.records.get_mut(&revision).unwrap().insert(Statement {
            graph: "DEFAULT".into(),
            predicate,
            object: oxrdf::Literal::new_typed_literal(
                "2026-03-01 23:00:00Z",
                oxrdf::NamedNode::new("http://www.w3.org/2001/XMLSchema#dateTime").unwrap(),
            )
            .into(),
        });
        assert_eq!(
            simulated_available(graph, &revision, "2026-03-01T00:00:00Z", operation).unwrap(),
            Selection::Unknown("UnsupportedAvailabilityTime")
        );
    });
}
#[test]
fn regression_cli_verifies_selected_evidence_without_reference_flag() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let temp = tempfile::tempdir().unwrap();
    let scope = ex("A15:public");
    let view = ex("A15:publicView");
    let reference = ex("A15:descriptionClaim:publicDescription:ref");
    let declaration = supplied_view(
        &scope,
        &view,
        &[ex("A15:publicEntity"), ex("public"), reference],
        &operation,
    );
    let common = String::from_utf8(fixture("valid/public-common.ttl"))
        .unwrap()
        .replace(
            "sha256:163e03548d3cb2ea8a0355c0f32e2d47d0c7e006a06359a2bb4a872bf9c122b9",
            &format!("sha256:{}", "0".repeat(64)),
        );
    let public = fixture("valid/public-boundary.ttl");
    let paths = [
        ("common.ttl", common.as_bytes()),
        ("public.ttl", public.as_slice()),
        ("view.ttl", declaration.as_slice()),
        (
            "policy.ttl",
            sparkles::shared_entity::profile(1)
                .unwrap()
                .policy
                .as_bytes(),
        ),
    ];
    let mut args = vec![text("--view"), view, text("--scope"), scope];
    for (name, bytes) in paths {
        let path = temp.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        args.extend([text("--rdf"), format!("ttl={}", path.display())]);
    }
    let source = temp.path().join("source.json");
    std::fs::write(&source, fixture("raw/public.json")).unwrap();
    args.extend([
        text("--source"),
        format!("{}={}", ex("public"), source.display()),
    ]);
    assert_eq!(run_cli(&args), Err(Failure::SourceDigestMismatch));
}

#[test]
fn exact_cumulative_delivery_budget_and_repeated_graph_types_are_supported() {
    let bytes=b"<urn:entity> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:x-sparkles:representation:Entity> .\n<urn:entity> <http://www.w3.org/1999/02/22-rdf-syntax-ns#type> <urn:x-sparkles:representation:Entity> <urn:named> .\n";
    let control = Control::none();
    let operation = Operation::new(
        &control,
        Limits {
            decoded: bytes.len() * 2,
            entities: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut graph =
        Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::NQuads)], &operation).unwrap();
    let delivery =
        Graph::load_documents(&[(bytes, oxrdfio::RdfFormat::NQuads)], &operation).unwrap();
    graph.merge_delivery(delivery, &operation).unwrap();
    assert_eq!(graph.records.len(), 1);
    assert_eq!(graph.records["urn:entity"].len(), 2);
}
#[test]
fn wrong_class_and_missing_domain_dependencies_have_supported_siblings() {
    for (kind, fields) in [
        (
            "Identification",
            "sr:identifier <urn:example:a01:A2:person> ; sr:entity <urn:example:a01:A2:person> ; sr:claim <urn:example:a01:A2:control:claim>",
        ),
        (
            "Identification",
            "sr:identifier <urn:missing:identifier> ; sr:entity <urn:missing:entity> ; sr:claim <urn:missing:claim>",
        ),
        (
            "Relation",
            "sr:subject <urn:example:a01:A2:person> ; sr:object <urn:example:a01:A2:account> ; sr:relationKind sr:ControlsAccount ; sr:claim <urn:missing:claim>",
        ),
        (
            "Relation",
            "sr:subject <urn:example:a01:A2:person> ; sr:object <urn:example:a01:A2:account> ; sr:relationKind sr:ControlsAccount ; sr:claim <urn:example:a01:A2:control:claim> ; sr:roleAssignment <urn:missing:role>",
        ),
        (
            "RoleAssignment",
            "sr:participant <urn:missing:participant> ; sr:role <urn:role>",
        ),
        (
            "Description",
            "sr:sourceRevision <urn:example:a01:r1> ; sr:selection <urn:missing:selection>",
        ),
    ] {
        let extra = format!(
            "PREFIX sr: <{SR}>\n<urn:test:record> a sr:{kind} ; sr:profileVersion 1 ; sr:domainScope <urn:example:a01:A2> ; {fields} ."
        );
        // RoleAssignment has no domainScope property in its closed shape.
        let extra = if kind == "RoleAssignment" {
            extra.replace("sr:domainScope <urn:example:a01:A2> ; ", "")
        } else {
            extra
        };
        with_resolution_graph(extra.as_bytes(), |graph, operation| {
            assert_eq!(
                graph.validate_control("urn:test:record", kind, operation),
                Err(Failure::UnresolvedReference)
            );
        });
    }
    with_resolution_graph(&[], |graph, operation| {
        graph
            .validate_control(&ex("A2:control"), "Relation", operation)
            .unwrap();
        graph
            .validate_control(&ex("A2:control:role"), "RoleAssignment", operation)
            .unwrap();
    });
}
#[test]
fn mandatory_description_selection_is_verified_without_reference_id_conventions() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let common = fixture("valid/public-common.ttl");
    let public = fixture("valid/public-boundary.ttl");
    let declaration = supplied_view(
        &ex("A15:public"),
        &ex("A15:view"),
        &[
            ex("A15:publicEntity"),
            ex("A15:publicDescription"),
            ex("public"),
        ],
        &operation,
    );
    let mut graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&public, oxrdfio::RdfFormat::Turtle),
            (&declaration, oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    let mut retained = RetainedSources::default();
    retained
        .insert(
            &graph,
            &ex("public"),
            fixture("raw/public.json").as_slice(),
            &operation,
        )
        .unwrap();
    let manifest = ex("A15:view:manifest");
    retained
        .verify_selected(&graph, &manifest, &operation)
        .unwrap();
    let reference = ex("A15:descriptionClaim:publicDescription:ref");
    let original = graph.records[&reference].clone();
    for (field, value, error) in [
        (
            "valueDigest",
            oxrdf::Literal::new_simple_literal(format!("sha256:{}", "0".repeat(64))).into(),
            Failure::SourceDigestMismatch,
        ),
        (
            "method",
            oxrdf::NamedNode::new("urn:unsupported:normalization")
                .unwrap()
                .into(),
            Failure::UnsupportedProfile,
        ),
        (
            "jsonPointer",
            oxrdf::Literal::new_simple_literal("/missing").into(),
            Failure::InvalidDomainRecord,
        ),
    ] {
        let predicate = format!("{EV}{field}");
        graph
            .records
            .get_mut(&reference)
            .unwrap()
            .retain(|s| s.predicate != predicate);
        graph
            .records
            .get_mut(&reference)
            .unwrap()
            .insert(Statement {
                graph: "DEFAULT".into(),
                predicate,
                object: value,
            });
        assert_eq!(
            retained.verify_selected(&graph, &manifest, &operation),
            Err(error)
        );
        graph.records.insert(reference.clone(), original.clone());
    }
}
#[test]
fn missing_selected_record_and_unknown_matching_rule_refuse_with_fixed_codes() {
    let control = Control::none();
    let operation = Operation::new(&control, Limits::default()).unwrap();
    let common = fixture("valid/common.ttl");
    let source = fixture("valid/resolution.ttl");
    let policy = sparkles::shared_entity::profile(1).unwrap().policy;
    let mut graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&source, oxrdfio::RdfFormat::Turtle),
            (policy.as_bytes(), oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    graph.records.remove("https://example.org/A");
    assert_eq!(
        resolve(&graph, &ex("A12:pair"), &operation).err().unwrap(),
        Failure::IncompleteInput
    );
    let changed = policy.replace("AcceptedPossibleMatch", "UnknownFutureMatch");
    let graph = Graph::load_documents(
        &[
            (&common, oxrdfio::RdfFormat::Turtle),
            (&source, oxrdfio::RdfFormat::Turtle),
            (changed.as_bytes(), oxrdfio::RdfFormat::Turtle),
        ],
        &operation,
    )
    .unwrap();
    assert_eq!(
        resolve(&graph, &ex("A12:pair"), &operation).err().unwrap(),
        Failure::UnsupportedProfile
    );
}
