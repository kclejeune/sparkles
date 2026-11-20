//! The rooted run of roadmap item 603's sixth phase: nominals and the object ABox in the
//! disjunctive rules (F24, "Nominals in the disjunctive rules"). Each case adds one
//! disjunctive axiom that no other axiom meets, so that the rooted run takes it, and runs
//! with counting off unless it says otherwise, on one and four threads under both
//! expansion strategies. Every run asks for the rooted run with `Limits::rooted`, which
//! is off by default, since item 804 lands it as an option: without it, a run with
//! individuals takes the EL path and leaves the disjunctive axiom out.

use sparkles_owl::classify::{Classification, Classifier, Consistency, Expansion, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

/// A disjunction that no other axiom meets, so that Tier D takes each case below.
const UNION: &str = "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))";

fn c(s: &str) -> Class {
    match s {
        "Nothing" => Class::nothing(),
        _ => Class::new(format!("{EX}{s}")),
    }
}

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n{UNION}\n)"
    ))
    .unwrap()
    .axioms
}

/// Classifies on one and four threads under both strategies, checks that Tier D took every
/// axiom and that the runs agree on the consistency and the asked subsumptions, and returns
/// the last answer.
fn rooted(body: &str, counting: bool, asked: &[(&str, &str)]) -> Classification {
    let ax = axioms(body);
    let mut outs: Vec<(Consistency, Vec<bool>)> = Vec::new();
    let mut last = None;
    for (threads, expansion) in [
        (1, Expansion::Cautious),
        (4, Expansion::Cautious),
        (1, Expansion::Eager),
        (4, Expansion::Eager),
    ] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                counting,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert!(out.stats.clauses > 0, "Tier D took no clause");
        assert!(out.complete);
        let got = asked
            .iter()
            .map(|(a, b)| out.taxonomy.is_subclass(&c(a), &c(b)))
            .collect();
        outs.push((out.consistency, got));
        last = Some(out);
    }
    assert!(
        outs.windows(2).all(|w| w[0] == w[1]),
        "the answer depends on the threads or the strategy: {outs:?}"
    );
    last.unwrap()
}

/// Checks the subsumptions that hold and those that do not, on a consistent ontology.
fn check(body: &str, yes: &[(&str, &str)], no: &[(&str, &str)]) {
    let asked: Vec<(&str, &str)> = yes.iter().chain(no).copied().collect();
    let out = rooted(body, false, &asked);
    assert_eq!(out.consistency, Consistency::Consistent);
    for (a, b) in yes {
        assert!(
            out.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is missing"
        );
        // a subsumption that holds because its class is wrongly empty proves nothing
        assert!(!out.taxonomy.is_unsatisfiable(&c(a)), "{a} is satisfiable");
    }
    for (a, b) in no {
        assert!(
            !out.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is not entailed"
        );
    }
}

#[test]
fn a_nominal_filler_takes_its_class_from_a_class_axiom() {
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(ObjectOneOf(:o) :B)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
        &[("A", "C")],
        &[("C", "A"), ("B", "C")],
    );
}

#[test]
fn two_paths_meet_at_one_nominal_and_combine_their_cases() {
    // the cases B and C of o meet D of o, which the second path gives, so o is an E
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectIntersectionOf(ObjectOneOf(:o) ObjectUnionOf(:B :C))))
         SubClassOf(:A ObjectSomeValuesFrom(:s ObjectIntersectionOf(ObjectOneOf(:o) :D)))
         SubClassOf(ObjectIntersectionOf(:B :D) :E)
         SubClassOf(ObjectIntersectionOf(:C :D) :E)
         SubClassOf(ObjectSomeValuesFrom(:r :E) :F)",
        &[("A", "F")],
        &[("A", "E"), ("A", "B"), ("A", "C")],
    );
}

#[test]
fn a_fact_one_class_gives_a_nominal_stays_conditional() {
    // A makes o a B only when A has an instance, so E, which also reaches o, is not an F
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(:A ObjectAllValuesFrom(:r :B))
         SubClassOf(:E ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(ObjectSomeValuesFrom(:r :B) :F)",
        &[("A", "F")],
        &[("E", "F")],
    );
}

#[test]
fn an_equality_of_individuals_carries_their_classes() {
    check(
        "SameIndividual(:a :b)
         ClassAssertion(:B :a)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
        &[("A", "C")],
        &[("C", "A")],
    );
}

#[test]
fn an_equality_of_individuals_carries_their_role_facts() {
    check(
        "SameIndividual(:a :b)
         ObjectPropertyAssertion(:s :a :c)
         ClassAssertion(:B :c)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         SubClassOf(ObjectSomeValuesFrom(:r ObjectSomeValuesFrom(:s :B)) :C)",
        &[("A", "C")],
        &[("C", "A")],
    );
}

