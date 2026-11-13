//! Tests of the OWL 2 EL classifier (spec F24): worked examples, a comparison with a
//! naive completion procedure on random ontologies, incremental runs against full ones,
//! and the limits of a run.

use proptest::prelude::*;
use sparkles_owl::classify::{Changes, Classifier, Error, Gap, Limits, Saturation};
use sparkles_owl::model::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether a concept is unsatisfiable, and its subsumers, in the naive completion.
type Subsumers = (bool, BTreeSet<usize>);

fn c(s: &str) -> Class {
    Class::new(format!("http://ex.org/{s}"))
}
fn ce(s: &str) -> ClassExpression {
    c(s).into()
}
fn p(s: &str) -> ObjectProperty {
    ObjectProperty::new(format!("http://ex.org/{s}"))
}
fn sub(a: impl Into<ClassExpression>, b: impl Into<ClassExpression>) -> Axiom {
    Axiom::sub_class_of(a, b)
}
fn some(r: &str, f: impl Into<ClassExpression>) -> ClassExpression {
    ClassExpression::some(p(r), f)
}
fn and(cs: impl IntoIterator<Item = ClassExpression>) -> ClassExpression {
    ClassExpression::and(cs)
}
fn ax(k: AxiomKind) -> Axiom {
    Axiom::new(k)
}
fn ope(s: &str) -> ObjectPropertyExpression {
    p(s).into()
}

fn run(axioms: &[Axiom]) -> sparkles_owl::classify::Classification {
    Classifier::of(axioms).classify(&Limits::default()).unwrap()
}

#[test]
fn told_hierarchy_and_equivalence() {
    let out = run(&[
        sub(c("A"), c("B")),
        sub(c("B"), c("C")),
        Axiom::equivalent_classes([ce("C"), ce("D")]),
    ]);
    let t = &out.taxonomy;
    assert!(out.consistent);
    assert!(t.is_subclass(&c("A"), &c("C")));
    assert!(t.is_subclass(&c("A"), &c("D")));
    assert_eq!(t.equivalents(&c("C")), &[c("C"), c("D")]);
    let parents: Vec<&Class> = t
        .parents(&c("A"))
        .iter()
        .map(|&n| &t.node(n).classes[0])
        .collect();
    assert_eq!(parents, vec![&c("B")]);
    assert_eq!(t.parents(&c("C")), &[0]);
    assert_eq!(t.unsatisfiable().count(), 0);
}

#[test]
fn definitions_by_existentials_and_intersections() {
    // Nucleus ⊑ ∃partOf.Cell, CellPart ≡ ∃partOf.Cell, Organelle ≡ CellPart ⊓ Structure
    let out = run(&[
        sub(c("Nucleus"), some("partOf", c("Cell"))),
        sub(c("Nucleus"), c("Structure")),
        Axiom::equivalent_classes([ce("CellPart"), some("partOf", c("Cell"))]),
        Axiom::equivalent_classes([ce("Organelle"), and([ce("CellPart"), ce("Structure")])]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("Nucleus"), &c("CellPart")));
    assert!(t.is_subclass(&c("Nucleus"), &c("Organelle")));
    let parents: Vec<&Class> = t
        .parents(&c("Nucleus"))
        .iter()
        .map(|&n| &t.node(n).classes[0])
        .collect();
    assert_eq!(parents, vec![&c("Organelle")]);
}

#[test]
fn role_hierarchy_chains_and_transitivity() {
    let out = run(&[
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("hasPart")),
            ope("hasComponent"),
        )),
        ax(AxiomKind::TransitiveObjectProperty(ope("partOf"))),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("locatedIn"), ope("partOf")]),
            ope("locatedIn"),
        )),
        sub(c("A"), some("hasPart", c("B"))),
        Axiom::equivalent_classes([ce("HasComp"), some("hasComponent", c("B"))]),
        sub(c("X"), some("partOf", c("Y"))),
        sub(c("Y"), some("partOf", c("Z"))),
        Axiom::equivalent_classes([ce("PartOfZ"), some("partOf", c("Z"))]),
        sub(c("L"), some("locatedIn", c("X"))),
        Axiom::equivalent_classes([ce("InZ"), some("locatedIn", c("Z"))]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("A"), &c("HasComp")));
    assert!(t.is_subclass(&c("X"), &c("PartOfZ")));
    assert!(t.is_subclass(&c("L"), &c("InZ")));
    assert!(!t.is_subclass(&c("Y"), &c("InZ")));
}

#[test]
fn domains_ranges_and_reflexivity() {
    let out = run(&[
        ax(AxiomKind::ObjectPropertyDomain(
            ope("teaches"),
            ce("Teacher"),
        )),
        ax(AxiomKind::ObjectPropertyRange(ope("teaches"), ce("Course"))),
        sub(
            c("Prof"),
            some("teaches", ClassExpression::Class(Class::thing())),
        ),
        Axiom::equivalent_classes([ce("TeachesCourse"), some("teaches", c("Course"))]),
        ax(AxiomKind::ReflexiveObjectProperty(ope("knows"))),
        Axiom::equivalent_classes([ce("KnowsPerson"), some("knows", c("Person"))]),
        sub(c("Student"), c("Person")),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("Prof"), &c("Teacher")));
    assert!(t.is_subclass(&c("Prof"), &c("TeachesCourse")));
    assert!(t.is_subclass(&c("Student"), &c("KnowsPerson")));
    assert!(t.is_subclass(&c("Person"), &c("KnowsPerson")));
}

#[test]
fn disjointness_and_unsatisfiable_classes() {
    let out = run(&[
        Axiom::disjoint_classes([ce("A"), ce("B"), ce("C")]),
        sub(c("AB"), c("A")),
        sub(c("AB"), c("B")),
        sub(c("D"), some("r", c("AB"))),
        sub(c("E"), c("A")),
    ]);
    let t = &out.taxonomy;
    let unsat: Vec<&Class> = t.unsatisfiable().collect();
    assert_eq!(unsat, vec![&c("AB"), &c("D")]);
    assert!(t.is_subclass(&c("D"), &c("E")));
    assert!(!t.is_unsatisfiable(&c("E")));
    assert!(out.consistent);
    let axioms = t.axioms();
    assert!(axioms.contains(&Axiom::equivalent_classes([
        ClassExpression::Class(Class::nothing()),
        ce("AB"),
        ce("D")
    ])));
}

#[test]
fn inconsistency_from_assertions() {
    let a = NamedIndividual::new("http://ex.org/a");
    let out = run(&[
        Axiom::disjoint_classes([ce("A"), ce("B")]),
        Axiom::class_assertion(ce("A"), Individual::Named(a.clone())),
        Axiom::class_assertion(ce("B"), Individual::Named(a)),
        sub(c("C"), c("A")),
    ]);
    assert!(!out.consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));
}

#[test]
fn equivalent_to_thing() {
    let thing = ClassExpression::Class(Class::thing());
    let out = run(&[sub(thing, c("Any")), sub(c("A"), c("B"))]);
    let t = &out.taxonomy;
    assert_eq!(t.top().classes, vec![Class::thing(), c("Any")]);
    assert!(t.is_subclass(&c("B"), &c("Any")));
}

#[test]
fn has_self_loops() {
    let self_r = ClassExpression::ObjectHasSelf(ope("r"));
    let out = run(&[
        sub(c("A"), self_r.clone()),
        sub(c("A"), c("B")),
        Axiom::equivalent_classes([ce("RB"), some("r", c("B"))]),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("r")),
            ope("s"),
        )),
        Axiom::equivalent_classes([ce("SelfS"), ClassExpression::ObjectHasSelf(ope("s"))]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("A"), &c("RB")));
    assert!(t.is_subclass(&c("A"), &c("SelfS")));
}

