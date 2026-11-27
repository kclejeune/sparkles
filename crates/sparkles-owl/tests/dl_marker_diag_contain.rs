//! Row 1231's marker repair diagnostic and Item 1250's explicit uncut `0/0` control arm,
//! with both ground-body selection and the decoupled ground-body cut turned off
//! (`SPARKLES_ROOTED_SELECT_GROUND=0`, `SPARKLES_ROOTED_GROUND_BODY_CUT=0`,
//! `SPARKLES_ROOTED_SEL_PARTNERS_X=0`). Because these environment variables are read once
//! per process, this binary holds one test.
//!
//! Covers:
//! 1. Row 1231's tautology fixture (`CASE`) under explicit unfiltered mode `1`, default
//!    filtered mode `129`, and repair off, asserting zero marker deficits and exact HermiT
//!    oracle agreement.
//! 2. The Item 1220/1250 fixtures that complete uncut (`z1n`, `r1_meet`, `r1_h41f`,
//!    `s5b_s5e`), anchoring the uncut `0/0` control arm against the same HermiT oracle
//!    taxonomies asserted by `dl_ground_body_cut_contain`.

use sparkles_owl::classify::{Classifier, Consistency, Expansion, Limits, Tier, marker_test_mode};
use sparkles_owl::model::{Axiom, Class, Entity};
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;

/// A cardinality fuzz case of the lane's corpus with its cardinality axioms removed: the
/// root returns `1 ≈ 3 → 0 ≉ 1 ∨ 3 ≈ f(2)`, and the holders' premise `→ 0 ≈ 1` makes the
/// product a tautology. HermiT 1.4.5 oracle: consistent, no non-trivial named subsumptions.
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

const Z1N: &str = "Prefix(:=<http://example.org/pf3#>)
Ontology(<http://example.org/pf3>
Declaration(Class(:K))
Declaration(Class(:B))
Declaration(Class(:D))
Declaration(Class(:Ans))
Declaration(Class(:G))
Declaration(ObjectProperty(:p))
Declaration(ObjectProperty(:r))
Declaration(NamedIndividual(:a))
Declaration(NamedIndividual(:b))
Declaration(NamedIndividual(:c))
Declaration(Class(:Z))
Declaration(Class(:H))
Declaration(ObjectProperty(:s))
SubClassOf(:K ObjectUnionOf(ObjectOneOf(:b) ObjectHasValue(:p :a)))
SubClassOf(:K ObjectAllValuesFrom(:p :B))
SubClassOf(:B ObjectSomeValuesFrom(:r :D))
SubClassOf(:D ObjectOneOf(:b))
SubClassOf(:Z ObjectSomeValuesFrom(:s :K))
SubClassOf(ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:r ObjectOneOf(:c))) :H)
SubClassOf(:Z ObjectAllValuesFrom(:s ObjectOneOf(:c)))
)";

const R1_MEET: &str = "Prefix(:=<http://example.org/r1m#>)
Ontology(<http://example.org/r1m>
Declaration(Class(:K))
Declaration(Class(:D))
Declaration(Class(:B))
Declaration(Class(:B2))
Declaration(Class(:F))
Declaration(Class(:F2))
Declaration(Class(:G))
Declaration(Class(:Ans))
Declaration(Class(:Q))
Declaration(Class(:Q1))
Declaration(Class(:Q2))
Declaration(ObjectProperty(:r))
Declaration(ObjectProperty(:s))
Declaration(ObjectProperty(:s2))
Declaration(ObjectProperty(:t))
Declaration(NamedIndividual(:v))
Declaration(NamedIndividual(:w))
SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
SubClassOf(:K ObjectSomeValuesFrom(:r :D))
SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
SubClassOf(ObjectSomeValuesFrom(:s2 :B2) :F2)
SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B)))
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :B2)))
SubClassOf(:D ObjectHasValue(:s :v))
SubClassOf(:D ObjectHasValue(:s2 :w))
SubClassOf(ObjectIntersectionOf(:F :F2) :G)
)";