#[test]
fn a_key_makes_two_individuals_with_a_shared_named_value_one() {
    // a and b are K and share the p-value c, so the key makes them one and b is a G
    check(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         ClassAssertion(:G :a)
         SubClassOf(:D ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         SubClassOf(ObjectSomeValuesFrom(:r :G) :H)",
        &[("D", "H")],
        &[("H", "D")],
    );
}

#[test]
fn a_has_value_restriction_is_an_existential_to_the_nominal() {
    check(
        "SubClassOf(:A ObjectHasValue(:r :o))
         ClassAssertion(:B :o)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
        &[("A", "C")],
        &[],
    );
}

#[test]
fn a_universal_over_the_inverse_at_a_nominal_reaches_back_to_its_predecessor() {
    // A's r-successor is o itself, so o's universal over r⁻ makes A an E, and a class below
    // a singleton as the filler gives the same
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(:B ObjectOneOf(:p))
         SubClassOf(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :G)))
         ClassAssertion(ObjectAllValuesFrom(ObjectInverseOf(:r) :E) :o)
         ClassAssertion(ObjectAllValuesFrom(ObjectInverseOf(:r) ObjectUnionOf(:E :F)) :p)
         SubClassOf(ObjectSomeValuesFrom(:r :G) ObjectComplementOf(:F))",
        &[("A", "E"), ("D", "E")],
        &[("E", "A"), ("D", "F")],
    );
}

