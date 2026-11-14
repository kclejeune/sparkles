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
        let mut gen_axioms = |n: usize, next: &mut dyn FnMut() -> u64| -> Vec<Axiom> {
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