const R1_H41F: &str = "Prefix(:=<http://example.org/r1f#>)
Ontology(<http://example.org/r1f>
Declaration(Class(:K))
Declaration(Class(:D))
Declaration(Class(:B))
Declaration(Class(:B2))
Declaration(Class(:F))
Declaration(Class(:F2))
Declaration(Class(:W))
Declaration(Class(:Ans))
Declaration(Class(:Q))
Declaration(Class(:Q1))
Declaration(Class(:Q2))
Declaration(Class(:E))
Declaration(Class(:A))
Declaration(Class(:A2))
Declaration(Class(:G))
Declaration(Class(:Z))
Declaration(ObjectProperty(:t))
Declaration(ObjectProperty(:r))
Declaration(ObjectProperty(:s))
Declaration(ObjectProperty(:e))
Declaration(NamedIndividual(:v))
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B :B2)))
SubClassOf(:K ObjectSomeValuesFrom(:r :D))
SubClassOf(:D ObjectHasValue(:s :v))
SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
SubClassOf(ObjectSomeValuesFrom(:s :B2) :F2)
SubClassOf(ObjectSomeValuesFrom(:r :W) :Ans)
SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
ClassAssertion(ObjectSomeValuesFrom(:e :E) :v)
SubClassOf(:F ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:e :A))))
SubClassOf(:F2 ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:e :A2))))
SubClassOf(ObjectSomeValuesFrom(:e ObjectIntersectionOf(:A :A2)) :G)
SubClassOf(ObjectSomeValuesFrom(:s :G) :W)
SubClassOf(:Z :W)
)";

const S5B_S5E: &str = "Prefix(:=<http://example.org/s5e#>)
Ontology(<http://example.org/s5e>
Declaration(Class(:K))
Declaration(Class(:S))
Declaration(Class(:M))
Declaration(Class(:N))
Declaration(Class(:Z))
Declaration(Class(:W))
Declaration(Class(:B))
Declaration(Class(:D))
Declaration(Class(:Cq))
Declaration(Class(:Ans))
Declaration(ObjectProperty(:r))
Declaration(ObjectProperty(:s))
Declaration(ObjectProperty(:p))
Declaration(ObjectProperty(:t))
Declaration(NamedIndividual(:u))
Declaration(NamedIndividual(:v))
SubClassOf(:K ObjectSomeValuesFrom(:r :S))
SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
SubClassOf(:M ObjectAllValuesFrom(:s :Z))
SubClassOf(:Z ObjectOneOf(:u :v))
SubClassOf(:W ObjectAllValuesFrom(:p :B))
ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
DisjointClasses(:B :D)
ClassAssertion(:Cq :u)
ClassAssertion(:Cq :v)
SubClassOf(ObjectSomeValuesFrom(:r :Cq) :Ans)
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) ObjectUnionOf(:M :N))))
SubClassOf(:N ObjectAllValuesFrom(ObjectInverseOf(:t) :Ans))
SameIndividual(:u :v)
)";

fn signature_classes(ax: &[Axiom]) -> Vec<Class> {
    let mut out: BTreeSet<Class> = BTreeSet::new();
    for a in ax {
        for entity in a.signature() {
            if let Entity::Class(c) = entity {
                out.insert(c);
            }
        }
    }
    out.insert(Class::thing());
    out.insert(Class::nothing());
    out.into_iter().collect()
}