#[test]
fn a_universal_at_a_predecessor_meets_the_nominal_it_reaches() {
    // A's r-successor is o, which A's universal makes a B, and B and C at o clash, so A is
    // empty, while D, which reaches o through a class below the singleton, is not
    let out = rooted(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(:A ObjectAllValuesFrom(:r ObjectUnionOf(:B :B2)))
         SubClassOf(:B2 :B)
         ClassAssertion(:C :o)
         DisjointClasses(:B :C)
         SubClassOf(:O ObjectOneOf(:o))
         SubClassOf(:D ObjectSomeValuesFrom(:r :O))
         SubClassOf(:E ObjectSomeValuesFrom(:r :O))
         SubClassOf(:E ObjectAllValuesFrom(:r :B))",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(out.taxonomy.is_unsatisfiable(&c("E")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("D")));
}

#[test]
fn a_nominal_covering_the_domain_gives_the_subsumptions_of_owl_thing() {
    // every element is a, which is b, so every element is a C, owl:Thing included
    let thing = Class::new("http://www.w3.org/2002/07/owl#Thing");
    let out = rooted(
        "SubClassOf(owl:Thing ObjectOneOf(:a))
         ClassAssertion(:C :b)
         SubClassOf(:D :C)",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_subclass(&thing, &c("C")));
    assert!(out.taxonomy.is_subclass(&c("Z"), &c("C")));
    assert!(!out.taxonomy.is_subclass(&thing, &c("D")));
}

#[test]
fn an_enumeration_is_a_case_analysis_over_its_individuals() {
    // every member of {a, b} is a D, so a class below the enumeration is too
    check(
        "SubClassOf(:A ObjectOneOf(:a :b))
         ClassAssertion(:D :a)
         ClassAssertion(:D :b)
         ClassAssertion(:E :a)",
        &[("A", "D")],
        &[("A", "E")],
    );
}

/// Classes and their subsumptions do not matter to an inconsistent ontology, so the cases
/// below check the consistency alone.
fn inconsistent(body: &str) {
    let out = rooted(body, false, &[("A", "Nothing")]);
    assert_eq!(out.consistency, Consistency::Inconsistent);
    assert!(!out.consistent);
}

#[test]
fn a_singleton_in_a_class_and_its_complement_is_inconsistent() {
    inconsistent(
        "SubClassOf(ObjectOneOf(:o) :B)
         SubClassOf(ObjectOneOf(:o) ObjectComplementOf(:B))
         SubClassOf(:A :B)",
    );
}

#[test]
fn contradictory_assertions_make_the_ontology_inconsistent() {
    // every class is satisfiable on its own, but a cannot be both
    inconsistent(
        "ClassAssertion(:B :a)
         ClassAssertion(ObjectComplementOf(:B) :a)
         SubClassOf(:A :C)",
    );
}

#[test]
fn a_negative_property_assertion_meets_the_edge_a_class_gives() {
    inconsistent(
        "NegativeObjectPropertyAssertion(:r :a :o)
         ClassAssertion(:B :a)
         SubClassOf(:B ObjectHasValue(:r :o))
         SubClassOf(:A :C)",
    );
}

#[test]
fn individuals_both_the_same_and_different_are_inconsistent() {
    inconsistent(
        "SameIndividual(:a :b)
         DifferentIndividuals(:a :b)
         SubClassOf(:A :C)",
    );
}

#[test]
fn a_disjunctive_fact_about_an_individual_closes_on_its_assertions() {
    // a is a B or a C, and C is closed for a, so a is a B, and so is the A that reaches it
    check(
        "ClassAssertion(ObjectUnionOf(:B :C) :a)
         ClassAssertion(ObjectComplementOf(:C) :a)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(ObjectSomeValuesFrom(:r :B) :D)",
        &[("A", "D")],
        &[],
    );
}

#[test]
fn a_minimum_of_two_into_one_nominal_needs_counting() {
    // read as ∃r.{o}, the minimum would make A a C
    let body = "SubClassOf(:A ObjectMinCardinality(2 :r ObjectOneOf(:o)))
                ClassAssertion(:B :o)
                SubClassOf(ObjectSomeValuesFrom(:r :B) :C)";
    // with counting A is empty in a consistent ontology
    let out = rooted(body, true, &[("A", "Nothing")]);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    // without it the minimum alone is left out and reported, and not read as an
    // existential, while Tier D takes the rest
    let mut k = Classifier::of(&axioms(body));
    let out = k
        .classify(&Limits {
            rooted: true,
            ..Limits::default()
        })
        .unwrap();
    let left: Vec<String> = k
        .excluded()
        .iter()
        .map(|e| format!("{:?}", e.axiom))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("MinCardinality"), "{left:?}");
    assert!(out.stats.clauses > 0, "Tier D took no clause");
    assert!(!out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(
        !out.taxonomy.is_subclass(&c("A"), &c("C")),
        "the minimum was weakened"
    );
}

#[test]
fn minimum_fillers_are_not_coloured_across_individuals() {
    // three sources each with two successors from a different pair of three individuals:
    // satisfiable, though globally disjoint fillers would ask to two-colour a triangle
    let out = rooted(
        "SubClassOf(:M ObjectMinCardinality(2 :r))
         SubClassOf(ObjectOneOf(:p) ObjectIntersectionOf(:M ObjectAllValuesFrom(:r ObjectOneOf(:a :b))))
         SubClassOf(ObjectOneOf(:q) ObjectIntersectionOf(:M ObjectAllValuesFrom(:r ObjectOneOf(:b :c))))
         SubClassOf(ObjectOneOf(:t) ObjectIntersectionOf(:M ObjectAllValuesFrom(:r ObjectOneOf(:a :c))))
         DifferentIndividuals(:a :b :c)",
        true,
        &[("M", "Nothing")],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(!out.taxonomy.is_unsatisfiable(&c("M")));
}

/// An irreflexive property beside a nominal is left out and reported, and the rooted run
/// takes the rest, as it does with a minimum above one when counting is off.
#[test]
fn an_irreflexive_property_beside_a_nominal_is_left_out() {
    // r(a, a) closes a loop that the rules check only on simple properties without loops
    let ax = axioms(
        "IrreflexiveObjectProperty(:r)
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(ObjectOneOf(:o) :B)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            rooted: true,
            ..Limits::default()
        })
        .unwrap();
    let left: Vec<_> = k.excluded().into_iter().map(|e| e.axiom).collect();
    assert!(
        matches!(left[..], [AxiomKind::IrreflexiveObjectProperty(_)]),
        "only the irreflexive axiom is left out: {left:#?}"
    );
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert!(out.stats.clauses > 0, "the rooted run takes the rest");
    assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
}

/// A rooted run without counting has a bound on its derived conclusions, `Limits::dl_work`.
/// A run that passes it classifies again without the rooted rules, which leaves the
/// disjunctive axioms out and reports them, as a run without the option does, and says so
/// in `Stats::dl_rooted_over`.
#[test]
fn a_rooted_run_past_its_bound_answers_without_the_rooted_rules() {
    // a is a B, since it is a B or a C and not a C, so the A that reaches it is a D; the
    // EL rules alone leave the union and the complement out and do not find it
    let body = "ClassAssertion(ObjectUnionOf(:B :C) :a)
                ClassAssertion(ObjectComplementOf(:C) :a)
                SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    let ax = axioms(body);
    let mut plain = Classifier::of(&ax);
    let want = plain.classify(&Limits::default()).unwrap();
    assert!(!want.taxonomy.is_subclass(&c("A"), &c("D")));
    let left = plain.excluded().len();
    assert!(left > 0, "the EL path leaves the disjunctive axioms out");
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                dl_work: Some(1),
                ..Limits::default()
            })
            .unwrap();
        assert!(
            out.stats.dl_rooted_over,
            "{threads} threads: the bound did not fall back"
        );
        assert!(out.stats.dl_over_budget);
        assert!(!out.stats.dl_counting_over);
        assert!(out.complete);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert_eq!(k.excluded().len(), left, "{:#?}", k.excluded());
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert_eq!(out.stats.tier, want.stats.tier);
    }
    // within the bound the rooted run answers in full
    let out = rooted(body, false, &[("A", "D")]);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert!(!out.stats.dl_rooted_over);
    assert!(!out.stats.dl_over_budget);
}

