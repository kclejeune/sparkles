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
use sparkles_owl::vocab;

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
        assert!(out.complete_for_taken);
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
            out.complete_for_taken,
            "{threads} threads: a partial answer instead of the fallback"
        );
        // the answer is not complete for the whole ontology: the left-out assertion is
        // in the module of the empty signature, and the nominal keeps the terminology's
        // modules from deciding, so every class is incomplete, but the consistency holds
        assert!(!out.complete);
        let named = out
            .taxonomy
            .classes()
            .filter(|k| !matches!(k.as_str(), vocab::OWL_THING | vocab::OWL_NOTHING))
            .count();
        assert_eq!(out.incomplete.len(), named);
        assert!(out.incomplete.contains(&c("A")));
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
            assert!(out.complete_for_taken, "{run}");
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

/// `∃top.A` and `≥1 top.A` are the same class, and the rewriting gives both `∃U.A`. Their
/// disjointness makes that class empty, and with an element in the domain A is empty,
/// as HermiT answers for this ontology (row 1304). The set of images alone held one
/// member, which the normaliser drops, and the run answered A satisfiable.
#[test]
fn a_disjointness_of_two_members_with_one_image_makes_the_image_empty() {
    let out = rooted(
        "DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A))
         SubClassOf(:K :A)
         ClassAssertion(:B :b)",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")), "A is empty");
    assert!(out.taxonomy.is_unsatisfiable(&c("K")), "K is empty");
    assert!(!out.taxonomy.is_unsatisfiable(&c("B")));
}

/// The control of the case above, with members whose images differ (row 1304). HermiT
/// answers A and K empty.
#[test]
fn a_disjointness_of_two_members_with_distinct_images_keeps_both() {
    let out = rooted(
        "DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :B))
         EquivalentClasses(:A :B)
         SubClassOf(:K :A)
         ClassAssertion(:C :b)",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(out.taxonomy.is_unsatisfiable(&c("K")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("C")));
}

/// `≥0 top.X` holds of every element, so it is `owl:Thing`, and the disjointness says the
/// domain is empty, which no interpretation allows. The ontology is inconsistent by this
/// derivation, since HermiT fails on it and Konclude is not at hand (row 1304). HermiT
/// does answer the variants below with a third member or two such members inconsistent.
#[test]
fn a_disjointness_of_owl_thing_and_at_least_none_is_inconsistent() {
    inconsistent(
        "DisjointClasses(owl:Thing ObjectMinCardinality(0 owl:topObjectProperty :X))
         SubClassOf(:A :X)",
    );
}

/// The control of the case above without a merge: `⊤ ⊑ ¬(≥0 top.X)` is `⊤ ⊑ ⊥`.
#[test]
fn owl_thing_below_the_complement_of_at_least_none_is_inconsistent() {
    inconsistent(
        "SubClassOf(owl:Thing ObjectComplementOf(ObjectMinCardinality(0 owl:topObjectProperty :X)))
         SubClassOf(:A :X)",
    );
}

/// Three members with one image and a fourth with another: the shared image is empty and
/// the other stays satisfiable. `≥0 top.X` and `≥0 top.Y` are both `owl:Thing`, so the
/// third case is inconsistent too. HermiT gives the same answers.
#[test]
fn a_disjointness_of_three_members_with_one_image_makes_it_empty() {
    let out = rooted(
        "DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty owl:Thing)
             ObjectMinCardinality(1 owl:topObjectProperty)
             ObjectMinCardinality(1 owl:topObjectProperty owl:Thing)
             :D)
         SubClassOf(:A :D)",
        false,
        &[],
    );
    // ∃top.⊤ holds of every element, so the domain is empty
    assert_eq!(out.consistency, Consistency::Inconsistent);
    let out = rooted(
        "DisjointClasses(ObjectSomeValuesFrom(owl:topObjectProperty :E)
             ObjectMinCardinality(1 owl:topObjectProperty :E)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(1 owl:topObjectProperty :E))
             :D)
         SubClassOf(:A :D)
         SubClassOf(:K :E)",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("K")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("A")));
    inconsistent(
        "DisjointClasses(ObjectMinCardinality(0 owl:topObjectProperty :X)
             ObjectMinCardinality(0 owl:topObjectProperty :Y))
         SubClassOf(:A :X)",
    );
}

/// The disjointness part of a disjoint union follows the rule of `DisjointClasses`, and
/// the union part keeps the set of images: C is the union of two equal classes that are
/// disjoint, so C and its members are empty. HermiT gives the same answers.
#[test]
fn a_disjoint_union_of_two_members_with_one_image_is_empty() {
    let out = rooted(
        "DisjointUnion(:C ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A))
         SubClassOf(:K :A)
         SubClassOf(:L :C)
         ClassAssertion(:B :b)",
        false,
        &[],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    for name in ["A", "K", "C", "L"] {
        assert!(out.taxonomy.is_unsatisfiable(&c(name)), "{name} is empty");
    }
    assert!(!out.taxonomy.is_unsatisfiable(&c("B")));
    // with a third member of another image, C is that member and stays satisfiable
    let out = rooted(
        "DisjointUnion(:C ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A) :D)
         SubClassOf(:K :A)",
        false,
        &[("C", "D"), ("D", "C")],
    );
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_unsatisfiable(&c("K")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("C")));
    assert!(out.taxonomy.is_subclass(&c("C"), &c("D")));
    assert!(out.taxonomy.is_subclass(&c("D"), &c("C")));
}

/// A rewritten axiom can equal an axiom the ontology states as it is. Here both members of
/// the disjointness rewrite to `≥2 r.⊤`, so the rewriting adds `≥2 r.⊤ ⊑ ⊥`, which the
/// first axiom states too. Without counting the run leaves that axiom out, and the report
/// names both original axioms (row 1304).
#[test]
fn the_report_names_every_original_axiom_of_a_rewritten_one_left_out() {
    let ax = axioms(
        "SubClassOf(ObjectMinCardinality(2 :r owl:Thing) owl:Nothing)
         DisjointClasses(ObjectMinCardinality(2 :r owl:Thing)
             ObjectMinCardinality(2 :r ObjectMinCardinality(0 owl:topObjectProperty :X)))
         ClassAssertion(:B :b)",
    );
    let find = |f: fn(&AxiomKind) -> bool| ax.iter().map(|a| a.kind.clone()).find(f).unwrap();
    let stated = find(|k| matches!(k, AxiomKind::SubClassOf(_, ClassExpression::Class(_))));
    let disjoint = find(|k| matches!(k, AxiomKind::DisjointClasses(_)));
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    let left: Vec<_> = k.excluded().into_iter().map(|e| e.axiom).collect();
    assert!(left.contains(&stated), "{left:#?}");
    assert!(left.contains(&disjoint), "{left:#?}");
    assert_eq!(left.len(), 2, "{left:#?}");
    // the disjunction is the only axiom outside OWL 2 EL that the run takes
    assert_eq!(out.stats.outside_el.taken, 1);
}

