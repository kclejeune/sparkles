//! The Horn rules with individuals (roadmap item 692, F24 "The Horn rules with
//! individuals"): the exchange between a context and its individual, singular contexts,
//! R7 over the predecessors of a singular context, R8 over roles that clash through a
//! nominal, universal restrictions on the top property, functional data properties and
//! keys beside the Horn rules, and the conjunction contexts a world creates. Each case
//! gives the entailments and non-entailments written from the semantics, which HermiT
//! agrees with, on one and eight threads.

use sparkles_owl::classify::{Classification, Classifier, Consistency, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

fn axioms(body: &str) -> Vec<Axiom> {
    let doc = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nPrefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(\n{body}\n)"
    ))
    .unwrap();
    doc.axioms
}

/// Classifies at one and at eight threads, checks that the answers are the same, and
/// returns one with the axioms left out.
fn run(body: &str) -> (Classification, Vec<sparkles_owl::classify::Excluded>) {
    let ax = axioms(body);
    let mut outs = Vec::new();
    for threads in [1, 8] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                ..Limits::default()
            })
            .unwrap();
        outs.push((out, k.excluded()));
    }
    let (b, _) = outs.pop().unwrap();
    let (a, ex) = outs.pop().unwrap();
    let pairs = |o: &Classification| -> Vec<(String, String)> {
        let t = &o.taxonomy;
        let cs: Vec<Class> = t.classes().cloned().collect();
        let mut v = Vec::new();
        for x in &cs {
            for y in &cs {
                if t.is_subclass(x, y) {
                    v.push((x.as_str().to_string(), y.as_str().to_string()));
                }
            }
        }
        v.sort();
        v
    };
    assert_eq!(pairs(&a), pairs(&b), "the answer depends on the threads");
    assert_eq!(
        a.consistency, b.consistency,
        "the consistency depends on the threads"
    );
    (a, ex)
}

/// Asserts that the whole ontology is taken by the Horn rules and that each pair is
/// entailed or not.
fn check(body: &str, yes: &[(&str, &str)], no: &[(&str, &str)]) -> Classification {
    let (out, ex) = run(body);
    assert!(ex.is_empty(), "left out: {ex:#?}");
    assert_eq!(out.stats.tier, Tier::Horn, "the Horn rules ran");
    let t = &out.taxonomy;
    for (a, b) in yes {
        assert!(t.is_subclass(&c(a), &c(b)), "{a} ⊑ {b} is entailed");
    }
    for (a, b) in no {
        assert!(!t.is_subclass(&c(a), &c(b)), "{a} ⊑ {b} is not entailed");
    }
    out
}

fn unsat(body: &str, classes: &[&str], sat: &[&str]) -> Classification {
    let (out, ex) = run(body);
    assert!(ex.is_empty(), "left out: {ex:#?}");
    for a in classes {
        assert!(out.taxonomy.is_unsatisfiable(&c(a)), "{a} is unsatisfiable");
    }
    for a in sat {
        assert!(!out.taxonomy.is_unsatisfiable(&c(a)), "{a} is satisfiable");
    }
    out
}

#[test]
fn r7_two_predecessors_of_a_nominal_through_an_inverse_functional_role() {
    // every A has r to a, and so has b, so every A is b
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         InverseFunctionalObjectProperty(:r)
         ClassAssertion(:B :b)
         ClassAssertion(:P :b)",
        &[("A", "B"), ("A", "P")],
        &[("B", "A"), ("P", "A")],
    );
}

#[test]
fn r7_cascades_through_three_singular_contexts() {
    // A and B are b, every C has s to that one element and so has d, so C is d
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         InverseFunctionalObjectProperty(:r)
         ClassAssertion(:B :b)
         SubClassOf(:C ObjectSomeValuesFrom(:s :A))
         SubClassOf(:D ObjectSomeValuesFrom(:s :B))
         InverseFunctionalObjectProperty(:s)
         ClassAssertion(:D :d)
         SubClassOf(:E ObjectSomeValuesFrom(:t :C))
         SubClassOf(:F ObjectSomeValuesFrom(:t :D))
         InverseFunctionalObjectProperty(:t)
         ClassAssertion(:F :f)",
        &[("A", "B"), ("C", "D"), ("E", "F")],
        &[("B", "A"), ("D", "C"), ("F", "E"), ("C", "A")],
    );
}