/// The bound's stop is the run's own, not the caller's, so a run that may answer in part
/// still falls back to the default answer, complete, instead of a partial answer.
#[test]
fn a_rooted_run_past_its_bound_falls_back_though_it_may_answer_in_part() {
    let body = "ClassAssertion(ObjectUnionOf(:B :C) :a)
                ClassAssertion(ObjectComplementOf(:C) :a)
                SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    let ax = axioms(body);
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                partial: true,
                dl_work: Some(1),
                ..Limits::default()
            })
            .unwrap();
        assert!(out.stats.dl_rooted_over, "{threads} threads");
        assert!(out.stats.dl_over_budget);
        assert!(
            out.complete,
            "{threads} threads: a partial answer instead of the fallback"
        );
        assert!(out.incomplete.is_empty());
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(!k.excluded().is_empty());
    }
}

/// A rooted run that counts keeps the counting bound: past it the run starts again
/// without counting, with the rooted bound, and past that one without the rooted rules.
/// It does so with `Limits::dl_steps` as without it (roadmap item 727).
#[test]
fn a_rooted_run_that_counts_falls_back_twice_past_a_tiny_bound() {
    let body = "ClassAssertion(ObjectUnionOf(:B :C) :a)
                ClassAssertion(ObjectComplementOf(:C) :a)
                SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                SubClassOf(:A ObjectMaxCardinality(1 :r))
                SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    let ax = axioms(body);
    for (threads, partial) in [(1, false), (4, false), (1, true), (4, true)] {
        for dl_steps in [false, true] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    rooted: true,
                    counting: true,
                    partial,
                    dl_steps,
                    dl_work: Some(1),
                    ..Limits::default()
                })
                .unwrap();
            let run = format!("{threads} threads, partial {partial}, steps {dl_steps}");
            assert!(out.stats.dl_counting_over, "{run}");
            assert!(out.stats.dl_rooted_over, "{run}");
            assert!(out.stats.dl_over_budget, "{run}");
            assert!(out.complete, "{run}");
            assert_eq!(out.consistency, Consistency::Consistent);
            assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
            assert!(!k.excluded().is_empty());
        }
    }
    // with a bound the run finishes within, it answers in full with counting
    let out = rooted(body, true, &[("A", "D")]);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert!(!out.stats.dl_over_budget);
}

/// A rooted run that counts gives a compound qualifier of a maximum a name, as a run
/// without the root does (roadmap item 897). `a` has two different r-successors, each in
/// `P` and `Q` and each with an s-successor in `C`, and at most one r-successor in the
/// intersection or with such a successor, which makes the ontology inconsistent. With one
/// of the two outside the qualifier it is consistent.
#[test]
fn a_rooted_run_counts_the_neighbours_in_a_compound_qualifier() {
    let facts = "ClassAssertion(:A :a) ObjectPropertyAssertion(:r :a :b)
                 ObjectPropertyAssertion(:r :a :c) DifferentIndividuals(:b :c)
                 ClassAssertion(:P :b) ClassAssertion(:Q :b) ClassAssertion(:P :c)
                 ObjectPropertyAssertion(:s :b :d) ClassAssertion(:C :d)
                 SubClassOf(:D ObjectUnionOf(:D1 :D2))";
    for (more, qualifier, consistency) in [
        (
            "ClassAssertion(:Q :c)",
            "ObjectIntersectionOf(:P :Q)",
            Consistency::Inconsistent,
        ),
        ("", "ObjectIntersectionOf(:P :Q)", Consistency::Consistent),
        (
            "ObjectPropertyAssertion(:s :c :d)",
            "ObjectSomeValuesFrom(:s :C)",
            Consistency::Inconsistent,
        ),
        ("", "ObjectSomeValuesFrom(:s :C)", Consistency::Consistent),
    ] {
        let body = format!("{facts} {more} SubClassOf(:A ObjectMaxCardinality(1 :r {qualifier}))");
        let out = rooted(&body, true, &[]);
        assert_eq!(out.consistency, consistency, "{qualifier} with {more:?}");
    }
}

#[test]
fn a_key_with_a_disjunction_of_its_class_still_merges_the_individuals() {
    // a is a K or an E, which is below K, so a is a K, and a and b share the r-value o
    check(
        "HasKey(:K (:r) ())
         SubClassOf(:E :K)
         ClassAssertion(ObjectUnionOf(:K :E) :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:r :a :o)
         ObjectPropertyAssertion(:r :b :o)
         ClassAssertion(:G :a)
         EquivalentClasses(:D ObjectOneOf(:b))",
        &[("D", "G"), ("D", "K")],
        &[("G", "D")],
    );
}

#[test]
fn keys_are_a_taxonomy_with_no_class_of_the_reduction() {
    let out = rooted(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)",
        false,
        &[],
    );
    for n in out.taxonomy.nodes() {
        for cl in &n.classes {
            assert!(
                !cl.as_str().starts_with("urn:sparkles:internal:"),
                "{}",
                cl.as_str()
            );
        }
    }
}