#[test]
fn axioms_outside_el_are_reported() {
    let mut k = Classifier::of(&[
        sub(c("A"), ClassExpression::or([ce("B"), ce("C")])),
        sub(c("A"), c("D")),
        sub(
            c("E"),
            ClassExpression::ObjectHasValue(ope("r"), Individual::named("http://ex.org/i")),
        ),
        ax(AxiomKind::InverseObjectProperties(ope("r"), ope("s"))),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    // classes of axioms left out are still in the taxonomy
    assert!(out.taxonomy.node_of(&c("B")).is_some());
    // the Horn rules take the inverse properties beside the individual (item 692), and
    // the union needs case analysis
    let ex = k.excluded();
    assert_eq!(ex.len(), 1, "{ex:#?}");
    let outside: Vec<&Excl> = ex
        .iter()
        .filter(|e| matches!(e.gap, Gap::Outside { .. }))
        .collect();
    assert_eq!(outside.len(), 1);
    assert!(out.taxonomy.is_subclass(&c("E"), &c("E")));
}

type Excl = sparkles_owl::classify::Excluded;

/// With `Limits::rooted`, the rooted run of Tier D takes the same axioms, which an ontology
/// with an individual otherwise leaves out, and reports nothing.
#[test]
fn a_rooted_run_takes_the_axioms_outside_el_beside_an_individual() {
    let mut k = Classifier::of(&[
        sub(c("A"), ClassExpression::or([ce("B"), ce("C")])),
        sub(c("A"), c("D")),
        sub(
            c("E"),
            ClassExpression::ObjectHasValue(ope("r"), Individual::named("http://ex.org/i")),
        ),
        ax(AxiomKind::InverseObjectProperties(ope("r"), ope("s"))),
    ]);
    let out = k
        .classify(&Limits {
            rooted: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out.complete);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert!(out.taxonomy.node_of(&c("B")).is_some());
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert_eq!(out.stats.tier, sparkles_owl::classify::Tier::Disjunctive);
    assert!(!out.stats.dl_rooted_over);
}

#[test]
fn chains_against_the_range_restriction_are_left_out() {
    let axioms = [
        ax(AxiomKind::ObjectPropertyRange(ope("s"), ce("R"))),
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("a"), ope("b")]),
            ope("s"),
        )),
        sub(c("A"), some("a", some("b", ce("X")))),
        ax(AxiomKind::EquivalentClasses(
            [ce("D"), some("s", and([ce("X"), ce("R")]))].into(),
        )),
    ];
    // with an individual the Horn rules take the chain too (item 692), and under the
    // ELK mode the chain against the range stays out
    let mut k = Classifier::of(axioms.iter().chain(&[is_a("E", "i")]));
    let out = k.classify(&Limits::default()).unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    let mut k = Classifier::of(axioms.iter().chain(&[is_a("E", "i")]));
    let out = k
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    let ex = k.excluded();
    assert_eq!(ex.len(), 1);
    assert!(matches!(&ex[0].gap, Gap::Outside { rule, .. } if rule.contains("2.2.6")));
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
    // without one, the Horn rules take the chain, and the range holds along it
    let mut k = Classifier::of(&axioms);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
}

#[test]
fn limits_stop_a_run_and_the_next_run_starts_again() {
    let mut axioms = Vec::new();
    for i in 0..300 {
        axioms.push(sub(
            c(&format!("A{i}")),
            some("r", c(&format!("A{}", i + 1))),
        ));
        axioms.push(sub(c(&format!("A{i}")), c(&format!("B{}", i % 7))));
    }
    axioms.push(ax(AxiomKind::TransitiveObjectProperty(ope("r"))));
    for i in 0..50 {
        axioms.push(Axiom::equivalent_classes([
            ce(&format!("E{i}")),
            some("r", c(&format!("A{}", i * 5))),
        ]));
    }
    let mut k = Classifier::of(&axioms);
    let past = Limits {
        deadline: Some(std::time::Instant::now()),
        ..Limits::default()
    };
    assert_eq!(k.classify(&past).unwrap_err(), Error::Timeout);
    let tiny = Limits {
        max_memory: Some(10_000),
        ..Limits::default()
    };
    assert!(matches!(
        k.classify(&tiny).unwrap_err(),
        Error::Memory { .. }
    ));
    let flag = Arc::new(AtomicBool::new(true));
    let f = flag.clone();
    let cancel = Limits {
        cancelled: Some(Arc::new(move || f.load(Ordering::Relaxed))),
        ..Limits::default()
    };
    assert_eq!(k.classify(&cancel).unwrap_err(), Error::Cancelled);
    flag.store(false, Ordering::Relaxed);
    let out = k.classify(&cancel).unwrap();
    assert!(!out.stats.incremental);
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert_eq!(out.taxonomy, full.taxonomy);
    assert!(out.taxonomy.is_subclass(&c("A0"), &c("E30")));
}

#[test]
fn incremental_runs_reuse_contexts() {
    let mut axioms = Vec::new();
    for i in 0..200 {
        axioms.push(sub(c(&format!("A{i}")), c(&format!("A{}", i + 1))));
        axioms.push(sub(c(&format!("X{i}")), c(&format!("Y{i}"))));
    }
    let mut k = Classifier::of(&axioms);
    k.classify(&Limits::default()).unwrap();
    let extra = sub(c("X5"), c("A199"));
    k.add([&extra]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.stats.incremental);
    assert!(out.stats.cleared < 10, "{:?}", out.stats);
    assert!(out.taxonomy.is_subclass(&c("X5"), &c("A200")));
    k.remove([&extra]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("X5"), &c("A200")));
    assert_eq!(
        out.taxonomy,
        Classifier::of(&axioms)
            .classify(&Limits::default())
            .unwrap()
            .taxonomy
    );
}

fn nested(depth: usize, filler: &str) -> ClassExpression {
    let mut e = ce(filler);
    for _ in 0..depth {
        e = some("r", e);
    }
    e
}

#[test]
fn an_expression_nested_too_deeply_leaves_its_axiom_out() {
    // a stand-in for the deep fillers would make the two left sides one expression
    let mut k = Classifier::of(&[sub(c("A"), nested(300, "C")), sub(nested(300, "D"), c("B"))]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("B")));
    let partial = k
        .excluded()
        .iter()
        .filter(|e| matches!(e.gap, Gap::Partial("class expressions nested too deeply")))
        .count();
    assert_eq!(partial, 2);
}

fn dp(s: &str) -> DataProperty {
    DataProperty::new(format!("http://ex.org/{s}"))
}

#[test]
fn a_change_to_a_data_axiom_runs_everything_again() {
    let has = ClassExpression::DataHasValue(dp("p"), Literal::simple("x"));
    let mut k = Classifier::of(&[sub(c("A"), has)]);
    k.classify(&Limits::default()).unwrap();
    let domain = ax(AxiomKind::DataPropertyDomain(dp("p"), ce("D")));
    k.add([&domain]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
}

#[test]
fn assertions_on_properties_with_ranges_are_complete() {
    let i = |s: &str| Individual::named(format!("http://ex.org/{s}"));
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        ax(AxiomKind::ObjectPropertyRange(ope("s"), ce("D"))),
        ax(AxiomKind::DisjointClasses(
            [ce("C"), ce("D")].into_iter().collect(),
        )),
        ax(AxiomKind::ObjectPropertyAssertion(ope("r"), i("a"), i("b"))),
        ax(AxiomKind::ObjectPropertyAssertion(ope("s"), i("d"), i("b"))),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.consistent);
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
}

// The four tests below are the ontologies of the agreement record of roadmap item 604
// where a property's range meets an individual. HermiT, Konclude and Whelk agree on
// them, and ELK 0.6.0 misses the first three, as the classifier did before item 587
// added the rules for individuals.

fn ind(s: &str) -> Individual {
    Individual::named(format!("http://ex.org/{s}"))
}

/// The axioms the classifier reports as used in part, by the reason it gives.
fn partial_reasons(k: &Classifier) -> BTreeMap<&'static str, usize> {
    let mut out = BTreeMap::new();
    for e in k.excluded() {
        if let Gap::Partial(why) = e.gap {
            *out.entry(why).or_insert(0) += 1;
        }
    }
    out
}

#[test]
fn a_range_on_an_asserted_property_reaches_the_individual() {
    // range(r) = C and r(a, b) put b in C, so Y ⊑ ∃s.{b} ⊑ ∃s.C ≡ X
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        ax(AxiomKind::ObjectPropertyAssertion(
            ope("r"),
            ind("a"),
            ind("b"),
        )),
        Axiom::equivalent_classes([ce("X"), some("s", c("C"))]),
        sub(c("Y"), ClassExpression::ObjectHasValue(ope("s"), ind("b"))),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.consistent);
    assert!(out.taxonomy.is_subclass(&c("Y"), &c("X")));
    assert!(partial_reasons(&k).is_empty());
}

#[test]
fn a_range_reached_through_has_value_reaches_the_individual() {
    // Z(a) and Z ⊑ ∃r.{b} give r(a, b), so b is in range(r) = C and Y ⊑ ∃s.{b} ⊑ X
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        sub(c("Z"), ClassExpression::ObjectHasValue(ope("r"), ind("b"))),
        ax(AxiomKind::ClassAssertion(ce("Z"), ind("a"))),
        Axiom::equivalent_classes([ce("X"), some("s", c("C"))]),
        sub(
            c("Y"),
            some(
                "s",
                ClassExpression::ObjectOneOf([ind("b")].into_iter().collect()),
            ),
        ),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.consistent);
    assert!(out.taxonomy.is_subclass(&c("Y"), &c("X")));
    assert!(!out.taxonomy.is_subclass(&c("Z"), &c("X")));
    assert!(partial_reasons(&k).is_empty());
}

#[test]
fn two_disjoint_ranges_on_one_individual_are_inconsistent() {
    // r(a, b) and q(d, b) put b in C and in D, which are disjoint
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        ax(AxiomKind::ObjectPropertyRange(ope("q"), ce("D"))),
        ax(AxiomKind::DisjointClasses(
            [ce("C"), ce("D")].into_iter().collect(),
        )),
        ax(AxiomKind::ObjectPropertyAssertion(
            ope("r"),
            ind("a"),
            ind("b"),
        )),
        ax(AxiomKind::ObjectPropertyAssertion(
            ope("q"),
            ind("d"),
            ind("b"),
        )),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.consistent);
    assert!(partial_reasons(&k).is_empty());
}