#[test]
fn r8_two_disjoint_roles_to_one_nominal_from_one_source() {
    unsat(
        "DisjointObjectProperties(:r :s)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:A ObjectSomeValuesFrom(:s ObjectOneOf(:a)))
         SubClassOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:C ObjectSomeValuesFrom(:s ObjectOneOf(:a)))",
        &["A"],
        &["B", "C"],
    );
}

#[test]
fn r8_through_a_context_that_derives_the_individual() {
    unsat(
        "DisjointObjectProperties(:r :s)
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(:C ObjectOneOf(:a))
         SubClassOf(:A ObjectSomeValuesFrom(:s ObjectOneOf(:a)))",
        &["A"],
        &["C"],
    );
}

#[test]
fn r8_an_asymmetric_cycle_through_a_nominal_is_inconsistent() {
    let (out, ex) = run("AsymmetricObjectProperty(:r)
         SubClassOf(ObjectOneOf(:a) ObjectSomeValuesFrom(:r :A))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))");
    assert!(ex.is_empty(), "left out: {ex:#?}");
    assert_eq!(out.consistency, Consistency::Inconsistent);
}

#[test]
fn a_loop_at_a_nominal_counts_under_a_functional_role() {
    check(
        "EquivalentClasses(:A ObjectOneOf(:a))
         ObjectPropertyAssertion(:r :a :a)
         FunctionalObjectProperty(:r)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A ObjectAllValuesFrom(:s :C))",
        &[("A", "B")],
        &[("B", "A")],
    );
}

#[test]
fn r5_merges_two_nominal_successors_under_a_functional_role() {
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         FunctionalObjectProperty(:r)
         ClassAssertion(:A :x)
         ClassAssertion(:P :a)
         EquivalentClasses(:Q ObjectOneOf(:b))
         SubClassOf(:Z ObjectAllValuesFrom(:s :Y))",
        &[("Q", "P")],
        &[("P", "Q")],
    );
}

#[test]
fn r6_identifies_a_predecessor_with_a_nominal_successor() {
    // every M has r⁻ to n, n has one r-successor, c, so every M is c
    check(
        "ObjectPropertyAssertion(:r :n :c)
         FunctionalObjectProperty(:r)
         SubClassOf(:M ObjectSomeValuesFrom(ObjectInverseOf(:r) ObjectOneOf(:n)))
         ClassAssertion(:P :c)",
        &[("M", "P")],
        &[("P", "M")],
    );
}

#[test]
fn a_universal_on_the_top_property_held_by_a_real_context_holds_everywhere() {
    check(
        "ClassAssertion(:A :a)
         SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))
         Declaration(Class(:C))
         SubClassOf(:C :C)",
        &[("C", "B"), ("A", "B")],
        &[("B", "C")],
    );
}

#[test]
fn a_universal_on_the_top_property_held_by_a_class_without_an_instance() {
    // everything is B where D has an instance, so D is B, and C is not
    check(
        "Declaration(NamedIndividual(:a))
         ClassAssertion(:E :a)
         SubClassOf(:D ObjectAllValuesFrom(owl:topObjectProperty :B))
         SubClassOf(:C :C)",
        &[("D", "B")],
        &[("C", "B"), ("E", "B")],
    );
}

