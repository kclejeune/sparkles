//! The rooted run of roadmap item 603's sixth phase: nominals and the object ABox in the
//! disjunctive rules (F24, "Nominals in the disjunctive rules"). Each case adds one
//! disjunctive axiom that no other axiom meets, so that the rooted run takes it, and runs
//! with counting off unless it says otherwise, on one and four threads under both
//! expansion strategies. Every run asks for the rooted run with `Limits::rooted`, which
//! is off by default, since item 804 lands it as an option: without it, a run with
//! individuals takes the EL path and leaves the disjunctive axiom out.

use sparkles_owl::classify::{
    Classification, Classifier, Consistency, Error, Expansion, Limits, Saturation, Tier,
};
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
    // without it the rooted rules would leave the minimum out, which the Horn rules take,
    // so the Horn rules take the axioms outside OWL 2 EL as in the run without the option
    // and leave the disjunction out (roadmap item 1286)
    let left = declined(body, true);
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("ObjectUnionOf"), "{left:?}");
    let mut k = Classifier::of(&axioms(body));
    let out = k.classify(&rooted_limits()).unwrap();
    assert_eq!(out.stats.clauses, 0, "Tier D took a clause");
    assert!(
        out.taxonomy.is_unsatisfiable(&c("A")) || !out.taxonomy.is_subclass(&c("A"), &c("C")),
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

/// `≥2 r.⊤ ⊑ ⊥` states the empty class of two members whose images are one, and two
/// replaced axioms can rewrite to one axiom, here `≥2 r.∃U.A ⊑ ⊥` (row 1304). Without
/// counting the rooted rules would leave that Horn axiom out, so the run gives the
/// original axioms to the Horn rules, which take them as the run without the option does
/// (roadmap item 1286). The crate's tests check the report of such an axiom in the run
/// without counting after the counting bound, which leaves it out.
#[test]
fn a_rewritten_horn_axiom_that_needs_counting_goes_to_the_horn_rules() {
    for body in [
        "SubClassOf(ObjectMinCardinality(2 :r owl:Thing) owl:Nothing)
         DisjointClasses(ObjectMinCardinality(2 :r owl:Thing)
             ObjectMinCardinality(2 :r ObjectMinCardinality(0 owl:topObjectProperty :X)))
         ClassAssertion(:B :b)",
        "DisjointClasses(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             owl:Nothing)
         ClassAssertion(:B :b)",
    ] {
        let left = declined(body, true);
        assert_eq!(left.len(), 1, "{left:#?}");
        assert!(left[0].contains("ObjectUnionOf"), "{left:#?}");
    }
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
fn a_functional_property_above_the_universal_property_goes_to_the_horn_rules() {
    // p is above the transitive property that stands for the universal one, so p is not
    // simple, and the rooted rules without counting would leave its functionality out,
    // which the Horn rules take, so the run answers as the run without the option and
    // leaves the union out (roadmap item 1286)
    let left = declined(
        "EquivalentObjectProperties(owl:topObjectProperty :p)
         FunctionalObjectProperty(:p)
         ClassAssertion(:A :a)",
        true,
    );
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].contains("ObjectUnionOf"), "{left:?}");
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

/// Checks structural invariants of a taxonomy: no duplicate classes, every `node_of`
/// agrees with the node's membership, parent/child edges are symmetric and sorted without
/// self-loops or duplicates, and the top and bottom nodes hold no parents.
fn assert_taxonomy_well_formed(t: &sparkles_owl::classify::Taxonomy) {
    use std::collections::BTreeSet;
    let mut seen_classes = BTreeSet::new();
    for (idx, node) in t.nodes().iter().enumerate() {
        for cl in &node.classes {
            assert!(
                seen_classes.insert(cl.clone()),
                "duplicate class {cl:?} in taxonomy"
            );
            assert_eq!(t.node_of(cl), Some(idx), "node_of mismatch for {cl:?}");
        }
        assert!(
            node.parents.windows(2).all(|w| w[0] < w[1]),
            "unsorted or duplicate parents on node {idx}: {:?}",
            node.parents
        );
        assert!(
            node.children.windows(2).all(|w| w[0] < w[1]),
            "unsorted or duplicate children on node {idx}: {:?}",
            node.children
        );
        assert!(
            !node.parents.contains(&idx) && !node.children.contains(&idx),
            "self-loop on node {idx}"
        );
        for &p in &node.parents {
            assert!(
                p < t.nodes().len() && t.node(p).children.contains(&idx),
                "broken parent edge {idx} -> {p}"
            );
        }
        for &ch in &node.children {
            assert!(
                ch < t.nodes().len() && t.node(ch).parents.contains(&idx),
                "broken child edge {idx} -> {ch}"
            );
        }
    }
    assert!(t.top().parents.is_empty());
    assert!(t.bottom().parents.is_empty());
    assert!(t.bottom().children.is_empty());
}

#[test]
fn classes_above_a_top_equivalent_class_are_lifted_in_a_rooted_run() {
    // Every element is a and an A, so owl:Thing ⊑ A. In A's context, x ≈ a rewrites the
    // core atom A(x) to A(a), which with r(a, a) and A ⊑ ∀r.B derives B(a) and thus B(x),
    // and then B ⊑ C ≡ D. In owl:Thing's context, x ↦ a does not rewrite A(x), so
    // owl:Thing's context derives A without B, C or D. Lifting every class above A moves
    // B, C and D to the top node with A, while keeping E and F below it and U at bottom.
    let body = "SubClassOf(owl:Thing ObjectOneOf(:a))
                SubClassOf(owl:Thing :A)
                SubClassOf(:U owl:Nothing)
                ObjectPropertyAssertion(:r :a :a)
                SubClassOf(:A ObjectAllValuesFrom(:r :B))
                SubClassOf(:B :C)
                EquivalentClasses(:C :D)
                SubClassOf(:E :C)
                SubClassOf(:F :E)";
    let ax = axioms(body);
    let thing = Class::thing();
    for (threads, expansion) in CONFIGS {
        let at = format!("{threads} threads {expansion:?}");
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{at}: {:#?}", k.excluded());
        assert_eq!(out.stats.tier, Tier::Disjunctive, "{at}");
        assert!(out.complete, "{at}");
        assert_eq!(out.consistency, Consistency::Consistent, "{at}");
        assert_taxonomy_well_formed(&out.taxonomy);
        let top = out.taxonomy.node_of(&thing).unwrap();
        let bottom = out.taxonomy.node_of(&Class::nothing()).unwrap();
        for name in ["A", "B", "C", "D"] {
            assert_eq!(
                out.taxonomy.node_of(&c(name)),
                Some(top),
                "{at}: {name} should be lifted to the top node"
            );
            assert!(
                out.taxonomy.is_subclass(&thing, &c(name)),
                "{at}: owl:Thing ⊑ {name}"
            );
        }
        let e_node = out.taxonomy.node_of(&c("E")).unwrap();
        let f_node = out.taxonomy.node_of(&c("F")).unwrap();
        assert_ne!(e_node, top, "{at}: E is strictly below owl:Thing");
        assert_ne!(f_node, top, "{at}: F is strictly below owl:Thing");
        assert_eq!(out.taxonomy.parents(&c("E")), &[top], "{at}");
        assert_eq!(out.taxonomy.parents(&c("F")), &[e_node], "{at}");
        assert!(out.taxonomy.top().children.contains(&e_node), "{at}");
        assert!(!out.taxonomy.top().children.contains(&f_node), "{at}");
        assert_eq!(out.taxonomy.node_of(&c("U")), Some(bottom), "{at}");

        // `describe` agrees with the taxonomy for lifted top classes and classes below them.
        let top_desc = k.describe(&thing).unwrap();
        for name in ["A", "B", "C", "D"] {
            assert!(
                top_desc.equivalents.contains(&c(name)),
                "{at}: describe(owl:Thing) missing equivalent {name}: {top_desc:?}"
            );
            let d = k.describe(&c(name)).unwrap();
            assert!(d.equivalents.contains(&thing), "{at}: {name} {d:?}");
            assert!(d.superclasses.is_empty(), "{at}: {name} {d:?}");
            assert!(d.direct_superclasses.is_empty(), "{at}: {name} {d:?}");
            assert!(d.subclasses.contains(&c("E")), "{at}: {name} {d:?}");
            assert!(d.direct_subclasses.contains(&c("E")), "{at}: {name} {d:?}");
        }
        let e_desc = k.describe(&c("E")).unwrap();
        assert!(e_desc.equivalents.is_empty(), "{at}: {e_desc:?}");
        for name in ["A", "B", "C", "D"] {
            assert!(
                e_desc.direct_superclasses.contains(&c(name)),
                "{at}: E direct_superclasses missing {name}: {e_desc:?}"
            );
        }
        assert!(
            e_desc.direct_superclasses.contains(&thing),
            "{at}: {e_desc:?}"
        );
    }
}

#[test]
fn inconsistent_rooted_run_with_lifted_top_chain_keeps_top_empty() {
    let body = "SubClassOf(owl:Thing ObjectOneOf(:a))
                SubClassOf(owl:Thing :A)
                ObjectPropertyAssertion(:r :a :a)
                SubClassOf(:A ObjectAllValuesFrom(:r :B))
                SubClassOf(:B :C)
                SubClassOf(:C owl:Nothing)";
    let ax = axioms(body);
    let thing = Class::thing();
    for (threads, expansion) in CONFIGS {
        let at = format!("{threads} threads {expansion:?}");
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                rooted: true,
                ..Limits::default()
            })
            .unwrap();
        assert_eq!(out.consistency, Consistency::Inconsistent, "{at}");
        assert!(!out.consistent, "{at}");
        assert_taxonomy_well_formed(&out.taxonomy);
        assert!(out.taxonomy.top().classes.is_empty(), "{at}");
        let bottom = out.taxonomy.node_of(&Class::nothing()).unwrap();
        assert_eq!(out.taxonomy.node_of(&thing), Some(bottom), "{at}");
        for name in ["A", "B", "C"] {
            assert_eq!(out.taxonomy.node_of(&c(name)), Some(bottom), "{at}");
        }
    }
}

/// Parses an ontology directly with `ofn_read::parse`, without injecting `UNION` (row 1313).
fn direct_axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

/// Asserts that a direct-parsed ontology whose item-805 rewriting turns a one-image
/// `DisjointUnion` into Horn axioms keeps the rewritten result without an injected
/// disjunction (row 1313).
#[test]
fn one_image_disjoint_union_becomes_horn_without_injected_disjunction() {
    let ax = direct_axioms(
        "DisjointUnion(:C
             ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A))
         SubClassOf(:K :A)
         SubClassOf(:L :C)
         ClassAssertion(:B :b)",
    );
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert!(out.incomplete.is_empty());
        assert_eq!(out.consistency, Consistency::Consistent);
        for name in ["A", "K", "C", "L"] {
            assert!(
                out.taxonomy.is_unsatisfiable(&c(name)),
                "{threads}: {name} is empty"
            );
        }
        assert!(!out.taxonomy.is_unsatisfiable(&c("B")));
        assert_eq!(out.stats.added, ax.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 0);
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 0);
        assert_eq!(out.stats.outside_el.taken, 1);
        assert_eq!(out.stats.clauses, 0, "Horn rules ran instead of Tier D");
        let instances: Vec<String> = k
            .describe(&Class::thing())
            .unwrap()
            .instances
            .iter()
            .map(|i| i.as_str().to_string())
            .collect();
        assert_eq!(instances, [format!("{EX}b")]);
        no_internal_names(
            &k,
            &out,
            &[
                Class::thing(),
                Class::nothing(),
                c("A"),
                c("B"),
                c("C"),
                c("K"),
                c("L"),
            ],
        );
    }

    // Re-running after removing the DisjointUnion resets the reduced state and matches a
    // fresh classifier without leaking internal names.
    let mut k = Classifier::of(&ax);
    k.classify(&rooted_limits()).unwrap();
    let du: Vec<Axiom> = ax
        .iter()
        .filter(|a| matches!(a.kind, AxiomKind::DisjointUnion(..)))
        .cloned()
        .collect();
    k.remove(&du);
    let after = k.classify(&rooted_limits()).unwrap();
    let rest: Vec<Axiom> = ax
        .iter()
        .filter(|a| !matches!(a.kind, AxiomKind::DisjointUnion(..)))
        .cloned()
        .collect();
    let mut fresh = Classifier::of(&rest);
    let want = fresh.classify(&rooted_limits()).unwrap();
    assert_eq!(after.taxonomy, want.taxonomy);
    assert_eq!(after.consistency, want.consistency);
    assert_eq!(k.excluded(), fresh.excluded());
    assert!(!after.taxonomy.is_unsatisfiable(&c("A")));
    no_internal_names(&k, &after, &[Class::thing(), c("A"), c("B"), c("K")]);
}

/// A one-image `DisjointUnion` whose rewriting drops `owl:topObjectProperty` via
/// `ObjectMinCardinality(0 owl:topObjectProperty :X)` becomes pure OWL 2 EL (row 1313).
#[test]
fn one_image_disjoint_union_becomes_el_without_universal_property() {
    let ax = direct_axioms(
        "DisjointUnion(:C
             ObjectSomeValuesFrom(:r owl:Thing)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
         SubClassOf(:D ObjectSomeValuesFrom(:r :Y))
         SubClassOf(:L :C)
         ClassAssertion(:Y :y)",
    );
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        for name in ["C", "L", "D"] {
            assert!(
                out.taxonomy.is_unsatisfiable(&c(name)),
                "{threads}: {name} is empty"
            );
        }
        for name in ["X", "Y"] {
            assert!(
                out.taxonomy.node_of(&c(name)).is_some(),
                "{threads}: {name} stays in signature"
            );
            assert!(
                !out.taxonomy.is_unsatisfiable(&c(name)),
                "{threads}: {name} stays satisfiable"
            );
        }
        assert_eq!(out.stats.added, ax.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 0);
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 0);
        assert_eq!(out.stats.outside_el.taken, 1);
        assert_eq!(out.stats.clauses, 0);
        assert_eq!(out.stats.conjunctions, 0);
        let instances: Vec<String> = k
            .describe(&Class::thing())
            .unwrap()
            .instances
            .iter()
            .map(|i| i.as_str().to_string())
            .collect();
        assert_eq!(instances, [format!("{EX}y")]);
        no_internal_names(
            &k,
            &out,
            &[Class::thing(), c("C"), c("D"), c("L"), c("X"), c("Y")],
        );
    }
}