/// Two replaced axioms can rewrite to one axiom, here `≥2 r.∃U.A ⊑ ⊥`, and the report
/// names both when the run leaves it out (row 1304).
#[test]
fn the_report_names_both_replaced_axioms_of_one_rewritten_axiom() {
    let ax = axioms(
        "DisjointClasses(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             owl:Nothing)
         ClassAssertion(:B :b)",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    let left: Vec<_> = k.excluded().into_iter().map(|e| e.axiom).collect();
    assert_eq!(left.len(), 2, "{left:#?}");
    assert!(
        left.iter()
            .any(|k| matches!(k, AxiomKind::DisjointClasses(_)))
    );
    assert!(left.iter().any(|k| matches!(k, AxiomKind::SubClassOf(..))));
    assert_eq!(out.stats.outside_el.taken, 1);
}

/// A disjoint union whose rewritten axioms are left out for two rules is reported once.
#[test]
fn a_disjoint_union_left_out_for_two_rules_is_reported_once() {
    let ax = axioms(
        "DisjointUnion(:C
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A))
             :D)
         ClassAssertion(:B :b)",
    );
    let mut k = Classifier::of(&ax);
    k.classify(&rooted_limits()).unwrap();
    let left = k.excluded();
    assert_eq!(left.len(), 1, "{left:#?}");
    assert!(matches!(left[0].axiom, AxiomKind::DisjointUnion(..)));
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
/// premise has a ground literal in its body. With the selection of such literals and
/// without selected partners in Hyper over x, the two premises never met, and the run
/// reported itself complete without the subsumption.
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

/// The premise lookups that leave out selected clauses (item 1220): Hyper at a constant,
/// Pred from a successor over a successor term of a constant, Join, Eq and Factor, and the
/// universals over role atoms at a constant. In each case the filter leaves out a selected
/// premise, and the run still finds K ⊑ Ans, with the selection on or off and with or
/// without selected partners.
#[test]
fn selected_premises_left_out_by_a_lookup_are_made_up_for() {
    for (name, extra) in [
        // a conjunction at a constant over two selected premises that keep an atom over x
        (
            "h41c",
            "SubClassOf(:F ObjectUnionOf(:Z ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :A))))
             SubClassOf(:F2 ObjectUnionOf(:Z ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :A2))))
             SubClassOf(ObjectIntersectionOf(:A :A2) :G)
             SubClassOf(ObjectSomeValuesFrom(:q :G) :W)
             SubClassOf(:Z :W)",
        ),
        // the same conjunction when the premises can go to the predecessor
        (
            "h41n",
            "SubClassOf(:F ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :A)))
             SubClassOf(:F2 ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :A2)))
             SubClassOf(ObjectIntersectionOf(:A :A2) :G)
             SubClassOf(ObjectSomeValuesFrom(:q :G) :W)",
        ),
        // two universals at v over v's successor term, whose conjunction the successor's Pred needs
        (
            "h41f",
            "ClassAssertion(ObjectSomeValuesFrom(:e :E) :v)
             SubClassOf(:F ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:e :A))))
             SubClassOf(:F2 ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:e :A2))))
             SubClassOf(ObjectSomeValuesFrom(:e ObjectIntersectionOf(:A :A2)) :G)
             SubClassOf(ObjectSomeValuesFrom(:s :G) :W)
             SubClassOf(:Z :W)",
        ),
        // a ground head that only a Join with a fact at c discharges, with an atom over x left
        (
            "h42",
            "SubClassOf(:F ObjectUnionOf(:Z ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :C))))
             SubClassOf(:F2 ObjectUnionOf(:Z ObjectSomeValuesFrom(:q ObjectIntersectionOf(ObjectOneOf(:c) :C2))))
             SubClassOf(ObjectOneOf(:c) ObjectUnionOf(ObjectComplementOf(:C) ObjectComplementOf(:C2) :G))
             SubClassOf(ObjectSomeValuesFrom(:q :G) :W)
             SubClassOf(:Z :W)",
        ),
        // a selected x ≈ c that meets a marker A(c)
        (
            "h43e",
            "SubClassOf(:K ObjectSomeValuesFrom(:t2 ObjectIntersectionOf(ObjectOneOf(:c) :A)))
             SubClassOf(:F ObjectUnionOf(:Z ObjectOneOf(:c)))
             SubClassOf(:Z :W)
             SubClassOf(:A :W)",
        ),
        // x ≈ c ∨ x ≈ d under a selected body, with A at c and d only under K
        (
            "h43f",
            "SubClassOf(:K ObjectSomeValuesFrom(:t2 ObjectIntersectionOf(ObjectOneOf(:c) :A)))
             SubClassOf(:K ObjectSomeValuesFrom(:t3 ObjectIntersectionOf(ObjectOneOf(:d) :A)))
             SubClassOf(:F ObjectUnionOf(ObjectOneOf(:c) ObjectOneOf(:d)))
             SubClassOf(:A :W)",
        ),
        // a selected role atom between constants, from x ≈ c in q(x, w)
        (
            "h44",
            "SubClassOf(:D ObjectHasValue(:q :w))
             SubClassOf(:F ObjectOneOf(:c))
             SubClassOf(:K ObjectSomeValuesFrom(:t2 ObjectIntersectionOf(ObjectOneOf(:c) ObjectAllValuesFrom(:q :M))))
             SubClassOf(ObjectSomeValuesFrom(:q :M) :W)",
        ),
        // two selected universals at v over a root role atom from v to w
        (
            "h44r",
            "ObjectPropertyAssertion(:p :v :w)
             SubClassOf(:F ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:p :A))))
             SubClassOf(:F2 ObjectUnionOf(:Z ObjectAllValuesFrom(:s ObjectAllValuesFrom(:p :A2))))
             SubClassOf(ObjectIntersectionOf(:A :A2 ObjectOneOf(:w)) :G0)
             SubClassOf(:D ObjectHasValue(:q :w))
             SubClassOf(ObjectSomeValuesFrom(:q :G0) :W)
             SubClassOf(:Z :W)",
        ),
    ] {
        let out = rooted(
            &format!(
                "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B :B2)))
                 SubClassOf(:K ObjectSomeValuesFrom(:r :D))
                 SubClassOf(:D ObjectHasValue(:s :v))
                 SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
                 SubClassOf(ObjectSomeValuesFrom(:s :B2) :F2)
                 SubClassOf(ObjectSomeValuesFrom(:r :W) :Ans)
                 SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
                 {extra}"
            ),
            false,
            &[("K", "Ans")],
        );
        assert_eq!(out.consistency, Consistency::Consistent, "{name}");
        assert!(out.taxonomy.is_subclass(&c("K"), &c("Ans")), "{name}: K ⊑ Ans is missing");
        assert!(!out.taxonomy.is_unsatisfiable(&c("K")), "{name}");
    }
}

/// A selected premise over a successor term of a constant (item 1220). The equality
/// `f(x) ≈ w` of E's existential is local, so Eq rewrites it with the marker of B(v) in the
/// body and gives a head atom over w's successor term, which Pred from that successor needs.
#[test]
fn a_selected_premise_over_a_successor_term_of_a_constant_reaches_pred() {
    check(
        "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:v) :B)))
         SubClassOf(:K ObjectSomeValuesFrom(:p :D))
         SubClassOf(:D ObjectHasValue(:s :v))
         SubClassOf(ObjectSomeValuesFrom(:s :B) :F)
         SubClassOf(:D ObjectSomeValuesFrom(:r :E))
         SubClassOf(:E ObjectUnionOf(ObjectOneOf(:u) ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) ObjectSomeValuesFrom(:e :A)))))
         SubClassOf(:F ObjectAllValuesFrom(:r :C))
         SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
         SubClassOf(:C ObjectAllValuesFrom(ObjectInverseOf(:r) :W))
         SubClassOf(:D ObjectHasValue(:z :w))
         SubClassOf(ObjectSomeValuesFrom(:z ObjectSomeValuesFrom(:e :A)) :W)
         SubClassOf(ObjectSomeValuesFrom(:p :W) :Ans)",
        &[("K", "Ans")],
        &[("D", "Ans")],
    );
}

