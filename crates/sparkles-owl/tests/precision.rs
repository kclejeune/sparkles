//! Exact-precision regressions. These use only pre-A1 public APIs, so this test file
//! and its fixture directory can be overlaid on exact K for a compilable red run.
use sparkles_owl::classify::{Changes, Classifier, Error, Limits, Saturation};
use sparkles_owl::{model::*, ofn_read, vocab};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn class(name: &str) -> Class {
    Class::new(format!("urn:precision663:{name}"))
}
fn fixture(name: &str) -> Vec<Axiom> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/precision663")
        .join(format!("{name}.ofn"));
    ofn_read::parse(&std::fs::read_to_string(path).unwrap())
        .unwrap()
        .axioms
}
fn manifest() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/precision663/manifest.json")).unwrap()
}

#[test]
fn precision_cases_are_whole_and_have_the_hand_derived_answers() {
    let inventory = manifest();
    assert_eq!(inventory["cases"].as_array().unwrap().len(), 17);
    for case in inventory["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let axioms = fixture(name);
        for threads in [1, 4, 8] {
            let mut classifier = Classifier::of(&axioms);
            let out = classifier
                .classify(&Limits {
                    threads,
                    ..Limits::default()
                })
                .unwrap();
            let expected = &case["expected"];
            assert!(out.complete, "{name}/{threads} stopped");
            assert!(
                classifier.excluded().is_empty(),
                "{name}/{threads}: {:?}",
                classifier.excluded()
            );
            assert_eq!(
                out.consistent,
                expected["consistent"].as_bool().unwrap(),
                "{name}/{threads}"
            );
            for pair in expected["entails"].as_array().unwrap() {
                assert!(
                    out.taxonomy.is_subclass(
                        &class(pair[0].as_str().unwrap()),
                        &class(pair[1].as_str().unwrap())
                    ),
                    "{name}/{threads} missing {pair}"
                );
            }
            for pair in expected["not_entails"].as_array().unwrap() {
                assert!(
                    !out.taxonomy.is_subclass(
                        &class(pair[0].as_str().unwrap()),
                        &class(pair[1].as_str().unwrap())
                    ),
                    "{name}/{threads} extra {pair}"
                );
            }
            if out.consistent {
                let expected: BTreeSet<_> = expected["unsatisfiable"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|v| class(v.as_str().unwrap()))
                    .collect();
                let actual: BTreeSet<_> = out.taxonomy.unsatisfiable().cloned().collect();
                assert_eq!(actual, expected, "{name}/{threads} unsatisfiable signature");
            }
        }
    }
}

#[test]
fn incremental_add_remove_equals_fresh_exact_classification() {
    for case in manifest()["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let axioms = fixture(name);
        let all: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
        for threads in [1, 4, 8] {
            let limits = Limits {
                threads,
                ..Limits::default()
            };
            let mut sat = Saturation::new();
            sat.classify(&all, None, &limits).unwrap();
            // Removing each individual logical axiom exercises data invalidation and
            // reuse. Every intermediate ontology is checked against a fresh run.
            for i in 0..all.len() {
                let subset: Vec<_> = all
                    .iter()
                    .enumerate()
                    .filter(|(j, _)| *j != i)
                    .map(|(_, &a)| a)
                    .collect();
                let removed = [all[i]];
                let inc = sat
                    .classify(
                        &subset,
                        Some(Changes {
                            added: &[],
                            removed: &removed,
                        }),
                        &limits,
                    )
                    .unwrap();
                let mut fresh = Saturation::new();
                let expected = fresh.classify(&subset, None, &limits).unwrap();
                assert_eq!(
                    inc.taxonomy, expected.taxonomy,
                    "{name}/{threads}/remove{i}"
                );
                assert_eq!(inc.consistent, expected.consistent);
                assert_eq!(inc.complete, expected.complete);
                assert_eq!(sat.excluded(), fresh.excluded());
                let inc = sat
                    .classify(
                        &all,
                        Some(Changes {
                            added: &removed,
                            removed: &[],
                        }),
                        &limits,
                    )
                    .unwrap();
                let mut fresh = Saturation::new();
                let expected = fresh.classify(&all, None, &limits).unwrap();
                assert_eq!(inc.taxonomy, expected.taxonomy, "{name}/{threads}/add{i}");
                assert_eq!(inc.consistent, expected.consistent);
                assert_eq!(inc.complete, expected.complete);
                assert_eq!(sat.excluded(), fresh.excluded());
            }
        }
    }
}