/// Two complemented disjointness members that merge to one image become `¬D ⊑ ⊥`, which is
/// Horn (`⊤ ⊑ D`), and must be kept without an injected disjunction (row 1313).
#[test]
fn two_complemented_disjointness_members_become_horn_without_injected_disjunction() {
    // Nested complement case: `¬(P ⊓ ¬∃top.A)` and `¬(P ⊓ ¬≥1 top.A)` merge to
    // `¬(P ⊓ ¬∃U.A) ⊑ ⊥`, giving `⊤ ⊑ P` and `A ⊑ ⊥`. Without item 1313, the original
    // disjointness fails Horn normalization because each positive complement allocates its
    // own fresh class.
    let ax_nested = direct_axioms(
        "DisjointClasses(
             ObjectComplementOf(ObjectIntersectionOf(:P ObjectComplementOf(ObjectSomeValuesFrom(owl:topObjectProperty :A))))
             ObjectComplementOf(ObjectIntersectionOf(:P ObjectComplementOf(ObjectMinCardinality(1 owl:topObjectProperty :A)))))
         ClassAssertion(:D :d)",
    );
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax_nested);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_subclass(&Class::thing(), &c("P")));
        assert!(out.taxonomy.is_subclass(&c("D"), &c("P")));
        assert!(!out.taxonomy.is_subclass(&c("P"), &c("D")));
        assert!(out.taxonomy.is_unsatisfiable(&c("A")));
        assert!(!out.taxonomy.is_unsatisfiable(&c("D")));
        assert_eq!(out.stats.added, ax_nested.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 0);
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 1);
        assert_eq!(out.stats.outside_el.taken, 1);
        assert_eq!(out.stats.clauses, 0);
        let instances: Vec<String> = k
            .describe(&Class::thing())
            .unwrap()
            .instances
            .iter()
            .map(|i| i.as_str().to_string())
            .collect();
        assert_eq!(instances, [format!("{EX}d")]);
        no_internal_names(&k, &out, &[Class::thing(), c("A"), c("D"), c("P")]);
    }

    // Consistent case with `U` and `hub`: `¬∃top.A` and `¬≥1 top.A` merge to `¬∃U.A ⊑ ⊥`,
    // giving `⊤ ⊑ ∃U.A` and thus `owl:Thing ⊑ :B`.
    let ax = direct_axioms(
        "DisjointClasses(
             ObjectComplementOf(ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectComplementOf(ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :A) :B)
         ClassAssertion(:D :d)",
    );
    for threads in [1, 4] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_subclass(&Class::thing(), &c("B")));
        assert!(out.taxonomy.is_subclass(&c("D"), &c("B")));
        assert!(!out.taxonomy.is_subclass(&c("B"), &c("D")));
        assert!(!out.taxonomy.is_unsatisfiable(&c("A")));
        assert_eq!(out.stats.added, ax.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 0);
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 1);
        assert_eq!(out.stats.outside_el.taken, 1);
        assert_eq!(out.stats.clauses, 0);
        let instances: Vec<String> = k
            .describe(&Class::thing())
            .unwrap()
            .instances
            .iter()
            .map(|i| i.as_str().to_string())
            .collect();
        assert_eq!(instances, [format!("{EX}d")]);
        no_internal_names(&k, &out, &[Class::thing(), c("A"), c("B"), c("D")]);
    }

    // Inconsistent case: `⊤ ⊑ ∃top.A` with `A ⊑ ⊥` has no model.
    let ax_unsat = direct_axioms(
        "DisjointClasses(
             ObjectComplementOf(ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectComplementOf(ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(:A owl:Nothing)
         ClassAssertion(:D :d)",
    );
    let mut k = Classifier::of(&ax_unsat);
    let out = k
        .classify(&Limits {
            rooted: true,
            strict: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.complete);
    assert!(!out.consistent);
    assert_eq!(out.consistency, Consistency::Inconsistent);
    assert_eq!(out.stats.added, ax_unsat.len());
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.stats.outside_el.horn, 0);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.global, 1);
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.clauses, 0);
    let instances: Vec<String> = k
        .describe(&Class::thing())
        .unwrap()
        .instances
        .iter()
        .map(|i| i.as_str().to_string())
        .collect();
    assert_eq!(instances, [format!("{EX}d")]);
    no_internal_names(&k, &out, &[Class::thing(), c("A"), c("D")]);

    // Case without `U`/`hub`: `¬∃r.⊤` and `¬∃r.(≥0 top.X)` merge to `¬∃r.⊤ ⊑ ⊥`, giving
    // `⊤ ⊑ ∃r.⊤` and thus `owl:Thing ⊑ :Dom`.
    let ax_no_u = direct_axioms(
        "DisjointClasses(
             ObjectComplementOf(ObjectSomeValuesFrom(:r owl:Thing))
             ObjectComplementOf(ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X))))
         ObjectPropertyDomain(:r :Dom)",
    );
    let mut k = Classifier::of(&ax_no_u);
    let out = k
        .classify(&Limits {
            rooted: true,
            strict: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.complete);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("Dom")));
    assert!(out.taxonomy.node_of(&c("X")).is_some());
    assert!(!out.taxonomy.is_unsatisfiable(&c("X")));
    assert_eq!(out.stats.added, ax_no_u.len());
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.stats.outside_el.horn, 0);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.global, 1);
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.clauses, 0);
    no_internal_names(&k, &out, &[Class::thing(), c("Dom"), c("X")]);
}

/// When item-805 rewriting turns the ontology Horn beside a `HasKey`, the Horn rules
/// classify both the key axioms and the rewritten disjoint union while hiding every fresh
/// key class and the hub individual (row 1313).
#[test]
fn one_image_disjoint_union_with_key_keeps_rewritten_horn_and_hides_key_names() {
    let ax = direct_axioms(
        "HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         ClassAssertion(:G :a)
         EquivalentClasses(:D ObjectOneOf(:b))
         DisjointUnion(:C
             ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A))",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            rooted: true,
            strict: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
    assert!(out.complete);
    assert_eq!(out.consistency, Consistency::Consistent);
    assert!(out.taxonomy.is_subclass(&c("D"), &c("G")));
    assert!(out.taxonomy.is_subclass(&c("D"), &c("K")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("D")));
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));
    assert_eq!(out.stats.added, ax.len());
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.stats.outside_el.horn, 0);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.clauses, 0);
    let instances: Vec<String> = k
        .describe(&Class::thing())
        .unwrap()
        .instances
        .iter()
        .map(|i| i.as_str().to_string())
        .collect();
    assert_eq!(
        instances,
        [format!("{EX}a"), format!("{EX}b"), format!("{EX}c")]
    );
    assert!(
        k.describe(&Class::new(format!("{INTERNAL}v0-0-1")))
            .is_none()
    );
    assert!(
        k.describe(&Class::new(format!("{INTERNAL}m0-0-0-1")))
            .is_none()
    );
    no_internal_names(
        &k,
        &out,
        &[Class::thing(), c("A"), c("C"), c("D"), c("G"), c("K")],
    );
}

/// When the rewritten ontology becomes Horn and every rewritten axiom is taken, an
/// unchanged axiom outside OWL 2 DL (such as functionality on a superproperty of
/// `owl:topObjectProperty`) stays excluded while the rewritten result is kept (row 1313).
#[test]
fn rewritten_horn_keeps_result_when_only_an_unchanged_axiom_is_excluded() {
    let ax = direct_axioms(
        "EquivalentObjectProperties(owl:topObjectProperty :p)
         FunctionalObjectProperty(:p)
         DisjointUnion(:C
             ObjectSomeValuesFrom(owl:topObjectProperty :A)
             ObjectMinCardinality(1 owl:topObjectProperty :A))
         ClassAssertion(:B :b)",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    assert!(!out.complete && out.complete_for_taken);
    assert!(out.consistent);
    assert_eq!(out.consistency, Consistency::Unknown);
    assert!(out.taxonomy.is_unsatisfiable(&c("A")));
    assert!(out.taxonomy.is_unsatisfiable(&c("C")));
    assert!(!out.taxonomy.is_unsatisfiable(&c("B")));
    let left = k.excluded();
    assert_eq!(left.len(), 1, "{left:#?}");
    assert!(matches!(
        left[0].axiom,
        AxiomKind::FunctionalObjectProperty(_)
    ));
    assert_eq!(out.stats.added, ax.len());
    assert_eq!(out.stats.tier, Tier::Disjunctive);
    assert_eq!(out.stats.outside_el.horn, 1);
    assert_eq!(out.stats.outside_el.disjunctive, 1);
    assert_eq!(out.stats.outside_el.taken, 1);
    assert_eq!(out.stats.clauses, 0);
    no_internal_names(&k, &out, &[Class::thing(), c("A"), c("B"), c("C")]);
}

/// Ineligible lower-tier rewrites must still be discarded and reported on the original
/// axioms when the lower-tier reduction contract does not hold (row 1313).
#[test]
fn ineligible_lower_tier_rewrites_still_fall_back_to_original_axioms() {
    let cases: &[(&str, &str)] = &[
        // Non-regular role hierarchy causes Horn to reject Symmetric(U) and Transitive(U).
        (
            "non-regular top chain",
            "SubObjectPropertyOf(ObjectPropertyChain(:r owl:topObjectProperty :r) :r)
             DisjointUnion(:C
                 ObjectSomeValuesFrom(owl:topObjectProperty :A)
                 ObjectMinCardinality(1 owl:topObjectProperty :A))
             ClassAssertion(:B :b)",
        ),
        // The duplicate image drops out. The self restriction over a transitive role in
        // the two remaining members makes the Horn-shaped union ineligible.
        (
            "horn-shaped disjoint union with a non-simple self role",
            "TransitiveObjectProperty(:s)
             DisjointUnion(:C
                 owl:Thing
                 ObjectHasSelf(:s)
                 ObjectSomeValuesFrom(owl:topObjectProperty :A)
                 ObjectMinCardinality(1 owl:topObjectProperty :A))
             ClassAssertion(:B :b)",
        ),
        // Rewritten ¬(≥65 r.∃U.A) ⊑ ⊥ is Horn-shaped but exceeds the minimum
        // cardinality supported by the Horn rules.
        (
            "complemented at-least-65 beside individuals",
            "DisjointClasses(
                 ObjectComplementOf(ObjectMinCardinality(65 :r ObjectSomeValuesFrom(owl:topObjectProperty :A)))
                 ObjectComplementOf(ObjectMinCardinality(65 :r ObjectMinCardinality(1 owl:topObjectProperty :A))))
             ClassAssertion(:B :b)",
        ),
        // Rewritten ≥3 r.∃U.A ⊑ ⊥ remains non-Horn (Tier::Disjunctive) and Tier D declines
        // it with counting off.
        (
            "at-least-3 stays disjunctive",
            "DisjointClasses(
                 ObjectMinCardinality(3 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
                 ObjectMinCardinality(3 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
             ClassAssertion(:B :b)",
        ),
    ];

    for &(label, body) in cases {
        let ax = direct_axioms(body);
        let mut k = Classifier::of(&ax);
        let out = k.classify(&rooted_limits()).unwrap();
        let mut plain = Classifier::of(&ax);
        let default = plain.classify(&Limits::default()).unwrap();
        assert!(!out.complete, "{label}: must stay incomplete");
        assert_eq!(out.taxonomy, default.taxonomy, "{label}: taxonomy");
        assert_eq!(out.consistency, default.consistency, "{label}: consistency");
        assert_eq!(k.excluded(), plain.excluded(), "{label}: excluded");
        assert!(
            k.excluded().iter().any(|e| matches!(
                e.axiom,
                AxiomKind::DisjointUnion(..) | AxiomKind::DisjointClasses(..)
            )),
            "{label}: original non-Horn axiom must be reported: {:#?}",
            k.excluded()
        );
        no_internal_names(&k, &out, &[Class::thing(), c("A"), c("B")]);
    }

    // Non-regular chain beside a HasKey and a one-image DisjointUnion: Horn rejects the
    // rewritten key's universal axiom, so the rewritten run must be discarded and the
    // original EL HasKey must still merge :a and :b.
    let ax = direct_axioms(
        "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
         SubObjectPropertyOf(ObjectPropertyChain(:s :p) :q)
         HasKey(:K (:p) ())
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:p :a :c)
         ObjectPropertyAssertion(:p :b :c)
         ClassAssertion(:G :a)
         EquivalentClasses(:D ObjectOneOf(:b))
         DisjointUnion(:C
             ObjectSomeValuesFrom(:r owl:Thing)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))",
    );
    let mut k = Classifier::of(&ax);
    let out = k.classify(&rooted_limits()).unwrap();
    let mut plain = Classifier::of(&ax);
    let default = plain.classify(&Limits::default()).unwrap();
    assert_eq!(out.taxonomy, default.taxonomy);
    assert_eq!(out.consistency, default.consistency);
    assert_eq!(k.excluded(), plain.excluded());
    assert!(out.taxonomy.is_subclass(&c("D"), &c("G")));
    assert!(
        k.excluded()
            .iter()
            .any(|e| matches!(e.axiom, AxiomKind::DisjointUnion(..))),
        "{:#?}",
        k.excluded()
    );
    assert!(
        !k.excluded()
            .iter()
            .any(|e| matches!(e.axiom, AxiomKind::HasKey(..))),
        "{:#?}",
        k.excluded()
    );
    no_internal_names(&k, &out, &[Class::thing(), c("C"), c("D"), c("G"), c("K")]);
}

