//! Item 1250 containment-arm regression tests: with ground-body selection and R1 partner
//! selection both off (`SPARKLES_ROOTED_SELECT_GROUND=0`,
//! `SPARKLES_ROOTED_SEL_PARTNERS_X=0`), Layer 1 (`Stored::gbody` cut, default-on) and
//! Layer 3 (`M-return-cf` default `129` when `Limits::rooted_marker_repair` is enabled)
//! keep every ground-body growth reproduction within the default rooted work bound, with
//! zero marker deficits and exact agreement with HermiT's independent oracle taxonomies
//! across threads, expansion strategies, and marker-repair modes.
//!
//! Because `SPARKLES_ROOTED_SELECT_GROUND` and `SPARKLES_ROOTED_SEL_PARTNERS_X` are cached
//! once per process in `OnceLock`, this binary holds one test that runs its cases in turn.
//! The companion binary `dl_marker_diag_contain` runs the explicit uncut `0/0` control arm
//! (`SPARKLES_ROOTED_GROUND_BODY_CUT=0`) on the fixtures that complete uncut.

use sparkles_owl::classify::{Classifier, Consistency, Expansion, Limits, Tier, marker_test_mode};
use sparkles_owl::model::{Axiom, AxiomKind, Class, Entity};
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;

/// Minimal 7-axiom reproduction (`z1n.ofn`) that expands 255x without Layer 1 under
/// `SELECT_GROUND=0`. HermiT 1.4.5 oracle: consistent, no non-trivial named subsumptions.
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
/// without Layer 1 under `SELECT_GROUND=0`. HermiT 1.4.5 oracle: consistent, no
/// non-trivial named subsumptions.
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
/// and 1 GiB without Layers 1 + 3 under `SELECT_GROUND=0`. HermiT 1.4.5 oracle:
/// consistent, `C1 ⊑ Nothing`, `Thing ⊑ C0` (hence `C2, C3, C4 ⊑ C0`).
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

/// Full 19-axiom generated ontology `g1-051_p0012.ofn` from roadmap item 1250.
/// HermiT 1.4.5 oracle: consistent, `C1 ⊑ Nothing`, `Thing ⊑ C0`.
const G1_051_P0012: &str = "Prefix(:=<http://example.org/d#>)
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
HasKey(:C1 (owl:topObjectProperty) ())
HasKey(:C3 (:r0 :r1) ())
ClassAssertion(ObjectUnionOf(:C0 :C4) :a0)
ClassAssertion(:C2 :a3)
ObjectPropertyAssertion(:r1 :a2 :a2)
ObjectPropertyAssertion(:r0 :a3 :a0)
ObjectPropertyAssertion(:r0 :a0 :a0)
ObjectPropertyAssertion(:r1 :a0 :a0)
DifferentIndividuals(:a0 :a2)
DifferentIndividuals(:a0 :a3)
DifferentIndividuals(:a2 :a3)
)";

/// 16-axiom deletion (`d00.ofn`) that explodes even with marker repair off under
/// `SELECT_GROUND=0` without Layer 1. HermiT 1.4.5 oracle: consistent, `C1 ⊑ Nothing`,
/// `Thing ⊑ C0`.
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

/// Generated neighbour `H1.ofn` of `g1-051_p0012`, which held the largest store of the
/// growth without the cut and passes 4 million conclusions uncut with the marker repair off,
/// filtered or unfiltered. HermiT 1.4.5 oracle: consistent, `C1 ⊑ Nothing`, `Thing ⊑ C0`.
const H1: &str = "Prefix(:=<http://example.org/d#>)
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
Declaration(NamedIndividual(:b0))
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
ClassAssertion(:C2 :a2)
ObjectPropertyAssertion(:r1 :a2 :a2)
ObjectPropertyAssertion(:r0 :a2 :a0)
ObjectPropertyAssertion(:r0 :a0 :a0)
ObjectPropertyAssertion(:r1 :a0 :a0)
DifferentIndividuals(:a0 :a2)
ClassAssertion(:C2 :b0)
ObjectPropertyAssertion(:r0 :b0 :a0)
DifferentIndividuals(:a0 :b0)
)";