#[test]
fn the_universal_property_in_a_universal_restriction_reaches_every_element() {
    // A has no p-successor, but ∀top.B makes every element a B, and a B is a C
    check(
        "SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))
         ClassAssertion(:A :a)
         EquivalentClasses(:D ObjectOneOf(:b))
         SubClassOf(:B :C)",
        &[("D", "B"), ("D", "C")],
        &[("B", "D")],
    );
}

#[test]
fn a_universal_restriction_over_the_universal_property_is_conditional() {
    // F is an A with an r-successor that is not a B, which ∀top.B forbids
    let out = rooted(
        "SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))
         SubClassOf(:E ObjectSomeValuesFrom(:r ObjectComplementOf(:B)))
         EquivalentClasses(:F ObjectIntersectionOf(:A :E))",
        false,
        &[],
    );
    assert!(out.taxonomy.is_unsatisfiable(&c("F")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("E")));
}

#[test]
fn a_subproperty_of_the_universal_property_is_dropped() {
    check(
        "SubObjectPropertyOf(:r owl:topObjectProperty)
         SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
        &[("A", "C")],
        &[("C", "A")],
    );
}

#[test]
fn an_existential_over_the_universal_property_reaches_a_nominal() {
    // every element sees o, so every element is a C
    check(
        "ClassAssertion(:B :o)
         SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :B) :C)
         SubClassOf(:A :A)",
        &[("A", "C")],
        &[("C", "A")],
    );
}

const INTERNAL: &str = "urn:sparkles:internal:";

fn rooted_limits() -> Limits {
    Limits {
        rooted: true,
        ..Limits::default()
    }
}

/// Asserts that no name the reductions of item 805 make up reaches the classifier's answer,
/// its description of each asked class or its report of the axioms left out.
fn no_internal_names(k: &Classifier, out: &Classification, asked: &[Class]) {
    for n in out.taxonomy.nodes() {
        for cl in &n.classes {
            assert!(!cl.as_str().starts_with(INTERNAL), "{}", cl.as_str());
        }
    }
    for cl in &out.incomplete {
        assert!(!cl.as_str().starts_with(INTERNAL), "{}", cl.as_str());
    }
    let excluded = format!("{:?}", k.excluded());
    assert!(!excluded.contains(INTERNAL), "{excluded}");
    for cl in asked {
        let d = format!("{:?}", k.describe(cl));
        assert!(!d.contains(INTERNAL), "{d}");
    }
}

#[test]
fn the_names_of_the_reductions_stay_out_of_every_answer() {
    let ax = axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))
         ClassAssertion(:A :a)
         ClassAssertion(:Q :d)
         SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :Q) :R)",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert!(out.stats.clauses > 0);
    assert_eq!(out.stats.added, ax.len());
    // every element is a B and an R, the hub individual among them, which stays out
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("B")));
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("R")));
    let d = k.describe(&Class::thing()).unwrap();
    assert!(d.instances.iter().any(|i| i.as_str() == format!("{EX}a")));
    no_internal_names(&k, &out, &[Class::thing(), c("K"), c("B"), c("R"), c("Z")]);
}

#[test]
fn runs_after_a_change_of_keys_answer_as_a_fresh_classifier() {
    let base = axioms(
        "ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         ClassAssertion(:G :a)
         SubClassOf(:D ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         SubClassOf(ObjectSomeValuesFrom(:r :G) :H)",
    );
    let key = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nOntology(\nHasKey(:K (:p) ())\nSubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))\n)"
    ))
    .unwrap()
    .axioms;
    let fresh = |ax: &[&Axiom]| {
        let mut k = Classifier::of(ax.iter().copied());
        k.classify(&rooted_limits()).unwrap().taxonomy
    };
    let without: Vec<&Axiom> = base.iter().collect();
    let with: Vec<&Axiom> = base.iter().chain(&key).collect();
    let mut k = Classifier::of(&base);
    let t = k.classify(&rooted_limits()).unwrap().taxonomy;
    assert_eq!(t, fresh(&without));
    assert!(!t.is_subclass(&c("D"), &c("H")));
    k.add(&key);
    let t = k.classify(&rooted_limits()).unwrap().taxonomy;
    assert_eq!(t, fresh(&with));
    assert!(t.is_subclass(&c("D"), &c("H")));
    // a second run of the same rewritten axioms
    let t = k.classify(&rooted_limits()).unwrap().taxonomy;
    assert_eq!(t, fresh(&with));
    k.remove(&key);
    let t = k.classify(&rooted_limits()).unwrap().taxonomy;
    assert_eq!(t, fresh(&without));
    assert!(!t.is_subclass(&c("D"), &c("H")));
}

