//! Row 1231's marker repair diagnostic under the containment arm, with the selection of
//! ground literals off (`SPARKLES_ROOTED_SELECT_GROUND=0`), which the process reads once,
//! so this binary holds one test. A returned clause whose products with the holder's
//! premises put an equality beside its inequality is owed nothing: derive drops such a
//! product as a tautology, and the deficits replay that test.

use sparkles_owl::classify::{Classifier, Expansion, Limits, Tier, marker_test_mode};
use sparkles_owl::ofn_read;

/// A cardinality fuzz case of the lane's corpus with its cardinality axioms removed: the
/// root returns `1 ≈ 3 → 0 ≉ 1 ∨ 3 ≈ f(2)`, and the holders' premise `→ 0 ≈ 1` makes the
/// product a tautology.
const CASE: &str = "Prefix(:=<http://example.org/f#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Ontology(<http://example.org/f#o>
ObjectPropertyAssertion(:r1 :a0 :a2)
ObjectPropertyRange(:r1 ObjectOneOf(:a0 :a3))
ObjectPropertyDomain(:r2 :C2)
SubClassOf(:C3 ObjectSomeValuesFrom(:r0 :C0))
SubObjectPropertyOf(:r0 :r1)
ObjectPropertyAssertion(:r1 :a2 :a2)
DisjointClasses(:C1 :C4)
ObjectPropertyRange(:r0 :C5)
SubObjectPropertyOf(:r0 :r0)
ClassAssertion(ObjectSomeValuesFrom(:r1 :C1) :a1)
SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
)";

#[test]
fn a_tautological_return_product_is_owed_nothing() {
    // SAFETY: the test binary holds this one test, and nothing reads the environment
    // before it is set
    unsafe { std::env::set_var("SPARKLES_ROOTED_SELECT_GROUND", "0") };
    let ax = ofn_read::parse(CASE).unwrap().axioms;
    for (threads, expansion, dl_steps) in [
        (1, Expansion::Cautious, true),
        (1, Expansion::Eager, true),
        (4, Expansion::Cautious, false),
        (4, Expansion::Eager, false),
    ] {
        for repair in [true, false] {
            let at = format!("{threads} threads {expansion:?}, repair {repair}");
            marker_test_mode(0, true);
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    rooted_decline: false,
                    rooted_marker_repair: repair,
                    dl_steps,
                    ..Limits::default()
                })
                .unwrap();
            marker_test_mode(0, false);
            assert_eq!(out.stats.tier, Tier::Disjunctive, "{at}");
            assert!(out.complete && !out.stats.dl_over_budget, "{at}: no answer");
            assert_eq!(out.stats.dl_marker_deficits, [0, 0, 0], "{at}");
        }
    }
}