#[test]
fn one_range_that_clashes_with_an_assertion_is_inconsistent() {
    // r(a, b) puts b in C, and D(b) with C and D disjoint makes the ontology inconsistent
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyRange(ope("r"), ce("C"))),
        ax(AxiomKind::DisjointClasses(
            [ce("C"), ce("D")].into_iter().collect(),
        )),
        ax(AxiomKind::ClassAssertion(ce("D"), ind("b"))),
        ax(AxiomKind::ObjectPropertyAssertion(
            ope("r"),
            ind("a"),
            ind("b"),
        )),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.consistent);
    assert!(partial_reasons(&k).is_empty());
}

#[test]
fn the_top_and_bottom_properties() {
    let top = ObjectPropertyExpression::from(ObjectProperty::top());
    let bottom = ObjectPropertyExpression::from(ObjectProperty::bottom());
    // every element is a top-successor of every element, so a domain of the top
    // property holds everywhere
    let mut k = Classifier::of(&[
        ax(AxiomKind::ObjectPropertyDomain(top.clone(), ce("C"))),
        sub(c("A"), c("B")),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
    assert!(!out.taxonomy.is_subclass(&c("C"), &c("A")));
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    // a reflexive bottom property relates every element to itself, which it cannot
    let out = run(&[ax(AxiomKind::ReflexiveObjectProperty(bottom.clone()))]);
    assert!(!out.consistent);
    // a subproperty of the bottom property relates nothing
    let out = run(&[
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("r")),
            bottom,
        )),
        sub(c("A"), some("r", c("B"))),
    ]);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    // once B has an instance, every element has a top-successor in B
    let out = run(&[
        ax(AxiomKind::ClassAssertion(ce("B"), ind("b"))),
        Axiom::equivalent_classes([
            ce("X"),
            ClassExpression::some(ObjectProperty::top(), c("B")),
        ]),
        sub(c("A"), c("D")),
    ]);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("X")));
    // every property is under the top property, so saying so is used in full
    let mut k = Classifier::of(&[
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope("r")),
            top.clone(),
        )),
        sub(c("A"), some("r", c("B"))),
    ]);
    k.classify(&Limits::default()).unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    // a chain through the top property is used in part
    let mut k = Classifier::of(&[ax(AxiomKind::SubObjectPropertyOf(
        SubObjectPropertyExpression::Chain(vec![top, ope("r")]),
        ope("s"),
    ))]);
    k.classify(&Limits::default()).unwrap();
    assert_eq!(
        partial_reasons(&k).get("property chains through the top object property"),
        Some(&1)
    );
}

// Individuals: what a context that stands for an individual takes from it and gives it.

fn one(s: &str) -> ClassExpression {
    ClassExpression::ObjectOneOf([ind(s)].into_iter().collect())
}
fn value(r: &str, s: &str) -> ClassExpression {
    ClassExpression::ObjectHasValue(ope(r), ind(s))
}
fn assert_(r: &str, a: &str, b: &str) -> Axiom {
    ax(AxiomKind::ObjectPropertyAssertion(ope(r), ind(a), ind(b)))
}
fn is_a(c: &str, i: &str) -> Axiom {
    ax(AxiomKind::ClassAssertion(ce(c), ind(i)))
}
fn range(r: &str, c: &str) -> Axiom {
    ax(AxiomKind::ObjectPropertyRange(ope(r), ce(c)))
}

#[test]
fn a_class_of_one_individual_takes_what_the_individual_has() {
    let out = run(&[sub(c("C"), one("a")), is_a("B", "a")]);
    assert!(out.taxonomy.is_subclass(&c("C"), &c("B")));
    assert!(!out.taxonomy.is_subclass(&c("B"), &c("C")));
}

#[test]
fn what_holds_where_a_class_has_an_instance_stays_in_its_world() {
    // Y ⊑ ∃r.{b} with range(r) = C puts b in C only where Y has an instance: Z, below Y,
    // reaches X, and W, which only links to b, does not
    let out = run(&[
        sub(c("Y"), value("r", "b")),
        range("r", "C"),
        Axiom::equivalent_classes([ce("X"), some("s", c("C"))]),
        sub(c("W"), value("s", "b")),
        sub(c("Z"), and([ce("Y"), value("s", "b")])),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("Z"), &c("X")));
    assert!(!t.is_subclass(&c("W"), &c("X")));
    assert!(!t.is_subclass(&c("Y"), &c("X")));
}

#[test]
fn a_world_follows_an_individual_back_to_the_class() {
    // where C has an instance it is a, which is then in B, so c is in ∃r.B ⊑ E and C in
    // ∃s.E ⊑ F; G, which reaches c alone, is not in F
    let out = run(&[
        sub(c("C"), one("a")),
        sub(c("C"), c("B")),
        assert_("r", "c", "a"),
        sub(some("r", c("B")), c("E")),
        sub(c("C"), value("s", "c")),
        sub(some("s", c("E")), c("F")),
        sub(c("G"), value("s", "c")),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("C"), &c("F")));
    assert!(!t.is_subclass(&c("G"), &c("F")));
}

#[test]
fn same_and_different_individuals() {
    let out = run(&[
        ax(AxiomKind::SameIndividual(
            [ind("a"), ind("b")].into_iter().collect(),
        )),
        is_a("B", "a"),
        sub(c("Y"), value("r", "b")),
        Axiom::equivalent_classes([ce("X"), some("r", c("B"))]),
        ax(AxiomKind::DifferentIndividuals(
            [ind("c"), ind("d")].into_iter().collect(),
        )),
        sub(c("U"), and([one("c"), one("d")])),
    ]);
    let t = &out.taxonomy;
    assert!(out.consistent);
    assert!(t.is_subclass(&c("Y"), &c("X")));
    assert!(t.is_unsatisfiable(&c("U")));
}

#[test]
fn a_link_from_an_individual_to_itself_is_a_loop() {
    let selfish = ClassExpression::ObjectHasSelf(ope("r"));
    let out = run(&[
        assert_("r", "a", "a"),
        Axiom::equivalent_classes([ce("X"), selfish.clone()]),
        sub(c("C"), one("a")),
        sub(c("Y"), value("s", "a")),
        Axiom::equivalent_classes([ce("Z"), some("s", c("X"))]),
        // a loop meets the range of its role
        sub(c("L"), ClassExpression::ObjectHasSelf(ope("q"))),
        range("q", "D"),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("C"), &c("X")));
    assert!(t.is_subclass(&c("Y"), &c("Z")));
    assert!(t.is_subclass(&c("L"), &c("D")));
}

#[test]
fn negative_property_assertions() {
    let not = |r: &str, a: &str, b: &str| {
        ax(AxiomKind::NegativeObjectPropertyAssertion(
            ope(r),
            ind(a),
            ind(b),
        ))
    };
    assert!(!run(&[assert_("r", "a", "b"), not("r", "a", "b")]).consistent);
    // through a chain
    let out = run(&[
        ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("r"), ope("s")]),
            ope("t"),
        )),
        assert_("r", "a", "b"),
        assert_("s", "b", "c"),
        not("t", "a", "c"),
    ]);
    assert!(!out.consistent);
    // a class whose instance would break one is unsatisfiable, and the ontology is not
    let out = run(&[
        sub(c("C"), and([one("a"), value("r", "b")])),
        not("r", "a", "b"),
    ]);
    assert!(out.consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));
}

// Data: the values of literals, the datatypes of OWL 2 EL, ranges and functional data
// properties.

fn xsd(l: &str) -> String {
    format!("http://www.w3.org/2001/XMLSchema#{l}")
}
fn lit(v: &str, dt: &str) -> Literal {
    Literal::typed(v, &xsd(dt))
}
fn dt(l: &str) -> DataRange {
    DataRange::Datatype(Datatype::new(xsd(l)))
}
fn dsome(d: &str, r: DataRange) -> ClassExpression {
    ClassExpression::DataSomeValuesFrom(vec![dp(d)], r)
}
fn dvalue(d: &str, l: Literal) -> ClassExpression {
    ClassExpression::DataHasValue(dp(d), l)
}

#[test]
fn literals_are_values() {
    let out = run(&[
        sub(c("C"), dvalue("d", lit("1", "integer"))),
        Axiom::equivalent_classes([ce("X"), dvalue("d", lit("1.0", "decimal"))]),
        Axiom::equivalent_classes([ce("Y"), dsome("d", dt("integer"))]),
        sub(c("H"), dvalue("d", lit("1.5", "decimal"))),
        Axiom::equivalent_classes([ce("S"), dsome("d", dt("string"))]),
        sub(c("L"), dvalue("d", Literal::lang("x", "en").unwrap())),
        Axiom::equivalent_classes([
            ce("P"),
            dsome(
                "d",
                DataRange::Datatype(Datatype::new(
                    "http://www.w3.org/1999/02/22-rdf-syntax-ns#PlainLiteral",
                )),
            ),
        ]),
    ]);
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("C"), &c("X")));
    assert!(t.is_subclass(&c("C"), &c("Y")));
    assert!(!t.is_subclass(&c("H"), &c("Y")));
    assert!(!t.is_subclass(&c("L"), &c("S")));
    assert!(t.is_subclass(&c("L"), &c("P")));
    assert!(t.is_subclass(&c("S"), &c("P")));
}

