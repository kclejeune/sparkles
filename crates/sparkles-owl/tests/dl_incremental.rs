//! Incremental runs of the Horn rules (roadmap item 753): a run after axioms are added
//! and removed keeps the contexts that the change cannot reach, and its answer is that
//! of a classifier that starts again, on fixed edits that take a support away and on
//! random sequences of edits over generated ontologies and the fixtures.

use proptest::prelude::*;
use sparkles_owl::classify::{Classification, Classifier, Excluded, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

/// The axioms of a functional-syntax body with the prefix `:` for `http://ex.org/`.
fn axioms(body: &str) -> Vec<Axiom> {
    let doc = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap();
    doc.axioms
}

fn limits(threads: usize) -> Limits {
    Limits {
        threads,
        ..Limits::default()
    }
}

/// What two classifications must share: every subsumption between classes, the
/// unsatisfiable classes, the consistency and the completeness.
fn answer(o: &Classification) -> (Vec<(String, String)>, Vec<String>, bool, bool) {
    let t = &o.taxonomy;
    let cs: Vec<Class> = t.classes().cloned().collect();
    let mut pairs = Vec::new();
    for x in &cs {
        for y in &cs {
            if t.is_subclass(x, y) {
                pairs.push((x.as_str().to_string(), y.as_str().to_string()));
            }
        }
    }
    pairs.sort();
    let mut unsat: Vec<String> = t.unsatisfiable().map(|c| c.as_str().to_string()).collect();
    unsat.sort();
    (pairs, unsat, o.consistent, o.complete)
}

/// Classifies the kept classifier and one that starts again over `held`, and checks
/// that they agree. The answer is the kept classifier's.
fn same_as_fresh(k: &mut Classifier, held: &[Axiom], threads: usize, at: &str) -> Classification {
    let out = k.classify(&limits(threads)).unwrap();
    let mut fresh = Classifier::of(held);
    let want = fresh.classify(&limits(threads)).unwrap();
    assert_eq!(answer(&out), answer(&want), "the answers differ {at}");
    assert_eq!(out.taxonomy, want.taxonomy, "the taxonomies differ {at}");
    let (a, b): (Vec<Excluded>, Vec<Excluded>) = (k.excluded(), fresh.excluded());
    assert_eq!(a, b, "the axioms left out differ {at}");
    out
}

/// A classifier over `base` that has run once, with the axioms it holds.
fn started(base: &str, threads: usize) -> (Classifier, Vec<Axiom>) {
    let held = axioms(base);
    let mut k = Classifier::of(&held);
    let out = same_as_fresh(&mut k, &held, threads, "at the start");
    assert!(!out.stats.incremental);
    (k, held)
}

fn add(k: &mut Classifier, held: &mut Vec<Axiom>, body: &str) {
    let more = axioms(body);
    k.add(&more);
    held.extend(more);
}

fn remove(k: &mut Classifier, held: &mut Vec<Axiom>, body: &str) {
    let gone = axioms(body);
    k.remove(&gone);
    held.retain(|a| !gone.iter().any(|g| g.kind == a.kind));
}

const UNIVERSALS: &str = "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
     SubClassOf(:A ObjectAllValuesFrom(:r :C))
     SubClassOf(ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)) :D)
     SubClassOf(:E :A)
     SubClassOf(:F ObjectSomeValuesFrom(:s :G))
     SubClassOf(:G ObjectAllValuesFrom(ObjectInverseOf(:s) :H))
     SubClassOf(:K :L)";

#[test]
fn a_change_after_a_horn_run_keeps_the_contexts_it_cannot_reach() {
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started(UNIVERSALS, threads);
        // a class beside the rest: nothing that holds it was saturated
        add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
        assert!(out.stats.incremental, "the run started again");
        assert_eq!(out.stats.cleared, 0);
        assert!(out.taxonomy.is_subclass(&c("M"), &c("L")));
        // a told subsumer of K: K and M are saturated again, the others are kept
        add(&mut k, &mut held, "SubClassOf(:K :H)");
        let out = same_as_fresh(&mut k, &held, threads, "after K got a subsumer");
        assert!(out.stats.incremental);
        assert_eq!(out.stats.cleared, 2);
        assert!(out.taxonomy.is_subclass(&c("M"), &c("H")));
    }
}

/// A universal over a transitive property, which the Horn rules unfold by the
/// property's automaton into universals over fresh classes, with a class beside it.
const UNFOLDED: &str = "TransitiveObjectProperty(:t)
     SubClassOf(:A ObjectAllValuesFrom(:t :B))
     SubClassOf(:A ObjectSomeValuesFrom(:t :C))
     SubClassOf(:C ObjectSomeValuesFrom(:t :D))
     SubClassOf(ObjectSomeValuesFrom(:t ObjectIntersectionOf(:D :B)) :P)
     SubClassOf(:E :A)
     SubClassOf(:K :L)";

#[test]
fn a_change_beside_an_unfolded_universal_keeps_its_holders() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(UNFOLDED, threads);
        let out = k.classify(&limits(threads)).unwrap();
        // D, two steps of t from A, is in B by the unfolded universal
        assert!(out.taxonomy.is_subclass(&c("E"), &c("P")));
        // the universals that the automaton made are not unfolded in the runs after
        // it, which would give them other classes each time and clear A and E
        for round in 0..4 {
            add(&mut k, &mut held, "SubClassOf(:M :K)");
            let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
            assert!(out.stats.incremental, "the run started again");
            assert_eq!(out.stats.cleared, 0, "in round {round}");
            remove(&mut k, &mut held, "SubClassOf(:M :K)");
            let out = same_as_fresh(&mut k, &held, threads, "after the leaf left");
            assert!(out.stats.incremental, "the run started again");
            assert!(out.stats.cleared <= 1, "in round {round}");
        }
        // a universal that the automaton made and an axiom then names is unfolded
        // as in a run that starts again
        remove(&mut k, &mut held, "SubClassOf(:E :A)");
        let out = same_as_fresh(&mut k, &held, threads, "after E left A");
        assert!(out.stats.incremental);
        add(&mut k, &mut held, "SubClassOf(:E :A)");
        same_as_fresh(&mut k, &held, threads, "after E came back");
    }
}

/// A universal over the result of a chain whose unfolding makes the universal `∀s.B`,
/// which no axiom names, with an instance of the chain into `s` under M.
const MADE_UNIVERSAL: &str = "SubObjectPropertyOf(ObjectPropertyChain(:u :v) :s)
     SubObjectPropertyOf(ObjectPropertyChain(:r :s) :t)
     SubClassOf(:A ObjectAllValuesFrom(:t :B))
     SubClassOf(:M ObjectSomeValuesFrom(:u :C))
     SubClassOf(:C ObjectSomeValuesFrom(:v :D))
     DisjointClasses(:B :D)";

#[test]
fn an_axiom_that_names_a_universal_an_automaton_made_has_it_unfolded() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(MADE_UNIVERSAL, threads);
        let out = k.classify(&limits(threads)).unwrap();
        assert_eq!(out.taxonomy.unsatisfiable().count(), 0);
        // the axiom's universal is the one the automaton of t made, and a run that
        // starts again unfolds it by the chain into s: D, two steps from M, is in B
        let named = "SubClassOf(:M ObjectAllValuesFrom(:s :B))";
        add(&mut k, &mut held, named);
        let out = same_as_fresh(&mut k, &held, threads, "after M named the universal");
        assert!(out.taxonomy.unsatisfiable().any(|x| *x == c("M")));
        let a = "SubClassOf(:A ObjectAllValuesFrom(:t :B))";
        remove(&mut k, &mut held, a);
        let out = same_as_fresh(&mut k, &held, threads, "after A's universal left");
        assert!(out.taxonomy.unsatisfiable().any(|x| *x == c("M")));
        add(&mut k, &mut held, a);
        same_as_fresh(&mut k, &held, threads, "after A's universal came back");
        remove(&mut k, &mut held, named);
        let out = same_as_fresh(&mut k, &held, threads, "after M's universal left");
        assert_eq!(out.taxonomy.unsatisfiable().count(), 0);
        add(&mut k, &mut held, named);
        let out = same_as_fresh(&mut k, &held, threads, "after M's universal came back");
        assert!(out.taxonomy.unsatisfiable().any(|x| *x == c("M")));
    }
}

/// Two negative existential restrictions with one filler over two transitive
/// properties, each unfolded by the automaton of its property's inverse. The inverse
/// restrictions on X and Y make the Horn rules take the ontology.
const TWO_UNFOLDINGS: &str = "TransitiveObjectProperty(:r)
     TransitiveObjectProperty(:s)
     SubClassOf(ObjectSomeValuesFrom(:r :B) :C)
     SubClassOf(ObjectSomeValuesFrom(:s :B) :D)
     SubClassOf(:A ObjectSomeValuesFrom(:r :B))
     SubClassOf(:X ObjectSomeValuesFrom(ObjectInverseOf(:r) owl:Thing))
     SubClassOf(:Y ObjectSomeValuesFrom(ObjectInverseOf(:s) owl:Thing))";

#[test]
fn two_unfoldings_from_one_filler_keep_their_own_states() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(TWO_UNFOLDINGS, threads);
        let out = k.classify(&limits(threads)).unwrap();
        // the states of the two automata are classes of their own: with the same ones,
        // B's predecessor by r would take what the restriction over s gives
        assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        let s = "SubClassOf(ObjectSomeValuesFrom(:s :B) :D)";
        remove(&mut k, &mut held, s);
        let out = same_as_fresh(&mut k, &held, threads, "after the restriction over s left");
        assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        add(&mut k, &mut held, s);
        let out = same_as_fresh(&mut k, &held, threads, "after it came back");
        assert!(out.taxonomy.is_subclass(&c("A"), &c("C")));
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        add(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectSomeValuesFrom(:s :B))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after A got a successor by s");
        assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    }
}

/// An ontology whose only axiom outside OWL 2 EL is a pair of inverse properties that
/// no rule of the Horn tier reads, as in ChEBI: the Horn normalization takes the axiom
/// and the EL rules saturate.
const IDLE_INVERSE: &str = "InverseObjectProperties(:p :q)
     TransitiveObjectProperty(:p)
     SubClassOf(:A ObjectSomeValuesFrom(:p :B))
     SubClassOf(:B ObjectSomeValuesFrom(:p :C))
     SubClassOf(:C :D)
     SubClassOf(:E :A)
     SubClassOf(:K :L)";

#[test]
fn a_run_whose_horn_normalization_gives_no_rule_keeps_its_contexts() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(IDLE_INVERSE, threads);
        add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
        assert!(out.stats.incremental, "the run started again");
        assert_eq!(out.stats.cleared, 0);
        add(&mut k, &mut held, "SubClassOf(:C :F)");
        let out = same_as_fresh(&mut k, &held, threads, "after C got a subsumer");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("C"), &c("F")));
        remove(&mut k, &mut held, "SubClassOf(:C :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after C lost one");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("C"), &c("D")));
        // a negative existential restriction, which the inverse gives a universal: the
        // Horn rules take over and the run starts again
        add(
            &mut k,
            &mut held,
            "SubClassOf(ObjectSomeValuesFrom(:p :D) :G) SubClassOf(:C :D)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the rules changed");
        assert!(out.taxonomy.is_subclass(&c("E"), &c("G")));
        add(&mut k, &mut held, "SubClassOf(:N :E)");
        let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined the Horn run");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("N"), &c("G")));
    }
}