#[test]
fn a_functional_data_property_meets_a_universal_in_a_conjunction_context() {
    unsat(
        "SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         FunctionalDataProperty(:d)
         SubClassOf(:C DataHasValue(:d \"1\"^^xsd:integer))
         SubClassOf(:B DataHasValue(:d \"2\"^^xsd:integer))
         SubClassOf(:D DataHasValue(:d \"1\"^^xsd:integer))",
        &["A"],
        &["B", "C", "D"],
    );
}

#[test]
fn a_key_merges_two_individuals_beside_the_horn_rules() {
    check(
        "HasKey(:P (:r) ())
         ClassAssertion(:P :a)
         ClassAssertion(:P :b)
         ObjectPropertyAssertion(:r :a :c)
         ObjectPropertyAssertion(:r :b :c)
         ClassAssertion(:A :a)
         EquivalentClasses(:B ObjectOneOf(:b))
         SubClassOf(:X ObjectAllValuesFrom(ObjectInverseOf(:t) :Y))",
        &[("B", "A")],
        &[("A", "B")],
    );
}

#[test]
fn a_horn_class_assertion_reaches_the_rules() {
    check(
        "ClassAssertion(ObjectAllValuesFrom(:r :B) :a)
         ObjectPropertyAssertion(:r :a :b)
         EquivalentClasses(:A ObjectOneOf(:b))",
        &[("A", "B")],
        &[("B", "A")],
    );
}

#[test]
fn a_world_creates_a_conjunction_context_of_its_own() {
    // in A's world A is b, takes ∀s.Y, and its s-successor is C ⊓ Y, whose universal on
    // the inverse gives A Z; B has no s-successor
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         InverseFunctionalObjectProperty(:r)
         ClassAssertion(:B :b)
         SubClassOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:B ObjectAllValuesFrom(:s :Y))
         SubClassOf(:A ObjectSomeValuesFrom(:s :C))
         SubClassOf(:Y ObjectAllValuesFrom(ObjectInverseOf(:s) :Z))",
        &[("A", "B"), ("A", "Z")],
        &[("B", "Z"), ("C", "Y")],
    );
}

#[test]
fn a_world_runs_r3_on_a_nominal_successor() {
    // D's r-successor is a ⊓ X in D's world, so a is X there, and X's universal on the
    // inverse gives D Z
    check(
        "SubClassOf(:D ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:D ObjectAllValuesFrom(:r :X))
         SubClassOf(:X ObjectAllValuesFrom(ObjectInverseOf(:r) :Z))
         SubClassOf(:E ObjectSomeValuesFrom(:r ObjectOneOf(:a)))",
        &[("D", "Z")],
        &[("E", "Z")],
    );
}

#[test]
fn distinct_predecessors_of_a_nominal_do_not_clash() {
    unsat(
        "DisjointObjectProperties(:r :s)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(:B ObjectSomeValuesFrom(:s ObjectOneOf(:a)))
         ClassAssertion(:A :x)
         ClassAssertion(:B :y)",
        &[],
        &["A", "B"],
    );
}

#[test]
fn an_at_least_restriction_above_one_stays_out_beside_individuals() {
    let (out, ex) = run("SubClassOf(:A ObjectMinCardinality(2 :r ObjectOneOf(:a)))
         SubClassOf(:A ObjectAllValuesFrom(:r :B))
         ClassAssertion(:C :c)");
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(out.stats.tier, Tier::Horn);
    assert_eq!(out.stats.outside_el.taken, 1);
}

#[test]
fn the_disjunctive_rules_keep_refusing_individuals_by_default() {
    // a union needs case analysis; with an individual the Horn rules take the rest
    let (out, ex) = run("SubClassOf(:A ObjectUnionOf(:B :C))
         SubClassOf(:A ObjectAllValuesFrom(:r :D))
         ClassAssertion(:A :a)");
    assert_eq!(ex.len(), 1, "{ex:#?}");
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.stats.outside_el.taken, 1);
}

#[test]
fn a_universal_on_the_inverse_of_the_top_property_is_a_fact_too() {
    // the top property is its own inverse, also when a Horn axiom is the first to name it
    check(
        "ClassAssertion(:A :a)
         SubClassOf(:A ObjectAllValuesFrom(ObjectInverseOf(owl:topObjectProperty) :B))
         SubClassOf(:C :C)",
        &[("C", "B"), ("A", "B")],
        &[("B", "C")],
    );
    let (out, ex) = run(
        "ClassAssertion(ObjectAllValuesFrom(ObjectInverseOf(owl:topObjectProperty) owl:Nothing) :a)",
    );
    assert!(ex.is_empty(), "left out: {ex:#?}");
    assert_eq!(out.consistency, Consistency::Inconsistent);
}

#[test]
fn r7_counts_the_context_itself_through_a_loop_with_a_qualified_filler() {
    // n has r to itself and at most one r-predecessor in B; m is in B with r to n, so m
    // is n
    check(
        "EquivalentClasses(:N ObjectOneOf(:n))
         ObjectPropertyAssertion(:r :n :n)
         SubClassOf(:N ObjectMaxCardinality(1 ObjectInverseOf(:r) :B))
         SubClassOf(:N :B)
         SubClassOf(:M ObjectSomeValuesFrom(:r ObjectOneOf(:n)))
         SubClassOf(:M :B)
         ClassAssertion(:M :m)",
        &[("M", "N"), ("N", "M")],
        &[("B", "M")],
    );
}
