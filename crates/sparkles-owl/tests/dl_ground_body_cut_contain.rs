//! Item 1250 containment-arm regression tests: with ground-body selection and R1 partner
//! selection both off (`SPARKLES_ROOTED_SELECT_GROUND=0`,
//! `SPARKLES_ROOTED_SEL_PARTNERS_X=0`), Layer 1 (`Stored::gbody` cut, default-on) and
//! Layer 3 (`M-return-cf` default when `Limits::rooted_marker_repair` is enabled) keep
//! every reduced reproduction of the ground-body growth explosion within the default rooted
//! work bound, with zero marker deficits and identical complete taxonomies across threads,
//! expansion strategies, and marker-repair modes.
//!
//! Because `SPARKLES_ROOTED_SELECT_GROUND` and `SPARKLES_ROOTED_SEL_PARTNERS_X` are cached
//! once per process in `OnceLock`, this binary holds one test that runs its cases in turn.

use sparkles_owl::classify::{Classifier, Expansion, Limits, Tier, marker_test_mode};
use sparkles_owl::model::{Axiom, AxiomKind, Class, Entity};
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;

/// Minimal 7-axiom reproduction (`z1n.ofn`) that expands 255x without Layer 1 under
/// `SELECT_GROUND=0`.
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

/// Minimal 7-axiom sibling (`v5_7_9.ofn`) that passes `ROOTED_WORK_BOUND` (676k work)
/// without Layer 1 under `SELECT_GROUND=0`.
const V5_7_9: &str = "Prefix(:=<http://example.org/pf3#>)
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
SubClassOf(:D ObjectOneOf(:b :c))
SubClassOf(:Z ObjectSomeValuesFrom(:s :K))
SubClassOf(ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:r ObjectOneOf(:c))) :H)
SubClassOf(:Z ObjectAllValuesFrom(:s ObjectOneOf(:b :c)))
)";

/// Minimal 17-axiom reproduction (`min.ofn`) of `g1-051_p0012` that passes 4M conclusions
/// and 1 GiB without Layers 1 + 3 under `SELECT_GROUND=0`.
const MIN_P0012: &str = "Prefix(:=<http://example.org/d#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Ontology(
Declaration(Class(:C0))
Declaration(Class(:C1))
Declaration(Class(:C2))
Declaration(Class(:C3))
Declaration(Class(:C4))
Declaration(ObjectProperty(:r0))
Declaration(ObjectProperty(:r1))
Declaration(NamedIndividual(:a0))
Declaration(NamedIndividual(:a2))
Declaration(NamedIndividual(:a3))
SubClassOf(ObjectHasValue(ObjectInverseOf(:r1) :a0) :C2)
SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) ObjectAllValuesFrom(:r1 :C4)) :C3)
SubClassOf(:C0 ObjectUnionOf(:C2 ObjectSomeValuesFrom(:r0 :C3)))
SubClassOf(ObjectSomeValuesFrom(:r1 :C0) ObjectHasValue(owl:topObjectProperty :a2))
SubClassOf(:C0 ObjectComplementOf(:C1))
SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty ObjectComplementOf(:C0)) ObjectOneOf(:a0))
SubClassOf(ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:C4 :C1)) ObjectSomeValuesFrom(:r1 ObjectHasValue(owl:topObjectProperty :a2)))
SubClassOf(:C0 ObjectUnionOf(:C3 :C4))
HasKey(:C3 (:r0 :r1) ())
ClassAssertion(ObjectUnionOf(:C0 :C4) :a0)
ClassAssertion(:C2 :a3)
ObjectPropertyAssertion(:r1 :a2 :a2)
ObjectPropertyAssertion(:r0 :a3 :a0)
ObjectPropertyAssertion(:r0 :a0 :a0)
ObjectPropertyAssertion(:r1 :a0 :a0)
DifferentIndividuals(:a0 :a2)
DifferentIndividuals(:a0 :a3)
)";

/// 16-axiom deletion (`d00.ofn`) that explodes even with marker repair off under
/// `SELECT_GROUND=0` without Layer 1.
const D00: &str = "Prefix(:=<http://example.org/d#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Ontology(
Declaration(Class(:C0))
Declaration(Class(:C1))
Declaration(Class(:C2))
Declaration(Class(:C3))
Declaration(Class(:C4))
Declaration(ObjectProperty(:r0))
Declaration(ObjectProperty(:r1))
Declaration(NamedIndividual(:a0))
Declaration(NamedIndividual(:a2))
Declaration(NamedIndividual(:a3))
SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) ObjectAllValuesFrom(:r1 :C4)) :C3)
SubClassOf(:C0 ObjectUnionOf(:C2 ObjectSomeValuesFrom(:r0 :C3)))
SubClassOf(ObjectSomeValuesFrom(:r1 :C0) ObjectHasValue(owl:topObjectProperty :a2))
SubClassOf(:C0 ObjectComplementOf(:C1))
SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty ObjectComplementOf(:C0)) ObjectOneOf(:a0))
SubClassOf(ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:C4 :C1)) ObjectSomeValuesFrom(:r1 ObjectHasValue(owl:topObjectProperty :a2)))
SubClassOf(:C0 ObjectUnionOf(:C3 :C4))
HasKey(:C3 (:r0 :r1) ())
ClassAssertion(ObjectUnionOf(:C0 :C4) :a0)
ClassAssertion(:C2 :a3)
ObjectPropertyAssertion(:r1 :a2 :a2)
ObjectPropertyAssertion(:r0 :a3 :a0)
ObjectPropertyAssertion(:r0 :a0 :a0)
ObjectPropertyAssertion(:r1 :a0 :a0)
DifferentIndividuals(:a0 :a2)
DifferentIndividuals(:a0 :a3)
)";