#[test]
fn data_ranges_and_definitions() {
    let count = Datatype::new("http://ex.org/count");
    let mut k = Classifier::of(&[
        ax(AxiomKind::DataPropertyRange(dp("d"), dt("integer"))),
        sub(c("C"), dsome("d", dt("decimal"))),
        Axiom::equivalent_classes([ce("Y"), dsome("d", dt("integer"))]),
        ax(AxiomKind::DataPropertyRange(dp("e"), dt("string"))),
        sub(c("U"), dvalue("e", lit("1", "integer"))),
        ax(AxiomKind::DatatypeDefinition(
            count.clone(),
            dt("nonNegativeInteger"),
        )),
        sub(c("N"), dsome("f", DataRange::Datatype(count))),
        Axiom::equivalent_classes([ce("I"), dsome("f", dt("integer"))]),
        sub(
            c("E"),
            dsome(
                "f",
                DataRange::IntersectionOf([dt("integer"), dt("string")].into_iter().collect()),
            ),
        ),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    let t = &out.taxonomy;
    assert!(t.is_subclass(&c("C"), &c("Y")));
    assert!(t.is_unsatisfiable(&c("U")));
    assert!(t.is_subclass(&c("N"), &c("I")));
    assert!(t.is_unsatisfiable(&c("E")));
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
}

#[test]
fn functional_data_properties_make_one_value() {
    let fun = |d: &str| ax(AxiomKind::FunctionalDataProperty(dp(d)));
    let subd = |a: &str, b: &str| ax(AxiomKind::SubDataPropertyOf(dp(a), dp(b)));
    let two = and([
        dvalue("d", lit("1", "integer")),
        dvalue("d", lit("2", "integer")),
    ]);
    assert!(
        run(&[fun("d"), sub(c("C"), two.clone())])
            .taxonomy
            .is_unsatisfiable(&c("C"))
    );
    assert!(!run(&[sub(c("C"), two)]).taxonomy.is_unsatisfiable(&c("C")));
    // the successors by two roles under one functional role are one value
    let out = run(&[
        fun("f"),
        subd("d1", "f"),
        subd("d2", "f"),
        sub(
            c("C"),
            and([
                dsome("d1", dt("integer")),
                dvalue("d2", lit("3", "decimal")),
            ]),
        ),
        Axiom::equivalent_classes([ce("X"), dvalue("d1", lit("3", "integer"))]),
        sub(
            c("U"),
            and([
                dsome("d1", dt("nonNegativeInteger")),
                dvalue("d2", lit("-3", "integer")),
            ]),
        ),
    ]);
    assert!(out.taxonomy.is_subclass(&c("C"), &c("X")));
    assert!(out.taxonomy.is_unsatisfiable(&c("U")));
}

#[test]
fn data_on_individuals_and_the_top_data_property() {
    let out = run(&[
        ax(AxiomKind::DataPropertyAssertion(
            dp("d"),
            ind("a"),
            Literal::simple("x"),
        )),
        sub(c("Y"), value("r", "a")),
        Axiom::equivalent_classes([ce("X"), some("r", dvalue("d", Literal::simple("x")))]),
        Axiom::equivalent_classes([
            ce("T"),
            ClassExpression::DataSomeValuesFrom(vec![DataProperty::top()], dt("integer")),
        ]),
        sub(c("A"), c("B")),
        ax(AxiomKind::NegativeDataPropertyAssertion(
            dp("e"),
            ind("a"),
            lit("1", "integer"),
        )),
        sub(c("Q"), and([one("a"), dvalue("e", lit("01", "integer"))])),
    ]);
    let t = &out.taxonomy;
    assert!(out.consistent);
    assert!(t.is_subclass(&c("Y"), &c("X")));
    assert!(t.is_subclass(&c("A"), &c("T")));
    assert!(t.is_unsatisfiable(&c("Q")));
}

#[test]
fn keys_make_individuals_one() {
    let key = |c: &str, ops: &[&str], dps: &[&str]| {
        ax(AxiomKind::HasKey(
            ce(c),
            ops.iter().map(|p| ope(p)).collect(),
            dps.iter().map(|p| dp(p)).collect(),
        ))
    };
    let base = [
        is_a("K", "a"),
        is_a("K", "b"),
        is_a("B", "a"),
        sub(c("Y"), value("s", "b")),
        Axiom::equivalent_classes([ce("X"), some("s", c("B"))]),
    ];
    let mut by_object = base.to_vec();
    by_object.extend([
        key("K", &["r"], &[]),
        assert_("r", "a", "c"),
        assert_("r", "b", "c"),
    ]);
    assert!(run(&by_object).taxonomy.is_subclass(&c("Y"), &c("X")));
    let mut by_data = base.to_vec();
    by_data.extend([
        key("K", &[], &["d"]),
        ax(AxiomKind::DataPropertyAssertion(
            dp("d"),
            ind("a"),
            lit("1", "integer"),
        )),
        ax(AxiomKind::DataPropertyAssertion(
            dp("d"),
            ind("b"),
            lit("01", "integer"),
        )),
    ]);
    assert!(run(&by_data).taxonomy.is_subclass(&c("Y"), &c("X")));
    // different values, or a value that is not named, keep them apart
    let mut apart = base.to_vec();
    apart.extend([key("K", &["r"], &[]), sub(c("K"), some("r", c("V")))]);
    assert!(!run(&apart).taxonomy.is_subclass(&c("Y"), &c("X")));
}

#[test]
fn opaque_literals_are_used_in_part() {
    let mut k = Classifier::of(&[sub(
        c("C"),
        dvalue("d", Literal::typed("x", "http://ex.org/dt")),
    )]);
    k.classify(&Limits::default()).unwrap();
    assert_eq!(partial_reasons(&k).len(), 1, "{:#?}", k.excluded());
}

#[test]
fn a_change_with_an_individual_runs_everything_again() {
    let mut k = Classifier::of(&[sub(c("Y"), value("r", "b")), range("r", "C")]);
    k.classify(&Limits::default()).unwrap();
    k.add([&is_a("D", "b")]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.stats.incremental);
    k.add([&sub(c("Z"), c("Y"))]);
    let out = k.classify(&Limits::default()).unwrap();
    // the previous run merged a context with b, which the invalidation does not follow
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("Z"), &c("Y")));
}

#[test]
fn the_property_closure_is_bounded_by_the_memory_limit() {
    let mut axioms = Vec::new();
    for i in 0..1000 {
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&format!("r{i}"))),
            ope(&format!("r{}", i + 1)),
        )));
    }
    let limit = Limits {
        max_memory: Some(128_000),
        ..Limits::default()
    };
    assert!(matches!(
        Classifier::of(&axioms).classify(&limit).unwrap_err(),
        Error::Memory { .. }
    ));
}

#[test]
fn the_estimate_bounds_the_run_with_its_taxonomy() {
    // the peak a run reports is the most it counted, the taxonomy included, so a budget
    // of that much lets the same run end and one byte less stops it
    let axioms = large_ontology(3000);
    let limits = Limits {
        threads: 1,
        ..Limits::default()
    };
    let stats = Classifier::of(&axioms).classify(&limits).unwrap().stats;
    assert!(stats.peak_memory >= stats.memory);
    let used = stats.peak_memory;
    let at = |max_memory| Limits {
        max_memory: Some(max_memory),
        ..limits.clone()
    };
    Classifier::of(&axioms).classify(&at(used)).unwrap();
    match Classifier::of(&axioms).classify(&at(used - 1)).unwrap_err() {
        Error::Memory { limit, used: u } => assert!(u > limit, "{u} <= {limit}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn waiting_messages_count_against_the_memory_limit() {
    // every link from A to B through a subrole of s meets every rule at B, so A's
    // to-do list takes the square of n messages and stores only n of them
    let n = 300;
    let mut axioms = Vec::new();
    for i in 0..n {
        let r = format!("r{i}");
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&r)),
            ope("s"),
        )));
        axioms.push(sub(c("A"), some(&r, c("B"))));
        axioms.push(sub(c("B"), c(&format!("F{i}"))));
        axioms.push(sub(some("s", c(&format!("F{i}"))), c(&format!("G{i}"))));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert!(full.taxonomy.is_subclass(&c("A"), &c("G7")));
    let limit = Limits {
        threads: 1,
        max_memory: Some(full.stats.memory + 200_000),
        ..Limits::default()
    };
    // the error reports what passed the limit, the waiting messages included
    match Classifier::of(&axioms).classify(&limit).unwrap_err() {
        Error::Memory { limit, used } => assert!(used > limit, "{used} <= {limit}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn chains_that_share_operands_are_charged_once_per_entry() {
    let mut axioms = Vec::new();
    for i in 0..100 {
        for (base, sub) in [("r", "rs"), ("s", "ss")] {
            axioms.push(ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ope(&format!("{sub}{i}"))),
                ope(base),
            )));
        }
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Chain(vec![ope("r"), ope("s")]),
            ope(&format!("t{i}")),
        )));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    let limit = Limits {
        max_memory: Some(full.stats.memory * 2),
        ..Limits::default()
    };
    Classifier::of(&axioms).classify(&limit).unwrap();
}