/// Generated neighbour `U2.ofn` with a three-way union, which also passes 4 million
/// conclusions uncut in every marker-repair mode. HermiT 1.4.5 oracle: consistent,
/// `C1 ⊑ Nothing`, `Thing ⊑ C0`.
const U2: &str = "Prefix(:=<http://example.org/d#>)
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
Declaration(Class(:C4_1))
SubClassOf(ObjectHasValue(ObjectInverseOf(:r1) :a0) :C2)
SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) ObjectAllValuesFrom(:r1 :C4)) :C3)
SubClassOf(:C0 ObjectUnionOf(:C2 ObjectSomeValuesFrom(:r0 :C3)))
SubClassOf(ObjectSomeValuesFrom(:r1 :C0) ObjectHasValue(owl:topObjectProperty :a2))
SubClassOf(:C0 ObjectComplementOf(:C1))
SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty ObjectComplementOf(:C0)) ObjectOneOf(:a0))
SubClassOf(ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:C4 :C1)) ObjectSomeValuesFrom(:r1 ObjectHasValue(owl:topObjectProperty :a2)))
SubClassOf(:C0 ObjectUnionOf(:C3 :C4 :C4_1))
HasKey(:C3 (:r0 :r1) ())
ClassAssertion(ObjectUnionOf(:C0 :C4) :a0)
ClassAssertion(:C2 :a2)
ObjectPropertyAssertion(:r1 :a2 :a2)
ObjectPropertyAssertion(:r0 :a2 :a0)
ObjectPropertyAssertion(:r0 :a0 :a0)
ObjectPropertyAssertion(:r1 :a0 :a0)
DifferentIndividuals(:a0 :a2)
SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) ObjectAllValuesFrom(:r1 :C4_1)) :C3)
SubClassOf(ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:C4_1 :C1)) ObjectSomeValuesFrom(:r1 ObjectHasValue(owl:topObjectProperty :a2)))
ClassAssertion(ObjectUnionOf(:C0 :C4_1) :a0)
)";

/// Genuine two-selected-premise R1 fixture (`MEET`, item 1220): at K's successor D, both
/// premises `B(v) -> F(x)` and `B2(w) -> F2(x)` carry ground bodies (`gbody=true, sel=false`)
/// and must meet in Hyper over `x` (`partners_x`) to derive `K ⊑ Ans`. HermiT 1.4.5 oracle:
/// consistent, `K ⊑ Ans`.
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

/// Genuine two-selected-premise Fc-Pred R1 fixture (`h41f`, item 1220): two ground-bodied
/// (`gbody=true, sel=false`) universals over `f_e(v)` must meet in `root_pred`
/// (`pred_partners`) to derive `K ⊑ Ans`. HermiT 1.4.5 oracle: consistent, `K ⊑ Ans`,
/// `Z ⊑ W`.
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

/// Query-form `x ≈ u` Eq/Factor fixture (`s5e`, item 1220 `s5b`): under `SELECT_GROUND=0`,
/// `Stored::gbody` must NOT block mixed non-local `x ≈ u` Eq/Factor (`Stored::sel` stays
/// false). HermiT 1.4.5 oracle: consistent, `K ⊑ Ans`, `Z ⊑ Cq`.
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

/// Query-form `x ≈ u` Eq/Factor fixture (`s5g`, item 1220 `s5b`). HermiT 1.4.5 oracle:
/// consistent, `K ⊑ Ans`, `S ⊑ Y`, `Z ⊑ Cq`.
const S5B_S5G: &str = "Prefix(:=<http://example.org/s5g#>)
Ontology(<http://example.org/s5g>
Declaration(Class(:K))
Declaration(Class(:S))
Declaration(Class(:M))
Declaration(Class(:N))
Declaration(Class(:Z))
Declaration(Class(:W))
Declaration(Class(:B))
Declaration(Class(:D))
Declaration(Class(:Cq))
Declaration(Class(:Y))
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
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) ObjectUnionOf(:M :N))))
SubClassOf(:N ObjectAllValuesFrom(ObjectInverseOf(:t) :Ans))
SubClassOf(:S :Y)
SubClassOf(:Y ObjectAllValuesFrom(ObjectInverseOf(:t) :Ans))
SubClassOf(:K ObjectSomeValuesFrom(:t ObjectOneOf(:v)))
)";

struct Fixture {
    name: &'static str,
    prefix: &'static str,
    src: &'static str,
    unsat: &'static [&'static str],
    top: &'static [&'static str],
    subs: &'static [(&'static str, &'static str)],
}

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