/// K reaches the successor `{v} ⊓ B0` by t4 at once, and by s7 only after its successor D2
/// read K's fact B1(v) and sent `∃r1.H` back, so the successor's `x ≈ v` came before the
/// second restriction did. With counting the restriction has a successor term, which takes
/// the constant only from that clause. A run that counts either finds K ⊑ Ans or does not
/// answer in full, which it marks by classifying again without counting (item 1229).
#[test]
fn a_late_restriction_into_a_successor_that_already_answered_reads_its_clauses() {
    let body = "SubClassOf(:K ObjectSomeValuesFrom(:t4 ObjectIntersectionOf(ObjectOneOf(:v) :B0)))
         SubClassOf(:K ObjectSomeValuesFrom(:t10 ObjectIntersectionOf(ObjectOneOf(:v) :B1)))
         SubClassOf(:K ObjectSomeValuesFrom(:r1 :D2))
         SubClassOf(:D2 ObjectHasValue(:s9 :v))
         SubClassOf(ObjectSomeValuesFrom(:s9 :B1) :G0)
         SubClassOf(:G0 :H)
         SubClassOf(ObjectSomeValuesFrom(:r1 :H) :A0)
         SubClassOf(:A0 :A)
         SubClassOf(:A ObjectSomeValuesFrom(:s7 ObjectIntersectionOf(ObjectOneOf(:v) :B0)))
         SubClassOf(ObjectSomeValuesFrom(:s7 :B1) :F0)
         SubClassOf(:F0 :Ans)
         SubClassOf(:Ctl ObjectSomeValuesFrom(:s7 ObjectIntersectionOf(ObjectOneOf(:v) :B0)))
         FunctionalObjectProperty(:zz)";
    let ax = axioms(body);
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
                counting: true,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert_eq!(out.consistency, Consistency::Consistent);
        let full = out.complete && k.excluded().is_empty();
        if full {
            assert!(!out.stats.dl_counting_over);
            assert!(
                out.taxonomy.is_subclass(&c("K"), &c("Ans")),
                "K ⊑ Ans is missing"
            );
        } else {
            // the run without counting leaves the functional property out and answers
            assert!(out.stats.dl_counting_over, "{:#?}", k.excluded());
            assert!(
                out.taxonomy.is_subclass(&c("K"), &c("Ans")),
                "K ⊑ Ans is missing"
            );
        }
        for (a, b) in [("Ctl", "Ans"), ("D2", "G0"), ("K", "B0")] {
            assert!(
                !out.taxonomy.is_subclass(&c(a), &c(b)),
                "{a} ⊑ {b} is not entailed"
            );
        }
    }
}

/// A successor S that is the context of a named class, and so a query context, holds a
/// selected disjunction of equalities of its element with constants under a marker of a
/// fact of K (item 1220). The query-form Eq and Factor leave out such a premise, and the
/// run must still find what a run without the selection finds, which is HermiT's answer.
#[test]
fn a_selected_equality_of_a_successor_with_constants_gives_the_answer_of_its_query() {
    for (body, yes) in [
        // s5e
        (
            "SubClassOf(:K ObjectSomeValuesFrom(:r :S))
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
             SameIndividual(:u :v)",
            &[("K", "Ans"), ("Z", "Cq")][..],
        ),
        // s5g
        (
            "SubClassOf(:K ObjectSomeValuesFrom(:r :S))
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
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectOneOf(:v)))",
            &[("K", "Ans"), ("S", "Y"), ("Z", "Cq")][..],
        ),
        // s5h
        (
            "SubClassOf(:K ObjectSomeValuesFrom(:r :S))
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
             SameIndividual(:u :v)",
            &[("K", "Ans"), ("S", "Y"), ("Z", "Cq")][..],
        ),
    ] {
        check(body, yes, &[("S", "Ans"), ("K", "Cq")]);
    }
}

/// The same successor, where S's element answers K only through S's own successor, so
/// the route through K's equalities with the constants is cut, with two or three
/// constants, a longer chain, a disjunctive marker and a branch that only an inequality
/// of the constants closes, for Factor (item 1220).
#[test]
fn a_selected_equality_of_a_successor_with_constants_reaches_its_own_successor() {
    for body in [
        // q1
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Cx :v)",
        // q2
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             SameIndividual(:u :v)",
        // q3
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) ObjectUnionOf(:M :N))))
             SubClassOf(:N ObjectAllValuesFrom(ObjectInverseOf(:t) :Ans))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Cx :v)",
        // q4
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :R))
             SubClassOf(:R ObjectSomeValuesFrom(:r2 :S))
             SubClassOf(ObjectSomeValuesFrom(:r2 :G) :G)
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Cx :v)",
        // q5
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v :w))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Cx :v)
             ClassAssertion(:Cx :w)",
        // q6
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Bv :v)
             DisjointClasses(:Bv :S)",
        // q7
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u :v))
             ClassAssertion(:Cx :u)
             ClassAssertion(:Bv :v)
             SubClassOf(:S ObjectComplementOf(:Bv))
             DifferentIndividuals(:u :v)",
        // q8
        "SubClassOf(:S ObjectSomeValuesFrom(ObjectInverseOf(:s) ObjectOneOf(:u)))
             SubClassOf(:M ObjectAllValuesFrom(:s :Z))
             SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))
             SubClassOf(ObjectSomeValuesFrom(:r :G) :Ans)
             SubClassOf(:S ObjectSomeValuesFrom(:q :T))
             SubClassOf(:Cx ObjectAllValuesFrom(:q :Fy))
             SubClassOf(:Fy ObjectAllValuesFrom(ObjectInverseOf(:q) :G))
             SubClassOf(:K ObjectSomeValuesFrom(:r :S))
             SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:u) :M)))
             SubClassOf(:Z ObjectOneOf(:u))
             ClassAssertion(:Cx :u)",
    ] {
        check(body, &[("K", "Ans")], &[("S", "Ans")]);
    }
}

