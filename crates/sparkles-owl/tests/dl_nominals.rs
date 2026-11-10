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
#[ignore = "needs the key reduction of item 805"]
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
#[test]
fn a_rooted_run_that_counts_falls_back_twice_past_a_tiny_bound() {
    let body = "ClassAssertion(ObjectUnionOf(:B :C) :a)
                ClassAssertion(ObjectComplementOf(:C) :a)
                SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                SubClassOf(:A ObjectMaxCardinality(1 :r))
                SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    let ax = axioms(body);
    for (threads, partial) in [(1, false), (4, false), (1, true), (4, true)] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                counting: true,
                partial,
                dl_work: Some(1),
                ..Limits::default()
            })
            .unwrap();
        assert!(
            out.stats.dl_counting_over,
            "{threads} threads, partial {partial}"
        );
        assert!(
            out.stats.dl_rooted_over,
            "{threads} threads, partial {partial}"
        );
        assert!(out.stats.dl_over_budget);
        assert!(out.complete);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(!k.excluded().is_empty());
    }
    // with a bound the run finishes within, it answers in full with counting
    let out = rooted(body, true, &[("A", "D")]);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    assert!(!out.stats.dl_over_budget);
}