/// Kept rewritten lower-tier runs must compute property-chain range violations for
/// `outside_el` from the original ontology rather than the rewritten ontology (row 1313):
/// rewriting drops `:s ⊑ owl:topObjectProperty` and turns `Range(top :T)` into `⊤ ⊑ :T`,
/// which would hide an original chain range violation and undercount `horn` and `taken`.
#[test]
fn kept_rewritten_chain_range_stats_do_not_undercount_original_horn_chains() {
    let under = direct_axioms(
        "DisjointUnion(:C
             ObjectSomeValuesFrom(:r owl:Thing)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
         SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
         SubObjectPropertyOf(:s owl:topObjectProperty)
         ObjectPropertyRange(owl:topObjectProperty :T)",
    );
    let plain_under = Classifier::of(&under).classify(&Limits::default()).unwrap();
    assert_eq!(plain_under.stats.tier, Tier::Disjunctive);
    assert_eq!(plain_under.stats.outside_el.horn, 1);
    assert_eq!(plain_under.stats.outside_el.disjunctive, 1);
    for threads in [1, 4] {
        let mut k = Classifier::of(&under);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_unsatisfiable(&c("C")));
        assert!(out.taxonomy.is_subclass(&Class::thing(), &c("T")));
        assert_eq!(out.stats.added, under.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 1, "{threads}: undercounted horn");
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 0);
        assert_eq!(
            out.stats.outside_el.taken, 2,
            "{threads}: undercounted taken"
        );
        assert_eq!(out.stats.clauses, 0);
        assert_eq!(out.stats.conjunctions, 0);
        no_internal_names(&k, &out, &[Class::thing(), c("C"), c("T"), c("X")]);
    }
}

/// Kept rewritten lower-tier runs must not overcount original EL chains whose rewritten
/// form fails the range check on `U` after `Range(owl:topObjectProperty :T)` becomes
/// `owl:Thing ⊑ :T` (row 1313).
#[test]
fn kept_rewritten_chain_range_stats_do_not_overcount_original_el_chains() {
    let over = direct_axioms(
        "DisjointUnion(:C
             ObjectSomeValuesFrom(:r owl:Thing)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))
         SubObjectPropertyOf(ObjectPropertyChain(:p owl:topObjectProperty) :s)
         ObjectPropertyRange(:s :T)
         ObjectPropertyRange(owl:topObjectProperty :T)",
    );
    let plain_over = Classifier::of(&over).classify(&Limits::default()).unwrap();
    assert_eq!(plain_over.stats.tier, Tier::Disjunctive);
    assert_eq!(plain_over.stats.outside_el.horn, 0);
    assert_eq!(plain_over.stats.outside_el.disjunctive, 1);
    for threads in [1, 4] {
        let mut k = Classifier::of(&over);
        let out = k
            .classify(&Limits {
                threads,
                rooted: true,
                strict: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{threads}: {:#?}", k.excluded());
        assert!(out.complete && out.complete_for_taken);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_unsatisfiable(&c("C")));
        assert!(out.taxonomy.is_subclass(&Class::thing(), &c("T")));
        assert_eq!(out.stats.added, over.len());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert_eq!(out.stats.outside_el.horn, 0, "{threads}: overcounted horn");
        assert_eq!(out.stats.outside_el.disjunctive, 1);
        assert_eq!(out.stats.outside_el.global, 0);
        assert_eq!(
            out.stats.outside_el.taken, 1,
            "{threads}: overcounted taken"
        );
        assert_eq!(out.stats.clauses, 0);
        assert!(k.describe(&Class::thing()).unwrap().instances.is_empty());
        no_internal_names(&k, &out, &[Class::thing(), c("C"), c("T"), c("X")]);
    }
}

/// Builds the item-1313 undercount fixture extended with `n_roles` fresh roles included in
/// `owl:topObjectProperty` and `n_ranges` distinct named ranges on `owl:topObjectProperty`.
/// Rewriting drops the role inclusions and turns the ranges into global subclass axioms,
/// so only the original range scan sees the added role hierarchy.
fn chain_range_scan_fixture(include_chain: bool, n_roles: usize, n_ranges: usize) -> Vec<Axiom> {
    let mut body = String::from(
        "DisjointUnion(:C
             ObjectSomeValuesFrom(:r owl:Thing)
             ObjectSomeValuesFrom(:r ObjectMinCardinality(0 owl:topObjectProperty :X)))\n",
    );
    if include_chain {
        body.push_str(
            "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :s)
             SubObjectPropertyOf(:s owl:topObjectProperty)\n",
        );
    }
    for i in 0..n_roles {
        body.push_str(&format!(
            "SubObjectPropertyOf(:extra{i} owl:topObjectProperty)\n"
        ));
    }
    for j in 0..n_ranges {
        body.push_str(&format!(
            "ObjectPropertyRange(owl:topObjectProperty :T{j})\n"
        ));
    }
    direct_axioms(&body)
}

/// On the kept-rewrite path, the original range scan resolves ranges only for properties
/// needed by candidate chains, admits its scratch against `max_memory` and the pool,
/// reflects its peak in `Stats::peak_memory`, polls limits cooperatively, and preserves
/// clean restart behavior after a stop (row 1313).
#[test]
fn kept_rewritten_original_range_scan_respects_budget_polling_and_restart() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const N_ROLES: usize = 4000;
    const N_RANGES: usize = 200;

    let no_chain_ax = chain_range_scan_fixture(false, N_ROLES, N_RANGES);
    let with_chain_ax = chain_range_scan_fixture(true, N_ROLES, N_RANGES);
    let no_chain: Vec<&AxiomKind> = no_chain_ax.iter().map(|a| &a.kind).collect();
    let with_chain: Vec<&AxiomKind> = with_chain_ax.iter().map(|a| &a.kind).collect();

    let base_limits = Limits {
        threads: 1,
        rooted: true,
        strict: true,
        ..Limits::default()
    };

    // Baseline without the candidate chain: the original range scan skips the hierarchy.
    let mut sat_no_chain = Saturation::new();
    let no_chain_out = sat_no_chain
        .classify(&no_chain, None, &base_limits)
        .unwrap();
    let no_chain_peak = no_chain_out.stats.peak_memory;

    // With the single candidate chain: scratch is linear (not N_ROLES * N_RANGES),
    // reflected in peak_memory, and charged to the memory pool.
    let granted = Arc::new(AtomicUsize::new(0));
    let charge_counter = granted.clone();
    let mut sat_full = Saturation::new();
    let full = sat_full
        .classify(
            &with_chain,
            None,
            &Limits {
                charge: Some(Arc::new(move |bytes| {
                    charge_counter.fetch_add(bytes, Ordering::Relaxed);
                    true
                })),
                ..base_limits.clone()
            },
        )
        .unwrap();
    assert!(sat_full.excluded().is_empty(), "{:#?}", sat_full.excluded());
    assert!(full.complete && full.complete_for_taken);
    assert_eq!(full.consistency, Consistency::Consistent);
    assert!(full.taxonomy.is_unsatisfiable(&c("C")));
    assert!(full.taxonomy.is_subclass(&Class::thing(), &c("T0")));
    assert_eq!(full.stats.tier, Tier::Disjunctive);
    assert_eq!(full.stats.outside_el.horn, 1);
    assert_eq!(full.stats.outside_el.disjunctive, 1);
    assert_eq!(full.stats.outside_el.taken, 2);
    assert!(
        full.stats.peak_memory > no_chain_peak + 32 * 1024,
        "original range scan scratch missing from peak_memory: with_chain={} no_chain={}",
        full.stats.peak_memory,
        no_chain_peak,
    );
    assert_eq!(
        granted.load(Ordering::Relaxed),
        full.stats.peak_memory,
        "pool grants must match peak_memory including original range scan scratch",
    );
    assert!(
        full.stats.peak_memory < 4 * 1024 * 1024,
        "original range scan must avoid N*M expansion: peak={}",
        full.stats.peak_memory,
    );

    // Cap sits above the rewritten run's peak and below the original range scan's peak.
    let cap = no_chain_peak + (full.stats.peak_memory - no_chain_peak) / 2;
    assert!(cap > no_chain_peak && cap < full.stats.peak_memory);

    // 1. max_memory refusal during the original range scan, followed by clean restarts.
    let mut sat_mem = Saturation::new();
    let mem_err = sat_mem.classify(
        &with_chain,
        None,
        &Limits {
            max_memory: Some(cap),
            ..base_limits.clone()
        },
    );
    assert!(
        matches!(mem_err, Err(Error::Memory { .. })),
        "expected Error::Memory during original range scan, got {mem_err:?}"
    );
    if let Some(desc) = sat_mem.describe(&c("C")) {
        for cls in desc
            .equivalents
            .iter()
            .chain(&desc.superclasses)
            .chain(&desc.subclasses)
        {
            assert!(
                !cls.as_str().starts_with("urn:sparkles:internal:"),
                "internal class leaked after stop: {cls:?}"
            );
        }
    }
    let restarted_rooted = sat_mem.classify(&with_chain, None, &base_limits).unwrap();
    assert_eq!(restarted_rooted.taxonomy, full.taxonomy);
    assert_eq!(restarted_rooted.stats.outside_el, full.stats.outside_el);

    // 2. Pool refusal during the original range scan, followed by an unrooted restart.
    let pool_used = Arc::new(AtomicUsize::new(0));
    let pool_tracker = pool_used.clone();
    let mut sat_pool = Saturation::new();
    let pool_err = sat_pool.classify(
        &with_chain,
        None,
        &Limits {
            charge: Some(Arc::new(move |bytes| {
                let prev = pool_tracker.load(Ordering::Relaxed);
                if prev.saturating_add(bytes) > cap {
                    return false;
                }
                pool_tracker.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..base_limits.clone()
        },
    );
    assert!(
        matches!(pool_err, Err(Error::Memory { .. })),
        "expected pool Error::Memory during original range scan, got {pool_err:?}"
    );
    let plain_fresh = Saturation::new()
        .classify(&with_chain, None, &Limits::default())
        .unwrap();
    let restarted_plain = sat_pool
        .classify(&with_chain, None, &Limits::default())
        .unwrap();
    assert_eq!(restarted_plain.taxonomy, plain_fresh.taxonomy);
    assert_eq!(restarted_plain.consistency, plain_fresh.consistency);
    assert_eq!(
        restarted_plain.stats.outside_el,
        plain_fresh.stats.outside_el
    );

    // 3. Cancellation triggered during original range scan admission stops cooperatively
    //    and leaves the classifier able to restart cleanly on both rooted and unrooted runs.
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let cancel_trigger = cancel_flag.clone();
    let cancel_check = cancel_flag.clone();
    let cancel_used = Arc::new(AtomicUsize::new(0));
    let mut sat_cancel = Saturation::new();
    let cancel_err = sat_cancel.classify(
        &with_chain,
        None,
        &Limits {
            charge: Some(Arc::new(move |bytes| {
                let total = cancel_used.fetch_add(bytes, Ordering::Relaxed) + bytes;
                if total > cap {
                    cancel_trigger.store(true, Ordering::Relaxed);
                }
                true
            })),
            cancelled: Some(Arc::new(move || cancel_check.load(Ordering::Relaxed))),
            ..base_limits.clone()
        },
    );
    assert!(
        matches!(cancel_err, Err(Error::Cancelled)),
        "expected Error::Cancelled during original range scan, got {cancel_err:?}"
    );
    cancel_flag.store(false, Ordering::Relaxed);
    let recovered_rooted = sat_cancel
        .classify(&with_chain, None, &base_limits)
        .unwrap();
    assert_eq!(recovered_rooted.taxonomy, full.taxonomy);
    assert_eq!(recovered_rooted.stats.outside_el, full.stats.outside_el);
    let recovered_plain = sat_cancel
        .classify(&with_chain, None, &Limits::default())
        .unwrap();
    assert_eq!(recovered_plain.taxonomy, plain_fresh.taxonomy);
}

// ============================================================================
// Item 1061 Phase 2: Rooted Constant Merging Tests (`Limits::rooted_merge`)
// ============================================================================

fn parse_hermit_subsumptions(
    text: &str,
    classes: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<(String, String)> {
    use std::collections::{BTreeMap, BTreeSet};
    let iris = |line: &str| -> Vec<String> {
        line.split('<')
            .skip(1)
            .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
            .collect()
    };
    let mut up: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for line in text.lines() {
        let ns = iris(line);
        if line.starts_with("SubClassOf(") && ns.len() == 2 {
            up.entry(ns[0].clone()).or_default().insert(ns[1].clone());
        } else if line.starts_with("EquivalentClasses(") {
            for a in &ns {
                for b in &ns {
                    up.entry(a.clone()).or_default().insert(b.clone());
                }
            }
        }
    }
    let above = |cls: &str| -> BTreeSet<String> {
        let mut seen = BTreeSet::from([cls.to_string(), vocab::OWL_THING.to_string()]);
        let mut stack = vec![cls.to_string()];
        while let Some(x) = stack.pop() {
            for y in up.get(&x).into_iter().flatten() {
                if seen.insert(y.clone()) {
                    stack.push(y.clone());
                }
            }
        }
        seen
    };
    let inconsistent = above(vocab::OWL_THING).contains(vocab::OWL_NOTHING);
    let mut out = BTreeSet::new();
    for a in classes {
        let s = above(a);
        let unsat = inconsistent || s.contains(vocab::OWL_NOTHING);
        for b in classes {
            if unsat || s.contains(b) || b == vocab::OWL_THING || a == vocab::OWL_NOTHING {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

fn taxonomy_subsumptions(
    out: &Classification,
    classes: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<(String, String)> {
    let mut got = std::collections::BTreeSet::new();
    for a in classes {
        for b in classes {
            if out
                .taxonomy
                .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
            {
                got.insert((a.clone(), b.clone()));
            }
        }
    }
    got
}

#[test]
fn a_rooted_run_that_merges_equal_individuals_finishes_shoi_022() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let doc = ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-022.ofn")).unwrap()).unwrap();
    let hermit_text = std::fs::read_to_string(dir.join("shoi-022.hermit.ofn")).unwrap();
    let mut classes: std::collections::BTreeSet<String> = doc
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::Class(cls)) => Some(cls.as_str().to_string()),
            _ => None,
        })
        .collect();
    classes.insert(vocab::OWL_THING.to_string());
    classes.insert(vocab::OWL_NOTHING.to_string());
    let want = parse_hermit_subsumptions(&hermit_text, &classes);

    // Control: merge OFF still overflows the rooted work bound on original shoi-022.ofn.
    let mut k_off = Classifier::of(&doc.axioms);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(
        off.stats.dl_rooted_over,
        "merge OFF control should still exceed rooted bound on shoi-022"
    );

    // Merge ON finishes original shoi-022 across all 4 configs × dl_steps {true, false}
    // and matches HermiT's full taxonomy exactly.
    for dl_steps in [true, false] {
        for (threads, expansion) in CONFIGS {
            let at = format!("shoi-022 {threads}t {expansion:?} dl_steps={dl_steps}");
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    rooted_merge: true,
                    dl_steps,
                    ..Limits::default()
                })
                .unwrap();
            assert!(out.complete, "{at}: expected complete: {:?}", out.stats);
            assert!(
                !out.stats.dl_rooted_over && !out.stats.dl_rooted_declined,
                "{at}: unexpected fallback: {:?}",
                out.stats
            );
            assert!(k.excluded().is_empty(), "{at}: {:?}", k.excluded());
            assert_eq!(out.consistency, Consistency::Consistent, "{at}");
            assert_eq!(out.stats.dl_rooted_restarts, 1, "{at}: {:?}", out.stats);
            assert_eq!(out.stats.dl_rooted_merged, 2, "{at}: {:?}", out.stats);
            assert!(out.stats.dl_rooted_spent > 0, "{at}: {:?}", out.stats);
            assert!(
                out.stats.dl_rooted_spent + out.stats.dl_work <= (1 << 19),
                "{at}: total work exceeded bound: {:?}",
                out.stats
            );
            let got = taxonomy_subsumptions(&out, &classes);
            assert_eq!(got, want, "{at}: taxonomy differs from HermiT");
        }
    }
}

#[test]
fn a_restart_counts_the_conclusions_before_it_against_the_bound() {
    let ax = axioms(
        "SubClassOf(:C0 ObjectOneOf(:a2))
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)
         ClassAssertion(ObjectUnionOf(:A :B) :a0)
         SubClassOf(:A :Done)
         SubClassOf(:B :Done)
         SubClassOf(:Q ObjectSomeValuesFrom(:r ObjectOneOf(:a2)))
         SubClassOf(ObjectSomeValuesFrom(:r :Done) :Out)",
    );
    let mut k = Classifier::of(&ax);
    let exact = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(exact.complete);
    assert_eq!(exact.stats.dl_rooted_restarts, 1);
    assert!(exact.stats.dl_rooted_spent > 0);
    assert!(exact.stats.dl_work > 0);
    assert!(exact.taxonomy.is_subclass(&c("Q"), &c("Out")));

    let total = exact.stats.dl_rooted_spent + exact.stats.dl_work;
    // Exact bound completes.
    let mut k_eq = Classifier::of(&ax);
    let at_bound = k_eq
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            dl_work: Some(total),
            ..Limits::default()
        })
        .unwrap();
    assert!(at_bound.complete, "exact total={total} must complete");
    assert!(!at_bound.stats.dl_rooted_over);

    // One less than exact total falls back as rooted work exhaustion.
    let mut k_less = Classifier::of(&ax);
    let under = k_less
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            dl_work: Some(total - 1),
            ..Limits::default()
        })
        .unwrap();
    assert!(
        under.stats.dl_rooted_over,
        "total-1={} must exhaust rooted bound: {:?}",
        total - 1,
        under.stats
    );
}