/// K's r-successor is a C and a D, and K derives its s-restriction into C only after a case
/// analysis. Under the eager strategy both restrictions reach the one successor whose core
/// is C and D, which sent its clauses over the predecessor when the r-edge was made, and
/// the s-edge comes later. The functional p makes the two successors one, so K ⊑ ∃s.D ⊑ Ans.
/// A rooted run that counts either finds K ⊑ Ans or classifies again without counting,
/// which leaves the functional property out and reports it (item 1229).
#[test]
fn a_late_restriction_that_shares_an_eager_successor_reads_its_clauses() {
    // the repair answers in full with counting, unless `SPARKLES_COUNTING_LATE_UNIT` is 0
    // or 1
    let repair = !matches!(
        std::env::var("SPARKLES_COUNTING_LATE_UNIT").as_deref(),
        Ok("0" | "1")
    );
    let ax = axioms(
        "SubClassOf(:K ObjectSomeValuesFrom(:r :C))
         SubClassOf(:K ObjectAllValuesFrom(:r :D))
         SubClassOf(:K ObjectSomeValuesFrom(:r1 ObjectUnionOf(:Y1 :Y2)))
         SubClassOf(:Y1 :Y)
         SubClassOf(:Y2 :Y)
         SubClassOf(ObjectSomeValuesFrom(:r1 :Y) :A)
         SubClassOf(:A :L0)
         SubClassOf(:L0 :L1)
         SubClassOf(:L1 ObjectSomeValuesFrom(:s :C))
         SubClassOf(:C ObjectIntersectionOf(:B :B2))
         SubObjectPropertyOf(:r :p)
         SubObjectPropertyOf(:s :p)
         FunctionalObjectProperty(:p)
         SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
         SubClassOf(:E ObjectSomeValuesFrom(:s :C))
         ClassAssertion(:K :a)",
    );
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
                counting: true,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert_eq!(out.consistency, Consistency::Consistent);
        let full = out.complete && k.excluded().is_empty();
        assert!(full || !repair, "the repair answers in full");
        if full {
            assert!(!out.stats.dl_counting_over && !out.stats.dl_over_budget);
            assert!(
                out.taxonomy.is_subclass(&c("K"), &c("Ans")),
                "K ⊑ Ans is missing ({threads}, {expansion:?})"
            );
        } else {
            // the run without counting leaves the functional property out, so K ⊑ Ans with it
            assert!(out.stats.dl_counting_over && out.stats.dl_over_budget);
            let left: Vec<_> = k.excluded().into_iter().map(|e| e.axiom).collect();
            assert!(
                matches!(left[..], [AxiomKind::FunctionalObjectProperty(_)]),
                "only the functional axiom is left out: {left:#?}"
            );
        }
        assert!(
            !out.taxonomy.is_subclass(&c("E"), &c("Ans")),
            "E ⊑ Ans is not entailed"
        );
    }
}

/// The same late s-edge into the eager successor whose core is C and D, in a rooted run
/// without counting, with K ⊑ ∀s.D in place of the functional property. The rooted
/// context registers with every successor over all its clauses, so the successor's clause
/// for ∃s.D ⊑ Ans arrived with the r-edge and the s-edge found it taken. The rooted run
/// finds K ⊑ Ans, and with `SPARKLES_COUNTING_LATE_UNIT=1` it classifies again without
/// the rooted rules (item 1229).
#[test]
fn a_late_edge_of_a_rooted_run_without_counting_reads_its_clauses() {
    let mode = std::env::var("SPARKLES_COUNTING_LATE_UNIT");
    let contained = mode.as_deref() == Ok("1");
    let ax = axioms(
        "SubClassOf(:K ObjectSomeValuesFrom(:r :C))
         SubClassOf(:K ObjectAllValuesFrom(:r :D))
         SubClassOf(:K ObjectAllValuesFrom(:s :D))
         SubClassOf(:K ObjectSomeValuesFrom(:r1 ObjectUnionOf(:Y1 :Y2)))
         SubClassOf(:Y1 :Y)
         SubClassOf(:Y2 :Y)
         SubClassOf(ObjectSomeValuesFrom(:r1 :Y) :A)
         SubClassOf(:A :L0)
         SubClassOf(:L0 :L1)
         SubClassOf(:L1 ObjectSomeValuesFrom(:s :C))
         SubClassOf(:C ObjectIntersectionOf(:B :B2))
         SubObjectPropertyOf(:r :p)
         SubObjectPropertyOf(:s :p)
         SubClassOf(ObjectSomeValuesFrom(:s :D) :Ans)
         SubClassOf(:E ObjectSomeValuesFrom(:s :C))
         ClassAssertion(:K :a)",
    );
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
        assert_eq!(out.consistency, Consistency::Consistent);
        if out.stats.dl_rooted_over {
            assert!(
                contained,
                "only the containment falls back ({threads}, {expansion:?})"
            );
            continue;
        }
        assert!(
            out.complete && k.excluded().is_empty(),
            "the rooted run answers in full ({threads}, {expansion:?})"
        );
        assert!(
            out.taxonomy.is_subclass(&c("K"), &c("Ans")),
            "K ⊑ Ans is missing ({threads}, {expansion:?})"
        );
        assert!(
            !out.taxonomy.is_subclass(&c("E"), &c("Ans")),
            "E ⊑ Ans is not entailed"
        );
    }
}

/// The axioms of row 1231's regression: a ground fact about a constant that holds only
/// under the query class, G(w) under K, whose consequence passes through an element that
/// exists without K and is linked to w two edges below an individual, where the link is no
/// literal of the root.
const AV2: &str = "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))
     SubClassOf(:Q ObjectUnionOf(:Q1 :Q2))";

/// The configurations of the regression's runs.
const CONFIGS: [(usize, Expansion); 4] = [
    (1, Expansion::Cautious),
    (4, Expansion::Cautious),
    (1, Expansion::Eager),
    (4, Expansion::Eager),
];

/// The claim predicate of the lane harnesses: a complete rooted answer of Tier D that took
/// every axiom and did not start again without the rooted rules.
fn rooted_claim(k: &Classifier, out: &Classification) -> bool {
    out.complete
        && out.incomplete.is_empty()
        && k.excluded().is_empty()
        && out.stats.tier == Tier::Disjunctive
        && !out.stats.dl_rooted_over
        && !out.stats.dl_rooted_declined
        && !out.stats.dl_over_budget
}

/// Row 1231: the rooted run of [`AV2`] either finds `K ⊑ Ans` or makes no claim of
/// completeness.
#[test]
fn a_conditional_fact_about_a_constant_reaches_an_element_outside_the_query_successors() {
    let ax = axioms(AV2);
    for (threads, expansion) in CONFIGS {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(
            !rooted_claim(&k, &out) || out.taxonomy.is_subclass(&c("K"), &c("Ans")),
            "{threads} threads {expansion:?}: a complete rooted run misses K ⊑ Ans"
        );
    }
}

/// The control of row 1231's decline: with it off, the rooted run of [`AV2`] claims an
/// answer that misses `K ⊑ Ans`, so the regression above fails without the decline.
#[test]
fn without_the_decline_a_rooted_run_claims_an_answer_that_misses_an_entailment() {
    let ax = axioms(AV2);
    for (threads, expansion) in CONFIGS {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                rooted: true,
                rooted_decline: false,
                ..Limits::default()
            })
            .unwrap();
        assert!(
            rooted_claim(&k, &out) && !out.taxonomy.is_subclass(&c("K"), &c("Ans")),
            "{threads} threads {expansion:?}: the control no longer misses K ⊑ Ans"
        );
    }
}

