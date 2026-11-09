//! Counter regressions over actual Interner insertion and public serial saturation.
//! This file is byte-identical on K and candidate. Values are prepared through the
//! common Ranges::literal API; no new-number representation is assumed by the probe.
use super::*;
use crate::classify::a1_probe_alloc::{Sample, measure};
use crate::classify::{Classifier, Limits};
use std::mem::size_of;

const SIZES: [usize; 3] = [1024, 2048, 4096];

#[derive(Debug)]
struct CapacityEvent {
    inserted: usize,
    before: usize,
    after: usize,
}
struct GrowthRow {
    n: usize,
    vector: Vec<CapacityEvent>,
    map: Vec<CapacityEvent>,
    sample: Sample,
}

fn prepared_values(n: usize) -> Vec<DataVal> {
    let mut ranges = Ranges::default();
    (0..n)
        .map(|i| {
            let lexical = i.to_string();
            let (value, opaque) =
                ranges.literal(&Literal::typed(lexical, &format!("{}integer", vocab::XSD)));
            assert!(
                !opaque && value.single().is_some(),
                "small value preparation {i}"
            );
            value
        })
        .collect()
}

#[test]
fn r3_interner_growth_is_geometric_at_required_sizes() {
    let mut rows = Vec::with_capacity(SIZES.len());
    for n in SIZES {
        let prepared = prepared_values(n);
        let mut interner = Interner::new();
        // Both observers are fully allocated before enabling allocation measurement.
        let mut vector = Vec::with_capacity(n);
        let mut map = Vec::with_capacity(n);
        let (_, sample) = measure(|| {
            for (i, value) in prepared.iter().enumerate() {
                let (vc, mc) = (interner.vals.capacity(), interner.val_ids.capacity());
                let id = interner.val(value.clone());
                assert_eq!(
                    id, i as u32,
                    "insertion was refused or deduplicated unexpectedly"
                );
                if vc != interner.vals.capacity() {
                    vector.push(CapacityEvent {
                        inserted: i + 1,
                        before: vc,
                        after: interner.vals.capacity(),
                    });
                }
                if mc != interner.val_ids.capacity() {
                    map.push(CapacityEvent {
                        inserted: i + 1,
                        before: mc,
                        after: interner.val_ids.capacity(),
                    });
                }
            }
        });
        assert_eq!(interner.vals.len(), n);
        assert_eq!(interner.val_ids.len(), n);
        for (i, value) in prepared.iter().enumerate() {
            assert_eq!(&interner.vals[i], value);
            assert_eq!(
                interner.val(value.clone()),
                i as u32,
                "duplicate identity changed"
            );
        }
        assert_eq!(
            interner.vals.len(),
            n,
            "semantic control must retain all distinct values"
        );
        for event in vector.iter().chain(&map) {
            assert!(
                event.inserted > 0 && event.inserted <= n && event.after > event.before,
                "invalid capacity observer record: {event:?}"
            );
        }
        // Prepared inputs, interner and observer buffers remain alive until after the
        // snapshot. No pre-region input or observer storage is freed while measured.
        println!(
            "A1_R3_GROWTH n={n} retained={n} data_val_size={} vector_growths={} map_growths={} sample={sample:?} vector={vector:?} map={map:?}",
            size_of::<DataVal>(),
            vector.len(),
            map.len()
        );
        rows.push(GrowthRow {
            n,
            vector,
            map,
            sample,
        });
    }
    // Assert only after all three sizes and their raw capacity traces are recorded.
    let failures: Vec<_> = rows
        .iter()
        .filter_map(|r| {
            let log = usize::BITS as usize - (r.n - 1).leading_zeros() as usize;
            let vector_limit = log + 3;
            let total_limit = 4 * log + 16;
            let good = r.vector.len() <= vector_limit
                && r.map.len() <= log + 4
                && r.sample.events <= total_limit;
            (!good).then_some((
                r.n,
                r.vector.len(),
                vector_limit,
                r.map.len(),
                r.sample.events,
                total_limit,
                r.sample.realloc_events,
            ))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "A1_EXPECTED_R3_GROWTH_REGRESSION {failures:?}"
    );
}

fn class(i: usize) -> Class {
    Class::new(format!("urn:a1-r3:A{i:04}"))
}
fn b() -> Class {
    Class::new("urn:a1-r3:B")
}
fn c() -> Class {
    Class::new("urn:a1-r3:C")
}
fn ontology(n: usize) -> Vec<Axiom> {
    let mut axioms = Vec::with_capacity(2 * n + 1);
    axioms.push(Axiom::new(AxiomKind::Declaration(Entity::Class(c()))));
    for i in 0..n {
        let literal = Literal::typed(i.to_string(), &format!("{}integer", vocab::XSD));
        axioms.push(Axiom::sub_class_of(
            class(i),
            ClassExpression::DataHasValue(DataProperty::new(format!("urn:a1-r3:d{i:04}")), literal),
        ));
        axioms.push(Axiom::sub_class_of(class(i), b()));
    }
    axioms
}
fn semantic_control(axioms: &[Axiom], n: usize, limits: &Limits) {
    let mut fresh = Classifier::of(axioms);
    let out = fresh.classify(limits).unwrap();
    assert!(out.consistent && out.complete);
    assert!(fresh.excluded().is_empty());
    assert!(!out.stats.horn_over_budget && !out.stats.dl_over_budget);
    assert_eq!(out.taxonomy.unsatisfiable().count(), 0);
    let actual_signature: std::collections::BTreeSet<_> = out
        .taxonomy
        .nodes()
        .iter()
        .flat_map(|node| node.classes.iter().cloned())
        .collect();
    let expected_signature: std::collections::BTreeSet<_> = (0..n)
        .map(class)
        .chain([b(), c(), Class::thing(), Class::nothing()])
        .collect();
    assert_eq!(
        actual_signature, expected_signature,
        "unexpected named taxonomy signature"
    );
    assert_eq!(out.taxonomy.equivalents(&b()), &[b()]);
    assert_eq!(out.taxonomy.equivalents(&c()), &[c()]);
    for i in 0..n {
        let a = class(i);
        assert_eq!(out.taxonomy.equivalents(&a), std::slice::from_ref(&a));
        let parents: Vec<_> = out
            .taxonomy
            .parents(&a)
            .iter()
            .flat_map(|&id| out.taxonomy.node(id).classes.iter().cloned())
            .collect();
        assert_eq!(parents, vec![b()]);
        assert!(out.taxonomy.is_subclass(&a, &b()));
        assert!(!out.taxonomy.is_subclass(&a, &c()));
        assert!(!out.taxonomy.is_subclass(&a, &class((i + 1) % n)));
    }
    for name in [b(), c()] {
        let parents: Vec<_> = out
            .taxonomy
            .parents(&name)
            .iter()
            .flat_map(|&id| out.taxonomy.node(id).classes.iter().cloned())
            .collect();
        assert_eq!(parents, vec![Class::thing()]);
    }
    println!(
        "A1_R3_SEMANTICS n={n} complete=true consistent=true exclusions=0 unsatisfiable=0 all_A_have_only_B_parent=true B_and_C_have_only_Thing_parent=true"
    );
}

#[test]
fn r3_accounting_visits_stay_linear_for_public_serial_saturation() {
    let mut rows = Vec::with_capacity(SIZES.len());
    for n in SIZES {
        let axioms = ontology(n);
        let prepared = prepared_values(n);
        let mut classifier = Classifier::of(&axioms);
        let limits = Limits {
            threads: 1,
            el_only: true,
            ..Limits::default()
        };
        // Inputs, classifier and the fixed three-row observer are prepared. Only the
        // real ownership traversal is counted; no allocating observer or test loop is
        // inserted into K's constant-time bytes implementation.
        a1_reset_accounting_visits();
        let saturated = classifier.saturate(&limits).unwrap();
        let visits = a1_accounting_visits();
        assert!(saturated.consistent);
        assert!(classifier.excluded().is_empty());
        assert_eq!(saturated.stats.taxonomy_memory, 0);
        assert!(!saturated.stats.horn_over_budget && !saturated.stats.dl_over_budget);
        assert_eq!(classifier.sat.interner.vals.len(), n);
        assert_eq!(classifier.sat.interner.val_ids.len(), n);
        for value in &prepared {
            assert!(classifier.sat.interner.val_ids.contains_key(value));
        }
        println!(
            "A1_R3_VISITS n={n} visits={visits} limit={} retained_values={n} axioms={} threads=1 taxonomy_memory=0",
            16 * n,
            axioms.len()
        );
        // Untimed and outside the visit snapshot, verify the full named taxonomy,
        // consistency, no exclusions and actual retained values independently.
        semantic_control(&axioms, n, &limits);
        rows.push((n, visits));
    }
    let failures: Vec<_> = rows
        .iter()
        .copied()
        .filter(|(n, visits)| *visits > 16 * n)
        .collect();
    assert!(
        failures.is_empty(),
        "A1_EXPECTED_R3_SCAN_REGRESSION {failures:?}"
    );
}