#[test]
fn opaque_controls_and_small_values_keep_their_contract() {
    for (text, dt) in [
        ("1e2", format!("{}decimal", vocab::XSD)),
        ("1/0", vocab::OWL_RATIONAL.to_string()),
        ("--1", format!("{}integer", vocab::XSD)),
        ("true", format!("{}boolean", vocab::XSD)),
        ("1.0", format!("{}float", vocab::XSD)),
        ("x", "urn:unknown-datatype".to_string()),
    ] {
        let ax = Axiom::sub_class_of(
            class("A"),
            ClassExpression::DataHasValue(
                DataProperty::new("urn:precision663:d"),
                Literal::typed(text, &dt),
            ),
        );
        let mut k = Classifier::of(&[ax]);
        k.classify(&Limits::default()).unwrap();
        assert!(!k.excluded().is_empty(), "{text} {dt}");
    }
    for (left, right) in [
        ("-0", "+0.000"),
        ("1", "1.0"),
        (
            "-170141183460469231731687303715884105728",
            "-170141183460469231731687303715884105728.0",
        ),
    ] {
        let text = format!(
            "Prefix(:=<urn:precision663:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(SubClassOf(:A DataHasValue(:d \"{left}\"^^xsd:integer)) SubClassOf(DataHasValue(:d \"{right}\"^^xsd:decimal) :B))"
        );
        let doc = ofn_read::parse(&text).unwrap();
        let mut k = Classifier::of(&doc.axioms);
        let out = k.classify(&Limits::default()).unwrap();
        assert!(k.excluded().is_empty());
        assert!(out.taxonomy.is_subclass(&class("A"), &class("B")));
    }
}

#[test]
fn memory_and_pool_refusal_are_errors_and_the_same_classifier_is_reusable() {
    let axioms = fixture("integer-decimal");
    let mut k = Classifier::of(&axioms);
    assert!(matches!(
        k.classify(&Limits {
            max_memory: Some(1024),
            ..Limits::default()
        }),
        Err(Error::Memory { .. })
    ));
    assert!(
        k.classify(&Limits::default())
            .unwrap()
            .taxonomy
            .is_subclass(&class("A"), &class("B"))
    );
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let limits = Limits {
        charge: Some(Arc::new(move |bytes| {
            let total = g.fetch_add(bytes, Ordering::Relaxed) + bytes;
            total < 1_000_000
        })),
        ..Limits::default()
    };
    assert!(matches!(k.classify(&limits), Err(Error::Memory { .. })));
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.consistent && out.complete);
    assert!(k.excluded().is_empty());
}

#[test]
fn horn_numeric_refusal_cannot_become_successful_el_fallback() {
    // The only large number occurs inside a Horn axiom outside EL. EL fallback
    // would omit that axiom and could otherwise appear to recover successfully.
    let doc = ofn_read::parse("Prefix(:=<urn:precision663:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(SubClassOf(:A ObjectAllValuesFrom(:r DataHasValue(:d \"170141183460469231731687303715884105728\"^^xsd:integer))))").unwrap();
    let mut k = Classifier::of(&doc.axioms);
    let actual = k.classify(&Limits {
        max_memory: Some(1_000_000),
        ..Limits::default()
    });
    assert!(
        matches!(&actual, Err(Error::Memory { .. })),
        "A1_R1_MEMORY_DIAGNOSTIC expected Memory, actual={actual:?}; excluded={:?}",
        k.excluded()
    );
    drop(actual);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.complete && out.consistent);
    assert!(k.excluded().is_empty());
    assert!(!out.stats.horn_over_budget && !out.stats.dl_over_budget);
}

#[test]
fn horn_numeric_pool_refusal_reports_memory_and_reuses_the_classifier() {
    // Only the Horn axiom contains a large number. The observer and its entire
    // bounded storage are allocated before classification and never grow inside
    // the charge callback. No numeric-halt guard is repaired by this test overlay.
    let doc = ofn_read::parse("Prefix(:=<urn:precision663:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(SubClassOf(:A ObjectAllValuesFrom(:r DataHasValue(:d \"170141183460469231731687303715884105728\"^^xsd:integer))))").unwrap();
    let mut classifier = Classifier::of(&doc.axioms);
    let events = Arc::new(std::sync::Mutex::new(Vec::<(usize, bool)>::with_capacity(
        256,
    )));
    let refused = Arc::new(AtomicUsize::new(0));
    let (record, rejects) = (events.clone(), refused.clone());
    let limits = Limits {
        threads: 1,
        charge: Some(Arc::new(move |bytes| {
            let accepted = bytes < 1_000_000;
            if !accepted {
                rejects.fetch_add(1, Ordering::Relaxed);
            }
            let mut observer = record.lock().unwrap();
            assert!(
                observer.len() < 256,
                "invalid probe: bounded charge observer exhausted"
            );
            observer.push((bytes, accepted));
            accepted
        })),
        ..Limits::default()
    };
    let actual = classifier.classify(&limits);
    let rejected = refused.load(Ordering::Relaxed);
    println!(
        "A1_R1_POOL refused={rejected} events={:?} actual={actual:?} excluded={:?}",
        events.lock().unwrap(),
        classifier.excluded()
    );
    assert!(
        rejected > 0,
        "invalid probe: numeric-size pool refusal was not reached"
    );
    assert!(
        matches!(&actual, Err(Error::Memory { .. })),
        "A1_EXPECTED_R1_POOL_REFUSAL_REGRESSION expected original Memory error, actual={actual:?}"
    );
    drop(actual);
    let unrestricted = classifier
        .classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .unwrap();
    assert!(unrestricted.complete && unrestricted.consistent);
    assert!(classifier.excluded().is_empty());
    assert!(!unrestricted.stats.horn_over_budget && !unrestricted.stats.dl_over_budget);
    println!("A1_R1_POOL_REUSE complete=true consistent=true exclusions=0 fallback=false");
}