#[test]
fn a_removed_universal_takes_its_conclusions_with_it() {
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started(UNIVERSALS, threads);
        let out = k.classify(&limits(threads)).unwrap();
        assert!(out.taxonomy.is_subclass(&c("E"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("F"), &c("H")));
        // R3's conjunction of B and C loses its support
        remove(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectAllValuesFrom(:r :C))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the universal left");
        assert!(out.stats.incremental, "the run started again");
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(!out.taxonomy.is_subclass(&c("E"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("F"), &c("H")));
        // R4 through the inverse loses its support
        remove(
            &mut k,
            &mut held,
            "SubClassOf(:G ObjectAllValuesFrom(ObjectInverseOf(:s) :H))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the inverse universal left");
        assert!(!out.taxonomy.is_subclass(&c("F"), &c("H")));
        // and both come back
        add(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectAllValuesFrom(:r :C))
             SubClassOf(:G ObjectAllValuesFrom(ObjectInverseOf(:s) :H))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after both came back");
        assert!(out.taxonomy.is_subclass(&c("E"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("F"), &c("H")));
    }
}

#[test]
fn new_expressions_move_the_conjunctions_and_the_kept_links_follow() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(UNIVERSALS, threads);
        // new classes and a new restriction take ids where the conjunction of B and C
        // was, and E, which is kept, links to it through A's told subsumers
        add(
            &mut k,
            &mut held,
            "SubClassOf(:N ObjectSomeValuesFrom(:t ObjectIntersectionOf(:P :Q)))
             SubClassOf(:P ObjectAllValuesFrom(ObjectInverseOf(:t) :R))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after new expressions");
        assert!(out.stats.incremental, "the run started again");
        assert!(out.taxonomy.is_subclass(&c("N"), &c("R")));
        assert!(out.taxonomy.is_subclass(&c("E"), &c("D")));
        // a change below the conjunction reaches A and E through their links
        add(
            &mut k,
            &mut held,
            "SubClassOf(:C :S) SubClassOf(ObjectSomeValuesFrom(:r :S) :T)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the filler got a subsumer");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("E"), &c("T")));
        remove(&mut k, &mut held, "SubClassOf(:C :S)");
        let out = same_as_fresh(&mut k, &held, threads, "after the filler lost it");
        assert!(!out.taxonomy.is_subclass(&c("E"), &c("T")));
    }
}

#[test]
fn at_most_restrictions_lose_and_regain_their_merges() {
    let base = "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A ObjectSomeValuesFrom(:r :C))
         SubClassOf(:A ObjectMaxCardinality(1 :r))
         SubClassOf(ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)) :D)
         DisjointClasses(:C :X)
         SubClassOf(:Z :A)
         SubClassOf(:Q ObjectSomeValuesFrom(:s :U))
         SubClassOf(:Q ObjectAllValuesFrom(:s :V))
         SubClassOf(ObjectSomeValuesFrom(:s ObjectIntersectionOf(:U :V)) :W)";
    // the universal on s keeps the run one of the Horn rules while the at-most
    // restriction is away, so that the run keeps its contexts
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started(base, threads);
        let out = k.classify(&limits(threads)).unwrap();
        assert!(out.taxonomy.is_subclass(&c("Z"), &c("D")));
        remove(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectMaxCardinality(1 :r))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the at-most left");
        assert!(out.stats.incremental, "the run started again");
        assert!(!out.taxonomy.is_subclass(&c("Z"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("Q"), &c("W")));
        add(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectMaxCardinality(1 :r))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the at-most came back");
        assert!(out.taxonomy.is_subclass(&c("Z"), &c("D")));
        // the merged successor becomes unsatisfiable, and A and Z with it
        add(&mut k, &mut held, "SubClassOf(:B :X)");
        let out = same_as_fresh(&mut k, &held, threads, "after the merge clashed");
        assert!(out.taxonomy.unsatisfiable().any(|u| *u == c("Z")));
        remove(&mut k, &mut held, "SubClassOf(:B :X)");
        let out = same_as_fresh(&mut k, &held, threads, "after the clash left");
        assert!(out.taxonomy.unsatisfiable().next().is_none());
    }
}

#[test]
fn a_minimum_keeps_its_fresh_classes_between_runs() {
    let base = "SubClassOf(:A ObjectMinCardinality(2 :r :B))
         SubClassOf(:A ObjectAllValuesFrom(:r :C))
         SubClassOf(ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C)) :D)
         SubClassOf(:E :A)
         SubClassOf(:K :L)";
    for threads in [1, 4] {
        let (mut k, mut held) = started(base, threads);
        add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
        assert!(out.stats.incremental, "the run started again");
        assert_eq!(
            out.stats.cleared, 0,
            "the minimum's classes changed their ids"
        );
        assert!(out.taxonomy.is_subclass(&c("E"), &c("D")));
        // another minimum takes fresh classes of its own
        add(
            &mut k,
            &mut held,
            "SubClassOf(:K ObjectMinCardinality(3 :r :B))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after a second minimum");
        assert!(out.stats.incremental);
        remove(
            &mut k,
            &mut held,
            "SubClassOf(:A ObjectMinCardinality(2 :r :B))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the first minimum left");
        assert!(!out.taxonomy.is_subclass(&c("E"), &c("D")));
    }
}

#[test]
fn a_filler_that_stops_being_absorbed_reaches_the_universals_on_it() {
    // C has no rule, so R3 passes over ∀r.C, until an axiom gives C a subsumer
    let base = "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:A ObjectAllValuesFrom(:r :C))
         SubClassOf(ObjectSomeValuesFrom(:r :D) :E)
         SubClassOf(:Z :A)";
    for threads in [1, 4] {
        let (mut k, mut held) = started(base, threads);
        add(&mut k, &mut held, "SubClassOf(:C :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after the filler got a rule");
        assert!(out.taxonomy.is_subclass(&c("Z"), &c("E")));
        remove(&mut k, &mut held, "SubClassOf(:C :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after the filler lost its rule");
        assert!(!out.taxonomy.is_subclass(&c("Z"), &c("E")));
    }
}

#[test]
fn changes_the_invalidation_does_not_follow_start_again() {
    let (mut k, mut held) = started(UNIVERSALS, 1);
    // a role axiom changes the role table
    add(&mut k, &mut held, "SubObjectPropertyOf(:r :s)");
    let out = same_as_fresh(&mut k, &held, 1, "after a role inclusion");
    assert!(!out.stats.incremental);
    // a domain is a range of the inverse role
    add(&mut k, &mut held, "ObjectPropertyDomain(:r :W)");
    let out = same_as_fresh(&mut k, &held, 1, "after a domain");
    assert!(!out.stats.incremental);
    // an axiom that needs case analysis moves the run to Tier D
    add(&mut k, &mut held, "SubClassOf(:K ObjectUnionOf(:A :F))");
    let out = same_as_fresh(&mut k, &held, 1, "after a union");
    assert!(!out.stats.incremental);
    remove(&mut k, &mut held, "SubClassOf(:K ObjectUnionOf(:A :F))");
    let out = same_as_fresh(&mut k, &held, 1, "after the union left");
    assert!(!out.stats.incremental);
    // an individual
    add(&mut k, &mut held, "ClassAssertion(:A :a)");
    let out = same_as_fresh(&mut k, &held, 1, "after an assertion");
    assert!(!out.stats.incremental);
    // the last axioms outside OWL 2 EL leave: the EL rules take over
    let (mut k, mut held) = started(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))",
        1,
    );
    remove(
        &mut k,
        &mut held,
        "SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) :C))",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after the last Horn axiom left");
    assert!(!out.stats.incremental);
    assert!(!out.taxonomy.is_subclass(&c("A"), &c("C")));
}

#[test]
fn a_tier_decided_over_removed_expressions_is_decided_again() {
    // The self restriction keeps Tier D from the union, so the run is one of the EL
    // rules with the union left out. Once it is removed, a run that starts again takes
    // the union with Tier D.
    let (mut k, mut held) = started(
        "SubClassOf(:A ObjectHasSelf(:r))
         SubClassOf(:B ObjectUnionOf(:C :D))
         SubClassOf(:C :E)
         SubClassOf(:D :E)",
        1,
    );
    assert!(!k.excluded().is_empty());
    remove(&mut k, &mut held, "SubClassOf(:A ObjectHasSelf(:r))");
    let out = same_as_fresh(&mut k, &held, 1, "after the self restriction left");
    assert!(out.taxonomy.is_subclass(&c("B"), &c("E")));
}

/// One axiom of a generated ontology over a few classes and roles, mostly in the Horn
/// fragment outside OWL 2 EL.
fn axiom(kind: u8, a: u8, b: u8, d: u8, r: u8) -> String {
    let (a, b, d) = (a % 7, b % 7, d % 7);
    // a second class that differs from the first, for the forms that need two operands
    let other = (a + 1 + b % 6) % 7;
    let r = r % 3;
    let role = |inv: bool| {
        if inv {
            format!("ObjectInverseOf(:r{r})")
        } else {
            format!(":r{r}")
        }
    };
    match kind % 12 {
        0 | 1 => format!("SubClassOf(:C{a} :C{b})"),
        2 => format!(
            "SubClassOf(:C{a} ObjectSomeValuesFrom({} :C{b}))",
            role(false)
        ),
        3 => format!(
            "SubClassOf(:C{a} ObjectSomeValuesFrom({} :C{b}))",
            role(true)
        ),
        4 => format!(
            "SubClassOf(:C{a} ObjectAllValuesFrom({} :C{b}))",
            role(false)
        ),
        5 => format!(
            "SubClassOf(:C{a} ObjectAllValuesFrom({} :C{b}))",
            role(true)
        ),
        6 => format!(
            "SubClassOf(ObjectSomeValuesFrom({} :C{a}) :C{b})",
            role(false)
        ),
        7 => format!("SubClassOf(ObjectIntersectionOf(:C{a} :C{other}) :C{d})"),
        8 => format!(
            "SubClassOf(:C{a} ObjectMaxCardinality(1 {} :C{b}))",
            role(false)
        ),
        9 => format!(
            "SubClassOf(:C{a} ObjectMinCardinality(2 {} :C{b}))",
            role(false)
        ),
        10 => format!("DisjointClasses(:C{a} :C{other} :C{d})"),
        _ => format!(
            "EquivalentClasses(:C{a} ObjectIntersectionOf(:C{b} ObjectSomeValuesFrom({} :C{d})))",
            role(false)
        ),
    }
}

type Gen = (u8, u8, u8, u8, u8);