#[test]
fn a_name_with_the_prefix_of_the_reductions_keeps_the_ontology_from_them() {
    let ax = axioms(&format!(
        "SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty <{INTERNAL}u>))
         SubClassOf(<{INTERNAL}m0-0-0-1> :A)"
    ));
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    // the ontology's own names stay in the taxonomy, and the universal restriction is
    // left out as it is without the reductions
    let own = Class::new(format!("{INTERNAL}m0-0-0-1"));
    assert!(out.taxonomy.is_subclass(&own, &c("A")));
    assert!(
        k.excluded()
            .iter()
            .any(|e| format!("{:?}", e.axiom).contains("ObjectAllValuesFrom")),
        "{:#?}",
        k.excluded()
    );
}

/// Classifies with the rooted run, checks that no made-up name reaches the answer, and
/// returns the axioms left out, for the cases the reductions decline. With `plain`, it also
/// checks that the answer and the axioms left out are those of a run without the option,
/// which holds when something other than the reductions keeps the rooted run away.
fn declined(body: &str, plain: bool) -> Vec<String> {
    let ax = axioms(body);
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    no_internal_names(&k, &out, &[Class::thing()]);
    if plain {
        let mut p = Classifier::of(&ax);
        let default = p.classify(&Limits::default()).unwrap();
        assert_eq!(out.taxonomy, default.taxonomy);
        assert_eq!(out.consistency, default.consistency);
        assert_eq!(k.excluded(), p.excluded());
    }
    k.excluded()
        .iter()
        .map(|e| format!("{:?}", e.axiom))
        .collect()
}

#[test]
fn a_universal_property_in_a_maximum_is_left_as_it_is() {
    // the rooted run takes the union and leaves the maximum out, as it did before the
    // reductions, and the maximum is reported as it is written
    let left = declined(
        "SubClassOf(:A ObjectMaxCardinality(3 owl:topObjectProperty))
         ClassAssertion(:A :a)",
        false,
    );
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("ObjectMaxCardinality"), "{left:?}");
    assert!(left[0].contains("topObjectProperty"), "{left:?}");
}

#[test]
fn a_minimum_of_one_over_the_universal_property_is_an_existential() {
    check(
        "ClassAssertion(:B :o)
         SubClassOf(ObjectMinCardinality(1 owl:topObjectProperty :B) :C)
         SubClassOf(:A :A)",
        &[("A", "C")],
        &[("C", "A")],
    );
}

#[test]
fn a_functional_property_above_the_universal_property_is_left_out() {
    // p is above the transitive property that stands for the universal one, so p is not
    // simple, and the rooted run leaves its functionality out and reports it, while it
    // takes the equivalence and the union
    let left = declined(
        "EquivalentObjectProperties(owl:topObjectProperty :p)
         FunctionalObjectProperty(:p)
         ClassAssertion(:A :a)",
        false,
    );
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("FunctionalObjectProperty"), "{left:?}");
}

#[test]
fn a_key_past_the_budget_of_its_reduction_is_left_as_it_is() {
    // seventy individuals make about 170,000 axioms of one key, past the bound of 131,072
    let mut body = String::from("HasKey(:K (:p) ())\n");
    for i in 0..70 {
        body.push_str(&format!("ClassAssertion(:K :i{i})\n"));
    }
    // the key keeps the rooted run away, and the EL rules take it as they do without the
    // option and leave the union out
    let left = declined(&body, true);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("ObjectUnionOf"), "{left:?}");
}

#[test]
fn without_the_rooted_run_keys_and_the_universal_property_are_left_as_they_are() {
    let ax = axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         SubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    no_internal_names(&k, &out, &[Class::thing()]);
}

#[test]
fn the_names_of_axioms_the_rewriting_drops_stay_in_the_signature() {
    // a key without two individuals, a tautology over the universal property and a
    // minimum of none over it make nothing, but their classes and individuals stay
    for (body, class) in [
        ("HasKey(:K (:p) ())", "K"),
        (
            "SubClassOf(:A ObjectMinCardinality(0 owl:topObjectProperty :Q))",
            "Q",
        ),
    ] {
        let mut k = Classifier::of(&axioms(body));
        let out = k.classify(&rooted_limits()).unwrap();
        let plain = Classifier::of(&axioms(body))
            .classify(&Limits::default())
            .unwrap();
        assert!(k.describe(&c(class)).is_some(), "{body}");
        let has = |t: &sparkles_owl::classify::Taxonomy| {
            t.nodes().iter().any(|n| n.classes.contains(&c(class)))
        };
        assert!(has(&out.taxonomy), "{body}");
        assert!(has(&plain.taxonomy), "{body}");
    }
    let mut k = Classifier::of(&axioms(
        "ObjectPropertyAssertion(owl:topObjectProperty :a :b)",
    ));
    k.classify(&rooted_limits()).unwrap();
    let names: Vec<String> = k
        .describe(&Class::thing())
        .unwrap()
        .instances
        .iter()
        .map(|i| i.as_str().to_string())
        .collect();
    assert_eq!(names, [format!("{EX}a"), format!("{EX}b")]);
}