#[test]
fn the_property_closure_polls_the_cancellation() {
    // 300 properties, each a subproperty of every later one, which is a closure of
    // millions of steps over fewer rows than one poll interval of rows would be
    let mut axioms = vec![sub(c("A"), c("B"))];
    for i in 0..300 {
        for j in i + 1..300 {
            axioms.push(ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(ope(&format!("r{i}"))),
                ope(&format!("r{j}")),
            )));
        }
    }
    let counting = |stop_at: usize| {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = n.clone();
        let limits = Limits {
            cancelled: Some(Arc::new(move || {
                m.fetch_add(1, Ordering::Relaxed) + 1 >= stop_at
            })),
            ..Limits::default()
        };
        (n, limits)
    };
    // a whole run polls the closure many times, more than its stages and
    // normalization poll together
    let (n, limits) = counting(usize::MAX);
    Classifier::of(&axioms).classify(&limits).unwrap();
    let polls = n.load(Ordering::Relaxed);
    assert!(polls > 50, "{polls} polls");
    // a cancellation in the middle of the closure stops the run there
    let (n, limits) = counting(polls / 2);
    assert_eq!(
        Classifier::of(&axioms).classify(&limits).unwrap_err(),
        Error::Cancelled
    );
    // one more poll tells the deadline from the cancellation
    assert_eq!(n.load(Ordering::Relaxed), polls / 2 + 1);
}

#[test]
fn a_stop_after_the_saturation_keeps_the_saturated_state() {
    let mut axioms = Vec::new();
    for i in 0..1000 {
        axioms.push(sub(c(&format!("A{i}")), c(&format!("A{}", i + 1))));
    }
    let counting = |stop_at: usize| {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let m = n.clone();
        let limits = Limits {
            threads: 1,
            cancelled: Some(Arc::new(move || {
                m.fetch_add(1, Ordering::Relaxed) + 1 == stop_at
            })),
            ..Limits::default()
        };
        (n, limits)
    };
    // the number of polls of a whole run, the last of them after the taxonomy
    let (n, limits) = counting(0);
    Classifier::of(&axioms).classify(&limits).unwrap();
    let last = n.load(Ordering::Relaxed);
    let (_, limits) = counting(last);
    let mut k = Classifier::of(&axioms);
    assert_eq!(k.classify(&limits).unwrap_err(), Error::Cancelled);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.stats.incremental);
    assert_eq!(out.stats.cleared, 0);
    assert!(out.taxonomy.is_subclass(&c("A0"), &c("A1000")));
}

#[test]
fn removing_an_axiom_not_held_does_not_cancel_a_later_addition() {
    let a = sub(c("A"), c("B"));
    let mut k = Classifier::new();
    k.remove([&a]);
    k.add([&a]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
}

// ---------------------------------------------------------------------------------
// A naive completion procedure for EL++ without concrete domains (Baader, Brandt and
// Lutz, "Pushing the EL Envelope", IJCAI 2005, with the range rule of their 2008
// follow-up), written independently of the classifier as an oracle. Nominals follow
// rule CR6 with its reachability condition: each class is completed in its own world,
// in which the basic concepts reachable from it, from owl:Thing and from the
// individuals have instances, and those that share an individual share their
// subsumers.

#[derive(Default)]
struct Naive {
    n: usize,
    names: BTreeMap<String, usize>,
    /// A1 ⊓ … ⊓ An ⊑ B
    nf1: Vec<(Vec<usize>, usize)>,
    /// A ⊑ ∃r.B
    nf2: Vec<(usize, usize, usize)>,
    /// ∃r.A ⊑ B
    nf3: Vec<(usize, usize, usize)>,
    roles: BTreeMap<String, usize>,
    role_sub: Vec<(usize, usize)>,
    chains: Vec<(usize, usize, usize)>,
    reflexive: BTreeSet<usize>,
    ranges: Vec<(usize, usize)>,
    /// the basic concepts of the individuals
    nominals: BTreeSet<usize>,
}

const NTOP: usize = 0;
const NBOT: usize = 1;

impl Naive {
    fn new() -> Naive {
        Naive {
            n: 2,
            ..Naive::default()
        }
    }
    fn fresh(&mut self) -> usize {
        self.n += 1;
        self.n - 1
    }
    fn role(&mut self, p: &ObjectPropertyExpression) -> usize {
        let k = p.named().as_str().to_string();
        let next = self.roles.len();
        *self.roles.entry(k).or_insert(next)
    }
    fn name(&mut self, e: &ClassExpression) -> usize {
        match e {
            ClassExpression::Class(c) if c == &Class::thing() => NTOP,
            ClassExpression::Class(c) if c == &Class::nothing() => NBOT,
            ClassExpression::Class(c) => {
                if let Some(&i) = self.names.get(c.as_str()) {
                    return i;
                }
                let i = self.fresh();
                self.names.insert(c.as_str().to_string(), i);
                i
            }
            ClassExpression::ObjectIntersectionOf(cs) => {
                let parts: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                let x = self.fresh();
                for &a in &parts {
                    self.nf1.push((vec![x], a));
                }
                self.nf1.push((parts, x));
                x
            }
            ClassExpression::ObjectSomeValuesFrom(r, f) => {
                let f = self.name(f);
                let r = self.role(r);
                let x = self.fresh();
                self.nf2.push((x, r, f));
                self.nf3.push((r, f, x));
                x
            }
            ClassExpression::ObjectOneOf(is) => match is.iter().next() {
                Some(Individual::Named(i)) => self.individual(i.as_str()),
                other => panic!("not in the generator: {other:?}"),
            },
            ClassExpression::ObjectHasValue(r, Individual::Named(i)) => {
                let f = self.individual(i.as_str());
                let r = self.role(r);
                let x = self.fresh();
                self.nf2.push((x, r, f));
                self.nf3.push((r, f, x));
                x
            }
            other => panic!("not in the generator: {other:?}"),
        }
    }
    fn individual(&mut self, i: &str) -> usize {
        let k = format!("{{{i}}}");
        if let Some(&x) = self.names.get(&k) {
            return x;
        }
        let x = self.fresh();
        self.names.insert(k, x);
        self.nominals.insert(x);
        x
    }
    fn named(&mut self, i: &Individual) -> usize {
        match i {
            Individual::Named(n) => self.individual(n.as_str()),
            other => panic!("not in the generator: {other:?}"),
        }
    }
    fn axiom(&mut self, a: &Axiom) {
        match &a.kind {
            AxiomKind::SubClassOf(x, y) => {
                let (x, y) = (self.name(x), self.name(y));
                self.nf1.push((vec![x], y));
            }
            AxiomKind::EquivalentClasses(cs) => {
                let ns: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                for &a in &ns {
                    for &b in &ns {
                        self.nf1.push((vec![a], b));
                    }
                }
            }
            AxiomKind::DisjointClasses(cs) => {
                let ns: Vec<usize> = cs.iter().map(|c| self.name(c)).collect();
                for i in 0..ns.len() {
                    for j in i + 1..ns.len() {
                        self.nf1.push((vec![ns[i], ns[j]], NBOT));
                    }
                }
            }
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(r), s) => {
                let (r, s) = (self.role(r), self.role(s));
                self.role_sub.push((r, s));
            }
            AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(rs), s) => {
                assert_eq!(rs.len(), 2);
                let (a, b, s) = (self.role(&rs[0]), self.role(&rs[1]), self.role(s));
                self.chains.push((a, b, s));
            }
            AxiomKind::TransitiveObjectProperty(r) => {
                let r = self.role(r);
                self.chains.push((r, r, r));
            }
            AxiomKind::ReflexiveObjectProperty(r) => {
                let r = self.role(r);
                self.reflexive.insert(r);
            }
            AxiomKind::ObjectPropertyDomain(r, c) => {
                let (r, c) = (self.role(r), self.name(c));
                self.nf3.push((r, NTOP, c));
            }
            AxiomKind::ObjectPropertyRange(r, c) => {
                let (r, c) = (self.role(r), self.name(c));
                self.ranges.push((r, c));
            }
            AxiomKind::Declaration(Entity::Class(c)) => {
                self.name(&c.clone().into());
            }
            AxiomKind::ClassAssertion(c, i) => {
                let (i, c) = (self.named(i), self.name(c));
                self.nf1.push((vec![i], c));
            }
            AxiomKind::ObjectPropertyAssertion(r, a, b) => {
                let (r, a, b) = (self.role(r), self.named(a), self.named(b));
                self.nf2.push((a, r, b));
            }
            AxiomKind::NegativeObjectPropertyAssertion(r, a, b) => {
                let (r, a, b) = (self.role(r), self.named(a), self.named(b));
                let x = self.fresh();
                self.nf3.push((r, b, x));
                self.nf1.push((vec![a, x], NBOT));
            }
            other => panic!("not in the generator: {other:?}"),
        }
    }

    /// The subsumers of every basic concept in the world of each named class, and of
    /// owl:Thing in the world where only it and the individuals have instances. A world
    /// is inconsistent when a concept with an instance there has ⊥.
    fn complete(mut self) -> (BTreeMap<String, usize>, Vec<Subsumers>) {
        let (sup, reflexive) = self.prepare();
        let names = self.names.clone();
        let mut out = vec![(false, BTreeSet::new()); self.n];
        let classes: Vec<usize> = names
            .iter()
            .filter(|(k, _)| !k.starts_with('{'))
            .map(|(_, &x)| x)
            .chain([NTOP])
            .collect();
        for x in classes {
            let (clash, s) = self.world(&sup, &reflexive, x);
            out[x] = (clash || s[x].contains(&NBOT), s[x].clone());
        }
        (names, out)
    }

    /// The role closure and the reflexive roles, with the ranges pushed into the
    /// fillers of existentials on the right.
    fn prepare(&mut self) -> (Vec<BTreeSet<usize>>, BTreeSet<usize>) {
        let nr = self.roles.len();
        // the reflexive-transitive closure of the role hierarchy
        let mut sup = vec![BTreeSet::new(); nr];
        for (r, s) in sup.iter_mut().enumerate() {
            s.insert(r);
        }
        loop {
            let mut changed = false;
            for &(r, s) in &self.role_sub {
                let add: Vec<usize> = sup[s].iter().copied().collect();
                for t in add {
                    changed |= sup[r].insert(t);
                }
            }
            if !changed {
                break;
            }
        }
        // ranges push into the fillers of existentials on the right
        let mut rng = vec![BTreeSet::new(); nr];
        for &(r, c) in &self.ranges {
            for (q, s) in sup.iter().enumerate() {
                if s.contains(&r) {
                    rng[q].insert(c);
                }
            }
        }
        let old = std::mem::take(&mut self.nf2);
        for (x, r, f) in old {
            if rng[r].is_empty() {
                self.nf2.push((x, r, f));
            } else {
                let y = self.fresh();
                self.nf1.push((vec![y], f));
                for &c in &rng[r] {
                    self.nf1.push((vec![y], c));
                }
                self.nf2.push((x, r, y));
            }
        }
        for &r in &self.reflexive.clone() {
            for &c in &rng[r] {
                self.nf1.push((vec![NTOP], c));
            }
        }
        let reflexive: BTreeSet<usize> = self
            .reflexive
            .iter()
            .flat_map(|&r| sup[r].iter().copied().collect::<Vec<_>>())
            .collect();
        (sup, reflexive)
    }

    /// The completion in the world of `root`: whether it is inconsistent, and the
    /// subsumers of each basic concept.
    #[allow(clippy::needless_range_loop)]
    fn world(
        &self,
        sup: &[BTreeSet<usize>],
        reflexive: &BTreeSet<usize>,
        root: usize,
    ) -> (bool, Vec<BTreeSet<usize>>) {
        let nr = self.roles.len();
        let n = self.n;
        let mut s: Vec<BTreeSet<usize>> = (0..n).map(|x| BTreeSet::from([x, NTOP])).collect();
        let mut rel: Vec<BTreeSet<(usize, usize)>> = vec![BTreeSet::new(); nr];
        for &r in reflexive {
            for x in 0..n {
                rel[r].insert((x, x));
            }
        }
        loop {
            let mut changed = false;
            for x in 0..n {
                for (lhs, b) in &self.nf1 {
                    if lhs.iter().all(|a| s[x].contains(a)) && !s[x].contains(b) {
                        s[x].insert(*b);
                        changed = true;
                    }
                }
                for &(a, r, b) in &self.nf2 {
                    if s[x].contains(&a) {
                        for &t in &sup[r] {
                            changed |= rel[t].insert((x, b));
                        }
                    }
                }
            }
            for r in 0..nr {
                let pairs: Vec<(usize, usize)> = rel[r].iter().copied().collect();
                for (x, y) in pairs {
                    for &(r2, a, b) in &self.nf3 {
                        if r2 == r && s[y].contains(&a) && !s[x].contains(&b) {
                            s[x].insert(b);
                            changed = true;
                        }
                    }
                    if s[y].contains(&NBOT) && !s[x].contains(&NBOT) {
                        s[x].insert(NBOT);
                        changed = true;
                    }
                    for &t in &sup[r] {
                        changed |= rel[t].insert((x, y));
                    }
                }
            }
            for &(r1, r2, t) in &self.chains {
                let left: Vec<(usize, usize)> = rel[r1].iter().copied().collect();
                let right: Vec<(usize, usize)> = rel[r2].iter().copied().collect();
                for &(x, y) in &left {
                    for &(y2, z) in &right {
                        if y == y2 {
                            for &u in &sup[t] {
                                changed |= rel[u].insert((x, z));
                            }
                        }
                    }
                }
            }
            // CR6: the concepts with instances in this world that share an individual
            // are one element
            let mut live: BTreeSet<usize> = [root, NTOP].into_iter().collect();
            live.extend(self.nominals.iter().copied());
            let mut stack: Vec<usize> = live.iter().copied().collect();
            while let Some(x) = stack.pop() {
                for edges in &rel {
                    for &(a, b) in edges {
                        if a == x && live.insert(b) {
                            stack.push(b);
                        }
                    }
                }
            }
            for &o in &self.nominals {
                let group: Vec<usize> = live
                    .iter()
                    .copied()
                    .filter(|&x| s[x].contains(&o))
                    .collect();
                let all: BTreeSet<usize> = group.iter().flat_map(|&x| s[x].clone()).collect();
                for x in group {
                    if s[x].len() < all.len() {
                        s[x] = all.clone();
                        changed = true;
                    }
                }
            }
            if !changed {
                let clash = live.iter().any(|&x| s[x].contains(&NBOT));
                return (clash, s);
            }
        }
    }
}