/// Row 1231's marker repair fixtures (PLAN v3.1's rule M): each holds a ground fact about
/// a constant that holds only under the query class K, whose consequence passes through an
/// element outside K's successors, so that a rooted run without the repair misses it.
const REPAIRS: [(&str, &str); 13] = [
    // the regression itself
    ("av2", AV2),
    // a return whose body has two ground atoms, both held by the query context
    ("multi", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectSomeValuesFrom(:t3 ObjectIntersectionOf(ObjectOneOf(:w2) :H)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(:E ObjectHasValue(:r3 :w2))
     SubClassOf(ObjectIntersectionOf(ObjectSomeValuesFrom(:r2 :G) ObjectSomeValuesFrom(:r3 :H)) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // a holder clause whose residual head is an atom over x, a disjunction
    ("resid", "SubClassOf(:K ObjectUnionOf(:B ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G))))
     SubClassOf(:B :Ans)
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // a holder clause whose residual head is a ground role atom, a disjunction
    ("resid2", "SubClassOf(:K ObjectUnionOf(ObjectHasValue(:t4 :v2) ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G))))
     SubClassOf(ObjectHasValue(:t4 :v2) :Ans)
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the linked element links to a constant equal to the one the condition is about
    ("cong", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SameIndividual(:w :w2)
     SubClassOf(:E ObjectHasValue(:r2 :w2))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the link to the constant comes from a deeper derivation of the linked context
    ("late", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E :E1)
     SubClassOf(:E1 ObjectSomeValuesFrom(:r5 :E2))
     SubClassOf(:E2 ObjectHasValue(:r6 :u))
     SubClassOf(ObjectSomeValuesFrom(:r6 ObjectOneOf(:u)) :E3)
     SubClassOf(ObjectSomeValuesFrom(:r5 :E3) :E4)
     SubClassOf(:E4 ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // a returned consequence is itself a condition that a second linked element needs
    ("chain", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C3)
     SubClassOf(:C3 ObjectHasValue(:s3 :w3))
     SubClassOf(:C3 ObjectAllValuesFrom(:s3 :G3))
     ClassAssertion(ObjectSomeValuesFrom(:p2 :D2) :u2)
     SubClassOf(:D2 ObjectSomeValuesFrom(:r :E5))
     SubClassOf(:E5 ObjectHasValue(:r7 :w3))
     SubClassOf(ObjectSomeValuesFrom(:r7 :G3) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the condition is a ground role atom rather than a class atom
    ("role", "SubClassOf(:K ObjectHasValue(:t :w))
     SubClassOf(:K ObjectSomeValuesFrom(:t9 ObjectIntersectionOf(ObjectOneOf(:w) ObjectHasValue(:m :w5))))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 ObjectHasValue(:m :w5)) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the condition is also a fact of the root, which the registered contexts take as one
    ("pos", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))
     ClassAssertion(:G :w)"),
    // the linked element denies the condition, and the denial returns as a contradiction
    ("denial2", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) owl:Nothing)"),
    // the linked element needs a consequence at the constant that the holder derives and sends in turn
    ("sel", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) :G)))
     SubClassOf(:G :C5)
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :C5) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the condition is an equality of two constants, and the root fact about one reaches the other
    ("eq", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) ObjectOneOf(:w2))))
     ClassAssertion(:G :w)
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w2))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
    // the condition is an equality and a fact under it, about two names of one constant
    ("eq2", "SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w) ObjectOneOf(:w2))))
     SubClassOf(:K ObjectSomeValuesFrom(:t ObjectIntersectionOf(ObjectOneOf(:w2) :G)))
     SubClassOf(:K ObjectHasValue(:q :v))
     SubClassOf(ObjectSomeValuesFrom(:q :Y) :Ans)
     ClassAssertion(ObjectSomeValuesFrom(:p :D) :u)
     SubClassOf(:D ObjectSomeValuesFrom(:r :E))
     SubClassOf(:E ObjectHasValue(:r2 :w))
     SubClassOf(ObjectSomeValuesFrom(:r2 :G) :C2)
     SubClassOf(:C2 ObjectHasValue(:s :v))
     SubClassOf(:C2 ObjectAllValuesFrom(:s :Y))"),
];

/// What a rooted run of a repair fixture must find: `K ⊑ Ans`, or for a denial `K ⊑ ⊥`.
fn repaired(name: &str, out: &Classification) -> bool {
    if name.starts_with("denial") {
        out.taxonomy.is_subclass(&c("K"), &c("Nothing"))
    } else {
        out.taxonomy.is_subclass(&c("K"), &c("Ans"))
    }
}

/// Row 1231's marker repair finds every fixture's entailment with a complete rooted answer,
/// on every configuration, with the decline off so that the answer is the rooted run's.
#[test]
fn the_marker_repair_delivers_conditional_facts_outside_the_query_successors() {
    for (name, body) in REPAIRS {
        let ax = axioms(body);
        // dl_steps off lets a bounded run saturate on several threads
        for ((threads, expansion), dl_steps) in
            CONFIGS.into_iter().flat_map(|c| [(c, true), (c, false)])
        {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    rooted_decline: false,
                    rooted_marker_repair: true,
                    dl_steps,
                    ..Limits::default()
                })
                .unwrap();
            let at = format!("{name}, {threads} threads {expansion:?}, steps {dl_steps}");
            assert!(rooted_claim(&k, &out), "{at}: no claim");
            assert!(
                repaired(name, &out),
                "{at}: the repair misses the entailment"
            );
            // nothing else changes: the linked element's class keeps out of the answer
            assert!(
                !out.taxonomy.is_subclass(&c("E"), &c("Ans")),
                "{at}: E ⊑ Ans"
            );
            assert_eq!(
                out.taxonomy.is_subclass(&c("K"), &c("Nothing")),
                name.starts_with("denial"),
                "{at}: K's satisfiability"
            );
        }
    }
}

/// The control of the repair fixtures: without the repair and the decline, the rooted run
/// claims an answer that misses each fixture's entailment, but for `pos`, whose condition
/// is a fact of the root.
#[test]
fn without_the_marker_repair_a_rooted_run_misses_each_fixture() {
    for (name, body) in REPAIRS {
        let ax = axioms(body);
        for (threads, expansion) in CONFIGS {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    rooted_decline: false,
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                rooted_claim(&k, &out),
                "{name}, {threads} threads {expansion:?}: no claim"
            );
            assert_eq!(
                repaired(name, &out),
                name == "pos",
                "{name}, {threads} threads {expansion:?}: the control changed"
            );
        }
    }
}

/// Row 1231's decline withdraws the rooted answer on every path a rooted run takes: a
/// classification, its repetition on the same classifier, a run that counts, a saturation
/// and a run of axioms that item 805 rewrites. Each reports the decline and its fallback,
/// leaves axioms out, and answers as the run without the rooted rules does.
#[test]
fn the_decline_withdraws_the_rooted_answer_on_every_path() {
    let keyed = format!("{AV2}\n     HasKey(:K (:q) ())");
    let counted = format!("{AV2}\n     SubClassOf(:K ObjectMaxCardinality(1 :q))");
    for (name, body, counting) in [
        ("plain", AV2.to_string(), false),
        ("counting", counted, true),
        ("keys", keyed, false),
    ] {
        let ax = axioms(&body);
        for (threads, expansion) in CONFIGS {
            let limits = Limits {
                threads,
                expansion,
                counting,
                rooted: true,
                ..Limits::default()
            };
            let at = format!("{name}, {threads} threads {expansion:?}");
            let mut k = Classifier::of(&ax);
            for round in 0..2 {
                let out = k.classify(&limits).unwrap();
                assert!(out.stats.dl_rooted_declined, "{at}, round {round}");
                assert!(out.stats.dl_over_budget, "{at}, round {round}");
                assert!(!out.stats.dl_rooted_over, "{at}, round {round}");
                assert!(!k.excluded().is_empty(), "{at}, round {round}");
                assert!(!rooted_claim(&k, &out), "{at}, round {round}");
                assert!(out.consistent, "{at}, round {round}");
                assert!(
                    out.taxonomy.is_subclass(&c("K"), &c("Ans")),
                    "{at}, round {round}: the fallback misses K ⊑ Ans"
                );
            }
            let mut k = Classifier::of(&ax);
            let sat = k.saturate(&limits).unwrap();
            assert!(
                sat.consistent && sat.stats.dl_rooted_declined,
                "{at}: saturate"
            );
            assert!(sat.stats.dl_over_budget, "{at}: saturate");
        }
    }
}