#[test]
fn a_description_keeps_its_direct_superclasses_beside_the_classes_of_a_key() {
    // the value c of the key is a Q, below the class that marks the values of b
    let mut k = Classifier::of(&axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         EquivalentClasses(:Q ObjectOneOf(:c))",
    ));
    let out = k.classify(&rooted_limits()).unwrap();
    assert!(out.stats.clauses > 0);
    let d = k.describe(&c("Q")).unwrap();
    assert_eq!(d.superclasses, [Class::thing()]);
    assert_eq!(d.direct_superclasses, [Class::thing()]);
}

#[test]
fn a_stopped_run_of_rewritten_axioms_reports_the_original_axioms() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    // the maximum in the key's class leaves axioms of the rewriting out without counting
    let ax = axioms(
        "HasKey(ObjectMaxCardinality(2 :q) (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)",
    );
    for n in [10, 20, 30, 1000] {
        let polls = Arc::new(AtomicUsize::new(0));
        let p = polls.clone();
        let mut k = Classifier::of(&ax);
        let _ = k.classify(&Limits {
            rooted: true,
            threads: 1,
            cancelled: Some(Arc::new(move || p.fetch_add(1, Ordering::Relaxed) >= n)),
            ..Limits::default()
        });
        let excluded = format!("{:?}", k.excluded());
        assert!(!excluded.contains(INTERNAL), "{n}: {excluded}");
    }
}

#[test]
fn the_statistics_count_the_original_axioms_outside_el() {
    let ax = axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)",
    );
    let out = Classifier::of(&ax).classify(&rooted_limits()).unwrap();
    assert!(out.stats.clauses > 0);
    // the union is the only axiom outside OWL 2 EL
    assert_eq!(out.stats.outside_el.horn, 0);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.taken, 1);
}

#[test]
fn the_statistics_count_a_chain_outside_el_for_its_range() {
    let ax = axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         SubObjectPropertyOf(ObjectPropertyChain(:p :q) :r)
         ObjectPropertyRange(:r :C)",
    );
    let out = Classifier::of(&ax).classify(&rooted_limits()).unwrap();
    assert!(out.stats.clauses > 0);
    assert_eq!(out.stats.outside_el.horn, 1);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.taken, 2);
}

#[test]
fn a_rewritten_run_that_does_not_take_tier_d_answers_as_a_fresh_classifier() {
    // r ∘ U ∘ r ⊑ r is not regular, so the rewritten run does not take the disjunctive
    // rules and starts again on the original axioms
    let base = axioms(
        "ClassAssertion(:A :a)
         ObjectPropertyAssertion(:r :a :b)",
    );
    let more = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\nSubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty :r) :r)\nSubClassOf(:A ObjectAllValuesFrom(owl:topObjectProperty :B))\n)"
    ))
    .unwrap()
    .axioms;
    let all: Vec<&Axiom> = base.iter().chain(&more).collect();
    let mut k = Classifier::of(&base);
    k.classify(&rooted_limits()).unwrap();
    k.add(&more);
    let out = k.classify(&rooted_limits()).unwrap();
    let mut fresh = Classifier::of(all.iter().copied());
    let want = fresh.classify(&rooted_limits()).unwrap();
    assert_eq!(out.taxonomy, want.taxonomy);
    assert_eq!(out.consistency, want.consistency);
    assert_eq!(k.excluded(), fresh.excluded());
    no_internal_names(&k, &out, &[Class::thing(), c("A"), c("B")]);
}

/// The fixtures of roadmap item 1061 in `tests/fixtures/dl1061` each entail `Z ⊑ Ans` by
/// HermiT 1.4.5. In each, Z holds a hypothetical role atom to the individual a with a class
/// P whose universal closes a cycle through the fact `R(b)`, its successors and a back-link
/// `s(a, x)`, so that only the conditional class P(a) under Z's hypothesis reaches the
/// successors of b. They guard the registration of the root's facts in a
/// context: a run that narrows which facts a context receives must still find the
/// subsumption, or must not report the run complete. The `fam-` files come from a family
/// that varies the chain of role facts from b, the depth of ordinary successors below b's
/// existential, where Z's role atom to a sits, and a second role atom from Z to b.
#[test]
fn a_conditional_class_of_a_nominal_reaches_the_successors_of_a_fact() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl1061");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "ofn"))
        .collect();
    files.sort();
    assert_eq!(files.len(), 9, "{files:?}");
    let z = Class::new("http://example.org/n#Z");
    let ans = Class::new("http://example.org/n#Ans");
    let mut checked = 0;
    for path in &files {
        let ax = ofn_read::parse(&std::fs::read_to_string(path).unwrap())
            .unwrap()
            .axioms;
        for (threads, expansion) in [
            (1, Expansion::Cautious),
            (4, Expansion::Cautious),
            (1, Expansion::Eager),
            (4, Expansion::Eager),
        ] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    ..Limits::default()
                })
                .unwrap();
            let name = path.file_name().unwrap().to_string_lossy();
            assert_eq!(out.consistency, Consistency::Consistent, "{name}");
            if out.complete && k.excluded().is_empty() {
                checked += 1;
                assert!(
                    out.taxonomy.is_subclass(&z, &ans),
                    "{name}, {threads} threads, {expansion:?}: a complete run misses Z ⊑ Ans"
                );
            }
        }
    }
    // a run that reports every answer incomplete would pass the loop above without a check
    assert!(checked > 0, "no run of the fixtures was complete");
}