fn naive_subsumptions(axioms: &[Axiom]) -> BTreeSet<(String, String)> {
    let mut nv = Naive::new();
    for a in axioms {
        nv.axiom(a);
    }
    let (names, s) = nv.complete();
    let mut all: Vec<(String, usize)> = names
        .into_iter()
        .filter(|(k, _)| !k.starts_with('{'))
        .collect();
    all.push((Class::thing().as_str().to_string(), NTOP));
    all.push((Class::nothing().as_str().to_string(), NBOT));
    let inconsistent = s[NTOP].0;
    let mut out = BTreeSet::new();
    for (a, x) in &all {
        for (b, y) in &all {
            if inconsistent || s[*x].1.contains(y) || s[*x].0 || *y == NTOP || *x == NBOT {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

fn classifier_subsumptions(t: &sparkles_owl::classify::Taxonomy) -> BTreeSet<(String, String)> {
    let mut all: Vec<Class> = t.classes().cloned().collect();
    all.push(Class::thing());
    all.push(Class::nothing());
    let mut out = BTreeSet::new();
    for a in &all {
        for b in &all {
            if t.is_subclass(a, b) {
                out.insert((a.as_str().to_string(), b.as_str().to_string()));
            }
        }
    }
    out
}

const CLASSES: usize = 6;
const ROLES: usize = 4;

const INDIVIDUALS: usize = 3;

fn gen_ind() -> impl Strategy<Value = Individual> {
    (0..INDIVIDUALS).prop_map(|i| ind(&format!("i{i}")))
}

fn gen_class() -> impl Strategy<Value = ClassExpression> {
    prop_oneof![
        8 => (0..CLASSES).prop_map(|i| ce(&format!("C{i}"))),
        1 => Just(ClassExpression::Class(Class::thing())),
        1 => gen_ind().prop_map(|i| ClassExpression::ObjectOneOf([i].into_iter().collect())),
    ]
}

fn gen_expr() -> impl Strategy<Value = ClassExpression> {
    gen_class().prop_recursive(2, 8, 2, |inner| {
        prop_oneof![
            3 => (0..ROLES, inner.clone()).prop_map(|(r, f)| some(&format!("r{r}"), f)),
            3 => (inner.clone(), inner).prop_map(|(a, b)| and([a, b])),
            1 => (0..ROLES, gen_ind()).prop_map(|(r, i)| ClassExpression::ObjectHasValue(ope(&format!("r{r}")), i)),
        ]
    })
}

fn r(i: usize) -> ObjectPropertyExpression {
    ope(&format!("r{i}"))
}

/// Ranges go on r0 and r1 only, and chains and transitivity on r2 and r3, which are
/// never subproperties of r0 or r1, so that every ontology meets the range restriction
/// of OWL 2 EL.
fn gen_axiom() -> impl Strategy<Value = Axiom> {
    let role_edges = [
        (0, 1),
        (1, 0),
        (0, 2),
        (0, 3),
        (1, 2),
        (1, 3),
        (2, 3),
        (3, 2),
    ];
    prop_oneof![
        10 => (gen_expr(), gen_expr()).prop_map(|(a, b)| sub(a, b)),
        6 => ((0..CLASSES), gen_expr()).prop_map(|(a, b)| sub(ce(&format!("C{a}")), b)),
        3 => ((0..CLASSES), gen_expr())
            .prop_map(|(a, b)| Axiom::equivalent_classes([ce(&format!("C{a}")), b])),
        1 => ((0..CLASSES), (0..CLASSES)).prop_filter("distinct", |(a, b)| a != b).prop_map(|(a, b)| {
            Axiom::disjoint_classes([ce(&format!("C{a}")), ce(&format!("C{b}"))])
        }),
        2 => (0..role_edges.len()).prop_map(move |i| {
            let (a, b) = role_edges[i];
            ax(AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(r(a)), r(b)))
        }),
        1 => ((0..ROLES), (0..ROLES), (2..ROLES)).prop_map(|(a, b, s)| {
            ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Chain(vec![r(a), r(b)]),
                r(s),
            ))
        }),
        1 => (2..ROLES).prop_map(|s| ax(AxiomKind::TransitiveObjectProperty(r(s)))),
        1 => (0..ROLES).prop_map(|s| ax(AxiomKind::ReflexiveObjectProperty(r(s)))),
        1 => ((0..ROLES), gen_class()).prop_map(|(s, c)| ax(AxiomKind::ObjectPropertyDomain(r(s), c))),
        2 => ((0..2usize), gen_class()).prop_map(|(s, c)| ax(AxiomKind::ObjectPropertyRange(r(s), c))),
        2 => (gen_expr(), gen_ind()).prop_map(|(c, i)| ax(AxiomKind::ClassAssertion(c, i))),
        2 => ((0..ROLES), gen_ind(), gen_ind()).prop_map(|(s, a, b)| ax(AxiomKind::ObjectPropertyAssertion(r(s), a, b))),
        1 => ((0..ROLES), gen_ind(), gen_ind()).prop_map(|(s, a, b)| ax(AxiomKind::NegativeObjectPropertyAssertion(r(s), a, b))),
    ]
}

fn with_declarations(mut axioms: Vec<Axiom>) -> Vec<Axiom> {
    for i in 0..CLASSES {
        axioms.push(Axiom::declaration(Entity::Class(c(&format!("C{i}")))));
    }
    axioms
}

/// An ontology large enough that the stages split their work between threads: a
/// hierarchy of `n` classes with up to three parents each, existential restrictions on
/// a transitive property, definitions, equivalences, individuals and unsatisfiable
/// classes, chosen by a fixed pseudo-random sequence.
fn large_ontology(n: usize) -> Vec<Axiom> {
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |m: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % m as u64) as usize
    };
    let name = |i: usize| format!("K{i}");
    let mut axioms = vec![ax(AxiomKind::TransitiveObjectProperty(ope("part")))];
    for i in 1..n {
        for _ in 0..1 + next(3) {
            axioms.push(sub(c(&name(i)), c(&name(next(i)))));
        }
        if next(8) == 0 {
            axioms.push(sub(c(&name(i)), some("part", c(&name(next(i))))));
        }
        if next(16) == 0 {
            let (a, b) = (next(n), next(n));
            axioms.push(sub(
                and([ce(&name(a)), some("part", ce(&name(b)))]),
                c(&name(i)),
            ));
        }
        if next(64) == 0 {
            axioms.push(Axiom::equivalent_classes([
                ce(&name(i)),
                ce(&name(next(n))),
            ]));
        }
        if next(256) == 0 {
            axioms.push(sub(c(&format!("U{i}")), c(&name(next(n)))));
            axioms.push(sub(c(&format!("U{i}")), c("Empty")));
        }
        if next(32) == 0 {
            axioms.push(ax(AxiomKind::ClassAssertion(
                ce(&name(i)),
                Individual::Named(NamedIndividual::new(format!("http://ex.org/i{i}"))),
            )));
        }
    }
    axioms.push(sub(c("Empty"), Class::nothing()));
    axioms
}