/// Row 1231's marker repair finds `K ⊑ Ans` on every path a rooted run takes: a
/// classification and its repetition on the same classifier, a run that counts, a run of
/// axioms that item 805 rewrites, a run after an edit, and runs on one classifier whose
/// repair setting differs from that of the run before. No rooted run keeps its contexts
/// for the next, so a run never meets contexts whose holders sent no notice; the test pins
/// that. A saturation and a classification of `denial2` with an element of `K` that the
/// ABox asserts find no model, as they do without the repair, since the facts of a context
/// that the root reaches hold without a condition.
#[test]
fn the_marker_repair_delivers_on_every_path() {
    let keyed = format!("{AV2}\n     HasKey(:K (:q) ())");
    let counted = format!("{AV2}\n     SubClassOf(:K ObjectMaxCardinality(1 :q))");
    let edit = axioms("SubClassOf(:F ObjectSomeValuesFrom(:t :G))");
    let limits = |threads, expansion, counting, repair| Limits {
        threads,
        expansion,
        counting,
        rooted: true,
        rooted_decline: false,
        rooted_marker_repair: repair,
        ..Limits::default()
    };
    for (name, body, counting) in [
        ("plain", AV2.to_string(), false),
        ("counting", counted, true),
        ("keys", keyed, false),
    ] {
        let ax = axioms(&body);
        for (threads, expansion) in CONFIGS {
            let on = limits(threads, expansion, counting, true);
            let off = limits(threads, expansion, counting, false);
            let at = format!("{name}, {threads} threads {expansion:?}");
            let found = |k: &Classifier, out: &Classification, what: &str| {
                assert!(rooted_claim(k, out), "{at}, {what}: no claim");
                out.taxonomy.is_subclass(&c("K"), &c("Ans"))
            };
            let mut k = Classifier::of(&ax);
            for round in 0..2 {
                let out = k.classify(&on).unwrap();
                assert!(
                    found(&k, &out, &format!("round {round}")),
                    "{at}, round {round}"
                );
            }
            // an edit that leaves the answer as it is
            k.add(&edit);
            let out = k.classify(&on).unwrap();
            assert!(found(&k, &out, "after an edit"), "{at}, after an edit");
            assert!(
                !out.stats.incremental,
                "{at}: a rooted run kept its contexts"
            );
            k.remove(&edit);
            // the repair setting changes between the runs of one classifier, both ways
            let mut k = Classifier::of(&ax);
            let out = k.classify(&off).unwrap();
            assert!(!found(&k, &out, "off first"), "{at}: the control changed");
            let out = k.classify(&on).unwrap();
            assert!(found(&k, &out, "on after off"), "{at}, on after off");
            let out = k.classify(&off).unwrap();
            assert!(!found(&k, &out, "off after on"), "{at}, off after on");
            k.add(&edit);
            let out = k.classify(&on).unwrap();
            assert!(
                found(&k, &out, "on after off and an edit"),
                "{at}, on after an edit"
            );
            assert!(
                !out.stats.incremental,
                "{at}: a rooted run kept its contexts"
            );
        }
    }
    // the denial of the condition of an element of K leaves the ontology without a model
    let denied = axioms(&format!(
        "{}\n     ClassAssertion(ObjectSomeValuesFrom(:p0 :K) :a)",
        REPAIRS.iter().find(|(n, _)| *n == "denial2").unwrap().1
    ));
    for (threads, expansion) in CONFIGS {
        let at = format!("{threads} threads {expansion:?}");
        let mut k = Classifier::of(&denied);
        let sat = k
            .saturate(&limits(threads, expansion, false, true))
            .unwrap();
        assert!(!sat.consistent, "{at}: the saturation finds a model");
        let mut k = Classifier::of(&denied);
        let out = k
            .classify(&limits(threads, expansion, false, true))
            .unwrap();
        assert!(!out.consistent, "{at}: the classification finds a model");
    }
}

/// A rooted run with counting that passes its bound starts again without counting, and
/// that run is rooted too: when it withdraws its answer, the run starts again without the
/// rooted rules and reports both fallbacks. The bound lies between the work of the run
/// with counting and that of the run without it.
#[test]
fn a_rooted_run_without_counting_after_the_bound_withdraws_its_answer_too() {
    let ax = axioms(&format!(
        "{AV2}\n     SubClassOf(:K ObjectMaxCardinality(1 :q))"
    ));
    let limits = Limits {
        threads: 1,
        counting: true,
        rooted: true,
        dl_work: Some(180),
        ..Limits::default()
    };
    let mut k = Classifier::of(&ax);
    let out = k.classify(&limits).unwrap();
    assert!(out.stats.dl_counting_over);
    assert!(out.stats.dl_rooted_declined);
    assert!(out.stats.dl_over_budget && !out.stats.dl_rooted_over);
    assert!(!rooted_claim(&k, &out));
    // the control: without the decline the run without counting answers rooted
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            rooted_decline: false,
            ..limits
        })
        .unwrap();
    assert!(out.stats.dl_counting_over && !out.stats.dl_rooted_declined);
    assert!(out.stats.tier == Tier::Disjunctive && !out.stats.dl_rooted_over);
}

/// The axioms of the rooted-key cases below. Their only individual is in an axiom outside
/// OWL 2 EL, so the EL part has none and Tier D takes the run with the option as it does
/// without it, and the key is left for item 805's rewriting.
const KEYED: &str = "SubClassOf(:A ObjectUnionOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(:a))))
     SubClassOf(:C ObjectUnionOf(:A :B))
     SubClassOf(:B ObjectSomeValuesFrom(:r :A))";

/// Classifies with and without the rooted run on one and four threads under both
/// strategies, checks that each pair agrees on the taxonomy, the consistency, the
/// completeness and the axioms left out, and returns those left out. With `tier_d`, it also
/// checks that Tier D took clauses. A key that item 805's rewriting declines keeps the run
/// from being rooted, so it answers as the run without the option does (roadmap item 1265).
fn keys_left(body: &str, tier_d: bool) -> Vec<String> {
    let ax = axioms(body);
    let mut left = Vec::new();
    for (threads, expansion) in [
        (1, Expansion::Cautious),
        (4, Expansion::Cautious),
        (1, Expansion::Eager),
        (4, Expansion::Eager),
    ] {
        let limits = Limits {
            threads,
            expansion,
            ..Limits::default()
        };
        let mut p = Classifier::of(&ax);
        let plain = p.classify(&limits).unwrap();
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                rooted: true,
                ..limits
            })
            .unwrap();
        if tier_d {
            assert_eq!(out.stats.tier, Tier::Disjunctive);
            assert!(out.stats.clauses > 0, "Tier D took no clause");
        }
        assert_eq!(out.taxonomy, plain.taxonomy);
        assert_eq!(out.consistency, plain.consistency);
        assert_eq!(out.complete, plain.complete);
        assert_eq!(out.incomplete, plain.incomplete);
        assert_eq!(k.excluded(), p.excluded());
        left = k
            .excluded()
            .iter()
            .map(|e| format!("{:?}", e.axiom))
            .collect();
    }
    left
}