fn gen_axiom() -> impl Strategy<Value = Gen> {
    (
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
        any::<u8>(),
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    /// Random sequences of additions and removals over a generated Horn ontology: every
    /// run of the kept classifier answers as a classifier that starts again.
    #[test]
    fn edits_of_a_horn_ontology_answer_as_a_fresh_run(
        base in prop::collection::vec(gen_axiom(), 4..24),
        edits in prop::collection::vec(
            (prop::collection::vec(gen_axiom(), 0..3), prop::collection::vec(any::<u8>(), 0..3)),
            1..8,
        ),
        threads in prop::sample::select(vec![1usize, 4, 8]),
    ) {
        let text = |g: &Gen| axiom(g.0, g.1, g.2, g.3, g.4);
        let mut held: Vec<Axiom> = Vec::new();
        for g in &base {
            for a in axioms(&text(g)) {
                if !held.iter().any(|h| h.kind == a.kind) {
                    held.push(a);
                }
            }
        }
        let mut k = Classifier::of(&held);
        same_as_fresh(&mut k, &held, threads, "at the start");
        for (step, (added, removed)) in edits.iter().enumerate() {
            for &i in removed {
                if !held.is_empty() {
                    let gone = held.remove(i as usize % held.len());
                    k.remove([&gone]);
                }
            }
            for g in added {
                for a in axioms(&text(g)) {
                    if !held.iter().any(|h| h.kind == a.kind) {
                        k.add([&a]);
                        held.push(a);
                    }
                }
            }
            same_as_fresh(&mut k, &held, threads, &format!("after edit {step}"));
        }
    }
}

/// Seeded sequences of edits, counted: how many steps kept contexts, how many of those
/// cleared some, how many cleared none and how many cleared fewer contexts than the
/// taxonomy before the edit had classes, each of which had a saturated context, so
/// that the step kept one of them at least. The comparison with a fresh classifier
/// says little when most steps start again, so the counts have floors.
#[test]
fn seeded_edits_keep_contexts_often_enough_to_test_them() {
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut steps, mut incremental, mut cleared_some, mut cleared_none, mut kept_a_class) =
        (0usize, 0, 0, 0, 0);
    for round in 0..240usize {
        let threads = [1usize, 4, 8][round % 3];
        let gen_axioms = |n: usize, next: &mut dyn FnMut() -> u64| -> Vec<Axiom> {
            let mut out = Vec::new();
            for _ in 0..n {
                let x = next();
                let text = axiom(
                    x as u8,
                    (x >> 8) as u8,
                    (x >> 16) as u8,
                    (x >> 24) as u8,
                    (x >> 32) as u8,
                );
                out.extend(axioms(&text));
            }
            out
        };
        let mut held: Vec<Axiom> = Vec::new();
        for a in gen_axioms(8 + (next() % 16) as usize, &mut next) {
            if !held.iter().any(|h| h.kind == a.kind) {
                held.push(a);
            }
        }
        let mut k = Classifier::of(&held);
        let first = same_as_fresh(&mut k, &held, threads, "at the start");
        let mut classes_before = first.taxonomy.classes().count();
        for step in 0..6 {
            for _ in 0..next() % 3 {
                if !held.is_empty() {
                    let gone = held.remove(next() as usize % held.len());
                    k.remove([&gone]);
                }
            }
            for a in gen_axioms((next() % 3) as usize, &mut next) {
                if !held.iter().any(|h| h.kind == a.kind) {
                    k.add([&a]);
                    held.push(a);
                }
            }
            let at = format!("in round {round} after edit {step}");
            let out = same_as_fresh(&mut k, &held, threads, &at);
            steps += 1;
            if out.stats.incremental {
                incremental += 1;
                cleared_some += usize::from(out.stats.cleared > 0);
                cleared_none += usize::from(out.stats.cleared == 0);
                kept_a_class +=
                    usize::from(out.stats.cleared > 0 && out.stats.cleared < classes_before);
            }
            classes_before = out.taxonomy.classes().count();
        }
    }
    eprintln!(
        "TALLY steps={steps} incremental={incremental} cleared_some={cleared_some} \
         cleared_none={cleared_none} kept_a_class={kept_a_class}"
    );
    // The run measured 1413 incremental steps of 1440, 1261 that cleared a context, 152
    // that cleared none and 595 that kept a class's context beside cleared ones. The
    // floors are about half of each, so that a rule that makes most steps start again
    // fails here.
    assert!(incremental >= 700, "{incremental} incremental steps");
    assert!(
        cleared_some >= 600,
        "{cleared_some} steps cleared a context"
    );
    assert!(cleared_none >= 75, "{cleared_none} steps cleared none");
    assert!(
        kept_a_class >= 300,
        "{kept_a_class} steps kept a class beside cleared ones"
    );
}

const UNIONS: &str = "SubClassOf(:A ObjectUnionOf(:B :C))
     SubClassOf(:B :D)
     SubClassOf(:C :D)
     SubClassOf(:E :A)
     SubClassOf(:F ObjectSomeValuesFrom(:r :A))
     SubClassOf(ObjectSomeValuesFrom(:r :D) :G)
     SubClassOf(:K :L)";

/// A classifier over `base` that has run once with Tier D.
fn started_disjunctive(base: &str, threads: usize) -> (Classifier, Vec<Axiom>) {
    let (k, held) = started(base, threads);
    assert!(k.excluded().is_empty(), "{:?}", k.excluded());
    (k, held)
}

#[test]
fn a_change_after_a_run_of_tier_d_keeps_the_contexts_it_cannot_reach() {
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started_disjunctive(UNIONS, threads);
        let out = k.classify(&limits(threads)).unwrap();
        assert_eq!(out.stats.tier, sparkles_owl::classify::Tier::Disjunctive);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("F"), &c("G")));
        // a class beside the rest
        add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
        assert!(out.stats.incremental, "the run started again");
        assert_eq!(out.stats.cleared, 0);
        assert!(out.taxonomy.is_subclass(&c("M"), &c("L")));
        // a told subsumer of K: K and M are saturated again
        add(&mut k, &mut held, "SubClassOf(:K :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after K got a subsumer");
        assert!(out.stats.incremental);
        assert_eq!(out.stats.cleared, 2);
        assert!(out.taxonomy.is_subclass(&c("M"), &c("D")));
    }
}

#[test]
fn a_test_that_held_loses_its_support() {
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started_disjunctive(UNIONS, threads);
        // A is below D only by the case analysis, which a test of A and not D shows
        remove(&mut k, &mut held, "SubClassOf(:C :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after a case lost its subsumer");
        assert!(out.stats.incremental, "the run started again");
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(!out.taxonomy.is_subclass(&c("E"), &c("D")));
        assert!(!out.taxonomy.is_subclass(&c("F"), &c("G")));
        assert!(out.taxonomy.is_subclass(&c("K"), &c("L")));
        add(&mut k, &mut held, "SubClassOf(:C :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after the case got it back");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("E"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("F"), &c("G")));
    }
}

#[test]
fn a_union_comes_and_goes_beside_another() {
    for threads in [1, 4, 8] {
        let (mut k, mut held) = started_disjunctive(UNIONS, threads);
        add(
            &mut k,
            &mut held,
            "SubClassOf(:K ObjectUnionOf(:P :Q)) SubClassOf(:P :L2) SubClassOf(:Q :L2)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after a second union");
        assert!(out.stats.incremental, "the run started again");
        assert!(out.taxonomy.is_subclass(&c("K"), &c("L2")));
        remove(&mut k, &mut held, "SubClassOf(:K ObjectUnionOf(:P :Q))");
        let out = same_as_fresh(&mut k, &held, threads, "after the second union left");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("K"), &c("L2")));
        assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
        // the last union leaves: the EL rules take over, and the run starts again
        remove(&mut k, &mut held, "SubClassOf(:A ObjectUnionOf(:B :C))");
        let out = same_as_fresh(&mut k, &held, threads, "after the last union left");
        assert!(!out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
    }
}

#[test]
fn a_clause_of_every_context_clears_every_context() {
    for threads in [1, 4] {
        let (mut k, mut held) = started_disjunctive(UNIONS, threads);
        // a complement on the left is a clause with owl:Thing as its body
        add(&mut k, &mut held, "SubClassOf(ObjectComplementOf(:B) :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after a complement on the left");
        assert!(out.stats.incremental, "the run started again");
        assert!(out.stats.cleared >= 7, "{} cleared", out.stats.cleared);
        assert!(out.taxonomy.is_subclass(&c("K"), &c("D")));
        remove(&mut k, &mut held, "SubClassOf(ObjectComplementOf(:B) :D)");
        let out = same_as_fresh(&mut k, &held, threads, "after it left");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("K"), &c("D")));
    }
}

#[test]
fn a_successor_of_tier_d_answers_a_predecessor_that_is_saturated_again() {
    for threads in [1, 4, 8] {
        // B's successor is conditional in A, so A registers with the hypothetical
        // successor of the restriction, and the universal sends it a marker
        let (mut k, mut held) = started_disjunctive(
            "SubClassOf(:A ObjectUnionOf(:X ObjectSomeValuesFrom(:r :B)))
             SubClassOf(:A ObjectAllValuesFrom(:r :C))
             SubClassOf(ObjectIntersectionOf(:B :C) owl:Nothing)
             SubClassOf(:X :Y)
             SubClassOf(:Z ObjectSomeValuesFrom(:r :B))
             SubClassOf(:Z ObjectAllValuesFrom(:r :C2))
             SubClassOf(ObjectIntersectionOf(:B :C2) :W)
             SubClassOf(ObjectSomeValuesFrom(:r :W) :V)",
            threads,
        );
        let out = k.classify(&limits(threads)).unwrap();
        assert!(out.taxonomy.is_subclass(&c("A"), &c("Y")));
        assert!(out.taxonomy.is_subclass(&c("Z"), &c("V")));
        // A is saturated again and registers again with the successors that are kept
        add(&mut k, &mut held, "SubClassOf(:A :N)");
        let out = same_as_fresh(&mut k, &held, threads, "after A got a subsumer");
        assert!(out.stats.incremental, "the run started again");
        assert!(out.taxonomy.is_subclass(&c("A"), &c("Y")));
        // the disjointness leaves, and with it the clause that the successor sent back
        remove(
            &mut k,
            &mut held,
            "SubClassOf(ObjectIntersectionOf(:B :C) owl:Nothing)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the disjointness left");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("Y")));
        assert!(out.taxonomy.is_subclass(&c("Z"), &c("V")));
        add(
            &mut k,
            &mut held,
            "SubClassOf(ObjectIntersectionOf(:B :C) owl:Nothing)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "after the disjointness came back");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("Y")));
    }
}

#[test]
fn changes_that_tier_d_does_not_follow_start_again() {
    let (mut k, mut held) = started_disjunctive(UNIONS, 1);
    add(&mut k, &mut held, "SubObjectPropertyOf(:r :s)");
    let out = same_as_fresh(&mut k, &held, 1, "after a role inclusion");
    assert!(!out.stats.incremental);
    add(&mut k, &mut held, "ObjectPropertyDomain(:r :W)");
    let out = same_as_fresh(&mut k, &held, 1, "after a domain");
    assert!(!out.stats.incremental);
    add(&mut k, &mut held, "SubClassOf(:K :L3)");
    let out = same_as_fresh(&mut k, &held, 1, "after a told subsumer");
    assert!(out.stats.incremental);
    // an individual takes the union from Tier D
    add(&mut k, &mut held, "ClassAssertion(:A :a)");
    let out = same_as_fresh(&mut k, &held, 1, "after an assertion");
    assert!(!out.stats.incremental);
    // a run that counts has a bound on its conclusions and starts again
    let counting = Limits {
        counting: true,
        ..Limits::default()
    };
    let held = axioms(&format!(
        "{UNIONS} SubClassOf(:A ObjectMaxCardinality(2 :r :B))"
    ));
    let mut k = Classifier::of(&held);
    k.classify(&counting).unwrap();
    let more = axioms("SubClassOf(:M :K)");
    k.add(&more);
    let out = k.classify(&counting).unwrap();
    assert!(!out.stats.incremental);
}