/// Facts over constants that hold only under K, a successor of K that reaches the same
/// constants, and an inference over the successor whose two premises each come from one
/// of those facts (item 1220). The successor holds the facts only as markers, so each
/// premise has a ground literal in its body. With the selection of such literals, the two
/// premises never met, and the run reported itself complete without the subsumption.
const MEET: &str = "SubClassOf(:K ObjectSomeValuesFrom(:r :D))
     SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
     SubClassOf(ObjectSomeValuesFrom(:s2 :B2) :F2)
     SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)";

#[test]
fn two_facts_under_a_class_meet_at_its_successor_over_two_constants() {
    check(
        &format!(
            "{MEET}
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B)))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :B2)))
             SubClassOf(:D ObjectHasValue(:s :v))
             SubClassOf(:D ObjectHasValue(:s2 :w))
             SubClassOf(ObjectIntersectionOf(:F :F2) :G)"
        ),
        &[("K", "Ans")],
        &[("D", "G"), ("K", "G")],
    );
}

#[test]
fn two_facts_under_a_class_meet_at_its_successor_over_one_constant() {
    check(
        &format!(
            "{MEET}
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B :B2)))
             SubClassOf(:D ObjectHasValue(:s :v))
             SubClassOf(:D ObjectHasValue(:s2 :v))
             SubClassOf(ObjectIntersectionOf(:F :F2) :G)"
        ),
        &[("K", "Ans")],
        &[("D", "G")],
    );
}

#[test]
fn two_facts_under_a_class_meet_even_when_the_predecessor_reads_their_atoms() {
    // ∃r.F ⊑ X and ∃r.F2 ⊑ Y make the successor's F and F2 triggers of its predecessor,
    // which still never learns that one successor is in both
    for extra in [
        "SubClassOf(ObjectSomeValuesFrom(:r :F) :X)",
        "SubClassOf(ObjectSomeValuesFrom(:r :F) :X)
         SubClassOf(ObjectSomeValuesFrom(:r :F2) :Y)",
    ] {
        check(
            &format!(
                "{MEET}
                 {extra}
                 SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B :B2)))
                 SubClassOf(:D ObjectHasValue(:s :v))
                 SubClassOf(:D ObjectHasValue(:s2 :v))
                 SubClassOf(ObjectIntersectionOf(:F :F2) :G)"
            ),
            &[("K", "Ans"), ("K", "X")],
            &[("D", "G")],
        );
    }
}

/// The successor's clash `B(v) ∧ B2(w) → ⊥` reaches K, which holds both facts as its own.
/// Join resolves one literal at a time, and the restriction on ground inferences dropped
/// the ground clause with a body that the first Join leaves, although no ancestor of K
/// holds the facts to make the inference in its place (item 1220).
#[test]
fn a_clash_of_two_facts_under_a_class_at_its_successor_makes_the_class_empty() {
    for (succ, clash) in [
        (
            "SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
             SubClassOf(ObjectSomeValuesFrom(:s2 :B2) :F2)",
            "DisjointClasses(:F :F2)",
        ),
        (
            "SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
             SubClassOf(:B2 ObjectAllValuesFrom(ObjectInverseOf(:s2) :F2))",
            "DisjointClasses(:F :F2)",
        ),
    ] {
        let out = rooted(
            &format!(
                "SubClassOf(:K ObjectSomeValuesFrom(:r :D))
                 SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B)))
                 SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :B2)))
                 SubClassOf(:D ObjectHasValue(:s :v))
                 SubClassOf(:D ObjectHasValue(:s2 :w))
                 {succ}
                 {clash}
                 SubClassOf(:E ObjectSomeValuesFrom(:r :D))"
            ),
            false,
            &[("K", "Nothing"), ("D", "Nothing"), ("E", "Nothing")],
        );
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_unsatisfiable(&c("K")), "{succ} {clash}");
        assert!(!out.taxonomy.is_unsatisfiable(&c("D")));
        assert!(!out.taxonomy.is_unsatisfiable(&c("E")));
    }
}