/// Whether one of the axioms left out names an individual in an enumeration.
fn left_with_individual(left: &[String]) -> bool {
    left.iter().any(|l| l.contains("ObjectOneOf"))
}

#[test]
fn a_key_kept_by_a_name_with_the_prefix_of_the_reductions_keeps_the_run_from_being_rooted() {
    let left = keys_left(
        &format!("{KEYED}\nHasKey(:B (:r) ())\nSubClassOf(<{INTERNAL}x> :A)"),
        true,
    );
    assert!(left_with_individual(&left), "{left:?}");
}

#[test]
fn a_key_beside_an_assertion_outside_owl_2_el_keeps_the_run_from_being_rooted() {
    let left = keys_left(
        &format!(
            "ClassAssertion(ObjectUnionOf(:B ObjectComplementOf(:A)) :a)
             SubClassOf(:A ObjectUnionOf(:B :C))
             SubClassOf(:C ObjectUnionOf(:A :B))
             HasKey(:B (:r) ())
             SubClassOf(:B ObjectSomeValuesFrom(:r :A))
             SubClassOf(<{INTERNAL}x> :A)"
        ),
        true,
    );
    assert!(
        left.iter().any(|l| l.contains("ClassAssertion")),
        "{left:?}"
    );
}

#[test]
fn a_key_past_the_budget_of_its_reduction_keeps_the_run_from_being_rooted() {
    // seventy-one individuals put the key's reduction past its bound, and none of them is
    // in the EL part
    let mut body = format!("{KEYED}\nHasKey(:B (:r) ())\n");
    for i in 0..70 {
        body.push_str(&format!(
            "ClassAssertion(ObjectAllValuesFrom(:s :N) :i{i})\n"
        ));
    }
    let left = keys_left(&body, true);
    assert!(left_with_individual(&left), "{left:?}");
}

#[test]
fn a_key_beside_a_construct_without_a_reduction_keeps_the_run_from_being_rooted() {
    let left = keys_left(
        &format!(
            "{KEYED}\nHasKey(:B (:r) ())\nSubClassOf(:C ObjectMaxCardinality(3 owl:topObjectProperty))"
        ),
        true,
    );
    assert!(left_with_individual(&left), "{left:?}");
}

#[test]
fn a_key_beside_data_outside_owl_2_el_keeps_the_run_from_being_rooted() {
    let left = keys_left(
        &format!(
            "{KEYED}\nHasKey(:B (:r) ())
             SubClassOf(:C ObjectUnionOf(:A DataSomeValuesFrom(:d <http://www.w3.org/2001/XMLSchema#integer>)))"
        ),
        true,
    );
    assert!(left_with_individual(&left), "{left:?}");
}

#[test]
fn a_key_beside_the_bottom_property_outside_owl_2_el_keeps_the_run_from_being_rooted() {
    let left = keys_left(
        &format!(
            "{KEYED}\nHasKey(:B (:r) ())
             SubClassOf(:C ObjectUnionOf(:A ObjectSomeValuesFrom(owl:bottomObjectProperty :B)))"
        ),
        true,
    );
    assert!(left_with_individual(&left), "{left:?}");
}

#[test]
fn a_key_with_a_data_property_answers_as_the_run_without_the_option() {
    // the key's data property keeps Tier D from the run whether or not it is rooted
    keys_left(&format!("{KEYED}\nHasKey(:B (:r) (:d))"), false);
}

#[test]
fn a_key_outside_owl_2_el_leaves_the_run_rooted() {
    // the key is left out, so the EL part has none and the rooted run takes the
    // enumeration, which the run without the option leaves out
    let ax = axioms(&format!(
        "{KEYED}\nHasKey(ObjectUnionOf(:B :C) (:r) ())\nSubClassOf(<{INTERNAL}x> :A)"
    ));
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    let left: Vec<String> = k
        .excluded()
        .iter()
        .map(|e| format!("{:?}", e.axiom))
        .collect();
    assert!(!left_with_individual(&left), "{left:?}");
    assert!(left.iter().any(|l| l.contains("HasKey")), "{left:?}");
}

#[test]
fn a_run_whose_only_individual_is_in_a_key_outside_owl_2_el_answers_as_without_the_option() {
    // the retained key keeps the run from being rooted, so it takes the minimum it used to
    // leave out with an empty root and derives X ⊑ Y, as the run without the option does
    let body = format!(
        "HasKey(:B (:r) ())
         HasKey(ObjectUnionOf(:B ObjectOneOf(:a)) (:s) ())
         SubClassOf(<{INTERNAL}x> :A)
         SubClassOf(:X ObjectMinCardinality(2 :s :N))
         SubClassOf(ObjectSomeValuesFrom(:s :N) :Y)"
    );
    let left = keys_left(&body, true);
    assert!(left_with_individual(&left), "{left:?}");
    assert!(
        !left.iter().any(|l| l.contains("MinCardinality")),
        "{left:?}"
    );
    for threads in [1, 4] {
        let ax = axioms(&body);
        let out = Classifier::of(&ax)
            .classify(&Limits {
                threads,
                ..rooted_limits()
            })
            .unwrap();
        assert!(
            out.taxonomy.is_subclass(&c("X"), &c("Y")),
            "{threads} threads"
        );
        assert!(!out.taxonomy.is_unsatisfiable(&c("X")));
    }
}

#[test]
fn an_inclusion_in_the_top_data_property_keeps_the_rewriting() {
    // roadmap item 1282: the inclusion holds of every ontology, so its data property keeps
    // nothing from item 805's rewriting of the universal property
    let out = rooted(
        "SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D)
         SubClassOf(:X ObjectSomeValuesFrom(owl:topObjectProperty :A))
         SubDataPropertyOf(:d owl:topDataProperty)",
        false,
        &[("A", "D")],
    );
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
}

/// The axioms of [`KEYED`] with an anonymous individual in place of `:a`, which makes a
/// run rooted as a named one does (roadmap items 1265 and 1281).
const KEYED_ANONYMOUS: &str =
    "SubClassOf(:A ObjectUnionOf(:B ObjectSomeValuesFrom(:r ObjectOneOf(_:a))))
     SubClassOf(:C ObjectUnionOf(:A :B))
     SubClassOf(:B ObjectSomeValuesFrom(:r :A))";

#[test]
fn a_key_left_after_the_rewriting_keeps_a_run_with_an_anonymous_individual_from_being_rooted() {
    // the only individual is anonymous and outside OWL 2 EL, so without item 1265's check
    // the normalization would make the run rooted beside the key the rewriting declined
    let left = keys_left(
        &format!("{KEYED_ANONYMOUS}\nHasKey(:B (:r) ())\nSubClassOf(<{INTERNAL}x> :A)"),
        true,
    );
    assert!(left_with_individual(&left), "{left:?}");
    let left = keys_left(
        &format!(
            "SubClassOf(:A ObjectUnionOf(:B :C))
             ClassAssertion(ObjectUnionOf(:B ObjectComplementOf(:A)) _:x)
             HasKey(:B (:r) ())
             SubClassOf(:B ObjectSomeValuesFrom(:r :A))
             SubClassOf(<{INTERNAL}x> :A)"
        ),
        true,
    );
    assert!(
        left.iter().any(|l| l.contains("ClassAssertion")),
        "{left:?}"
    );
}

#[test]
fn a_key_left_after_the_rewriting_beside_an_anonymous_individual_in_owl_2_el() {
    // the anonymous individual is in the EL part, which keeps the disjunctive rules from
    // the run, and the key keeps it from being rooted, so both runs answer alike
    keys_left(
        &format!(
            "{KEYED}\nHasKey(:B (:r) ())\nClassAssertion(:B _:x)\nSubClassOf(<{INTERNAL}x> :A)"
        ),
        false,
    );
}

#[test]
fn a_rewritten_key_does_not_bind_an_anonymous_individual_in_a_rooted_run() {
    // item 805's rewriting enumerates the named individuals alone, so in a rooted run of
    // Tier D the key makes :a equal to a named :x and not to an anonymous _:x
    let run = |x: &str| {
        let ax = axioms(&format!(
            "HasKey(:K (:p) ()) ClassAssertion(:K {x}) ObjectPropertyAssertion(:p {x} :v)
             ClassAssertion(:K :a) ObjectPropertyAssertion(:p :a :v)
             ClassAssertion(:A {x}) ClassAssertion(:B :a) DisjointClasses(:A :B)"
        ));
        let mut outs = Vec::new();
        for threads in [1, 4] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads,
                    ..rooted_limits()
                })
                .unwrap();
            assert_eq!(out.stats.tier, Tier::Disjunctive, "{x}");
            assert!(k.excluded().is_empty(), "{x}: {:#?}", k.excluded());
            outs.push(out.consistent);
        }
        assert_eq!(outs[0], outs[1], "{x}");
        outs[0]
    };
    assert!(run("_:x"));
    assert!(!run(":x"));
}