/// One axiom of a generated ontology that needs case analysis, or one of [`axiom`].
fn disjunctive_axiom(kind: u8, a: u8, b: u8, d: u8, r: u8) -> String {
    let (x, y) = (a % 7, b % 7);
    // a second operand that differs from the first, which a union needs
    let z = (y + 1 + d % 6) % 7;
    let role = r % 3;
    match kind % 20 {
        0 | 1 => format!("SubClassOf(:C{x} ObjectUnionOf(:C{y} :C{z}))"),
        2 => format!("SubClassOf(:C{x} ObjectComplementOf(:C{y}))"),
        3 => format!("SubClassOf(:C{x} ObjectSomeValuesFrom(:r{role} ObjectUnionOf(:C{y} :C{z})))"),
        4 => format!("SubClassOf(:C{x} ObjectUnionOf(:C{y} ObjectSomeValuesFrom(:r{role} :C{z})))"),
        5 => format!("SubClassOf(:C{x} ObjectAllValuesFrom(:r{role} ObjectUnionOf(:C{y} :C{z})))"),
        6 => format!("SubClassOf(ObjectAllValuesFrom(:r{role} :C{x}) :C{y})"),
        7 => format!(
            "EquivalentClasses(:C{x} ObjectUnionOf(:C{y} ObjectAllValuesFrom(:r{role} :C{z})))"
        ),
        _ => axiom(kind, a, b, d, r),
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(192))]

    /// Random sequences of additions and removals over a generated ontology with
    /// unions, complements and universals in every position: every run of the kept
    /// classifier answers as a classifier that starts again.
    #[test]
    fn edits_of_a_disjunctive_ontology_answer_as_a_fresh_run(
        base in prop::collection::vec(gen_axiom(), 4..24),
        edits in prop::collection::vec(
            (prop::collection::vec(gen_axiom(), 0..3), prop::collection::vec(any::<u8>(), 0..3)),
            1..8,
        ),
        threads in prop::sample::select(vec![1usize, 4, 8]),
    ) {
        let text = |g: &Gen| disjunctive_axiom(g.0, g.1, g.2, g.3, g.4);
        let mut held: Vec<Axiom> = Vec::new();
        for g in &base {
            for a in axioms(&text(g)) {
                if !held.iter().any(|h| h.kind == a.kind) {
                    held.push(a);
                }
            }
        }
        let mut k = Classifier::of(&held);
        same_as_fresh(&mut k, &held, threads, "at the start");
        for (step, (added, removed)) in edits.iter().enumerate() {
            for &i in removed {
                if !held.is_empty() {
                    let gone = held.remove(i as usize % held.len());
                    k.remove([&gone]);
                }
            }
            for g in added {
                for a in axioms(&text(g)) {
                    if !held.iter().any(|h| h.kind == a.kind) {
                        k.add([&a]);
                        held.push(a);
                    }
                }
            }
            same_as_fresh(&mut k, &held, threads, &format!("after edit {step}"));
        }
    }
}

/// Seeded sequences of edits over generated ontologies that need case analysis,
/// counted as [`seeded_edits_keep_contexts_often_enough_to_test_them`] counts those of
/// the Horn rules, with the steps whose run was one of Tier D.
#[test]
fn seeded_edits_of_tier_d_keep_contexts_often_enough_to_test_them() {
    use sparkles_owl::classify::Tier;
    let mut seed = 0xD1B5_4A32_D192_ED03u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut steps, mut disjunctive, mut incremental, mut cleared_some, mut cleared_none) =
        (0usize, 0, 0, 0, 0);
    let (mut kept_a_class, mut tests_held) = (0usize, 0);
    for round in 0..360usize {
        let threads = [1usize, 4, 8][round % 3];
        let gen_axioms = |n: usize, next: &mut dyn FnMut() -> u64| -> Vec<Axiom> {
            let mut out = Vec::new();
            for _ in 0..n {
                let x = next();
                let text = disjunctive_axiom(
                    x as u8,
                    (x >> 8) as u8,
                    (x >> 16) as u8,
                    (x >> 24) as u8,
                    (x >> 32) as u8,
                );
                out.extend(axioms(&text));
            }
            out
        };
        let mut held: Vec<Axiom> = Vec::new();
        for a in gen_axioms(8 + (next() % 16) as usize, &mut next) {
            if !held.iter().any(|h| h.kind == a.kind) {
                held.push(a);
            }
        }
        let mut k = Classifier::of(&held);
        let first = same_as_fresh(&mut k, &held, threads, "at the start");
        let mut classes_before = first.taxonomy.classes().count();
        for step in 0..6 {
            for _ in 0..next() % 3 {
                if !held.is_empty() {
                    let gone = held.remove(next() as usize % held.len());
                    k.remove([&gone]);
                }
            }
            for a in gen_axioms((next() % 3) as usize, &mut next) {
                if !held.iter().any(|h| h.kind == a.kind) {
                    k.add([&a]);
                    held.push(a);
                }
            }
            let at = format!("in round {round} after edit {step}");
            let out = same_as_fresh(&mut k, &held, threads, &at);
            steps += 1;
            if out.stats.tier == Tier::Disjunctive && out.stats.outside_el.taken > 0 {
                disjunctive += 1;
                if out.stats.incremental {
                    incremental += 1;
                    cleared_some += usize::from(out.stats.cleared > 0);
                    cleared_none += usize::from(out.stats.cleared == 0);
                    kept_a_class +=
                        usize::from(out.stats.cleared > 0 && out.stats.cleared < classes_before);
                    tests_held += usize::from(out.stats.dl_tests_held > 0);
                }
            }
            classes_before = out.taxonomy.classes().count();
        }
    }
    eprintln!(
        "TALLY steps={steps} disjunctive={disjunctive} incremental={incremental} \
         cleared_some={cleared_some} cleared_none={cleared_none} kept_a_class={kept_a_class} \
         tests_held={tests_held}"
    );
    // The run measured 2160 steps, 2143 of them runs of Tier D, 2138 that kept
    // contexts, 1838 that cleared a context, 300 that cleared none, 316 that kept a
    // class's context beside cleared ones and 308 in which a test held. The floors are
    // about half of each.
    assert!(incremental >= 1000, "{incremental} incremental steps");
    assert!(
        cleared_some >= 900,
        "{cleared_some} steps cleared a context"
    );
    assert!(cleared_none >= 150, "{cleared_none} steps cleared none");
    assert!(
        kept_a_class >= 150,
        "{kept_a_class} steps kept a class beside cleared ones"
    );
    assert!(
        tests_held >= 150,
        "{tests_held} steps with a test that held"
    );
}

/// The ontologies of a fixture folder: the files `NAME.ofn`, without the reasoners'
/// answers beside them.
fn fixture_files(dir: &str) -> Vec<std::path::PathBuf> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir);
    let mut cases: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    cases.sort();
    cases
}

/// What [`edits_of_files`] counted.
#[derive(Default, Debug)]
struct Tally {
    files: usize,
    steps: usize,
    incremental: usize,
    horn: usize,
    disjunctive: usize,
    kept_a_class: usize,
}

/// Seeded removals and returns of axioms over each ontology, on 1, 4 and 8 threads:
/// a few axioms leave, a run, they come back, a run, and each run of the kept
/// classifier answers as a classifier that starts again.
fn edits_of_files(
    files: &[std::path::PathBuf],
    rounds: usize,
    limits_of: &dyn Fn(usize) -> Limits,
) -> Tally {
    use sparkles_owl::classify::Tier;
    let mut tally = Tally::default();
    for (n, path) in files.iter().enumerate() {
        let text = std::fs::read_to_string(path).unwrap();
        let Ok(doc) = ofn_read::parse(&text) else {
            continue;
        };
        tally.files += 1;
        let mut seed =
            0x9E37_79B9_7F4A_7C15u64 ^ (n as u64 + 1).wrapping_mul(0xD1B5_4A32_D192_ED03);
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for threads in [1usize, 4, 8] {
            let limits = limits_of(threads);
            let mut held: Vec<Axiom> = doc.axioms.clone();
            let mut k = Classifier::of(&held);
            let first = k.classify(&limits).unwrap();
            let mut classes_before = first.taxonomy.classes().count();
            let mut check = |k: &mut Classifier, held: &[Axiom], at: &str, tally: &mut Tally| {
                let out = k.classify(&limits).unwrap();
                let mut fresh = Classifier::of(held);
                let want = fresh.classify(&limits).unwrap();
                let at = format!("{at} of {} on {threads} threads", path.display());
                assert_eq!(answer(&out), answer(&want), "the answers differ {at}");
                assert_eq!(out.taxonomy, want.taxonomy, "the taxonomies differ {at}");
                assert_eq!(
                    k.excluded(),
                    fresh.excluded(),
                    "the axioms left out differ {at}"
                );
                tally.steps += 1;
                if out.stats.incremental {
                    tally.incremental += 1;
                    match out.stats.tier {
                        Tier::Disjunctive if out.stats.clauses > 0 => tally.disjunctive += 1,
                        Tier::El => {}
                        _ => tally.horn += usize::from(out.stats.outside_el.taken > 0),
                    }
                    tally.kept_a_class += usize::from(out.stats.cleared < classes_before);
                }
                classes_before = out.taxonomy.classes().count();
            };
            for round in 0..rounds {
                let mut gone: Vec<Axiom> = Vec::new();
                for _ in 0..1 + next() % 3 {
                    if !held.is_empty() {
                        let a = held.remove(next() as usize % held.len());
                        k.remove([&a]);
                        gone.push(a);
                    }
                }
                check(&mut k, &held, &format!("after removal {round}"), &mut tally);
                for a in gone {
                    if !held.iter().any(|h| h.kind == a.kind) {
                        k.add([&a]);
                        held.push(a);
                    }
                }
                check(&mut k, &held, &format!("after return {round}"), &mut tally);
            }
        }
    }
    tally
}

/// The fixtures of roadmap item 603: generated families of ALC to SROIQ and
/// hand-written cases, with the limits of a default run. The floors are about half of
/// what the run counted, so that a rule that makes most steps start again fails here.
#[test]
fn edits_of_the_dl603_fixtures_answer_as_a_fresh_run() {
    let tally = edits_of_files(&fixture_files("dl603"), 2, &limits);
    eprintln!("TALLY dl603 {tally:?}");
    // The run counted 640 files and 7680 steps, of which 1298 kept contexts, 953 of
    // them with Tier D and 287 with the Horn rules, and 800 kept a class's context.
    // The other steps start again: they change a declaration of a property, a role
    // axiom or an axiom with data or an individual, or the ontology counts.
    assert!(tally.files >= 600, "{tally:?}");
    assert!(tally.incremental >= 650, "{tally:?}");
    assert!(tally.disjunctive >= 475, "{tally:?}");
    assert!(tally.horn >= 140, "{tally:?}");
    assert!(tally.kept_a_class >= 400, "{tally:?}");
}