#[test]
fn an_entailed_equality_with_different_individuals_is_inconsistent() {
    let cases = [
        // Asserted SameIndividual + DifferentIndividuals
        "SameIndividual(:a :b)
         DifferentIndividuals(:a :b)",
        // Entailed equality + DifferentIndividuals
        "SubClassOf(ObjectOneOf(:a) :C)
         SubClassOf(:C ObjectOneOf(:b))
         DifferentIndividuals(:a :b)",
        // 3-member DifferentIndividuals with two colliding
        "SameIndividual(:a :b)
         DifferentIndividuals(:a :b :c)",
        // Entailed equality with 3-member DifferentIndividuals
        "SubClassOf(ObjectOneOf(:a) :C)
         SubClassOf(:C ObjectOneOf(:b))
         DifferentIndividuals(:a :b :c)",
    ];
    for body in cases {
        let ax = axioms(body);
        for rooted_merge in [false, true] {
            for (threads, expansion) in CONFIGS {
                let mut k = Classifier::of(&ax);
                let out = k
                    .classify(&Limits {
                        threads,
                        expansion,
                        rooted: true,
                        rooted_merge,
                        ..Limits::default()
                    })
                    .unwrap();
                assert_eq!(
                    out.consistency,
                    Consistency::Inconsistent,
                    "merge={rooted_merge} {threads}t {expansion:?} body={body}"
                );
            }
        }
    }
}

#[test]
fn repeated_name_in_different_individuals_stays_consistent() {
    // Construct a 1-member DifferentIndividuals directly (as survives structural set
    // deduplication) and verify it does not falsely collide with itself.
    let mut ax = axioms("ClassAssertion(:A :a)");
    ax.push(Axiom::new(AxiomKind::DifferentIndividuals(
        [Individual::named(format!("{EX}a"))].into(),
    )));
    for rooted_merge in [false, true] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge,
                ..Limits::default()
            })
            .unwrap();
        assert_eq!(out.consistency, Consistency::Consistent);
    }
}

#[test]
fn different_individuals_collision_with_anonymous_individuals() {
    let cases = [
        // Named + anonymous asserted equal and different
        "SameIndividual(:a _:b0)
         DifferentIndividuals(:a _:b0)",
        // Named + anonymous entailed equal and different
        "SubClassOf(ObjectOneOf(:a) :C)
         SubClassOf(:C ObjectOneOf(_:b0))
         DifferentIndividuals(:a _:b0)",
        // Anonymous + anonymous asserted equal and different
        "SameIndividual(_:x _:y)
         DifferentIndividuals(_:x _:y)",
        // 3-member mixed named + anonymous collision
        "SameIndividual(:a _:b0)
         DifferentIndividuals(:a _:b0 :c)",
    ];
    for body in cases {
        let ax = axioms(body);
        for rooted_merge in [false, true] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge,
                    ..Limits::default()
                })
                .unwrap();
            assert_eq!(
                out.consistency,
                Consistency::Inconsistent,
                "merge={rooted_merge} body={body}"
            );
        }
    }
}

#[test]
fn anonymous_and_named_merge_picks_named_representative() {
    // ASCII "_:b0" < "http://ex.org/a", so naive IRI order would wrongly pick _:b0.
    // 3-tier ordering must keep :a as representative, preserve HasKey over :a, and
    // expose :a (never _:b0) in describe().
    let cases = [
        "SameIndividual(:a _:b0)
         ClassAssertion(:C _:b0)
         ClassAssertion(:C :b)
         ObjectPropertyAssertion(:r _:b0 :v)
         ObjectPropertyAssertion(:r :b :v)
         HasKey(:C (:r) ())
         ClassAssertion(:Tag :b)",
        "SubClassOf(ObjectOneOf(_:b0) :Link)
         SubClassOf(:Link ObjectOneOf(:a))
         ClassAssertion(:C _:b0)
         ClassAssertion(:C :b)
         ObjectPropertyAssertion(:r _:b0 :v)
         ObjectPropertyAssertion(:r :b :v)
         HasKey(:C (:r) ())
         ClassAssertion(:Tag :b)",
    ];
    for (idx, body) in cases.iter().enumerate() {
        let ax = axioms(body);
        let mut k_off = Classifier::of(&ax);
        let off = k_off
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: false,
                ..Limits::default()
            })
            .unwrap();
        let d_off = k_off.describe(&c("Tag")).unwrap();

        let mut k_on = Classifier::of(&ax);
        let on = k_on
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .unwrap();
        let d_on = k_on.describe(&c("Tag")).unwrap();

        assert_eq!(on.consistency, off.consistency);
        assert!(on.complete);
        // Under OWL 2 Direct Semantics, _:b0 ≈ :a and HasKey(:C (:r) ()) identify :a with :b,
        // so both :a and :b are instances of :Tag (and _:b0 never leaks).
        let want_instances = vec![
            NamedIndividual::new(format!("{EX}a")),
            NamedIndividual::new(format!("{EX}b")),
        ];
        assert_eq!(d_on.instances, want_instances, "case {idx}: body={body}");
        if idx == 0 {
            assert_eq!(
                d_on.instances, d_off.instances,
                "case 0 must match merge OFF"
            );
        }
        assert!(
            d_on.instances.iter().all(|i| !i.as_str().starts_with("_:")),
            "anonymous label leaked into describe: {:?}",
            d_on.instances
        );
    }
}

#[test]
fn input_with_internal_prefix_iri_merged_with_blank_keeps_named_status() {
    // Legal user input IRI `urn:sparkles:internal:hub` (Astra consult §3): item 805
    // declines reduction on prefix collision, so this name is a genuine user input name
    // (Tier 0), must beat `_:b0` (Tier 1), participate in HasKey, and stay visible in
    // describe().
    let body = "Prefix(:=<http://ex.org/>)
        Ontology(
            SameIndividual(<urn:sparkles:internal:hub> _:b0)
            ClassAssertion(:K _:b0)
            ClassAssertion(:K :other)
            ObjectPropertyAssertion(:r _:b0 :v)
            ObjectPropertyAssertion(:r :other :v)
            HasKey(:K (:r) ())
            ClassAssertion(:Mark :other)
        )";
    let doc = ofn_read::parse(body).unwrap();
    let mut k_off = Classifier::of(&doc.axioms);
    let _ = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let d_off = k_off.describe(&c("Mark")).unwrap();

    let mut k_on = Classifier::of(&doc.axioms);
    let _ = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    let d_on = k_on.describe(&c("Mark")).unwrap();

    let hub_in = NamedIndividual::new("urn:sparkles:internal:hub");
    assert_eq!(d_on.instances, d_off.instances);
    assert!(
        d_on.instances.contains(&hub_in),
        "input IRI urn:sparkles:internal:hub must remain a named key participant and public instance: {:?}",
        d_on.instances
    );
}

#[test]
fn operands_that_a_merge_makes_equal_keep_their_disjointness() {
    // Astra disjointness counterexample: both DisjointClasses and DisjointUnion.
    let cases = [
        "SubClassOf(ObjectOneOf(:a) ObjectOneOf(:b))
         DisjointClasses(ObjectHasValue(:r :a) ObjectHasValue(:r :b) :D)
         SubClassOf(:A ObjectHasValue(:r :a))",
        "SameIndividual(:a :b)
         DisjointClasses(ObjectHasValue(:r :a) ObjectHasValue(:r :b) :D)
         SubClassOf(:A ObjectHasValue(:r :a))",
        "SubClassOf(ObjectOneOf(:a) ObjectOneOf(:b))
         DisjointUnion(:U ObjectHasValue(:r :a) ObjectHasValue(:r :b) :D)
         SubClassOf(:A ObjectHasValue(:r :a))",
    ];
    for body in cases {
        let ax = axioms(body);
        for rooted_merge in [false, true] {
            let mut k = Classifier::of(&ax);
            let out = k
                .classify(&Limits {
                    threads: 1,
                    rooted: true,
                    rooted_merge,
                    ..Limits::default()
                })
                .unwrap();
            assert_eq!(out.consistency, Consistency::Consistent);
            assert!(
                out.taxonomy.is_unsatisfiable(&c("A")),
                "merge={rooted_merge}: :A must be unsatisfiable for body={body}"
            );
        }
    }
}

#[test]
fn multi_hop_and_multi_restart_merges() {
    // Deterministic staged 2-restart chain:
    // Pass 1 discovers a0 ≈ a1 (via C0). Pass 2 merges a0,a1 so C0(a0) + B(a1) meet on
    // the single constant a0, deriving C1(a0) -> a0 ≈ a2 (restart 2)!
    let ax = axioms(
        "SubClassOf(ObjectOneOf(:a0) :C0)
         SubClassOf(:C0 ObjectOneOf(:a1))
         ClassAssertion(:NeedA :a0)
         ClassAssertion(:NeedB :a1)
         SubClassOf(ObjectIntersectionOf(:NeedA :NeedB) ObjectOneOf(:a2))
         ClassAssertion(:Goal :a2)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Goal) :Ans)",
    );
    let mut k_off = Classifier::of(&ax);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut k_on = Classifier::of(&ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(on.complete && off.complete);
    assert_eq!(on.stats.dl_rooted_merged, 2);
    assert!(
        on.stats.dl_rooted_restarts >= 2,
        "expected >= 2 staged restarts, got {:?}",
        on.stats
    );
    assert!(on.taxonomy.is_subclass(&c("Probe"), &c("Ans")));
    assert!(off.taxonomy.is_subclass(&c("Probe"), &c("Ans")));
}