#[test]
fn the_taxonomy_is_the_same_for_every_number_of_threads() {
    let axioms = large_ontology(3000);
    let one = Classifier::of(&axioms)
        .classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .unwrap();
    assert!(one.consistent);
    assert!(
        one.taxonomy.nodes().len() > 1000,
        "{}",
        one.taxonomy.nodes().len()
    );
    assert!(one.taxonomy.unsatisfiable().count() > 0);
    for threads in [2, 8] {
        let limits = Limits {
            threads,
            ..Limits::default()
        };
        let many = Classifier::of(&axioms).classify(&limits).unwrap();
        assert_eq!(one.taxonomy, many.taxonomy, "{threads} threads");
        let kinds: Vec<&AxiomKind> = axioms.iter().map(|a| &a.kind).collect();
        let shared = Saturation::new().classify(&kinds, None, &limits).unwrap();
        assert_eq!(one.taxonomy, shared.taxonomy, "{threads} threads, in place");
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, ..ProptestConfig::default() })]

    #[test]
    fn agrees_with_naive_completion(axioms in prop::collection::vec(gen_axiom(), 0..20), threads in 1..4usize) {
        let axioms = with_declarations(axioms);
        let want = naive_subsumptions(&axioms);
        let out = Classifier::of(&axioms)
            .classify(&Limits { threads, ..Limits::default() })
            .unwrap();
        let got = classifier_subsumptions(&out.taxonomy);
        prop_assert_eq!(got, want);
    }

    #[test]
    fn incremental_equals_full(
        base in prop::collection::vec(gen_axiom(), 0..12),
        steps in prop::collection::vec((prop::collection::vec(gen_axiom(), 0..4), prop::collection::vec(any::<prop::sample::Index>(), 0..4)), 1..5),
        threads in 1..4usize,
    ) {
        let limits = Limits { threads, ..Limits::default() };
        let mut held = with_declarations(base);
        let mut k = Classifier::of(&held);
        k.classify(&limits).unwrap();
        for (adds, removes) in steps {
            let mut removed = Vec::new();
            for i in removes {
                if !held.is_empty() {
                    removed.push(held.remove(i.index(held.len())));
                }
            }
            k.remove(&removed);
            k.add(&adds);
            held.extend(adds);
            let inc = k.classify(&limits).unwrap();
            let full = Classifier::of(&held).classify(&limits).unwrap();
            prop_assert_eq!(&inc.taxonomy, &full.taxonomy);
            prop_assert_eq!(inc.consistent, full.consistent);
        }
    }

    #[test]
    fn a_saturation_follows_the_changes_it_is_given(
        base in prop::collection::vec(gen_axiom(), 0..12),
        steps in prop::collection::vec((prop::collection::vec(gen_axiom(), 0..4), prop::collection::vec(any::<prop::sample::Index>(), 0..4)), 1..5),
        threads in 1..4usize,
    ) {
        let limits = Limits { threads, ..Limits::default() };
        let mut held = with_declarations(base);
        let kinds = |xs: &[Axiom]| -> BTreeSet<AxiomKind> { xs.iter().map(|a| a.kind.clone()).collect() };
        let mut sat = Saturation::new();
        let mut before = kinds(&held);
        let all: Vec<&AxiomKind> = before.iter().collect();
        sat.classify(&all, None, &limits).unwrap();
        for (adds, removes) in steps {
            for i in removes {
                if !held.is_empty() {
                    held.remove(i.index(held.len()));
                }
            }
            held.extend(adds);
            let now = kinds(&held);
            let added: Vec<&AxiomKind> = now.difference(&before).collect();
            let removed: Vec<&AxiomKind> = before.difference(&now).collect();
            let all: Vec<&AxiomKind> = now.iter().collect();
            let inc = sat
                .classify(&all, Some(Changes { added: &added, removed: &removed }), &limits)
                .unwrap();
            let mut k = Classifier::of(&held);
            let full = k.classify(&limits).unwrap();
            prop_assert_eq!(&inc.taxonomy, &full.taxonomy);
            prop_assert_eq!(inc.consistent, full.consistent);
            prop_assert_eq!(sat.excluded().to_vec(), k.excluded());
            before = now;
        }
    }
}

#[test]
fn a_number_past_128_bits_is_not_zero() {
    let huge = Literal::typed("170141183460469231731687303715884105728.0", &xsd("decimal"));
    let mut k = Classifier::of(&[
        sub(c("A"), dvalue("d", huge)),
        sub(dvalue("d", lit("0", "decimal")), c("B")),
    ]);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("B")));
    assert!(k.excluded().is_empty());
    let mut equal = Classifier::of(&[
        sub(
            c("A"),
            dvalue(
                "d",
                lit("170141183460469231731687303715884105728", "integer"),
            ),
        ),
        sub(
            dvalue(
                "d",
                lit("170141183460469231731687303715884105728.0", "decimal"),
            ),
            c("B"),
        ),
    ]);
    let out = equal.classify(&Limits::default()).unwrap();
    assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
    assert!(equal.excluded().is_empty());
    // zero is a number like any other
    let out = run(&[
        sub(c("A"), dvalue("d", lit("0.00", "decimal"))),
        sub(dvalue("d", lit("0", "integer")), c("B")),
    ]);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
}