/// Builds the exact expected `(Class, Class)` subsumption matrix from a HermiT oracle
/// specification (`unsat`, `top`, and explicit named `subs`).
fn oracle_pairs(f: &Fixture, classes: &[Class]) -> BTreeSet<(Class, Class)> {
    let cls = |s: &str| Class::new(format!("{}{s}", f.prefix));
    let unsat: BTreeSet<Class> = f.unsat.iter().map(|s| cls(s)).collect();
    let top: BTreeSet<Class> = f.top.iter().map(|s| cls(s)).collect();
    let mut out = BTreeSet::new();
    for a in classes {
        out.insert((a.clone(), a.clone()));
        out.insert((a.clone(), Class::thing()));
        out.insert((Class::nothing(), a.clone()));
        if unsat.contains(a) {
            for b in classes {
                out.insert((a.clone(), b.clone()));
            }
        }
        if top.contains(a) {
            for b in classes {
                out.insert((b.clone(), a.clone()));
            }
        }
    }
    for &(a, b) in f.subs {
        out.insert((cls(a), cls(b)));
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
        Fixture {
            name: "z1n",
            prefix: "http://example.org/pf3#",
            src: Z1N,
            unsat: &[],
            top: &[],
            subs: &[],
        },
        Fixture {
            name: "v5_7_9",
            prefix: "http://example.org/pf3#",
            src: V5_7_9,
            unsat: &[],
            top: &[],
            subs: &[],
        },
        Fixture {
            name: "min_p0012",
            prefix: "http://example.org/d#",
            src: MIN_P0012,
            unsat: &["C1"],
            top: &["C0"],
            subs: &[("C2", "C0"), ("C3", "C0"), ("C4", "C0")],
        },
        Fixture {
            name: "g1_051_p0012",
            prefix: "http://example.org/d#",
            src: G1_051_P0012,
            unsat: &["C1"],
            top: &["C0"],
            subs: &[("C2", "C0"), ("C3", "C0"), ("C4", "C0")],
        },
        Fixture {
            name: "d00",
            prefix: "http://example.org/d#",
            src: D00,
            unsat: &["C1"],
            top: &["C0"],
            subs: &[("C2", "C0"), ("C3", "C0"), ("C4", "C0")],
        },
        Fixture {
            name: "h1",
            prefix: "http://example.org/d#",
            src: H1,
            unsat: &["C1"],
            top: &["C0"],
            subs: &[("C2", "C0"), ("C3", "C0"), ("C4", "C0")],
        },
        Fixture {
            name: "u2",
            prefix: "http://example.org/d#",
            src: U2,
            unsat: &["C1"],
            top: &["C0"],
            subs: &[("C2", "C0"), ("C3", "C0"), ("C4", "C0"), ("C4_1", "C0")],
        },
        Fixture {
            name: "r1_meet",
            prefix: "http://example.org/r1m#",
            src: R1_MEET,
            unsat: &[],
            top: &[],
            subs: &[("K", "Ans")],
        },
        Fixture {
            name: "r1_h41f",
            prefix: "http://example.org/r1f#",
            src: R1_H41F,
            unsat: &[],
            top: &[],
            subs: &[("K", "Ans"), ("Z", "W")],
        },
        Fixture {
            name: "s5b_s5e",
            prefix: "http://example.org/s5e#",
            src: S5B_S5E,
            unsat: &[],
            top: &[],
            subs: &[("K", "Ans"), ("Z", "Cq")],
        },
        Fixture {
            name: "s5b_s5g",
            prefix: "http://example.org/s5g#",
            src: S5B_S5G,
            unsat: &[],
            top: &[],
            subs: &[("K", "Ans"), ("S", "Y"), ("Z", "Cq")],
        },
    ];

    let cfgs = [
        (1usize, Expansion::Cautious, true),
        (4usize, Expansion::Eager, false),
    ];

    for f in &fixtures {
        let ax = ofn_read::parse(f.src).expect(f.name).axioms;
        let classes = declared_classes(&ax);
        let want = oracle_pairs(f, &classes);

        // Compare: (repair_off, default_cf_129, unfiltered_1) against the HermiT oracle.
        for (label, test_mode, repair) in [
            ("repair_off", 0u8, false),
            ("repair_default_cf", 0u8, true),
            ("repair_unfiltered_1", 1u8, true),
        ] {
            for &(threads, expansion, dl_steps) in &cfgs {
                let at = format!(
                    "{} [{label}, t={threads}, {expansion:?}, steps={dl_steps}]",
                    f.name
                );
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
                assert_eq!(
                    out.consistency,
                    Consistency::Consistent,
                    "{at}: expected consistent"
                );
                assert!(
                    out.complete && !out.stats.dl_over_budget && !out.stats.dl_rooted_over,
                    "{at}: incomplete or over budget (complete={}, over={}, rooted_over={}, work={}, stored={}, peak_memory={})",
                    out.complete,
                    out.stats.dl_over_budget,
                    out.stats.dl_rooted_over,
                    out.stats.dl_work,
                    out.stats.dl_stored,
                    out.stats.peak_memory
                );
                if repair {
                    assert_eq!(
                        out.stats.dl_marker_deficits,
                        [0, 0, 0],
                        "{at}: non-zero marker deficits"
                    );
                }

                let got = pairs(&out.taxonomy, &classes);
                assert_eq!(got, want, "{at}: taxonomy mismatch vs HermiT oracle");
            }
        }
    }
}