/// Two-selected-premise R1 scenario (item 1220): under `SELECT_GROUND=0, SEL_PARTNERS_X=0`,
/// `Stored::gbody` must NOT block Hyper over `x` or Fc Pred partners (`s.sel == false`).
const R1_CONTAIN: &str = "Prefix(:=<http://example.org/r1#>)
Prefix(owl:=<http://www.w3.org/2002/07/owl#>)
Ontology(<http://example.org/r1>
Declaration(Class(:K))
Declaration(Class(:A))
Declaration(Class(:B))
Declaration(Class(:C))
Declaration(Class(:D))
Declaration(Class(:H))
Declaration(Class(:Ans))
Declaration(Class(:Z))
Declaration(Class(:Z1))
Declaration(Class(:Z2))
Declaration(ObjectProperty(:r))
Declaration(ObjectProperty(:s))
Declaration(NamedIndividual(:u))
SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))
SubClassOf(:K ObjectHasValue(:r :u))
SubClassOf(:K ObjectSomeValuesFrom(:s :C))
SubClassOf(ObjectHasValue(:r :u) :H)
ClassAssertion(:H :u)
SubClassOf(ObjectIntersectionOf(:H :C) :D)
SubClassOf(:C ObjectUnionOf(:A :B))
SubClassOf(:A ObjectHasValue(:r :u))
SubClassOf(:B ObjectHasValue(:r :u))
SubClassOf(ObjectIntersectionOf(:A :D) :Ans)
SubClassOf(ObjectIntersectionOf(:B :D) :Ans)
)";

fn declared_classes(ax: &[Axiom]) -> Vec<Class> {
    let mut out: BTreeSet<Class> = BTreeSet::new();
    for a in ax {
        if let AxiomKind::Declaration(Entity::Class(c)) = &a.kind {
            out.insert(c.clone());
        }
    }
    let mut classes: Vec<Class> = out.into_iter().collect();
    classes.push(Class::thing());
    classes.push(Class::nothing());
    classes
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

#[test]
fn ground_body_cut_and_m_return_cf_contain_growth_without_losing_answers() {
    // SAFETY: this test binary holds this one test, and nothing reads the environment
    // before these variables are set.
    unsafe {
        std::env::set_var("SPARKLES_ROOTED_SELECT_GROUND", "0");
        std::env::set_var("SPARKLES_ROOTED_SEL_PARTNERS_X", "0");
    }

    let fixtures = [
        ("z1n", Z1N),
        ("v5_7_9", V5_7_9),
        ("min_p0012", MIN_P0012),
        ("d00", D00),
        ("r1_contain", R1_CONTAIN),
    ];

    let cfgs = [
        (1usize, Expansion::Cautious, true),
        (4usize, Expansion::Eager, false),
    ];

    for (name, src) in fixtures {
        let ax = ofn_read::parse(src).expect(name).axioms;
        let classes = declared_classes(&ax);
        let mut baseline_tax: Option<BTreeSet<(Class, Class)>> = None;

        // Compare: (repair_off, default_cf_129, unfiltered_1)
        for (label, test_mode, repair) in [
            ("repair_off", 0u8, false),
            ("repair_default_cf", 0u8, true),
            ("repair_unfiltered_1", 1u8, true),
        ] {
            for &(threads, expansion, dl_steps) in &cfgs {
                let at = format!("{name} [{label}, t={threads}, {expansion:?}, steps={dl_steps}]");
                marker_test_mode(test_mode, true);
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
                assert!(
                    out.complete && !out.stats.dl_over_budget && !out.stats.dl_rooted_over,
                    "{at}: incomplete or over budget (complete={}, over={}, rooted_over={}, work={})",
                    out.complete,
                    out.stats.dl_over_budget,
                    out.stats.dl_rooted_over,
                    out.stats.dl_work
                );
                if repair {
                    assert_eq!(
                        out.stats.dl_marker_deficits,
                        [0, 0, 0],
                        "{at}: non-zero marker deficits"
                    );
                }

                let got = pairs(&out.taxonomy, &classes);
                if let Some(ref want) = baseline_tax {
                    assert_eq!(&got, want, "{at}: taxonomy mismatch vs baseline");
                } else {
                    baseline_tax = Some(got);
                }
            }
        }
    }
}