/// M-send skips the marker of a trigger that is a live unit of the root, since the contexts
/// registered for its constants take the unit as a fact. A unit `→ l` of the root dies only
/// when a clause that subsumes it arrives, and only the empty clause does: the store takes
/// no second copy of the unit, and an equality adds clauses beside the unit rather than
/// rewriting it. So a unit live at the skip dies only when the root becomes unsatisfiable,
/// which sends the empty clause to every registered context. The cases: `pos` with an
/// equality of the constant that the root derives later, along a chain or through a
/// successor and back, with the linked element on either name, and `pos` whose root derives
/// a contradiction with the unit after the skip.
#[test]
fn a_root_unit_live_at_the_skip_stays_live_or_ends_the_root() {
    let pos = REPAIRS.iter().find(|(n, _)| *n == "pos").unwrap().1;
    let chain = "ClassAssertion(:H0 :w)
     SubClassOf(:H0 :H1)
     SubClassOf(:H1 :H2)
     SubClassOf(:H2 ObjectOneOf(:w2))";
    let back = "ClassAssertion(ObjectSomeValuesFrom(:p9 :H0) :w)
     SubClassOf(:H0 ObjectSomeValuesFrom(:p8 :H1))
     SubClassOf(:H1 ObjectHasValue(:p7 :w2))
     SubClassOf(ObjectSomeValuesFrom(:p7 ObjectOneOf(:w2)) :H4)
     SubClassOf(ObjectSomeValuesFrom(:p8 :H4) :H5)
     SubClassOf(ObjectSomeValuesFrom(:p9 :H5) ObjectOneOf(:w2))";
    let to_w2 = "SubClassOf(:E ObjectHasValue(:r2 :w2))";
    let cases = [
        format!("{pos}\n     {chain}"),
        format!("{pos}\n     {chain}\n     {to_w2}"),
        format!("{pos}\n     {back}"),
        format!("{pos}\n     {back}\n     {to_w2}"),
    ];
    let ended = format!(
        "{pos}\n     ClassAssertion(:H0 :w)
     SubClassOf(:H0 :H1)
     SubClassOf(:H1 :H2)
     DisjointClasses(:H2 :G)"
    );
    for (threads, expansion) in CONFIGS {
        let limits = Limits {
            threads,
            expansion,
            rooted: true,
            rooted_decline: false,
            rooted_marker_repair: true,
            ..Limits::default()
        };
        let at = format!("{threads} threads {expansion:?}");
        for (i, body) in cases.iter().enumerate() {
            let mut k = Classifier::of(&axioms(body));
            let out = k.classify(&limits).unwrap();
            assert!(rooted_claim(&k, &out), "{at}, case {i}: no claim");
            assert!(
                out.taxonomy.is_subclass(&c("K"), &c("Ans")),
                "{at}, case {i}: K ⊑ Ans missed"
            );
        }
        let mut k = Classifier::of(&axioms(&ended));
        let out = k.classify(&limits).unwrap();
        assert!(rooted_claim(&k, &out), "{at}, ended: no claim");
        assert!(!out.consistent, "{at}: the contradiction is missed");
    }
}

/// The repair in the run without counting after a rooted run with counting passes its bound:
/// that run answers rooted and finds `K ⊑ Ans`. The bound lies between the work of the two
/// runs with the repair, 303 without counting and 453 to 464 with it.
#[test]
fn the_marker_repair_delivers_in_the_run_without_counting_after_the_bound() {
    let ax = axioms(&format!(
        "{AV2}\n     SubClassOf(:K ObjectMaxCardinality(1 :q))"
    ));
    for (threads, expansion) in CONFIGS {
        let at = format!("{threads} threads {expansion:?}");
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                counting: true,
                rooted: true,
                rooted_decline: false,
                rooted_marker_repair: true,
                dl_work: Some(380),
                ..Limits::default()
            })
            .unwrap();
        assert!(
            out.stats.dl_counting_over,
            "{at}: the run with counting finished"
        );
        // The fallback finishes for the axioms it takes and leaves the maximum out.
        assert_eq!(out.stats.tier, Tier::Disjunctive, "{at}: unexpected tier");
        assert!(out.complete_for_taken, "{at}: the fallback did not finish");
        assert!(!out.complete, "{at}: the fallback left the maximum out");
        assert!(
            !out.stats.dl_rooted_over && !out.stats.dl_rooted_declined,
            "{at}: the run started again without the rooted rules"
        );
        assert!(
            out.taxonomy.is_subclass(&c("K"), &c("Ans")),
            "{at}: K ⊑ Ans missed"
        );
        // the control: without the repair, the same path under the bound of 180, between
        // 145 and 216, misses K ⊑ Ans
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                counting: true,
                rooted: true,
                rooted_decline: false,
                dl_work: Some(180),
                ..Limits::default()
            })
            .unwrap();
        assert!(
            out.stats.dl_counting_over && !out.stats.dl_rooted_over,
            "{at}: control"
        );
        assert!(
            !out.taxonomy.is_subclass(&c("K"), &c("Ans")),
            "{at}: the control finds K ⊑ Ans"
        );
    }
}