#[test]
fn disjunctive_and_conditional_equalities_are_never_merged() {
    let ax = axioms(
        "SubClassOf(owl:Thing ObjectOneOf(:a :b))
         SubClassOf(:P ObjectIntersectionOf(ObjectOneOf(:a) ObjectOneOf(:b)))
         ClassAssertion(:A :a)
         SubClassOf(:Q ObjectSomeValuesFrom(:r ObjectOneOf(:b)))
         SubClassOf(ObjectSomeValuesFrom(:r :A) :Hit)",
    );
    let mut k_off = Classifier::of(&ax);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut k_on = Classifier::of(&ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(on.complete && off.complete);
    assert_eq!(on.stats.dl_rooted_restarts, 0);
    assert_eq!(on.stats.dl_rooted_merged, 0);
    assert_eq!(
        on.taxonomy.is_subclass(&c("Q"), &c("Hit")),
        off.taxonomy.is_subclass(&c("Q"), &c("Hit"))
    );
    assert!(!on.taxonomy.is_subclass(&c("Q"), &c("Hit")));
}

#[test]
fn punning_keeps_class_and_property_names_separate() {
    let ax = axioms(
        "Declaration(Class(:x))
         Declaration(Class(:y))
         Declaration(NamedIndividual(:x))
         Declaration(NamedIndividual(:y))
         SameIndividual(:x :y)
         SubClassOf(:x :OnlyX)
         ClassAssertion(:Tag :x)",
    );
    let mut k_on = Classifier::of(&ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(on.complete);
    assert_eq!(on.stats.dl_rooted_merged, 1);
    assert!(on.taxonomy.is_subclass(&c("x"), &c("OnlyX")));
    assert!(!on.taxonomy.is_subclass(&c("y"), &c("OnlyX")));
    assert!(!on.taxonomy.is_subclass(&c("x"), &c("y")));
    assert!(!on.taxonomy.is_subclass(&c("y"), &c("x")));
}

#[test]
fn a_description_lists_every_merged_individual() {
    // Consistent case: SameIndividual + entailed equality + anonymous + universal hub.
    let consistent_ax = axioms(
        "SameIndividual(:a :b)
         SubClassOf(ObjectOneOf(:b) :Link)
         SubClassOf(:Link ObjectOneOf(:c))
         SameIndividual(:a _:anon0)
         SubClassOf(:Q ObjectSomeValuesFrom(owl:topObjectProperty :T))
         ClassAssertion(:T :a)",
    );
    let mut k_off = Classifier::of(&consistent_ax);
    let _ = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut k_on = Classifier::of(&consistent_ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(on.complete);
    let d_on = k_on.describe(&c("T")).unwrap();
    // :a ≈ :b ≈ :c ⇒ all three named individuals are instances of :T, and neither
    // _:anon0 nor urn:sparkles:internal:hub leaks into describe().
    assert_eq!(
        d_on.instances,
        vec![
            NamedIndividual::new(format!("{EX}a")),
            NamedIndividual::new(format!("{EX}b")),
            NamedIndividual::new(format!("{EX}c")),
        ]
    );
    assert!(
        k_off
            .describe(&c("T"))
            .unwrap()
            .instances
            .iter()
            .all(|i| d_on.instances.contains(i))
    );

    // Inconsistent case: describe must still list every original named individual (no
    // anonymous or internal hub names) and match merge OFF.
    let inconsistent_ax = axioms(
        "SameIndividual(:a :b)
         SameIndividual(:a _:anon0)
         DifferentIndividuals(:a :b)
         ClassAssertion(:T :c)",
    );
    let mut ki_off = Classifier::of(&inconsistent_ax);
    let _ = ki_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut ki_on = Classifier::of(&inconsistent_ax);
    let out_i = ki_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert_eq!(out_i.consistency, Consistency::Inconsistent);
    assert_eq!(
        ki_on.describe(&c("T")).unwrap().instances,
        ki_off.describe(&c("T")).unwrap().instances
    );
}

#[test]
fn the_hub_of_the_universal_property_merges_with_a_named_individual() {
    let ax = axioms(
        "SubClassOf(owl:Thing ObjectOneOf(:a))
         SubClassOf(:Q ObjectSomeValuesFrom(owl:topObjectProperty :Tag))
         ClassAssertion(:K :a)
         ClassAssertion(:K :b)
         ObjectPropertyAssertion(:r :a :a)
         ObjectPropertyAssertion(:r :b :a)
         HasKey(:K (:r) ())
         ClassAssertion(:Tag :a)",
    );
    let mut k_off = Classifier::of(&ax);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut k_on = Classifier::of(&ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(on.complete && off.complete);
    assert!(on.stats.dl_rooted_restarts >= 1);
    assert_eq!(on.consistency, off.consistency);
    let d_on = k_on.describe(&c("Tag")).unwrap();
    let d_off = k_off.describe(&c("Tag")).unwrap();
    assert_eq!(d_on.instances, d_off.instances);
    assert!(
        d_on.instances
            .iter()
            .all(|i| !i.as_str().starts_with("urn:sparkles:internal:"))
    );
}

#[test]
fn parallel_normalization_chunks_share_canon_map() {
    let mut body = String::from(
        "SameIndividual(:a0 :a1)
         ClassAssertion(:Seed :a1)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Seed) :Ans)\n",
    );
    for i in 0..1200usize {
        body.push_str(&format!("SubClassOf(:N{i} :M{i})\n"));
    }
    let ax = axioms(&body);
    let mut k1 = Classifier::of(&ax);
    let out1 = k1
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    let mut k4 = Classifier::of(&ax);
    let out4 = k4
        .classify(&Limits {
            threads: 4,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out1.complete && out4.complete);
    assert_eq!(out1.stats.dl_rooted_merged, 1);
    assert_eq!(out4.stats.dl_rooted_merged, 1);
    assert!(out1.taxonomy.is_subclass(&c("Probe"), &c("Ans")));
    assert!(out4.taxonomy.is_subclass(&c("Probe"), &c("Ans")));
    assert_eq!(
        k1.describe(&c("Seed")).unwrap().instances,
        k4.describe(&c("Seed")).unwrap().instances
    );
}

#[test]
fn a_restart_after_the_counting_fallback_keeps_counting_off_and_its_bound() {
    // Counting + rooted hits `dl_work` in the counting run (same AV2 + maxCardinality
    // counting trigger used above), falls back to `no_counting` rooted retry starting
    // from asserted partition only, and performs a derived constant-merge restart cleanly
    // within its actual rooted limit.
    let ax = axioms(&format!(
        "{AV2}
         SubClassOf(:K ObjectMaxCardinality(1 :q))
         SubClassOf(:C0 ObjectOneOf(:a2))
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)
         ClassAssertion(:Tag :a2)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)"
    ));
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            threads: 1,
            counting: true,
            rooted: true,
            rooted_decline: false,
            rooted_merge: true,
            dl_work: Some(380),
            ..Limits::default()
        })
        .unwrap();
    assert!(
        out.stats.dl_counting_over,
        "dl_work=380 must trigger counting fallback: {:?}",
        out.stats
    );
    assert!(
        !out.stats.dl_rooted_over,
        "no_counting retry must finish within rooted limit: {:?}",
        out.stats
    );
    assert!(out.complete_for_taken);
    assert!(
        out.stats.dl_rooted_restarts >= 1,
        "expected derived merge restart on no_counting retry: {:?}",
        out.stats
    );
    assert!(out.stats.dl_rooted_merged >= 1, "{:?}", out.stats);
    assert!(out.taxonomy.is_subclass(&c("Probe"), &c("Hit")));
}

#[test]
fn a_restart_on_several_threads_without_steps_is_repeated_on_one() {
    // Multi-thread `dl_steps = false` attempt that exhausts a tiny work bound retries on
    // 1 thread with a cleanly reset asserted-only Merge partition (`spent = 0`).
    let ax = axioms(
        "SameIndividual(:s0 :s1)
         SubClassOf(:C0 ObjectOneOf(:a2))
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)
         ClassAssertion(:Tag :a2)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            threads: 4,
            rooted: true,
            rooted_merge: true,
            dl_steps: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(out.complete);
    assert!(!out.stats.dl_rooted_over);
    assert_eq!(out.stats.dl_rooted_restarts, 1);
    assert_eq!(out.stats.dl_rooted_merged, 2);
    assert!(out.taxonomy.is_subclass(&c("Probe"), &c("Hit")));

    // With a tight bound below 1t work, both 4t and 1t retry exhaust cleanly and fall back
    // without stale merge state.
    let mut k_tight = Classifier::of(&ax);
    let out_tight = k_tight
        .classify(&Limits {
            threads: 4,
            rooted: true,
            rooted_merge: true,
            dl_steps: false,
            dl_work: Some(10),
            ..Limits::default()
        })
        .unwrap();
    assert!(out_tight.stats.dl_rooted_over);
    assert_eq!(out_tight.stats.dl_rooted_merged, 0);
}

#[test]
fn merge_after_decline_clears_canon_on_unrooted_fallback() {
    // Combines AV2 (triggers item 1231 decline when rooted_decline=true, repair=false)
    // with an entailed constant merge (restarts >= 1 before decline fires on the final
    // quotient). The unrooted fallback must clear canon and match merge OFF.
    let ax = axioms(&format!(
        "{AV2}
         SubClassOf(:M0 ObjectOneOf(:m2))
         SubClassOf(ObjectOneOf(:m0 :m2) :M0)
         ClassAssertion(:Tag :m0)"
    ));
    let mut k_off = Classifier::of(&ax);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_decline: true,
            rooted_marker_repair: false,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let mut k_on = Classifier::of(&ax);
    let on = k_on
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_decline: true,
            rooted_marker_repair: false,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(off.stats.dl_rooted_declined);
    assert!(on.stats.dl_rooted_declined);
    assert_eq!(
        on.stats.dl_rooted_merged, 0,
        "unrooted fallback after decline must clear canon"
    );
    assert_eq!(
        k_on.describe(&c("Tag")).unwrap().instances,
        k_off.describe(&c("Tag")).unwrap().instances
    );
}

#[test]
fn merge_with_marker_repair_and_shoi_022() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let doc = ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-022.ofn")).unwrap()).unwrap();
    let mut k = Classifier::of(&doc.axioms);
    let out = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_marker_repair: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out.complete);
    assert!(!out.stats.dl_rooted_over && !out.stats.dl_rooted_declined);
    assert_eq!(out.stats.dl_rooted_restarts, 1);
    assert_eq!(out.stats.dl_rooted_merged, 2);

    // AV2 + entailed equality with repair ON (and decline off, matching existing repair
    // tests): completes and entails K ⊑ Ans.
    let av2_merge = axioms(&format!(
        "{AV2}
         SubClassOf(:M0 ObjectOneOf(:m2))
         SubClassOf(ObjectOneOf(:m0 :m2) :M0)"
    ));
    let mut k2 = Classifier::of(&av2_merge);
    let out2 = k2
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_decline: false,
            rooted_merge: true,
            rooted_marker_repair: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out2.complete);
    assert!(!out2.stats.dl_rooted_declined);
    assert_eq!(out2.stats.dl_rooted_restarts, 1);
    assert!(out2.taxonomy.is_subclass(&c("K"), &c("Ans")));
}

#[test]
fn incremental_classify_after_merged_run_resets_cleanly() {
    // 1. Removing a class inclusion that supported a derived equality (without naming an
    //    individual!) must drop the derived merge on the next call.
    let bridge = Axiom::sub_class_of(c("Mid"), c("Gate"));
    let mut k = Classifier::of(&axioms(
        "SubClassOf(ObjectOneOf(:a0) :Mid)
         SubClassOf(:Gate ObjectOneOf(:a1))
         ClassAssertion(:Tag :a1)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)",
    ));
    k.add([&bridge]);
    let lim_on = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        ..Limits::default()
    };
    let r1 = k.classify(&lim_on).unwrap();
    assert_eq!(r1.stats.dl_rooted_restarts, 1);
    assert_eq!(r1.stats.dl_rooted_merged, 1);
    assert!(r1.taxonomy.is_subclass(&c("Probe"), &c("Hit")));

    // Remove only `Mid ⊑ Gate` (mentions NO individuals!).
    k.remove([&bridge]);
    let r2 = k.classify(&lim_on).unwrap();
    assert_eq!(r2.stats.dl_rooted_restarts, 0);
    assert_eq!(r2.stats.dl_rooted_merged, 0);
    assert!(!r2.taxonomy.is_subclass(&c("Probe"), &c("Hit")));

    // 2. Toggling `rooted_merge` OFF -> ON on a reused Classifier with empty canon on both
    //    sides must still force a fresh root and discover the equality.
    k.add([&bridge]);
    let lim_off = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: false,
        ..Limits::default()
    };
    let r3 = k.classify(&lim_off).unwrap();
    assert_eq!(r3.stats.dl_rooted_restarts, 0);
    assert!(r3.taxonomy.is_subclass(&c("Probe"), &c("Hit")));
    let r4 = k.classify(&lim_on).unwrap();
    assert_eq!(r4.stats.dl_rooted_restarts, 1);
    assert_eq!(r4.stats.dl_rooted_merged, 1);
    assert!(r4.taxonomy.is_subclass(&c("Probe"), &c("Hit")));

    // 3. Removing SameIndividual resets canon cleanly.
    let same = Axiom::new(AxiomKind::SameIndividual(
        [
            Individual::named(format!("{EX}x")),
            Individual::named(format!("{EX}y")),
        ]
        .into(),
    ));
    let mut k2 = Classifier::of(&axioms(
        "ClassAssertion(:Tag :x)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:y)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)",
    ));
    k2.add([&same]);
    let s1 = k2.classify(&lim_on).unwrap();
    assert_eq!(s1.stats.dl_rooted_merged, 1);
    assert!(s1.taxonomy.is_subclass(&c("Probe"), &c("Hit")));
    k2.remove([&same]);
    let s2 = k2.classify(&lim_on).unwrap();
    assert_eq!(s2.stats.dl_rooted_merged, 0);
    assert!(!s2.taxonomy.is_subclass(&c("Probe"), &c("Hit")));
}