fn pairs(tax: &sparkles_owl::classify::Taxonomy, classes: &[Class]) -> BTreeSet<(Class, Class)> {
    let mut out = BTreeSet::new();
    for a in classes {
        for b in classes {
            if tax.is_subclass(a, b) {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

fn oracle_pairs(
    prefix: &str,
    classes: &[Class],
    subs: &[(&str, &str)],
) -> BTreeSet<(Class, Class)> {
    let cls = |s: &str| Class::new(format!("{prefix}{s}"));
    let mut out = BTreeSet::new();
    for a in classes {
        out.insert((a.clone(), a.clone()));
        out.insert((a.clone(), Class::thing()));
        out.insert((Class::nothing(), a.clone()));
    }
    for &(a, b) in subs {
        out.insert((cls(a), cls(b)));
    }
    out
}

#[test]
fn a_tautological_return_product_is_owed_nothing() {
    // SAFETY: the test binary holds this one test, and nothing reads the environment
    // before it is set.
    unsafe {
        std::env::set_var("SPARKLES_ROOTED_SELECT_GROUND", "0");
        std::env::set_var("SPARKLES_ROOTED_GROUND_BODY_CUT", "0");
        std::env::set_var("SPARKLES_ROOTED_SEL_PARTNERS_X", "0");
    }
    let ax = ofn_read::parse(CASE).unwrap().axioms;
    let case_classes = signature_classes(&ax);
    assert_eq!(
        case_classes.len(),
        11,
        "CASE signature should include all 9 named classes plus owl:Thing and owl:Nothing"
    );
    let case_want = oracle_pairs("http://example.org/f#", &case_classes, &[]);
    for (threads, expansion, dl_steps) in [
        (1, Expansion::Cautious, true),
        (1, Expansion::Eager, true),
        (4, Expansion::Cautious, false),
        (4, Expansion::Eager, false),
    ] {
        // Explicit mode 1 preserves the original unfiltered product coverage, alongside
        // default filtered mode 129 and repair off.
        for (label, mode, repair) in [
            ("unfiltered_1", 1u8, true),
            ("default_cf_129", 0u8, true),
            ("repair_off", 0u8, false),
        ] {
            let at = format!("{threads} threads {expansion:?}, {label}");
            marker_test_mode(mode, true);
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
            assert_eq!(out.consistency, Consistency::Consistent, "{at}");
            assert!(out.complete && !out.stats.dl_over_budget, "{at}: no answer");
            assert_eq!(out.stats.dl_marker_deficits, [0, 0, 0], "{at}");
            assert_eq!(
                pairs(&out.taxonomy, &case_classes),
                case_want,
                "{at}: CASE taxonomy mismatch vs HermiT oracle"
            );
        }
    }

    // Anchor the uncut 0/0 control arm against the same HermiT oracle taxonomies asserted
    // by `dl_ground_body_cut_contain` on the fixtures that complete without Layer 1.
    for (name, prefix, src, subs) in [
        ("z1n", "http://example.org/pf3#", Z1N, &[][..]),
        (
            "r1_meet",
            "http://example.org/r1m#",
            R1_MEET,
            &[("K", "Ans")][..],
        ),
        (
            "r1_h41f",
            "http://example.org/r1f#",
            R1_H41F,
            &[("K", "Ans"), ("Z", "W")][..],
        ),
        (
            "s5b_s5e",
            "http://example.org/s5e#",
            S5B_S5E,
            &[("K", "Ans"), ("Z", "Cq")][..],
        ),
    ] {
        let ax = ofn_read::parse(src).expect(name).axioms;
        let classes = signature_classes(&ax);
        let want = oracle_pairs(prefix, &classes, subs);
        for (label, mode, repair) in [("repair_off", 0u8, false), ("unfiltered_1", 1u8, true)] {
            let at = format!("uncut 0/0 {name} [{label}]");
            marker_test_mode(mode, true);
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads: 1,
                    expansion: Expansion::Cautious,
                    rooted: true,
                    rooted_decline: false,
                    rooted_marker_repair: repair,
                    dl_steps: true,
                    ..Limits::default()
                })
                .unwrap();
            marker_test_mode(0, false);
            assert_eq!(out.stats.tier, Tier::Disjunctive, "{at}");
            assert_eq!(out.consistency, Consistency::Consistent, "{at}");
            assert!(
                out.complete && !out.stats.dl_over_budget && !out.stats.dl_rooted_over,
                "{at}: incomplete or over budget (work={})",
                out.stats.dl_work
            );
            if repair {
                assert_eq!(out.stats.dl_marker_deficits, [0, 0, 0], "{at}");
            }
            assert_eq!(
                pairs(&out.taxonomy, &classes),
                want,
                "{at}: uncut control mismatch vs HermiT oracle"
            );
        }
    }
}