/// The generated ontologies of roadmap item 897, whose number restrictions have
/// compound qualifiers.
#[test]
fn edits_of_the_dl897_fixtures_answer_as_a_fresh_run() {
    let tally = edits_of_files(&fixture_files("dl897"), 3, &limits);
    eprintln!("TALLY dl897 {tally:?}");
    // The run counted 27 files and 486 steps, of which 286 kept contexts, 285 of them
    // with Tier D, and 254 kept a class's context.
    assert!(tally.files >= 25, "{tally:?}");
    assert!(tally.disjunctive >= 140, "{tally:?}");
    assert!(tally.kept_a_class >= 125, "{tally:?}");
}

/// The files of the ORE 2015 sample that `SPARKLES_ORE_DIR` names, when it is set:
/// every `.owl` file in functional-style syntax of at most `SPARKLES_ORE_MAX_BYTES`,
/// 300,000 by default.
#[test]
fn edits_of_ore_files_answer_as_a_fresh_run() {
    let Ok(dir) = std::env::var("SPARKLES_ORE_DIR") else {
        return;
    };
    let most: u64 = std::env::var("SPARKLES_ORE_MAX_BYTES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300_000);
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "owl" || x == "ofn")
                && std::fs::metadata(p).is_ok_and(|m| m.len() <= most)
        })
        .collect();
    files.sort();
    let tally = edits_of_files(&files, 2, &limits);
    eprintln!("TALLY ore {tally:?}");
    assert!(tally.files > 0, "no file of {dir} was read");
}

/// The two bases of the tests of a kept run's bounds: one that the Horn rules take and
/// one that Tier D takes.
const BOUNDED: [&str; 2] = [UNIVERSALS, UNIONS];

#[test]
fn a_kept_run_that_passes_its_memory_budget_starts_again_in_full() {
    // Pad each base with unrelated classes so holding the last index beside the new
    // one cleanly exceeds the least budget of a fresh run on both the Horn and Tier D
    // bases, rather than depending on a few bytes of transient scratch accounting.
    let pad: String = (0..64)
        .map(|i| format!("SubClassOf(:Pad{i} :L)\n"))
        .collect();
    for base in BOUNDED {
        let padded = format!("{base}\n{pad}");
        let held = {
            let mut held = axioms(&padded);
            held.extend(axioms("SubClassOf(:M :K)"));
            held
        };
        // the least budget with which a classifier that starts again over the edited
        // axioms takes every axiom: below it the run falls back to the EL rules and
        // leaves axioms out, or fails
        let fresh = |limit: usize| {
            let mut k = Classifier::of(&held);
            let out = k
                .classify(&Limits {
                    max_memory: Some(limit),
                    ..limits(1)
                })
                .ok()?;
            k.excluded().is_empty().then_some(out)
        };
        let (mut low, mut high) = (0usize, 1usize << 30);
        assert!(fresh(high).is_some());
        while high - low > 64 {
            let mid = low + (high - low) / 2;
            if fresh(mid).is_some() {
                high = mid;
            } else {
                low = mid;
            }
        }
        let want = fresh(high).unwrap();
        // a kept run holds more than that, the last index among it, and answers as the
        // classifier that starts again does
        let mut restarted = 0;
        for more in [0usize, 64, 256, 1024, 4096] {
            let (mut k, _) = started(&padded, 1);
            k.add(&axioms("SubClassOf(:M :K)"));
            let out = k
                .classify(&Limits {
                    max_memory: Some(high + more),
                    ..limits(1)
                })
                .unwrap();
            assert_eq!(answer(&out), answer(&want));
            assert_eq!(out.taxonomy, want.taxonomy);
            assert_eq!(out.stats.tier, want.stats.tier);
            assert_eq!(k.excluded(), Vec::new());
            restarted += usize::from(!out.stats.incremental);
            // the run after it keeps contexts again
            k.add(&axioms("SubClassOf(:M2 :K)"));
            let out = k.classify(&limits(1)).unwrap();
            assert!(out.stats.incremental);
        }
        assert!(
            restarted > 0,
            "no kept run passed the budget of a fresh one"
        );
    }
}

#[test]
fn a_run_after_a_change_beside_a_data_value_fits_the_budget_of_a_fresh_run() {
    // an integer whose value takes most of the budget, in an ontology of the Horn rules
    let base = format!(
        "{UNIVERSALS}\nSubClassOf(:A DataHasValue(:d \"{}\"^^<http://www.w3.org/2001/XMLSchema#integer>))",
        "9".repeat(20_000)
    );
    let held = {
        let mut held = axioms(&base);
        held.extend(axioms("SubClassOf(:M :K)"));
        held
    };
    let fresh = |limit: usize| {
        let mut k = Classifier::of(&held);
        let out = k
            .classify(&Limits {
                max_memory: Some(limit),
                ..limits(1)
            })
            .ok()?;
        k.excluded().is_empty().then_some(out)
    };
    let (mut low, mut high) = (0usize, 1usize << 30);
    assert!(fresh(high).is_some());
    while high - low > 64 {
        let mid = low + (high - low) / 2;
        if fresh(mid).is_some() {
            high = mid;
        } else {
            low = mid;
        }
    }
    let want = fresh(high).unwrap();
    // the run after the change holds no index of the last run beside the value, whose
    // budget a stop would not leave for a run that starts again
    for more in [0usize, 64, 256, 1024, 4096] {
        let (mut k, _) = started(&base, 1);
        k.add(&axioms("SubClassOf(:M :K)"));
        let out = k
            .classify(&Limits {
                max_memory: Some(high + more),
                ..limits(1)
            })
            .unwrap();
        assert_eq!(answer(&out), answer(&want));
        assert_eq!(out.taxonomy, want.taxonomy);
        assert_eq!(k.excluded(), Vec::new());
        assert!(!out.stats.incremental);
    }
}

#[test]
fn a_kept_run_that_is_cancelled_leaves_a_classifier_that_starts_again() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    for base in BOUNDED {
        let mut stopped = 0;
        for polls in 0..40usize {
            let (mut k, mut held) = started(base, 1);
            add(&mut k, &mut held, "SubClassOf(:A :N) SubClassOf(:N :K)");
            let seen = Arc::new(AtomicUsize::new(0));
            let s = seen.clone();
            let cancelled = Limits {
                cancelled: Some(Arc::new(move || s.fetch_add(1, Ordering::Relaxed) >= polls)),
                ..limits(1)
            };
            if k.classify(&cancelled).is_err() {
                stopped += 1;
            }
            let out = same_as_fresh(&mut k, &held, 1, &format!("after a stop at poll {polls}"));
            assert!(out.taxonomy.is_subclass(&c("A"), &c("L")));
            // a deadline that has passed stops the next run too
            add(&mut k, &mut held, "SubClassOf(:N :N2)");
            let late = Limits {
                deadline: Some(std::time::Instant::now()),
                ..limits(1)
            };
            assert!(k.classify(&late).is_err());
            let out = same_as_fresh(&mut k, &held, 1, "after a deadline");
            assert!(out.taxonomy.is_subclass(&c("A"), &c("N2")));
        }
        assert!(stopped > 0, "no run was cancelled");
    }
}

#[test]
fn a_run_starts_again_once_it_keeps_twice_the_expressions_of_a_full_run() {
    for base in BOUNDED {
        let (mut k, mut held) = started(base, 4);
        let mut restarts = 0;
        let mut kept = 0;
        for round in 0..7 {
            // each axiom interns a class and an existential restriction
            let more: String = (0..500)
                .map(|i| format!("SubClassOf(:K ObjectSomeValuesFrom(:r :N{round}x{i}))\n"))
                .collect();
            add(&mut k, &mut held, &more);
            let out = k.classify(&limits(4)).unwrap();
            let mut fresh = Classifier::of(&held);
            let want = fresh.classify(&limits(4)).unwrap();
            assert_eq!(out.taxonomy, want.taxonomy, "after batch {round}");
            assert_eq!(k.excluded(), fresh.excluded());
            restarts += usize::from(!out.stats.incremental);
            kept += usize::from(out.stats.incremental);
        }
        assert!(restarts >= 1, "the kept state never passed its bound");
        assert!(kept >= 4, "{kept} runs kept contexts");
    }
}

/// A universal over a transitive property with a chain into it whose operands no
/// existential restriction uses, so that the automaton that unfolds the universal has no
/// transition by them (roadmap item 693).
const DEAD_CHAIN: &str = "TransitiveObjectProperty(:t)
     SubObjectPropertyOf(ObjectPropertyChain(:v :w) :t)
     SubClassOf(:A ObjectAllValuesFrom(:t :B))
     SubClassOf(:A ObjectSomeValuesFrom(:t :C))
     SubClassOf(:C ObjectSomeValuesFrom(:t :D))
     SubClassOf(ObjectSomeValuesFrom(:t ObjectIntersectionOf(:D :B)) :P)
     SubClassOf(:E :A)
     DisjointClasses(:Q :B)
     SubClassOf(:K :L)";