#[test]
fn empty_universal_properties_are_inconsistent() {
    let out = run(&[ax(AxiomKind::SubObjectPropertyOf(
        SubObjectPropertyExpression::Expression(ObjectPropertyExpression::from(
            ObjectProperty::top(),
        )),
        ObjectPropertyExpression::from(ObjectProperty::bottom()),
    ))]);
    assert!(!out.consistent);
    let out = run(&[ax(AxiomKind::SubDataPropertyOf(
        DataProperty::top(),
        DataProperty::bottom(),
    ))]);
    assert!(!out.consistent);
}

#[test]
fn keys_on_universal_properties_hold_for_every_pair() {
    let different = ax(AxiomKind::DifferentIndividuals(
        [ind("a"), ind("b")].into_iter().collect(),
    ));
    let by_object = run(&[
        ax(AxiomKind::HasKey(
            ce("C"),
            [ObjectPropertyExpression::from(ObjectProperty::top())].into(),
            Default::default(),
        )),
        is_a("C", "a"),
        is_a("C", "b"),
        different.clone(),
    ]);
    assert!(!by_object.consistent);
    let by_data = run(&[
        ax(AxiomKind::HasKey(
            ce("C"),
            Default::default(),
            [DataProperty::top()].into(),
        )),
        is_a("C", "a"),
        is_a("C", "b"),
        different,
    ]);
    assert!(!by_data.consistent);
}

#[test]
fn worlds_count_against_the_memory_limit() {
    // A's world makes every C_i real, so the individual a, which each C_i is, takes every
    // D_i and gives it back to every C_i: the world holds the square of n conclusions,
    // where the global saturation holds a few per class
    let n = 400;
    let mut axioms = Vec::new();
    for i in 0..n {
        let ci = format!("C{i}");
        axioms.push(sub(c("A"), some("r", c(&ci))));
        axioms.push(sub(c(&ci), one("a")));
        axioms.push(sub(c(&ci), c(&format!("D{i}"))));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert!(full.consistent);
    assert!(!full.taxonomy.is_subclass(&c("C0"), &c("D7")));
    // the peak counts the world's copies, which are gone once it ends
    assert!(full.stats.peak_memory > full.stats.memory + 1_000_000);
    let limit = Limits {
        threads: 1,
        max_memory: Some(full.stats.memory + 1_000_000),
        ..Limits::default()
    };
    match Classifier::of(&axioms).classify(&limit).unwrap_err() {
        Error::Memory { limit, used } => assert!(used > limit, "{used} <= {limit}"),
        e => panic!("{e:?}"),
    }
}

#[test]
fn messages_a_world_sends_count_against_the_memory_limit() {
    // in A's world B is real, so the individual a, which B is, takes every G_j, and F,
    // which is a too, takes them from a. Each G_j at F sends H_j back over the n links
    // from A, so F sends the square of n messages to A, which stores only n of them.
    let n = 400;
    let mut axioms = vec![
        sub(c("A"), some("t", c("B"))),
        sub(c("B"), one("a")),
        sub(c("F"), one("a")),
    ];
    for i in 0..n {
        let r = format!("r{i}");
        axioms.push(ax(AxiomKind::SubObjectPropertyOf(
            SubObjectPropertyExpression::Expression(ope(&r)),
            ope("s"),
        )));
        axioms.push(sub(c("A"), some(&r, c("F"))));
        axioms.push(sub(c("B"), c(&format!("G{i}"))));
        axioms.push(sub(some("s", c(&format!("G{i}"))), c(&format!("H{i}"))));
    }
    let full = Classifier::of(&axioms)
        .classify(&Limits::default())
        .unwrap();
    assert!(full.taxonomy.is_subclass(&c("A"), &c("H7")));
    assert!(!full.taxonomy.is_subclass(&c("F"), &c("G7")));
    let limit = Limits {
        threads: 1,
        max_memory: Some(full.stats.memory + 1_000_000),
        ..Limits::default()
    };
    match Classifier::of(&axioms).classify(&limit).unwrap_err() {
        Error::Memory { limit, used } => assert!(used > limit, "{used} <= {limit}"),
        e => panic!("{e:?}"),
    }
}

// DL queries: what the saturated contexts say about one class, where some classes and
// the query's fresh class are classified in worlds of their own.

/// Checks that `describe` answers as the taxonomy does for every satisfiable named class.
fn describe_matches_taxonomy(axioms: &[Axiom]) {
    let t = run(axioms).taxonomy;
    let mut k = Classifier::of(axioms);
    k.saturate(&Limits::default()).unwrap();
    let firsts = |ns: Vec<usize>| -> Vec<Class> {
        let mut v: Vec<Class> = ns
            .into_iter()
            .flat_map(|n| t.node(n).classes.clone())
            .collect();
        v.sort();
        v
    };
    for class in t.classes().cloned().collect::<Vec<_>>() {
        if t.is_unsatisfiable(&class) || class == Class::thing() {
            continue;
        }
        let d = k.describe(&class).unwrap();
        let n = t.node_of(&class).unwrap();
        let mut eq: Vec<Class> = t.equivalents(&class).to_vec();
        eq.retain(|e| *e != class);
        eq.sort();
        assert_eq!(d.equivalents, eq, "{class:?}");
        assert_eq!(
            d.direct_superclasses,
            firsts(t.node(n).parents.clone()),
            "{class:?}"
        );
        assert_eq!(d.superclasses, firsts(t.ancestors(n)), "{class:?}");
        assert_eq!(
            d.direct_subclasses,
            firsts(t.node(n).children.clone()),
            "{class:?}"
        );
        assert_eq!(d.subclasses, firsts(t.descendants(n)), "{class:?}");
    }
}

fn describe_with(axioms: &[Axiom], q: ClassExpression) -> sparkles_owl::classify::Description {
    let mut all = axioms.to_vec();
    all.push(Axiom::equivalent_classes([ce("Q"), q]));
    let mut k = Classifier::of(&all);
    k.saturate(&Limits::default()).unwrap();
    k.describe(&c("Q")).unwrap()
}

fn named(cs: &[&str]) -> Vec<Class> {
    let mut v: Vec<Class> = cs
        .iter()
        .map(|s| if *s == "Thing" { Class::thing() } else { c(s) })
        .collect();
    v.sort();
    v
}

#[test]
fn describe_reads_the_subsumers_a_world_finds() {
    // T ≡ ∃r.{b} ⊓ ∃s.{b}, where T has an instance, puts b in C by range(r) = C, so
    // T ⊑ ∃s.C ≡ X in T's world only, and Q ≡ Y ⊓ ∃s.{b} is below T and X
    let axioms = [
        sub(c("Y"), value("r", "b")),
        range("r", "C"),
        Axiom::equivalent_classes([ce("X"), some("s", c("C"))]),
        Axiom::equivalent_classes([ce("T"), and([value("r", "b"), value("s", "b")])]),
        sub(c("W"), value("s", "b")),
        sub(c("Z"), and([ce("Y"), value("s", "b")])),
    ];
    describe_matches_taxonomy(&axioms);
    let d = describe_with(&axioms, and([ce("Y"), value("s", "b")]));
    assert!(d.satisfiable);
    assert_eq!(d.superclasses, named(&["T", "Thing", "X", "Y"]));
    // X is above T, which only T's world shows
    assert_eq!(d.direct_superclasses, named(&["T", "Y"]));
    assert_eq!(d.subclasses, named(&["Z"]));
    let x = describe_with(&axioms, some("s", c("C")));
    assert_eq!(x.equivalents, named(&["X"]));
    assert_eq!(x.subclasses, named(&["T", "Z"]));
    assert_eq!(x.direct_subclasses, named(&["T"]));
}

#[test]
fn describe_answers_for_individuals_and_nominals() {
    // a is in Y, so b is in C everywhere, and d, which reaches b by s, is in X
    let axioms = [
        sub(c("Y"), value("r", "b")),
        range("r", "C"),
        Axiom::equivalent_classes([ce("X"), some("s", c("C"))]),
        sub(c("W"), value("s", "b")),
        is_a("Y", "a"),
        assert_("s", "d", "b"),
    ];
    describe_matches_taxonomy(&axioms);
    let x = describe_with(&axioms, some("s", c("C")));
    assert_eq!(x.equivalents, named(&["X"]));
    assert_eq!(x.subclasses, named(&["W"]));
    assert_eq!(x.instances, vec![NamedIndividual::new("http://ex.org/d")]);
    // a class of one individual takes what the individual has
    let a = describe_with(&axioms, one("a"));
    assert_eq!(a.superclasses, named(&["Thing", "Y"]));
    assert_eq!(a.instances, vec![NamedIndividual::new("http://ex.org/a")]);
    let b = describe_with(&axioms, value("s", "b"));
    assert_eq!(b.superclasses, named(&["Thing", "X"]));
    assert_eq!(b.subclasses, named(&["W"]));
    assert_eq!(b.instances, vec![NamedIndividual::new("http://ex.org/d")]);
}