#[test]
fn merge_stop_never_returns_partial_taxonomy() {
    let ax = axioms(
        "SubClassOf(:C0 ObjectOneOf(:a2))
         SubClassOf(ObjectOneOf(:a0 :a2) :C0)
         ClassAssertion(:Tag :a2)
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a0)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)",
    );
    let mut k = Classifier::of(&ax);
    let out = k
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            partial: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(
        out.complete,
        "merge stop must restart, never return partial"
    );
    assert!(out.complete_for_taken);
    assert_eq!(out.stats.dl_rooted_restarts, 1);
    assert!(out.taxonomy.is_subclass(&c("Probe"), &c("Hit")));
}

/// Sol P1 #1 / Neon B1 regression: starting with `SameIndividual(_:x _:y)` and deriving
/// `_:x ≈ :a` in the root must deduplicate `_:x` across asserted and harvested paths,
/// build a flat `CanonMap` (`_:x -> :a`, `_:y -> :a`), and finish with zero panic or
/// spurious fallback.
#[test]
fn asserted_anonymous_same_individual_followed_by_root_derived_named_merge() {
    let ax = axioms(
        "SameIndividual(_:x _:y)
         ClassAssertion(:Tag _:y)
         SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))
         SubClassOf(ObjectOneOf(_:x) :Bridge)
         SubClassOf(:Bridge ObjectOneOf(:a))
         SubClassOf(:Probe ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
         SubClassOf(ObjectSomeValuesFrom(:r :Tag) :Hit)",
    );
    let mut k_off = Classifier::of(&ax);
    let off = k_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    let d_off = k_off.describe(&c("Tag")).unwrap();

    for (threads, expansion) in CONFIGS {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                expansion,
                rooted: true,
                rooted_merge: true,
                ..Limits::default()
            })
            .unwrap();
        assert!(out.complete, "{threads}t {expansion:?}");
        assert!(!out.stats.dl_over_budget && !out.stats.dl_rooted_over);
        assert_eq!(out.consistency, off.consistency);
        assert_eq!(out.stats.dl_rooted_restarts, 1);
        assert_eq!(out.stats.dl_rooted_merged, 2);
        assert!(
            out.taxonomy.is_subclass(&c("Probe"), &c("Hit")),
            "{threads}t {expansion:?}: _:y assertion must reach :a via flat CanonMap"
        );
        assert_eq!(
            out.taxonomy.is_subclass(&c("Probe"), &c("Hit")),
            off.taxonomy.is_subclass(&c("Probe"), &c("Hit"))
        );
        let d_on = k.describe(&c("Tag")).unwrap();
        assert_eq!(d_on.instances, vec![NamedIndividual::new(format!("{EX}a"))]);
        assert!(
            d_off.instances.iter().all(|i| d_on.instances.contains(i)),
            "merge ON must retain every public instance from merge OFF"
        );
        assert!(
            d_on.instances.iter().all(|i| !i.as_str().starts_with("_:")),
            "anonymous individual leaked into describe(): {:?}",
            d_on.instances
        );
    }
}

/// Sol P1 #2 / Neon B2 regression: if post-merge `dl_reduce::plan` declines on Pass 1
/// (after Pass 0 reduced and derived aliases / hub merge), derived aliases and any
/// generated hub must be cleared cleanly before Horn fallback runs on the original axioms,
/// matching a fresh run on the original axioms and never leaking `urn:sparkles:internal:hub`.
#[test]
fn post_merge_reduction_decline_clears_derived_aliases_and_never_leaks_hub() {
    // Part A (Sol P1 #2): Anonymous witness equality `_:w0 ≈ _:w1` derived ONLY from a
    // disjunctive axiom beside `HasKey` over 8 long-IRI named individuals `:k00..:k07`.
    // Merging the two 256 KiB anonymous witnesses leaves `n = 8` unchanged while adding
    // `UF + Canon` (~1 MiB) to Pass 1's `plan_budgeted` base (`H + UF + Canon + R`),
    // strictly exceeding both Pass 0 (`H + R + I`) and restart `build_canon`
    // (`H + UF + Canon`). Choosing `cap = off_peak + 128 KiB` guarantees Pass 0 succeeds
    // and merges, `build_canon` succeeds, Pass 1's `plan_budgeted` declines, and Horn
    // fallback runs cleanly without inheriting `_:w0 ≈ _:w1` (`:Probe ⊑ :OnlyFromDisj`).
    let w0 = format!("w0_{}", "a".repeat(256 * 1024));
    let w1 = format!("w1_{}", "b".repeat(256 * 1024));
    let mut body_a = format!(
        "HasKey(:K (:r) ())
         SubClassOf(ObjectOneOf(_:{w0}) ObjectUnionOf(:P :Q))
         SubClassOf(:P ObjectOneOf(_:{w1}))
         SubClassOf(:Q ObjectOneOf(_:{w1}))
         ClassAssertion(:Flag _:{w1})
         SubClassOf(:Probe ObjectSomeValuesFrom(:s ObjectOneOf(_:{w0})))
         SubClassOf(ObjectSomeValuesFrom(:s :Flag) :OnlyFromDisj)\n"
    );
    for i in 0..8usize {
        body_a.push_str(&format!(
            "ClassAssertion(:K :k{i:02}_{})\n",
            "k".repeat(2048)
        ));
    }
    let ax_a = axioms(&body_a);
    let mut fresh_a = Classifier::of(&ax_a);
    let expected_a = fresh_a
        .classify(&Limits {
            threads: 1,
            rooted: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(!fresh_a.excluded().is_empty());
    assert!(
        !expected_a
            .taxonomy
            .is_subclass(&c("Probe"), &c("OnlyFromDisj")),
        "fresh Horn run must not derive disjunctive witness equality"
    );

    // Unrestricted merged run confirms Pass 0 reduces, derives `_:w0 ≈ _:w1`, and restarts.
    let mut unres_a = Classifier::of(&ax_a);
    let base_a = unres_a
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(base_a.complete);
    assert_eq!(base_a.stats.dl_rooted_restarts, 1);
    assert_eq!(base_a.stats.dl_rooted_merged, 1);
    assert!(base_a.taxonomy.is_subclass(&c("Probe"), &c("OnlyFromDisj")));

    // Measure Pass 0 peak (`rooted_merge: false`, no `SameIndividual` axioms so Pass 0
    // sees the identical `reduce_mem` and `plan_base`), then verify `cap_a` strictly
    // allows Pass 0 to complete in full while declining Pass 1 after the merge restart.
    let mut off_unres_a = Classifier::of(&ax_a);
    let off_base_a = off_unres_a
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(off_base_a.complete);
    let cap_a = off_base_a.stats.peak_memory + (128 * 1024);
    assert!(
        cap_a < base_a.stats.peak_memory,
        "expected cap_a ({cap_a}) below Pass 1 replan peak ({})",
        base_a.stats.peak_memory
    );

    let mut k_a_off = Classifier::of(&ax_a);
    let off_capped_a = k_a_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            max_memory: Some(cap_a),
            ..Limits::default()
        })
        .unwrap();
    assert!(
        off_capped_a.complete,
        "Pass 0 reduction and saturation must succeed under cap_a ({cap_a}, off peak={})",
        off_capped_a.stats.peak_memory
    );
    assert!(
        off_capped_a
            .taxonomy
            .is_subclass(&c("Probe"), &c("OnlyFromDisj")),
        "Pass 0 without merge must still derive disjunctive witness equality"
    );

    let mut k_a = Classifier::of(&ax_a);
    let got_a = k_a
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            max_memory: Some(cap_a),
            ..Limits::default()
        })
        .unwrap();
    assert!(!got_a.complete, "Pass 1 reduction must decline into Horn");
    assert!(
        got_a.stats.peak_memory > expected_a.stats.peak_memory,
        "Pass 0 must have reduced and saturated before Pass 1 declined (got peak {} vs fresh Horn {})",
        got_a.stats.peak_memory,
        expected_a.stats.peak_memory
    );
    assert_eq!(
        got_a.stats.dl_rooted_merged, 0,
        "declined Pass 1 must clear derived aliases"
    );
    assert_eq!(got_a.consistency, expected_a.consistency);
    assert_eq!(k_a.excluded(), fresh_a.excluded());
    assert!(
        !got_a.taxonomy.is_subclass(&c("Probe"), &c("OnlyFromDisj")),
        "Horn fallback after Pass 1 reduction decline must NOT inherit derived disjunctive alias"
    );
    assert_eq!(
        k_a.describe(&c("Flag")).unwrap().instances,
        fresh_a.describe(&c("Flag")).unwrap().instances
    );

    // Part B (Neon B2): Pass 0 synthesizes `urn:sparkles:internal:hub` via
    // `owl:topObjectProperty` (and retargets a wide long-IRI `EquivalentObjectProperties`
    // axiom) and merges `hub -> :a` alongside long anonymous witness merge `_:w0 ≈ _:w1`.
    // On restart, `UF + Canon + retargeted roles` exceeds `cap_b = off_peak + 256 KiB`,
    // so Pass 1 reduction declines under `max_memory`. Verify Pass 0 succeeds under
    // `cap_b` (`rooted_merge: false`), Pass 1 declines cleanly into Horn, and `describe()`
    // NEVER leaks `urn:sparkles:internal:hub` and matches a fresh run.
    let w0_b = format!("w0b_{}", "a".repeat(512 * 1024));
    let w1_b = format!("w1b_{}", "b".repeat(512 * 1024));
    let mut body_b = format!(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))
         SubClassOf(ObjectOneOf(_:{w0_b}) :Bridge)
         SubClassOf(:Bridge ObjectOneOf(_:{w1_b}))
         SubClassOf(:Q ObjectSomeValuesFrom(owl:topObjectProperty :Tag))
         SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :Seed) ObjectOneOf(:a))
         ClassAssertion(:Seed :a)
         ClassAssertion(:Tag :a)
         EquivalentObjectProperties(owl:topObjectProperty"
    );
    for i in 0..100usize {
        body_b.push_str(&format!(" :hp{i:03}_{}", "p".repeat(8192)));
    }
    body_b.push_str(")\n");
    let ax_b = axioms(&body_b);
    let mut fresh_b = Classifier::of(&ax_b);
    let expected_b = fresh_b
        .classify(&Limits {
            threads: 1,
            rooted: false,
            ..Limits::default()
        })
        .unwrap();
    let fresh_desc_b = fresh_b.describe(&c("Tag")).unwrap();

    // Unrestricted merged baseline proves Pass 0 merges and triggers a restart.
    let mut unres_b = Classifier::of(&ax_b);
    let base_b = unres_b
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(base_b.complete);
    assert!(
        base_b.stats.dl_rooted_restarts >= 1,
        "Part B unrestricted run must perform at least one merge restart"
    );

    let mut off_unres_b = Classifier::of(&ax_b);
    let off_base_b = off_unres_b
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            ..Limits::default()
        })
        .unwrap();
    assert!(off_base_b.complete);
    let cap_b = off_base_b.stats.peak_memory + (256 * 1024);
    assert!(
        cap_b < base_b.stats.peak_memory,
        "expected cap_b ({cap_b}) below Pass 1 replan peak ({})",
        base_b.stats.peak_memory
    );

    let mut k_b_off = Classifier::of(&ax_b);
    let off_capped_b = k_b_off
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: false,
            max_memory: Some(cap_b),
            ..Limits::default()
        })
        .unwrap();
    assert!(
        off_capped_b.complete,
        "Part B Pass 0 reduction and saturation must succeed under cap_b ({cap_b}, off peak={})",
        off_capped_b.stats.peak_memory
    );

    let mut k_b = Classifier::of(&ax_b);
    let got_b = k_b
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            max_memory: Some(cap_b),
            ..Limits::default()
        })
        .unwrap();
    assert!(!got_b.complete, "Part B Pass 1 reduction must decline");
    assert!(
        got_b.stats.peak_memory > expected_b.stats.peak_memory,
        "Part B Pass 0 must have reduced and saturated before Pass 1 declined (got peak {} vs fresh Horn {})",
        got_b.stats.peak_memory,
        expected_b.stats.peak_memory
    );
    assert_eq!(got_b.stats.dl_rooted_merged, 0);
    assert_eq!(got_b.consistency, expected_b.consistency);
    assert_eq!(k_b.excluded(), fresh_b.excluded());
    let d_b = k_b.describe(&c("Tag")).unwrap();
    assert!(
        d_b.instances
            .iter()
            .all(|i| !i.as_str().starts_with("urn:sparkles:internal:")),
        "generated hub leaked into describe(): {:?}",
        d_b.instances
    );
    assert_eq!(d_b.instances, fresh_desc_b.instances);
}