#[test]
fn a_property_that_gains_its_first_link_gets_its_transitions_back() {
    for threads in [1, 4] {
        let (mut k, mut held) = started(DEAD_CHAIN, threads);
        let out = k.classify(&limits(threads)).unwrap();
        assert!(out.taxonomy.is_subclass(&c("E"), &c("P")));
        for round in 0..3 {
            // C reaches Q by v and w, which is a step of t, so A's universal reaches Q
            add(
                &mut k,
                &mut held,
                "SubClassOf(:C ObjectSomeValuesFrom(:v :V))
                 SubClassOf(:V ObjectSomeValuesFrom(:w :Q))",
            );
            let out = same_as_fresh(&mut k, &held, threads, "after the chain got links");
            assert!(out.stats.incremental, "the run started again");
            assert!(out.taxonomy.is_unsatisfiable(&c("E")), "in round {round}");
            assert!(!out.taxonomy.is_unsatisfiable(&c("C")), "in round {round}");
            // one operand without a link leaves the chain without an instance
            remove(
                &mut k,
                &mut held,
                "SubClassOf(:V ObjectSomeValuesFrom(:w :Q))",
            );
            let out = same_as_fresh(&mut k, &held, threads, "after w lost its link");
            assert!(out.stats.incremental, "the run started again");
            assert!(!out.taxonomy.is_unsatisfiable(&c("E")), "in round {round}");
            assert!(
                out.taxonomy.is_subclass(&c("E"), &c("P")),
                "in round {round}"
            );
            remove(
                &mut k,
                &mut held,
                "SubClassOf(:C ObjectSomeValuesFrom(:v :V))",
            );
            let out = same_as_fresh(&mut k, &held, threads, "after v lost its link");
            assert!(
                out.taxonomy.is_subclass(&c("E"), &c("P")),
                "in round {round}"
            );
            // a class beside the rest still keeps every context
            add(&mut k, &mut held, "SubClassOf(:M :K)");
            let out = same_as_fresh(&mut k, &held, threads, "after a leaf joined");
            assert!(out.stats.incremental, "the run started again");
            remove(&mut k, &mut held, "SubClassOf(:M :K)");
            same_as_fresh(&mut k, &held, threads, "after the leaf left");
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 2: EL data lattice & Item 972 DtIx checker incremental maintenance
// ---------------------------------------------------------------------------

/// Functional-syntax helper with XSD, RDF, RDFS, and OWL prefixes for data tests.
fn data_axioms(body: &str) -> Vec<Axiom> {
    let doc = ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(rdf:=<http://www.w3.org/1999/02/22-rdf-syntax-ns#>)\n\
         Prefix(rdfs:=<http://www.w3.org/2000/01/rdf-schema#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Ontology(\n{body}\n)"
    ))
    .unwrap();
    doc.axioms
}

fn data_started(base: &str, threads: usize) -> (Classifier, Vec<Axiom>) {
    let held = data_axioms(base);
    let mut k = Classifier::of(&held);
    let out = same_as_fresh(&mut k, &held, threads, "at the start");
    assert!(!out.stats.incremental);
    (k, held)
}

fn data_add(k: &mut Classifier, held: &mut Vec<Axiom>, body: &str) {
    let more = data_axioms(body);
    k.add(&more);
    held.extend(more);
}

fn data_remove(k: &mut Classifier, held: &mut Vec<Axiom>, body: &str) {
    let gone = data_axioms(body);
    k.remove(&gone);
    held.retain(|a| !gone.iter().any(|g| g.kind == a.kind));
}

#[test]
fn el_data_lattice_additions_deletions_and_functional_groups_stay_incremental() {
    let base = "SubDataPropertyOf(:d1 :df)
        SubDataPropertyOf(:d2 :df)
        FunctionalDataProperty(:df)
        DataPropertyRange(:d1 xsd:integer)
        SubClassOf(:A DataSomeValuesFrom(:d1 xsd:decimal))
        SubClassOf(:A DataHasValue(:d2 \"5\"^^xsd:integer))
        SubClassOf(:K :L)
        SubClassOf(:P DataSomeValuesFrom(:u xsd:string))
        SubClassOf(DataSomeValuesFrom(:u xsd:string) :Q)";
    for threads in [1, 4, 8] {
        let (mut k, mut held) = data_started(base, threads);
        let init = same_as_fresh(&mut k, &held, threads, "initial functional EL data");
        assert!(init.taxonomy.is_subclass(&c("P"), &c("Q")));
        assert!(!init.taxonomy.is_unsatisfiable(&c("A")));

        // Editing an unrelated class keeps all functional/data contexts untouched.
        data_add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(&mut k, &held, threads, "unrelated leaf added");
        assert!(out.stats.incremental);
        assert_eq!(out.stats.cleared, 0);

        // Adding a negative data restriction matching A's merged functional group (:d1 + :d2 -> 5)
        // must clear A via `changed_neg_roles` even though :d1 alone only had xsd:integer.
        let hit_d1 = "SubClassOf(DataHasValue(:d1 \"5.0\"^^xsd:decimal) :HitD1)";
        data_add(&mut k, &mut held, hit_d1);
        let out = same_as_fresh(&mut k, &held, threads, "negative data restriction on :d1");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("HitD1")));

        // Adding a negative data restriction on the functional superproperty :df also matches.
        let hit_df = "SubClassOf(DataHasValue(:df \"5\"^^xsd:integer) :HitDf)";
        data_add(&mut k, &mut held, hit_df);
        let out = same_as_fresh(&mut k, &held, threads, "negative data restriction on :df");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("HitDf")));

        // Removing the negative restriction retracts the subsumption incrementally.
        data_remove(&mut k, &mut held, hit_d1);
        let out = same_as_fresh(&mut k, &held, threads, "removed :d1 negative data");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("HitD1")));
        assert!(out.taxonomy.is_subclass(&c("A"), &c("HitDf")));

        // Adding a conflicting functional value makes A unsatisfiable; removing restores it.
        let conflict = "SubClassOf(:A DataHasValue(:d2 \"6\"^^xsd:integer))";
        data_add(&mut k, &mut held, conflict);
        let out = same_as_fresh(&mut k, &held, threads, "functional conflict added");
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_unsatisfiable(&c("A")));
        assert!(!out.taxonomy.is_unsatisfiable(&c("P")));

        data_remove(&mut k, &mut held, conflict);
        let out = same_as_fresh(&mut k, &held, threads, "functional conflict removed");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_unsatisfiable(&c("A")));
        assert!(out.taxonomy.is_subclass(&c("A"), &c("HitDf")));

        // Editing a non-functional EL data restriction on :u works incrementally too.
        data_remove(
            &mut k,
            &mut held,
            "SubClassOf(DataSomeValuesFrom(:u xsd:string) :Q)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "removed :u negative restriction");
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_subclass(&c("P"), &c("Q")));
    }
}

#[test]
fn horn_with_el_data_and_functional_groups_stays_incremental_and_falls_back_on_role_edits() {
    let base = "FunctionalDataProperty(:df)
        DataPropertyRange(:df xsd:integer)
        SubClassOf(:A ObjectAllValuesFrom(:r :B))
        SubClassOf(:A ObjectSomeValuesFrom(:r :C))
        SubClassOf(ObjectIntersectionOf(:B :C) DataHasValue(:df \"10\"^^xsd:integer))
        SubClassOf(:C DataHasValue(:df \"10\"^^xsd:integer))
        SubClassOf(DataSomeValuesFrom(:df xsd:decimal) :Num)
        SubClassOf(:K :L)";
    for threads in [1, 4, 8] {
        let (mut k, mut held) = data_started(base, threads);
        data_add(&mut k, &mut held, "SubClassOf(:M :K)");
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "unrelated edit in Horn+functional data",
        );
        assert!(out.stats.incremental);
        assert_eq!(out.stats.cleared, 0);

        // Adding a conflicting functional value under :B makes :B ⊓ :C (and thus :A) unsat.
        let bad = "SubClassOf(:B DataHasValue(:df \"11\"^^xsd:integer))";
        data_add(&mut k, &mut held, bad);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "functional conflict in Horn successor",
        );
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_unsatisfiable(&c("A")));

        data_remove(&mut k, &mut held, bad);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "functional conflict removed in Horn",
        );
        assert!(out.stats.incremental);
        assert!(!out.taxonomy.is_unsatisfiable(&c("A")));

        // Role-table and datatype-definition edits safely fall back to full rebuilds.
        data_add(
            &mut k,
            &mut held,
            "DataPropertyRange(:df xsd:nonNegativeInteger)",
        );
        let out = same_as_fresh(&mut k, &held, threads, "DataPropertyRange fallback");
        assert!(!out.stats.incremental);

        data_remove(&mut k, &mut held, "FunctionalDataProperty(:df)");
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "FunctionalDataProperty removal fallback",
        );
        assert!(!out.stats.incremental);

        data_add(
            &mut k,
            &mut held,
            "DatatypeDefinition(:SmallInt DatatypeRestriction(xsd:integer xsd:maxInclusive \"5\"^^xsd:integer))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "DatatypeDefinition fallback");
        assert!(!out.stats.incremental);

        data_add(
            &mut k,
            &mut held,
            "SubClassOf(:K DataSomeValuesFrom(owl:topDataProperty xsd:integer))",
        );
        let out = same_as_fresh(&mut k, &held, threads, "topDataProperty fallback");
        assert!(!out.stats.incremental);
    }
}

#[test]
fn removing_data_axiom_later_allows_tier_d_to_take_added_disjunction() {
    let base = "SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))
        SubClassOf(:A :G)";
    let (mut k, mut held) = data_started(base, 1);
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after removing data axiom");
    assert!(out.stats.incremental);
    // Now add a disjunction: stale_tier must force a fresh interner so Tier D takes it.
    data_add(
        &mut k,
        &mut held,
        "SubClassOf(:B ObjectUnionOf(:C :D))
         SubClassOf(:C :E)
         SubClassOf(:D :E)",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after adding disjunction");
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("B"), &c("E")));
    assert!(k.excluded().is_empty());
}

#[test]
fn deleting_data_axiom_with_mixed_zero_min_disjunction_restarts_into_tier_d() {
    // Tier D short-circuits ObjectMinCardinality(0, ..) to TOP without inspecting its
    // filler, so a disjunctive axiom containing data only beneath a zero minimum is
    // accepted by Tier D once the blocking EL data axiom is removed.
    let horn_base = "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
        SubClassOf(:P ObjectAllValuesFrom(:r :Q))
        SubClassOf(:A ObjectIntersectionOf(
            ObjectUnionOf(:B :C)
            ObjectMinCardinality(0 :r DataSomeValuesFrom(:z xsd:integer))))
        SubClassOf(:B :G)
        SubClassOf(:C :G)";
    let (mut k, mut held) = data_started(horn_base, 1);
    assert!(
        !k.classify(&limits(1))
            .unwrap()
            .taxonomy
            .is_subclass(&c("A"), &c("G"))
    );
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(
        &mut k,
        &held,
        1,
        "after deleting blocking EL data axiom with mixed zero-min disjunction (Horn)",
    );
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("G")));
    assert!(k.excluded().is_empty());

    // Same transition when the initial run had no Horn axiom (KeptTier::El).
    let el_base = "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
        SubClassOf(:A ObjectIntersectionOf(
            ObjectUnionOf(:B :C)
            ObjectMinCardinality(0 :r DataSomeValuesFrom(:z xsd:integer))))
        SubClassOf(:B :G)
        SubClassOf(:C :G)";
    let (mut k, mut held) = data_started(el_base, 1);
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(
        &mut k,
        &held,
        1,
        "after deleting blocking EL data axiom with mixed zero-min disjunction (EL)",
    );
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("G")));
    assert!(k.excluded().is_empty());

    // And when the mixed zero-min disjunction is added only after an earlier data deletion.
    let (mut k, mut held) = data_started(
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
         SubClassOf(:B :G)
         SubClassOf(:C :G)",
        1,
    );
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after deleting data axiom");
    assert!(out.stats.incremental);
    data_add(
        &mut k,
        &mut held,
        "SubClassOf(:A ObjectIntersectionOf(
            ObjectUnionOf(:B :C)
            ObjectMinCardinality(0 :r DataSomeValuesFrom(:z xsd:integer))))",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after adding mixed zero-min disjunction");
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("G")));
    assert!(k.excluded().is_empty());

    // DisjointClasses with 1 unique data operand and a repeated disjunctive non-data
    // operand is accepted by Tier D because Norm::axiom skips unique operands when
    // unique.len() < 2 while forcing every repeated operand empty.
    let disj_horn_base = "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
        SubClassOf(:P ObjectAllValuesFrom(:r :Q))
        DisjointClasses(
            DataSomeValuesFrom(:z xsd:integer)
            ObjectComplementOf(ObjectUnionOf(:B :C))
            ObjectComplementOf(ObjectUnionOf(:B :C)))
        SubClassOf(:B :G)
        SubClassOf(:C :G)";
    let (mut k, mut held) = data_started(disj_horn_base, 1);
    assert!(
        !k.classify(&limits(1))
            .unwrap()
            .taxonomy
            .is_subclass(&Class::thing(), &c("G"))
    );
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(
        &mut k,
        &held,
        1,
        "after deleting blocking EL data axiom with mixed DisjointClasses (Horn)",
    );
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("G")));
    assert!(k.excluded().is_empty());

    // Same DisjointClasses transition from KeptTier::El.
    let disj_el_base = "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
        DisjointClasses(
            DataSomeValuesFrom(:z xsd:integer)
            ObjectComplementOf(ObjectUnionOf(:B :C))
            ObjectComplementOf(ObjectUnionOf(:B :C)))
        SubClassOf(:B :G)
        SubClassOf(:C :G)";
    let (mut k, mut held) = data_started(disj_el_base, 1);
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(
        &mut k,
        &held,
        1,
        "after deleting blocking EL data axiom with mixed DisjointClasses (EL)",
    );
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("G")));
    assert!(k.excluded().is_empty());

    // And when the mixed DisjointClasses is added after an earlier data deletion.
    let (mut k, mut held) = data_started(
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))
         SubClassOf(:B :G)
         SubClassOf(:C :G)",
        1,
    );
    data_remove(
        &mut k,
        &mut held,
        "SubClassOf(:X DataSomeValuesFrom(:d xsd:integer))",
    );
    let out = same_as_fresh(
        &mut k,
        &held,
        1,
        "after deleting data axiom before DisjointClasses",
    );
    assert!(out.stats.incremental);
    data_add(
        &mut k,
        &mut held,
        "DisjointClasses(
            DataSomeValuesFrom(:z xsd:integer)
            ObjectComplementOf(ObjectUnionOf(:B :C))
            ObjectComplementOf(ObjectUnionOf(:B :C)))",
    );
    let out = same_as_fresh(&mut k, &held, 1, "after adding mixed DisjointClasses");
    assert!(!out.stats.incremental);
    assert!(out.taxonomy.is_subclass(&Class::thing(), &c("G")));
    assert!(k.excluded().is_empty());
}