#[test]
fn ordinary_normalization_before_numeric_refusal_keeps_accounting_and_reuse() {
    const N: usize = 1024;
    let mut text = String::from(
        "Prefix(:=<urn:precision663:>) Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>) Ontology(",
    );
    for i in 0..N {
        text.push_str(&format!("SubClassOf(:C{i} :D)\n"));
    }
    text.push_str("SubClassOf(:A DataHasValue(:d \"170141183460469231731687303715884105728\"^^xsd:integer))\nSubClassOf(DataHasValue(:d \"170141183460469231731687303715884105728.0\"^^xsd:decimal) :B))");
    let doc = ofn_read::parse(&text).unwrap();
    let refs: Vec<_> = doc.axioms.iter().map(|a| &a.kind).collect();
    assert_eq!(refs.len(), N + 2);
    // The public Saturation API preserves this deliberate order. Classifier's held
    // axioms are a hash map, which would not prove ordinary work precedes the number.
    let mut classifier = Saturation::new();
    let events = Arc::new(std::sync::Mutex::new(Vec::<(usize, bool)>::with_capacity(
        4 * N + 256,
    )));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (record, grants) = (events.clone(), accepted.clone());
    let actual = classifier.classify(
        &refs,
        None,
        &Limits {
            threads: 1,
            charge: Some(Arc::new(move |bytes| {
                let allow = bytes < 1_000_000;
                let mut observer = record.lock().unwrap();
                assert!(
                    observer.len() < observer.capacity(),
                    "bounded mixed-input observer exhausted"
                );
                observer.push((bytes, allow));
                if allow {
                    grants.fetch_add(bytes, Ordering::Relaxed);
                }
                allow
            })),
            ..Limits::default()
        },
    );
    let observed = events.lock().unwrap();
    let refusals: Vec<_> = observed
        .iter()
        .enumerate()
        .filter(|(_, (_, ok))| !ok)
        .collect();
    assert_eq!(
        refusals.len(),
        1,
        "actual numeric-size refusal must occur once"
    );
    let (at, &(refused_bytes, _)) = refusals[0];
    assert_eq!(
        at + 1,
        observed.len(),
        "no later grant may mask a numeric halt"
    );
    assert!(
        at >= N,
        "ordinary normalization must refresh admission before the number"
    );
    assert!(refused_bytes >= 1_000_000);
    let accepted_bytes = accepted.load(Ordering::Relaxed);
    match actual {
        Err(Error::Memory { used, .. }) => assert_eq!(
            used,
            accepted_bytes + refused_bytes,
            "ordinary retained accounting must be included in the numeric workspace request"
        ),
        other => panic!("mixed numeric refusal must return original Memory, actual={other:?}"),
    }
    println!(
        "A1_MIXED_REFRESH ordinary={N} prior_grants={at} accepted={accepted_bytes} refused={refused_bytes} original_memory=true"
    );
    drop(observed);
    let out = classifier
        .classify(
            &refs,
            None,
            &Limits {
                threads: 1,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(out.complete && out.consistent);
    assert!(classifier.excluded().is_empty());
    assert!(!out.stats.horn_over_budget && !out.stats.dl_over_budget);
    assert_eq!(out.taxonomy.unsatisfiable().count(), 0);
    let signature: BTreeSet<_> = out
        .taxonomy
        .nodes()
        .iter()
        .flat_map(|node| node.classes.iter().cloned())
        .collect();
    let expected: BTreeSet<_> = (0..N)
        .map(|i| class(&format!("C{i}")))
        .chain([
            class("A"),
            class("B"),
            class("D"),
            Class::thing(),
            Class::nothing(),
        ])
        .collect();
    assert_eq!(signature, expected);
    for i in 0..N {
        let c = class(&format!("C{i}"));
        assert_eq!(out.taxonomy.equivalents(&c), std::slice::from_ref(&c));
        let parents: Vec<_> = out
            .taxonomy
            .parents(&c)
            .iter()
            .flat_map(|&id| out.taxonomy.node(id).classes.iter().cloned())
            .collect();
        assert_eq!(parents, vec![class("D")]);
        assert!(!out.taxonomy.is_subclass(&c, &class("B")));
    }
    for (c, parent) in [
        (class("A"), class("B")),
        (class("B"), Class::thing()),
        (class("D"), Class::thing()),
    ] {
        assert_eq!(out.taxonomy.equivalents(&c), std::slice::from_ref(&c));
        let parents: Vec<_> = out
            .taxonomy
            .parents(&c)
            .iter()
            .flat_map(|&id| out.taxonomy.node(id).classes.iter().cloned())
            .collect();
        assert_eq!(parents, vec![parent]);
    }
    assert!(!out.taxonomy.is_subclass(&class("A"), &class("D")));
    println!(
        "A1_MIXED_REFRESH_REUSE ordinary={N} complete=true consistent=true exclusions=0 full_hierarchy=true"
    );
}
