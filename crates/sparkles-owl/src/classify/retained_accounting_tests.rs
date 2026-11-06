use super::*;
use std::sync::{Arc, Mutex};

fn axioms(body: &str) -> Vec<Axiom> {
    crate::ofn_read::parse(&format!(
        "Prefix(:=<urn:retained:>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(\n{body}\n)"
    ))
    .expect("test ontology")
    .axioms
}

fn numeric(n: usize) -> Vec<Axiom> {
    let mut body = String::from("Declaration(Class(:B))\n");
    for i in 0..n {
        body.push_str(&format!(
            "SubClassOf(:C{i} DataHasValue(:d \"{i}\"^^xsd:integer))\n\
             SubClassOf(DataHasValue(:d \"{i}.0\"^^xsd:decimal) :B)\n"
        ));
    }
    axioms(&body)
}

fn same_answer(a: &Classification, b: &Classification) {
    assert!(a.complete && b.complete);
    assert!(a.incomplete.is_empty() && b.incomplete.is_empty());
    assert_eq!(a.consistency, b.consistency);
    let names = |out: &Classification| {
        out.taxonomy
            .nodes()
            .iter()
            .flat_map(|n| n.classes.iter().cloned())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let classes = names(a);
    assert_eq!(classes, names(b));
    for sub in &classes {
        for sup in &classes {
            assert_eq!(
                a.taxonomy.is_subclass(sub, sup),
                b.taxonomy.is_subclass(sub, sup)
            );
        }
    }
    assert_eq!(
        a.taxonomy.unsatisfiable().collect::<Vec<_>>(),
        b.taxonomy.unsatisfiable().collect::<Vec<_>>()
    );
}

#[test]
fn retained_estimates_agree_for_fresh_and_unchanged_reuse() {
    let fixtures = [
        axioms("SubClassOf(:A :B)\nSubClassOf(:B :C)"),
        numeric(16),
        numeric(1024),
        axioms(
            "FunctionalDataProperty(:d)\n\
             SubClassOf(:A DataHasValue(:d \"1\"^^xsd:integer))\n\
             SubClassOf(:A DataHasValue(:d \"2\"^^xsd:integer))",
        ),
        axioms(
            "HasKey(:K () (:d))\nClassAssertion(:K :a)\nClassAssertion(:K :b)\n\
             DataPropertyAssertion(:d :a \"1\"^^xsd:integer)\n\
             DataPropertyAssertion(:d :b \"1.0\"^^xsd:decimal)\n\
             DifferentIndividuals(:a :b)",
        ),
        axioms(
            "SubClassOf(:A ObjectSomeValuesFrom(:r :B))\n\
             SubClassOf(:A ObjectAllValuesFrom(:r :C))\n\
             SubClassOf(ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)) :D)",
        ),
        axioms(
            "SubClassOf(:A ObjectUnionOf(:B :C))\n\
             SubClassOf(:B :D)\nSubClassOf(:C :D)",
        ),
    ];
    for (fixture, input) in fixtures.iter().enumerate() {
        let limits = Limits {
            threads: 1,
            ..Limits::default()
        };
        let mut reused = Classifier::of(input);
        let saturated = reused.saturate(&limits).expect("saturation");
        let actual = reused
            .classify(&limits)
            .expect("same-object classification");
        assert!(reused.excluded().is_empty());
        let mut fresh = Classifier::of(input);
        let expected = fresh.classify(&limits).expect("fresh classification");
        assert!(fresh.excluded().is_empty());
        same_answer(&actual, &expected);
        assert_eq!(
            actual.stats.memory, expected.stats.memory,
            "fixture {fixture}"
        );
        let again = reused.classify(&limits).expect("repeated classification");
        same_answer(&again, &expected);
        assert_eq!(
            again.stats.memory, expected.stats.memory,
            "fixture {fixture}"
        );
        let independently_saturated = Classifier::of(input).saturate(&limits).unwrap();
        assert_eq!(saturated.stats.memory, independently_saturated.stats.memory);
        for stats in [
            &saturated.stats,
            &actual.stats,
            &expected.stats,
            &again.stats,
        ] {
            assert!(stats.peak_memory >= stats.memory);
        }
        let class = |name: &str| Class::new(format!("urn:retained:{name}"));
        match fixture {
            0 => {
                assert!(expected.taxonomy.is_subclass(&class("A"), &class("C")));
                assert!(!expected.taxonomy.is_subclass(&class("C"), &class("A")));
            }
            1 | 2 => {
                assert!(expected.taxonomy.is_subclass(&class("C0"), &class("B")));
                assert!(!expected.taxonomy.is_subclass(&class("B"), &class("C0")));
            }
            3 => assert!(expected.taxonomy.unsatisfiable().any(|c| c == &class("A"))),
            4 => assert!(!expected.consistent),
            5 | 6 => assert!(expected.taxonomy.is_subclass(&class("A"), &class("D"))),
            _ => unreachable!(),
        }
    }
}

#[test]
fn retained_estimate_after_refusal_matches_fresh() {
    let input = numeric(16);
    let unrestricted = Limits {
        threads: 1,
        ..Limits::default()
    };
    let fresh = Classifier::of(&input).classify(&unrestricted).unwrap();
    for pool in [false, true] {
        let mut classifier = Classifier::of(&input);
        let bounded = Limits {
            threads: 1,
            max_memory: (!pool).then_some(0),
            charge: pool.then(|| Arc::new(|_| false) as Arc<dyn Fn(usize) -> bool + Send + Sync>),
            ..Limits::default()
        };
        assert!(matches!(
            classifier.saturate(&bounded),
            Err(Error::Memory { .. })
        ));
        let reused = classifier.classify(&unrestricted).unwrap();
        same_answer(&reused, &fresh);
        assert_eq!(reused.stats.memory, fresh.stats.memory);
    }
}

#[test]
fn retained_reconciliation_keeps_peak_credit_and_enforces_both_bounds() {
    let grants = Arc::new(Mutex::new(Vec::new()));
    let observed = grants.clone();
    let limits = Limits {
        charge: Some(Arc::new(move |n| {
            observed.lock().unwrap().push(n);
            true
        })),
        ..Limits::default()
    };
    assert_eq!(Saturation::admit_retained(80, 100, &limits).unwrap(), 100);
    assert!(grants.lock().unwrap().is_empty());
    assert_eq!(Saturation::admit_retained(120, 100, &limits).unwrap(), 120);
    assert_eq!(*grants.lock().unwrap(), [20]);
    let hard = Limits {
        max_memory: Some(119),
        ..limits.clone()
    };
    assert!(matches!(
        Saturation::admit_retained(120, 100, &hard),
        Err(Error::Memory {
            limit: 119,
            used: 120
        })
    ));
    assert_eq!(
        *grants.lock().unwrap(),
        [20],
        "hard refusal must not charge the pool"
    );
    let pool = Limits {
        charge: Some(Arc::new(|_| false)),
        ..Limits::default()
    };
    assert!(matches!(
        Saturation::admit_retained(120, 100, &pool),
        Err(Error::Memory {
            limit: usize::MAX,
            used: 120
        })
    ));
}