#[test]
fn dtix_multi_component_edits_preserve_untouched_components_across_renumbering() {
    // Component 1 (:d): facet + cover; Component 2 (:e1, :e2): disjoint/cardinality;
    // Component 3 (:g): unrelated EL data property.
    let base = "SubClassOf(:A DataSomeValuesFrom(:d DataUnionOf(xsd:integer xsd:string)))
        SubClassOf(DataSomeValuesFrom(:d xsd:integer) :B1)
        SubClassOf(DataSomeValuesFrom(:d xsd:string) :B2)
        SubClassOf(:KeepD DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive \"10\"^^xsd:integer)))
        SubClassOf(DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive \"0\"^^xsd:integer)) :NonNeg)
        SubClassOf(:E DataMinCardinality(2 :e1 xsd:integer))
        SubClassOf(DataSomeValuesFrom(:e1 xsd:integer) :HasE1)
        SubClassOf(:E2 DataSomeValuesFrom(:e2 xsd:int))
        SubClassOf(:G DataSomeValuesFrom(:g xsd:string))
        SubClassOf(DataSomeValuesFrom(:g xsd:string) :GOut)
        SubClassOf(:K :L)";
    for threads in [1, 4, 8] {
        let (mut k, mut held) = data_started(base, threads);
        let init = same_as_fresh(&mut k, &held, threads, "initial multi-component DtIx");
        assert!(init.taxonomy.is_subclass(&c("KeepD"), &c("NonNeg")));
        assert!(init.taxonomy.is_subclass(&c("E"), &c("HasE1")));
        assert!(init.taxonomy.is_subclass(&c("G"), &c("GOut")));
        assert!(
            !k.excluded().is_empty(),
            "component :d should report COVER in excluded"
        );

        // Editing component :e1/:e2 (adding DisjointDataProperties, merging :e1 and :e2
        // and shifting component IDs!) must stay incremental and keep :KeepD and :A's COVER.
        let disj = "DisjointDataProperties(:e1 :e2)";
        data_add(&mut k, &mut held, disj);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "added DisjointDataProperties(:e1 :e2)",
        );
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("KeepD"), &c("NonNeg")));
        assert!(out.taxonomy.is_subclass(&c("G"), &c("GOut")));

        // Add a brand-new checker component :z to further shift component numbering.
        let comp_z = "SubClassOf(:Z DataMaxCardinality(1 :z xsd:integer))
            SubClassOf(:Z DataMinCardinality(2 :z xsd:integer))";
        data_add(&mut k, &mut held, comp_z);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "added brand-new checker component :z",
        );
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_unsatisfiable(&c("Z")));

        // Remove DisjointDataProperties(:e1 :e2) (splitting component back).
        data_remove(&mut k, &mut held, disj);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "removed DisjointDataProperties(:e1 :e2)",
        );
        assert!(out.stats.incremental);

        // Transition :A's component :d from COVER -> FINE by telling :A :B1 (DtFire refire!),
        // then back from FINE -> COVER.
        let resolve = "SubClassOf(:A :B1)";
        data_add(&mut k, &mut held, resolve);
        let out = same_as_fresh(&mut k, &held, threads, "resolved COVER via DtFire");
        assert!(out.stats.incremental);
        assert!(k.excluded().is_empty(), "COVER should have cleared");

        data_remove(&mut k, &mut held, resolve);
        let out = same_as_fresh(&mut k, &held, threads, "re-exposed COVER");
        assert!(out.stats.incremental);
        assert!(!k.excluded().is_empty(), "COVER should have returned");

        // Remove brand-new component :z completely.
        data_remove(&mut k, &mut held, comp_z);
        let out = same_as_fresh(&mut k, &held, threads, "removed component :z completely");
        assert!(out.stats.incremental);
    }
}

#[test]
fn dtix_orphan_and_cyclic_cover_cleanup_and_reconnection() {
    // Anonymous conjunction successor (:X ⊓ ∃d.(int ∪ str)) produces COVER on :d,
    // and links to a kept successor :U via :s, while :KeepD holds a FINE atom on :d.
    let base = "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectIntersectionOf(:X DataSomeValuesFrom(:d DataUnionOf(xsd:integer xsd:string)))))
        SubClassOf(DataSomeValuesFrom(:d xsd:integer) :B1)
        SubClassOf(DataSomeValuesFrom(:d xsd:string) :B2)
        SubClassOf(:X ObjectSomeValuesFrom(:s :U))
        SubClassOf(:U :V)
        SubClassOf(ObjectSomeValuesFrom(:s :V) :HasV)
        SubClassOf(:KeepD DataSomeValuesFrom(:d xsd:int))
        SubClassOf(:CycRoot ObjectSomeValuesFrom(:r ObjectIntersectionOf(:C1 :C2)))
        SubClassOf(ObjectIntersectionOf(:C1 :C2) ObjectSomeValuesFrom(:t ObjectIntersectionOf(:C2 :C3)))
        SubClassOf(ObjectIntersectionOf(:C2 :C3) ObjectSomeValuesFrom(:t ObjectIntersectionOf(:C1 :C2)))
        SubClassOf(ObjectIntersectionOf(:C2 :C3) DataSomeValuesFrom(:e DataUnionOf(xsd:integer xsd:string)))
        SubClassOf(DataSomeValuesFrom(:e xsd:integer) :E1)
        SubClassOf(DataSomeValuesFrom(:e xsd:string) :E2)
        SubClassOf(:KeepE DataSomeValuesFrom(:e xsd:int))";
    for threads in [1, 4, 8] {
        let (mut k, mut held) = data_started(base, threads);
        assert!(!k.excluded().is_empty());

        // Remove link from :A to the conjunction context: orphan_prune must clear it
        // so :d no longer reports COVER in excluded.
        let link_a = "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectIntersectionOf(:X DataSomeValuesFrom(:d DataUnionOf(xsd:integer xsd:string)))))";
        data_remove(&mut k, &mut held, link_a);
        let out = same_as_fresh(&mut k, &held, threads, "removed link to COVER orphan");
        assert!(out.stats.incremental);

        // Remove link from :CycRoot to the disconnected 2-cycle of conjunction contexts:
        // forward reachability in orphan_prune must clear the entire cycle so :e's COVER
        // also disappears!
        let link_cyc =
            "SubClassOf(:CycRoot ObjectSomeValuesFrom(:r ObjectIntersectionOf(:C1 :C2)))";
        data_remove(&mut k, &mut held, link_cyc);
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "removed link to cyclic COVER orphan",
        );
        assert!(out.stats.incremental);
        assert!(
            k.excluded().is_empty(),
            "all orphan COVERs must be pruned: {:?}",
            k.excluded()
        );

        // Re-connect :A to the previously pruned conjunction context (which links to kept :U)
        // and verify :U replays its propagations (:HasV) cleanly!
        data_add(&mut k, &mut held, link_a);
        data_add(
            &mut k,
            &mut held,
            "SubClassOf(ObjectSomeValuesFrom(:r :HasV) :AReachV)",
        );
        let out = same_as_fresh(
            &mut k,
            &held,
            threads,
            "reconnected pruned orphan to kept :U",
        );
        assert!(out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("AReachV")));
    }
}

#[test]
fn el_lattice_to_and_from_dtix_checker_transitions_fall_back_safely() {
    for threads in [1, 4, 8] {
        // 1. Whole ontology transitions: None -> Some -> None
        let (mut k, mut held) = data_started(
            "SubClassOf(:A DataSomeValuesFrom(:d xsd:integer))
             SubClassOf(DataSomeValuesFrom(:d xsd:decimal) :B)",
            threads,
        );
        let facet = "SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive \"1\"^^xsd:integer)))";
        data_add(&mut k, &mut held, facet);
        let out = same_as_fresh(&mut k, &held, threads, "whole ontology EL -> DtIx");
        assert!(!out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));

        data_remove(&mut k, &mut held, facet);
        let out = same_as_fresh(&mut k, &held, threads, "whole ontology DtIx -> EL");
        assert!(!out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));

        // 2. Single property switches mode while another checker component (:c) remains active:
        //    :e switches EL -> checker and then checker -> EL.
        let (mut k, mut held) = data_started(
            "SubClassOf(:C DataAllValuesFrom(:c xsd:integer))
             SubClassOf(:C DataSomeValuesFrom(:c xsd:string))
             SubClassOf(:A DataSomeValuesFrom(:e xsd:integer))
             SubClassOf(DataSomeValuesFrom(:e xsd:decimal) :B)",
            threads,
        );
        let e_trigger = "SubClassOf(:A DataAllValuesFrom(:e xsd:integer))";
        data_add(&mut k, &mut held, e_trigger);
        let out = same_as_fresh(&mut k, &held, threads, "property :e EL -> checker");
        assert!(!out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
        assert!(out.taxonomy.is_unsatisfiable(&c("C")));

        data_remove(&mut k, &mut held, e_trigger);
        let out = same_as_fresh(&mut k, &held, threads, "property :e checker -> EL");
        assert!(!out.stats.incremental);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("B")));
        assert!(out.taxonomy.is_unsatisfiable(&c("C")));
    }
}

