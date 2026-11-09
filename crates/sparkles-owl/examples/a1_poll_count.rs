//! One public classification of the exact existing 40,000-class Horn test input.
//! The cancellation callback only counts. There is no exhaustive cancellation loop.
use sparkles_owl::classify::{Classifier, Limits, Tier};
use sparkles_owl::model::Class;
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const N: usize = 40_000;
fn class(local: &str) -> Class {
    Class::new(format!("http://ex.org/{local}"))
}
fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("input OFN path");
    let output = args.next().expect("output JSON path");
    assert!(
        args.next().is_none(),
        "expected exactly input and output paths"
    );
    let text = std::fs::read_to_string(&input).expect("read fixed input");
    let doc = ofn_read::parse(&text).expect("parse fixed input");
    assert_eq!(doc.axioms.len(), N + 1);
    let mut classifier = Classifier::of(&doc.axioms);
    let counter = Arc::new(AtomicUsize::new(0));
    let observed = counter.clone();
    let limits = Limits {
        threads: 1,
        cancelled: Some(Arc::new(move || {
            observed.fetch_add(1, Ordering::Relaxed);
            false
        })),
        ..Limits::default()
    };
    // Exactly one classification per invocation on both versions. Construct/parse
    // the input before enabling the public callback; check its result afterwards.
    let out = classifier.classify(&limits).expect("classification");
    let polls = counter.load(Ordering::Relaxed);
    println!(
        "A1_PROVISIONAL_PUBLIC_POLL_COUNT {}",
        serde_json::json!({"polls": polls, "classification_calls": 1, "semantic_controls_passed": false})
    );
    assert!(polls > 0, "public cancellation callback was never reached");
    assert!(out.consistent && out.complete);
    assert_eq!(out.stats.tier, Tier::Horn);
    assert!(!out.stats.horn_over_budget && !out.stats.dl_over_budget);
    assert!(classifier.excluded().is_empty());
    assert_eq!(out.taxonomy.unsatisfiable().count(), 0);
    let actual: BTreeSet<_> = out
        .taxonomy
        .nodes()
        .iter()
        .flat_map(|node| node.classes.iter())
        .cloned()
        .collect();
    let expected: BTreeSet<_> = (0..N)
        .map(|i| class(&format!("C{i}")))
        .chain([
            class("D"),
            class("X"),
            class("Y"),
            Class::thing(),
            Class::nothing(),
        ])
        .collect();
    assert_eq!(actual, expected, "complete named-class signature");
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
        assert!(
            !out.taxonomy
                .is_subclass(&c, &class(&format!("C{}", (i + 1) % N)))
        );
        assert!(!out.taxonomy.is_subclass(&c, &class("X")));
        assert!(!out.taxonomy.is_subclass(&c, &class("Y")));
    }
    for c in [class("D"), class("X"), class("Y")] {
        assert_eq!(out.taxonomy.equivalents(&c), std::slice::from_ref(&c));
        let parents: Vec<_> = out
            .taxonomy
            .parents(&c)
            .iter()
            .flat_map(|&id| out.taxonomy.node(id).classes.iter().cloned())
            .collect();
        assert_eq!(parents, vec![Class::thing()]);
    }
    assert_eq!(
        counter.load(Ordering::Relaxed),
        polls,
        "result inspection polled again"
    );
    let result = serde_json::json!({
        "schema": "precision663-public-40k-poll-count/v1", "input": input,
        "input_axioms": N + 1, "subclass_inputs": N, "classification_calls": 1,
        "threads": 1, "polls": polls, "complete": true, "consistent": true,
        "tier": "Horn", "exclusions": 0, "fallback": false,
        "signature_classes": N + 5, "unsatisfiable_classes": 0,
        "all_C_have_only_D_parent": true, "D_X_Y_have_only_Thing_parent": true,
        "neighbor_X_Y_nonentailments_checked": true,
        "semantic_controls_passed": true
    });
    std::fs::write(&output, serde_json::to_vec_pretty(&result).unwrap()).expect("write receipt");
    println!("A1_PUBLIC_POLL_COUNT {}", result);
}