/// Design v12 §4.1: Primary `shoi-042` acceptance gate across all 4 `(threads, expansion)`
/// configs × `dl_steps ∈ {true, false}` × `rooted_merge ∈ {true, false}` + cross-lane
/// controls + Split OFF control + exact HermiT taxonomy and independent `describe(&C)`
/// instance expectations.
#[test]
fn a_rooted_run_that_splits_disjunctive_equalities_finishes_shoi_042() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let doc = ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap()).unwrap();
    let hermit_text = std::fs::read_to_string(dir.join("shoi-042.hermit.ofn")).unwrap();
    let mut classes: std::collections::BTreeSet<String> = doc
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::Class(cls)) => Some(cls.as_str().to_string()),
            _ => None,
        })
        .collect();
    classes.insert(vocab::OWL_THING.to_string());
    classes.insert(vocab::OWL_NOTHING.to_string());
    let want_tax = parse_hermit_subsumptions(&hermit_text, &classes);

    let d = |s: &str| Class::new(format!("http://example.org/d#{s}"));
    let ind = |s: &str| NamedIndividual::new(format!("http://example.org/d#{s}"));
    // Exact semantic instances of shoi-042.ofn (proven by M1: a1=a2, C0={a2}, C3={a3}
    // and M2: a0=a1=a3, C0=C3={a3}):
    let expected_instances: Vec<(Class, Vec<NamedIndividual>)> = vec![
        (d("C0"), vec![ind("a1")]),
        (d("C1"), vec![ind("a1")]),
        (d("C5"), vec![ind("a1")]),
        (d("C2"), vec![]),
        (d("C3"), vec![ind("a3")]),
        (d("C4"), vec![]),
        (
            Class::thing(),
            vec![ind("a0"), ind("a1"), ind("a2"), ind("a3")],
        ),
        (Class::nothing(), vec![]),
    ];

    // 1. Control: Split OFF still overflows the rooted work bound on shoi-042.ofn for both
    //    `rooted_merge = false` and `rooted_merge = true`.
    for rooted_merge in [false, true] {
        let mut k_off = Classifier::of(&doc.axioms);
        let off = k_off
            .classify(&Limits {
                threads: 1,
                rooted: true,
                rooted_merge,
                rooted_eq_split: false,
                ..Limits::default()
            })
            .unwrap();
        assert!(
            off.stats.dl_rooted_over,
            "split OFF (merge={rooted_merge}) control must still exceed rooted bound on shoi-042"
        );
        assert_eq!(off.stats.dl_rooted_splits, 0);
        assert_eq!(off.stats.dl_rooted_leaves, 0);
    }

    // 2. Split ON finishes shoi-042 across all 4 configs × dl_steps {true, false} ×
    //    rooted_merge {true, false} with exact HermiT taxonomy AND exact describe() instances.
    for rooted_merge in [true, false] {
        let mut baseline_splits = None;
        let mut baseline_leaves = None;
        for dl_steps in [true, false] {
            for (threads, expansion) in CONFIGS {
                let at = format!(
                    "shoi-042 merge={rooted_merge} dl_steps={dl_steps} {threads}t {expansion:?}"
                );
                let mut k = Classifier::of(&doc.axioms);
                let out = k
                    .classify(&Limits {
                        threads,
                        expansion,
                        rooted: true,
                        rooted_merge,
                        rooted_eq_split: true,
                        dl_steps,
                        ..Limits::default()
                    })
                    .unwrap();
                assert!(out.complete, "{at}: expected complete: {:?}", out.stats);
                assert!(
                    !out.stats.dl_rooted_over && !out.stats.dl_rooted_declined,
                    "{at}: unexpected fallback: {:?}",
                    out.stats
                );
                assert!(k.excluded().is_empty(), "{at}: {:?}", k.excluded());
                assert_eq!(out.consistency, Consistency::Consistent, "{at}");
                assert!(
                    out.stats.dl_rooted_splits >= 1,
                    "{at}: expected real fork: {:?}",
                    out.stats
                );
                assert!(
                    out.stats.dl_rooted_leaves >= 2,
                    "{at}: expected >= 2 leaves: {:?}",
                    out.stats
                );
                if dl_steps {
                    if let Some(s) = baseline_splits {
                        assert_eq!(
                            out.stats.dl_rooted_splits, s,
                            "{at}: stepped split count mismatch"
                        );
                    } else {
                        baseline_splits = Some(out.stats.dl_rooted_splits);
                    }
                    if let Some(l) = baseline_leaves {
                        assert_eq!(
                            out.stats.dl_rooted_leaves, l,
                            "{at}: stepped leaf count mismatch"
                        );
                    } else {
                        baseline_leaves = Some(out.stats.dl_rooted_leaves);
                    }
                }
                assert!(
                    out.stats.dl_rooted_spent + out.stats.dl_work <= (1 << 19),
                    "{at}: total work exceeded bound: {:?}",
                    out.stats
                );
                let got_tax = taxonomy_subsumptions(&out, &classes);
                assert_eq!(got_tax, want_tax, "{at}: taxonomy differs from HermiT");
                for (cls, want_insts) in &expected_instances {
                    let desc = k
                        .describe(cls)
                        .unwrap_or_else(|| panic!("{at}: describe({cls:?}) returned None"));
                    assert_eq!(
                        desc.instances, *want_insts,
                        "{at}: instances mismatch for {cls:?}"
                    );
                }
                assert!(
                    k.describe(&Class::new("http://example.org/d#UnknownClass"))
                        .is_none(),
                    "{at}: unknown class must describe as None"
                );
            }
        }
    }

    // 3. Cross-lane control: marker repair ON on shoi-042 completes with exact taxonomy.
    let mut k_rep = Classifier::of(&doc.axioms);
    let out_rep = k_rep
        .classify(&Limits {
            threads: 1,
            rooted: true,
            rooted_merge: true,
            rooted_eq_split: true,
            rooted_marker_repair: true,
            ..Limits::default()
        })
        .unwrap();
    assert!(out_rep.complete);
    assert!(!out_rep.stats.dl_rooted_over && !out_rep.stats.dl_rooted_declined);
    assert_eq!(taxonomy_subsumptions(&out_rep, &classes), want_tax);
}

/// Design v12 §4.2: Full 50/50 `shoi` HermiT oracle parity with `rooted_eq_split = true`
/// (and `rooted_merge = true`) in `(1, Cautious)` and `(4, Eager)`: all 50/50 complete
/// within the 524,288 bound with 0 taxonomy differences against HermiT!
#[test]
fn all_50_shoi_fixtures_complete_within_bound_and_match_hermit_with_rooted_eq_split() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let mut split_fixtures = 0usize;
    for i in 0..50 {
        let name = format!("shoi-{i:03}");
        let doc =
            ofn_read::parse(&std::fs::read_to_string(dir.join(format!("{name}.ofn"))).unwrap())
                .unwrap();
        let hermit_text = std::fs::read_to_string(dir.join(format!("{name}.hermit.ofn"))).unwrap();
        let mut classes: std::collections::BTreeSet<String> = doc
            .axioms
            .iter()
            .filter_map(|a| match &a.kind {
                AxiomKind::Declaration(Entity::Class(cls)) => Some(cls.as_str().to_string()),
                _ => None,
            })
            .collect();
        classes.insert(vocab::OWL_THING.to_string());
        classes.insert(vocab::OWL_NOTHING.to_string());
        let want = parse_hermit_subsumptions(&hermit_text, &classes);

        for (threads, expansion) in [(1, Expansion::Cautious), (4, Expansion::Eager)] {
            let at = format!("{name} {threads}t {expansion:?}");
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    rooted_merge: true,
                    rooted_eq_split: true,
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                out.complete && !out.stats.dl_rooted_over && !out.stats.dl_rooted_declined,
                "{at}: did not complete cleanly: {:?}",
                out.stats
            );
            assert!(k.excluded().is_empty(), "{at}: {:?}", k.excluded());
            assert!(
                out.stats.dl_rooted_spent + out.stats.dl_work <= (1 << 19),
                "{at}: exceeded 524,288 bound: {:?}",
                out.stats
            );
            if threads == 1 && out.stats.dl_rooted_splits > 0 {
                split_fixtures += 1;
            }
            let got = taxonomy_subsumptions(&out, &classes);
            assert_eq!(got, want, "{at}: taxonomy differs from HermiT");
        }
    }
    assert!(
        split_fixtures >= 1,
        "at least shoi-042 must have forked during the 50-fixture sweep"
    );
}

/// Design v12 §4.3.1: Completeness controls E1 (+ negative), E2, and E3 (+ negative) with
/// `rooted_eq_split` BOTH `false` AND `true` across all 4 `(threads, expansion)` configs.
#[test]
fn completeness_controls_e1_e2_e3_with_split_off_and_on() {
    // E1: Disjunctive equality `a = b ∨ a = c` carried into `x`-clause (`A ⊑ ∃r.{a}`,
    // `B(b)`, `B(c)`, `∃r.B ⊑ D`) => `A ⊑ D`.
    let e1_pos = "ClassAssertion(ObjectOneOf(:b :c) :a)
                  ClassAssertion(:B :b)
                  ClassAssertion(:B :c)
                  SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                  SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    let e1_neg = "ClassAssertion(ObjectOneOf(:b :c) :a)
                  ClassAssertion(:B :b)
                  SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
                  SubClassOf(ObjectSomeValuesFrom(:r :B) :D)";
    // E2: Disjunctive equality `a = b ∨ a = c` with universal restriction at a predecessor:
    // `A ⊑ ∃r.{a} ⊓ ∀r.P`, `¬P(b)`, `¬P(c)` => `A ⊑ ⊥` (ontology stays consistent!).
    let e2 = "ClassAssertion(ObjectOneOf(:b :c) :a)
              ClassAssertion(ObjectComplementOf(:P) :b)
              ClassAssertion(ObjectComplementOf(:P) :c)
              SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:a)))
              SubClassOf(:A ObjectAllValuesFrom(:r :P))";
    // E3: Disjunctive equality `a = b ∨ a = c` interacts with `x ≈ b` / `x = c` across
    // branches (`A ⊑ {a}`, `B ⊑ {b}`, `C ⊑ {c}`, `B ⊑ D`, `C ⊑ D` => `A ⊑ D`; negative
    // drops `C ⊑ D`).
    let e3_pos = "ClassAssertion(ObjectOneOf(:b :c) :a)
                  SubClassOf(:A ObjectOneOf(:a))
                  SubClassOf(ObjectOneOf(:b) :D)
                  SubClassOf(ObjectOneOf(:c) :D)";
    let e3_neg = "ClassAssertion(ObjectOneOf(:b :c) :a)
                  SubClassOf(:A ObjectOneOf(:a))
                  SubClassOf(ObjectOneOf(:b) :D)";

    for rooted_eq_split in [false, true] {
        for (threads, expansion) in CONFIGS {
            let lim = Limits {
                threads,
                expansion,
                rooted: true,
                rooted_merge: true,
                rooted_eq_split,
                ..Limits::default()
            };
            let run_case = |body: &str| {
                let ax = axioms(body);
                let mut k = Classifier::of(&ax);
                let out = k.classify(&lim).unwrap();
                assert!(out.complete);
                (k, out)
            };

            let (_, r1p) = run_case(e1_pos);
            assert_eq!(r1p.consistency, Consistency::Consistent);
            assert!(r1p.taxonomy.is_subclass(&c("A"), &c("D")));
            assert!(!r1p.taxonomy.is_unsatisfiable(&c("A")));

            let (_, r1n) = run_case(e1_neg);
            assert_eq!(r1n.consistency, Consistency::Consistent);
            assert!(!r1n.taxonomy.is_subclass(&c("A"), &c("D")));

            let (_, r2) = run_case(e2);
            assert_eq!(r2.consistency, Consistency::Consistent);
            assert!(r2.taxonomy.is_unsatisfiable(&c("A")));

            let (_, r3p) = run_case(e3_pos);
            assert_eq!(r3p.consistency, Consistency::Consistent);
            assert!(r3p.taxonomy.is_subclass(&c("A"), &c("D")));

            let (_, r3n) = run_case(e3_neg);
            assert_eq!(r3n.consistency, Consistency::Consistent);
            assert!(!r3n.taxonomy.is_subclass(&c("A"), &c("D")));
        }
    }
}