#[test]
fn incremental_data_and_dtix_memory_budget_and_cancellation() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let pad: String = (0..48)
        .map(|i| format!("SubClassOf(:Pad{i} :L)\n"))
        .collect();
    let base = format!(
        "FunctionalDataProperty(:df)
         SubClassOf(:A DataSomeValuesFrom(:d DatatypeRestriction(xsd:integer xsd:minInclusive \"0\"^^xsd:integer)))
         SubClassOf(DataSomeValuesFrom(:d xsd:integer) :NumD)
         SubClassOf(:E DataMinCardinality(2 :e xsd:integer))
         SubClassOf(DataSomeValuesFrom(:e xsd:integer) :HasE)
         SubClassOf(:F DataHasValue(:df \"1\"^^xsd:integer))
         SubClassOf(:K :L)
         {pad}"
    );
    let edit = "SubClassOf(:M :K)\nSubClassOf(:E2 DataAllValuesFrom(:e xsd:integer))";
    let mut held = data_axioms(&base);
    held.extend(data_axioms(edit));

    // Find tight fresh budget for the edited ontology.
    let fresh = |limit: usize| {
        let mut k = Classifier::of(&held);
        let out = k
            .classify(&Limits {
                max_memory: Some(limit),
                ..limits(1)
            })
            .ok()?;
        k.excluded().is_empty().then_some(out)
    };
    let (mut low, mut high) = (0usize, 1usize << 30);
    assert!(fresh(high).is_some());
    while high - low > 128 {
        let mid = low + (high - low) / 2;
        if fresh(mid).is_some() {
            high = mid;
        } else {
            low = mid;
        }
    }
    let want = fresh(high).unwrap();

    // Tight memory cap + pool charge callback must never exceed cap and must match fresh.
    for more in [0usize, 512, 4096] {
        let cap = high + more;
        let granted = Arc::new(AtomicUsize::new(0));
        let g = granted.clone();
        let bounded = Limits {
            max_memory: Some(cap),
            charge: Some(Arc::new(move |bytes| {
                let prev = g.fetch_add(bytes, Ordering::Relaxed);
                if prev.saturating_add(bytes) > cap {
                    g.fetch_sub(bytes, Ordering::Relaxed);
                    false
                } else {
                    true
                }
            })),
            ..limits(1)
        };
        let (mut k, _) = data_started(&base, 1);
        k.add(&data_axioms(edit));
        let out = k.classify(&bounded).unwrap();
        assert!(
            granted.load(Ordering::Relaxed) <= cap,
            "pool charge exceeded cap {cap}"
        );
        assert_eq!(answer(&out), answer(&want));
        assert_eq!(out.taxonomy, want.taxonomy);
        assert_eq!(k.excluded(), Vec::new());
    }

    // Pool-credited LateRestart::Incompatible on EL-lattice -> DtIx mode switch:
    // total pool grant must stay bounded by peak (not double-charging build).
    let (mut k, mut mode_held) = data_started(
        "SubClassOf(:C DataAllValuesFrom(:c xsd:integer))
         SubClassOf(:A DataSomeValuesFrom(:e xsd:integer))
         SubClassOf(DataSomeValuesFrom(:e xsd:decimal) :B)",
        1,
    );
    let switch = "SubClassOf(:A DataAllValuesFrom(:e xsd:integer))";
    data_add(&mut k, &mut mode_held, switch);
    let granted = Arc::new(AtomicUsize::new(0));
    let g = granted.clone();
    let out = k
        .classify(&Limits {
            charge: Some(Arc::new(move |bytes| {
                g.fetch_add(bytes, Ordering::Relaxed);
                true
            })),
            ..limits(1)
        })
        .unwrap();
    assert!(!out.stats.incremental);
    assert!(
        granted.load(Ordering::Relaxed) <= out.stats.peak_memory,
        "granted {} exceeded reported peak {}",
        granted.load(Ordering::Relaxed),
        out.stats.peak_memory
    );

    // Cooperative cancellation & deadline polling across incremental data + DtIx edits.
    let mut stopped = 0;
    for polls in 0..50usize {
        let (mut k, mut h) = data_started(&base, 1);
        data_add(&mut k, &mut h, edit);
        let seen = Arc::new(AtomicUsize::new(0));
        let s = seen.clone();
        let cancelled = Limits {
            cancelled: Some(Arc::new(move || s.fetch_add(1, Ordering::Relaxed) >= polls)),
            ..limits(1)
        };
        if k.classify(&cancelled).is_err() {
            stopped += 1;
        }
        same_as_fresh(&mut k, &h, 1, &format!("after cancel at poll {polls}"));
    }
    assert!(stopped > 0, "expected at least one cancellation stop");

    let (mut k, mut h) = data_started(&base, 1);
    data_add(&mut k, &mut h, edit);
    let late = Limits {
        deadline: Some(std::time::Instant::now()),
        ..limits(1)
    };
    assert!(k.classify(&late).is_err());
    same_as_fresh(&mut k, &h, 1, "after expired deadline");
}

#[test]
fn random_horn_el_data_and_dtix_edit_sequences_match_fresh() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    let xsd = |s: &str| Datatype::new(format!("http://www.w3.org/2001/XMLSchema#{s}"));
    let dp = |s: &str| DataProperty::new(format!("{EX}{s}"));
    let op = |s: &str| ObjectPropertyExpression::Property(ObjectProperty::new(format!("{EX}{s}")));
    let cl = |i: usize| ClassExpression::Class(c(&format!("C{i}")));

    let random_data_expr = |rng: &mut Rng,
                            prop_idx: usize,
                            allow_checker: bool|
     -> ClassExpression {
        let p = dp(&format!("d{prop_idx}"));
        let kind = if allow_checker {
            rng.below(9)
        } else {
            rng.below(4)
        };
        match kind {
            0 => ClassExpression::DataSomeValuesFrom(vec![p], DataRange::Datatype(xsd("integer"))),
            1 => ClassExpression::DataSomeValuesFrom(vec![p], DataRange::Datatype(xsd("decimal"))),
            2 => ClassExpression::DataSomeValuesFrom(vec![p], DataRange::Datatype(xsd("string"))),
            3 => {
                let v = rng.below(3);
                ClassExpression::DataHasValue(
                    p,
                    Literal::typed(v.to_string(), "http://www.w3.org/2001/XMLSchema#integer"),
                )
            }
            4 => {
                let bound = rng.below(4);
                ClassExpression::DataSomeValuesFrom(
                    vec![p],
                    DataRange::DatatypeRestriction(
                        xsd("integer"),
                        std::collections::BTreeSet::from([FacetRestriction {
                            facet: Iri::new("http://www.w3.org/2001/XMLSchema#minInclusive")
                                .unwrap(),
                            value: Literal::typed(
                                bound.to_string(),
                                "http://www.w3.org/2001/XMLSchema#integer",
                            ),
                        }]),
                    ),
                )
            }
            5 => ClassExpression::DataSomeValuesFrom(
                vec![p],
                DataRange::UnionOf(std::collections::BTreeSet::from([
                    DataRange::Datatype(xsd("integer")),
                    DataRange::Datatype(xsd("string")),
                ])),
            ),
            6 => ClassExpression::DataAllValuesFrom(
                vec![p],
                DataRange::Datatype(if rng.below(2) == 0 {
                    xsd("integer")
                } else {
                    xsd("string")
                }),
            ),
            7 => ClassExpression::DataMinCardinality(
                1 + rng.below(2) as u32,
                p,
                Some(DataRange::Datatype(xsd("integer"))),
            ),
            _ => {
                ClassExpression::DataMaxCardinality(1, p, Some(DataRange::Datatype(xsd("integer"))))
            }
        }
    };

    let random_axiom = |rng: &mut Rng| -> Axiom {
        match rng.below(14) {
            0..=3 => {
                // EL-data property d0 (functional) or d1 (plain)
                let prop = rng.below(2);
                let de = random_data_expr(rng, prop, false);
                let a = cl(rng.below(6));
                if rng.below(2) == 0 {
                    Axiom::sub_class_of(a, de)
                } else {
                    Axiom::sub_class_of(de, a)
                }
            }
            4..=8 => {
                // Checker properties d2, d3, or mode-switching d4
                let prop = 2 + rng.below(3);
                let allow_checker = prop < 4 || rng.below(3) == 0;
                let de = random_data_expr(rng, prop, allow_checker);
                let a = cl(rng.below(6));
                if rng.below(2) == 0 {
                    Axiom::sub_class_of(a, de)
                } else {
                    Axiom::sub_class_of(de, a)
                }
            }
            9 => {
                // Horn object restriction carrying a data restriction
                let a = cl(rng.below(6));
                let b = cl(rng.below(6));
                if rng.below(2) == 0 {
                    Axiom::sub_class_of(a, ClassExpression::some(op("r"), b))
                } else {
                    Axiom::sub_class_of(a, ClassExpression::all(op("r"), b))
                }
            }
            10 => Axiom::sub_class_of(cl(rng.below(6)), cl(rng.below(6))),
            11 => {
                let a = rng.below(6);
                let b = (a + 1 + rng.below(5)) % 6;
                Axiom::disjoint_classes([cl(a), cl(b)])
            }
            12 => Axiom::new(AxiomKind::DisjointDataProperties(
                vec![dp("d2"), dp("d3")].into(),
            )),
            _ => Axiom::new(AxiomKind::DataPropertyDomain(
                dp(&format!("d{}", rng.below(4))),
                cl(rng.below(6)),
            )),
        }
    };

    let mut total_steps = 0usize;
    let mut incremental_steps = 0usize;
    let mut kept_some = 0usize;
    for threads in [1, 4, 8] {
        for seed in 1..=12u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (threads as u64));
            let mut held = data_axioms(
                "FunctionalDataProperty(:d0)
                 DataPropertyRange(:d1 xsd:integer)
                 SubClassOf(:C0 DataAllValuesFrom(:d2 xsd:integer))
                 SubClassOf(:C1 DataSomeValuesFrom(:d3 xsd:int))
                 Declaration(Class(:C2))
                 Declaration(Class(:C3))
                 Declaration(Class(:C4))
                 Declaration(Class(:C5))",
            );
            for _ in 0..8 {
                let a = random_axiom(&mut rng);
                if !held.iter().any(|h| h.kind == a.kind) {
                    held.push(a);
                }
            }
            let mut k = Classifier::of(&held);
            same_as_fresh(&mut k, &held, threads, &format!("seed {seed} init"));
            for step in 0..10 {
                // Remove 0..=2 editable axioms (keep the first 2 role axioms fixed most of the time).
                let mut removed = Vec::new();
                for _ in 0..rng.below(3) {
                    if held.len() > 8 {
                        let idx = 2 + rng.below(held.len() - 2);
                        removed.push(held.remove(idx));
                    }
                }
                let mut added = Vec::new();
                for _ in 0..1 + rng.below(2) {
                    let a = random_axiom(&mut rng);
                    if !held.iter().any(|h| h.kind == a.kind)
                        && !added.iter().any(|h: &Axiom| h.kind == a.kind)
                    {
                        added.push(a);
                    }
                }
                k.remove(&removed);
                k.add(&added);
                held.extend(added);
                let out = same_as_fresh(
                    &mut k,
                    &held,
                    threads,
                    &format!("threads {threads} seed {seed} step {step}"),
                );
                total_steps += 1;
                if out.stats.incremental {
                    incremental_steps += 1;
                    if out.stats.cleared < out.taxonomy.classes().count() {
                        kept_some += 1;
                    }
                }
            }
        }
    }
    assert!(
        incremental_steps * 2 >= total_steps,
        "expected most data/DtIx edits to run incrementally: {incremental_steps}/{total_steps}"
    );
    assert!(
        kept_some > 0,
        "expected incremental data/DtIx runs to keep untouched classes"
    );
}