/// Design v12 §4.3.3: Hidden-only nominal inconsistency, hidden-intermediate closure paths,
/// declared-only named individuals, class unsat in leaf 1 & sat in leaf 2, `0`/`1`/`>=2`
/// consistent leaves, `A < B < C` vs `A < C`, `owl:Thing`/`owl:Nothing`/unknown class, and
/// named aliases across forced forks (triggered with `SPARKLES_ROOTED_EQ_SPLIT_PROBE=0`?
/// No — we test via public `Limits` on ontologies that naturally fork OR via a test helper;
/// here we combine a controlled `shoi-042`-style disjunctive equality gadget so splitting
/// genuinely fires under default probe settings OR test probe=0 cleanly without env races).
#[test]
fn split_export_edge_cases_hidden_nominals_declared_individuals_and_leaves() {
    // Helper: add a small disjunctive-equality + work gadget that makes `rooted_eq_split`
    // actually fork on `shoi-042.ofn` merged with extra axioms in a separate namespace!
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let shoi042 = ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
        .unwrap()
        .axioms;
    let with_fork = |extra: &str| {
        let mut ax: Vec<Axiom> = shoi042
            .iter()
            .filter(|a| !matches!(a.kind, AxiomKind::Declaration(Entity::DataProperty(_))))
            .cloned()
            .collect();
        ax.extend(axioms(extra));
        ax
    };
    let lim_split = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };

    // 1. Declared-only named individual `:ghost` + top-subsumed class `:Every`:
    //    `SubClassOf(owl:Thing :Every)` => `:ghost` MUST appear in `describe(:Every)` and
    //    `describe(owl:Thing)` even though `:ghost` only appears in `Declaration(...)`!
    {
        let ax = with_fork(
            "Declaration(NamedIndividual(:ghost))
             SubClassOf(owl:Thing :Every)
             Declaration(Class(:Unmet))",
        );
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                rooted_decline: false,
                rooted_marker_repair: true,
                ..lim_split.clone()
            })
            .unwrap();
        assert!(
            out.complete,
            "Part 1 declared-only+fork must complete: stats={:?} excluded={:?}",
            out.stats,
            k.excluded()
        );
        assert!(out.stats.dl_rooted_splits >= 1);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(out.taxonomy.is_subclass(&Class::thing(), &c("Every")));
        assert!(out.taxonomy.is_subclass(&c("Unmet"), &c("Every")));
        let ghost = NamedIndividual::new(format!("{EX}ghost"));
        assert!(k.describe(&c("Every")).unwrap().instances.contains(&ghost));
        assert!(
            k.describe(&Class::thing())
                .unwrap()
                .instances
                .contains(&ghost)
        );
        assert!(k.describe(&c("Unmet")).unwrap().instances.is_empty());
    }

    // 2. Hidden-only nominal inconsistency (anonymous witness `_:w` forced into an
    //    unsatisfiable class inside leaf `a1 == a2` while consistent in leaf `a1 != a2`),
    //    AND branch-specific all-inconsistent ontology (0 consistent leaves AFTER a real
    //    fork!) where declared-only `:ghost` belongs to every class (including `owl:Nothing`).
    {
        let ax_one_incons = with_fork(
            "ClassAssertion(:Hide _:w)
             ObjectPropertyAssertion(:rHide _:w <http://example.org/d#a1>)
             SubClassOf(ObjectHasValue(:rHide <http://example.org/d#a2>) ObjectComplementOf(:Hide))",
        );
        let mut k = Classifier::of(&ax_one_incons);
        let out = k.classify(&lim_split).unwrap();
        assert!(out.complete);
        assert!(out.stats.dl_rooted_splits >= 1);
        // Leaf `a1 == a2` is inconsistent because anonymous `_:w` is both `Hide` and `¬Hide`,
        // while `a1 != a2` leaves remain consistent!
        assert_eq!(out.consistency, Consistency::Consistent);
    }
    {
        // All-inconsistent ontology with a real fork (`dl_rooted_splits >= 1`, 0 consistent
        // leaves): put the existential contradiction axioms BEFORE `shoi-042` so `shoi-042`'s
        // higher-Ex non-root contexts are activated first and cross SPLIT_PROBE (16,384)
        // before `:Trap` resolves its contradiction; depth 0 forks and every terminal leaf
        // is proved inconsistent, and the resulting `SplitAnswer` (`build_inconsistent`)
        // lists declared-only `:ghost` in `describe(:Boom)` and `describe(owl:Nothing)`.
        let mut ax_all_incons = axioms(
            "Declaration(NamedIndividual(:ghost))
             Declaration(Class(:Boom))
             ClassAssertion(ObjectSomeValuesFrom(:rTrap1 :Trap1) <http://example.org/d#a0>)
             SubClassOf(:Trap1 ObjectSomeValuesFrom(:rTrap2 :Trap2))
             SubClassOf(:Trap2 ObjectSomeValuesFrom(:rTrap3 :Trap3))
             SubClassOf(:Trap3 :ContraA)
             SubClassOf(:Trap3 :ContraB)
             SubClassOf(:ContraA ObjectComplementOf(:ContraB))",
        );
        ax_all_incons.extend(
            shoi042
                .iter()
                .filter(|a| !matches!(a.kind, AxiomKind::Declaration(Entity::DataProperty(_))))
                .cloned(),
        );
        let mut k = Classifier::of(&ax_all_incons);
        let out = k.classify(&lim_split).unwrap();
        assert!(out.complete);
        assert_eq!(out.consistency, Consistency::Inconsistent);
        assert!(
            out.stats.dl_rooted_splits >= 1 && out.stats.dl_rooted_leaves >= 2,
            "all-inconsistent fixture must exercise a real fork with 0 consistent leaves: {:?}",
            out.stats
        );
        assert!(out.taxonomy.is_unsatisfiable(&c("Boom")));
        let ghost = NamedIndividual::new(format!("{EX}ghost"));
        assert!(
            k.describe(&c("Boom")).unwrap().instances.contains(&ghost),
            "declared-only :ghost must appear in describe(:Boom) on inconsistent ontology"
        );
        assert!(
            k.describe(&Class::nothing())
                .unwrap()
                .instances
                .contains(&ghost),
            "declared-only :ghost must appear in describe(owl:Nothing) on inconsistent ontology"
        );
    }

    // 3. Class unsat in leaf 1 (`a1 == a2`) and sat in leaf 2 (`a1 != a2`):
    //    `:Half` is satisfiable overall (`Half ⊑ Target` holds in all consistent leaves),
    //    and has ZERO instances.
    {
        let ax = with_fork(
            "ClassAssertion(:MarkA2 <http://example.org/d#a2>)
             SubClassOf(:Half ObjectSomeValuesFrom(:rHalf ObjectIntersectionOf(ObjectOneOf(<http://example.org/d#a1>) ObjectComplementOf(:MarkA2))))
             SubClassOf(:Half :Target)",
        );
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                rooted_decline: false,
                rooted_marker_repair: true,
                ..lim_split.clone()
            })
            .unwrap();
        assert!(
            out.complete,
            "Part 3 half-unsat+fork must complete: stats={:?} excluded={:?}",
            out.stats,
            k.excluded()
        );
        assert!(out.stats.dl_rooted_splits >= 1);
        assert_eq!(out.consistency, Consistency::Consistent);
        assert!(
            !out.taxonomy.is_unsatisfiable(&c("Half")),
            "class satisfiable in a1 != a2 leaves must stay satisfiable overall"
        );
        assert!(out.taxonomy.is_subclass(&c("Half"), &c("Target")));
        assert!(k.describe(&c("Half")).unwrap().instances.is_empty());
    }

    // 4. Hidden-intermediate closure path (`:Src ⊑ K_internal ⊑ :Dst` via `HasKey` reduction)
    //    beside an actual fork!
    {
        let ax = with_fork(
            "HasKey(:Src (:hk) ())
             SubClassOf(:Src :Mid)
             SubClassOf(:Mid :Dst)
             ClassAssertion(:Src :p0)
             ClassAssertion(:Src :p1)
             ObjectPropertyAssertion(:hk :p0 :v0)
             ObjectPropertyAssertion(:hk :p1 :v0)
             ClassAssertion(:Tag :p0)",
        );
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                rooted_decline: false,
                rooted_marker_repair: true,
                ..lim_split.clone()
            })
            .unwrap();
        assert!(
            out.complete,
            "Part 4 HasKey+fork must complete: stats={:?} excluded={:?}",
            out.stats,
            k.excluded()
        );
        assert!(out.stats.dl_rooted_splits >= 1);
        assert!(out.taxonomy.is_subclass(&c("Src"), &c("Dst")));
        let tag_insts = k.describe(&c("Tag")).unwrap().instances;
        assert!(tag_insts.contains(&NamedIndividual::new(format!("{EX}p0"))));
        assert!(tag_insts.contains(&NamedIndividual::new(format!("{EX}p1"))));
        no_internal_names(&k, &out, &[c("Src"), c("Mid"), c("Dst"), c("Tag")]);
    }
}

/// Design v12 §4.3.3 & §4.3.4: Strict `Error::Incomplete` invalidation on BOTH
/// `Classifier::classify` and `Saturation::classify`, switch-off incremental reuse,
/// `partial = true` / `saturate()` bypass, and inclusive work bound `Some(total)` vs
/// `Some(total - 1)`.
#[test]
fn split_lifecycle_strict_error_invalidation_incremental_reuse_and_inclusive_bound() {
    use sparkles_owl::classify::{Error, Saturation};

    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let shoi042 = ofn_read::parse(&std::fs::read_to_string(dir.join("shoi-042.ofn")).unwrap())
        .unwrap()
        .axioms;
    let c0 = Class::new("http://example.org/d#C0");
    let lim_split = Limits {
        threads: 1,
        rooted: true,
        rooted_merge: true,
        rooted_eq_split: true,
        ..Limits::default()
    };

    // 1. Strict `Error::Incomplete` invalidation on BOTH `Classifier::classify` AND
    //    `Saturation::classify`: after a successful forked classify (`describe` works!),
    //    a strict call that returns `Err(Error::Incomplete(_))` MUST leave `describe == None`!
    {
        let mut k = Classifier::of(&shoi042);
        let ok = k.classify(&lim_split).unwrap();
        assert!(ok.complete && ok.stats.dl_rooted_splits >= 1);
        assert!(k.describe(&c0).is_some());

        // Add an unsupported axiom (`IrreflexiveObjectProperty` beside nominal) and call
        // with `strict: true` -> returns `Err(Error::Incomplete(_))` and invalidates prior
        // `SplitAnswer` immediately!
        let irref = Axiom::new(AxiomKind::IrreflexiveObjectProperty(
            ObjectProperty::new("http://example.org/d#r0").into(),
        ));
        k.add([&irref]);
        let err = k
            .classify(&Limits {
                strict: true,
                ..lim_split.clone()
            })
            .unwrap_err();
        assert!(matches!(err, Error::Incomplete(_)), "got {err:?}");
        assert!(
            k.describe(&c0).is_none(),
            "Classifier::describe must return None immediately after Err(Error::Incomplete)"
        );
        // Remove the unsupported axiom -> subsequent classify recovers cleanly.
        k.remove([&irref]);
        let recovered = k.classify(&lim_split).unwrap();
        assert!(recovered.complete);
        assert!(k.describe(&c0).is_some());
    }
    {
        let refs: Vec<&AxiomKind> = shoi042.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        let ok = sat.classify(&refs, None, &lim_split).unwrap();
        assert!(ok.complete && ok.stats.dl_rooted_splits >= 1);
        assert!(sat.describe(&c0).is_some());

        let irref = AxiomKind::IrreflexiveObjectProperty(
            ObjectProperty::new("http://example.org/d#r0").into(),
        );
        let mut refs2 = refs.clone();
        refs2.push(&irref);
        let err = sat
            .classify(
                &refs2,
                None,
                &Limits {
                    strict: true,
                    ..lim_split.clone()
                },
            )
            .unwrap_err();
        assert!(matches!(err, Error::Incomplete(_)), "got {err:?}");
        assert!(
            sat.describe(&c0).is_none(),
            "Saturation::describe must return None immediately after Err(Error::Incomplete)"
        );
    }

    // 2. `partial = true` and `saturate()` bypass splitting cleanly (never return SplitAnswer).
    {
        let refs: Vec<&AxiomKind> = shoi042.iter().map(|a| &a.kind).collect();
        let mut sat = Saturation::new();
        let partial_out = sat
            .classify(
                &refs,
                None,
                &Limits {
                    partial: true,
                    dl_work: Some(100),
                    ..lim_split.clone()
                },
            )
            .unwrap();
        assert_eq!(partial_out.stats.dl_rooted_splits, 0);
        assert_eq!(partial_out.stats.dl_rooted_leaves, 0);

        let mut sat2 = Saturation::new();
        let sat_out = sat2
            .saturate(
                &refs,
                None,
                &Limits {
                    dl_work: Some(100),
                    ..lim_split.clone()
                },
            )
            .unwrap();
        assert_eq!(sat_out.stats.dl_rooted_splits, 0);
    }

    // 3. Inclusive work bound `Some(total)` completes vs `Some(total - 1)` exhausts!
    {
        let mut k = Classifier::of(&shoi042);
        let exact = k.classify(&lim_split).unwrap();
        assert!(exact.complete && exact.stats.dl_rooted_splits >= 1);
        let total = exact.stats.dl_rooted_spent + exact.stats.dl_work;
        assert!(total > 0);

        let mut k_eq = Classifier::of(&shoi042);
        let at_bound = k_eq
            .classify(&Limits {
                dl_work: Some(total),
                ..lim_split.clone()
            })
            .unwrap();
        assert!(
            at_bound.complete && !at_bound.stats.dl_rooted_over,
            "exact inclusive bound total={total} must complete: {:?}",
            at_bound.stats
        );

        let mut k_under = Classifier::of(&shoi042);
        let under = k_under
            .classify(&Limits {
                dl_work: Some(total - 1),
                ..lim_split.clone()
            })
            .unwrap();
        assert!(
            under.stats.dl_rooted_over,
            "total-1={} must exhaust rooted bound: {:?}",
            total - 1,
            under.stats
        );
    }

    // 4. Switch-off incremental reuse is preserved (`rooted_eq_split = false`).
    {
        let ax = axioms(
            "SubClassOf(:A ObjectUnionOf(:B :C))
             SubClassOf(:B :D)
             SubClassOf(:C :D)",
        );
        let mut k = Classifier::of(&ax);
        let lim_off = Limits {
            threads: 1,
            rooted: true,
            rooted_eq_split: false,
            ..Limits::default()
        };
        let r1 = k.classify(&lim_off).unwrap();
        assert!(r1.taxonomy.is_subclass(&c("A"), &c("D")));
        let extra = Axiom::sub_class_of(c("E"), c("F"));
        k.add([&extra]);
        let r2 = k.classify(&lim_off).unwrap();
        assert!(
            r2.stats.incremental,
            "switch-off reuse must keep incremental context state"
        );
        assert!(r2.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(r2.taxonomy.is_subclass(&c("E"), &c("F")));
    }
}
