//! The bound on the conclusions of a run of Tier D without counting or the rooted rules
//! (roadmap item 990): a run that passes it ends and the Horn rules answer, as past the
//! run's memory budget, a run that keeps the contexts of the last one (roadmap item 753)
//! counts the conclusions of the runs since the last that started again and starts again
//! in full past the bound, and the kept and the fresh run agree at every bound. On one
//! thread a product of more choices than its limit ends the attempt too (roadmap item
//! 1011).

use sparkles_owl::classify::{Classification, Classifier, Excluded, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

fn limits(threads: usize, bound: Option<usize>) -> Limits {
    Limits {
        threads,
        dl_attempt_work: bound,
        ..Limits::default()
    }
}

/// A bound no run here reaches.
const UNREACHED: usize = usize::MAX;

/// What a kept and a fresh run must share: every subsumption, the unsatisfiable classes,
/// the consistency, whether the bound ended the attempt, and the axioms left out.
type Answer = (
    Vec<(String, String)>,
    Vec<String>,
    bool,
    bool,
    bool,
    Vec<Excluded>,
);

fn answer(o: &Classification, k: &Classifier) -> Answer {
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
    (
        pairs,
        unsat,
        o.consistent,
        o.stats.dl_attempt_over,
        o.stats.dl_over_budget,
        k.excluded(),
    )
}

/// Unions under an existential restriction: the case analysis gives `A ⊑ D` and `F ⊑ G`.
const UNIONS: &str = "SubClassOf(:A ObjectUnionOf(:B :C))
     SubClassOf(:B :D)
     SubClassOf(:C :D)
     SubClassOf(:E :A)
     SubClassOf(:F ObjectSomeValuesFrom(:r :A))
     SubClassOf(ObjectSomeValuesFrom(:r :D) :G)
     SubClassOf(:K :L)";

/// The edit of the kept runs: a class below `A` with a union of its own, which the kept
/// run saturates with the rules of Tier D.
const EDIT: &str = "SubClassOf(:M :A) SubClassOf(:M ObjectUnionOf(:P :Q))
     SubClassOf(:P :L) SubClassOf(:Q :L)";

#[test]
fn an_attempt_past_its_bound_takes_the_answer_of_the_horn_rules() {
    let held = axioms(UNIONS);
    // what the run derives on one thread, whose order is fixed
    let work = {
        let mut k = Classifier::of(&held);
        k.classify(&limits(1, None)).unwrap().stats.dl_work
    };
    assert!(work > 1, "{work}");
    for threads in [1, 4, 8] {
        // the default bound: the run finishes with the case analysis
        let mut k = Classifier::of(&held);
        let out = k.classify(&limits(threads, None)).unwrap();
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert!(!out.stats.dl_attempt_over && !out.stats.dl_over_budget);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(k.excluded().is_empty(), "{:?}", k.excluded());
        // a bound below what the run derives: the Horn rules answer, which leave the
        // union out and report it, as past the memory budget. On more threads the run
        // can derive more or fewer, so the bound there is one conclusion.
        let below = if threads == 1 { work - 1 } else { 1 };
        let mut k = Classifier::of(&held);
        let out = k.classify(&limits(threads, Some(below))).unwrap();
        assert!(out.stats.dl_attempt_over, "{threads} threads");
        assert!(out.stats.dl_over_budget);
        assert!(!out.stats.dl_counting_over && !out.stats.dl_rooted_over);
        assert!(!out.taxonomy.is_subclass(&c("A"), &c("D")));
        assert!(out.taxonomy.is_subclass(&c("E"), &c("A")));
        assert_eq!(k.excluded().len(), 1, "{:?}", k.excluded());
        // the same answer as a run past its memory budget
        let (m, over) = past_memory(&held, threads);
        assert_eq!(over.taxonomy, out.taxonomy);
        assert_eq!(m.excluded(), k.excluded());
        // a bound at what the run derives keeps the whole answer
        if threads == 1 {
            let mut k = Classifier::of(&held);
            let out = k.classify(&limits(threads, Some(work))).unwrap();
            assert!(!out.stats.dl_attempt_over);
            assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
        }
        // no bound
        let mut k = Classifier::of(&held);
        let out = k.classify(&limits(threads, Some(0))).unwrap();
        assert!(!out.stats.dl_attempt_over);
        assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
    }
}

/// Universals over inverses beside a union, from item 710's generator, whose case
/// analysis on one thread makes a product of four choices.
const WIDE: &str = "EquivalentClasses(:A3 ObjectSomeValuesFrom(:r1 ObjectSomeValuesFrom(:r0 :A0)))
     SubClassOf(:A0 ObjectAllValuesFrom(:r2 :A3))
     SubClassOf(ObjectIntersectionOf(:A6 :A3) :A3)
     SubClassOf(ObjectIntersectionOf(:A2 :A1) :A4)
     SubClassOf(:A7 :A7)
     SubClassOf(ObjectSomeValuesFrom(ObjectInverseOf(:r1) ObjectSomeValuesFrom(:r1 :A5)) :A1)
     SubClassOf(:A4 :A7)
     SubClassOf(:A5 ObjectSomeValuesFrom(:r1 ObjectIntersectionOf(:A2 :A5)))
     SubClassOf(:A3 :A1)
     SubObjectPropertyOf(:r1 :r2)
     SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))";

#[test]
fn an_attempt_on_one_thread_past_its_width_takes_the_answer_of_the_horn_rules() {
    let held = axioms(WIDE);
    let run = |threads: usize, width: Option<usize>| {
        let mut k = Classifier::of(&held);
        let out = k
            .classify(&Limits {
                threads,
                dl_attempt_width: width,
                ..Limits::default()
            })
            .unwrap();
        let a = answer(&out, &k);
        (out, a)
    };
    let (full, whole) = run(1, None);
    assert_eq!(full.stats.tier, Tier::Disjunctive);
    assert!(!full.stats.dl_attempt_over && !full.stats.dl_over_budget);
    // a product of more choices than the limit ends the attempt as its bound does, and
    // the Horn rules answer
    let (wide, horn) = run(1, Some(3));
    assert!(wide.stats.dl_attempt_over && wide.stats.dl_over_budget);
    assert!(!wide.stats.dl_counting_over && !wide.stats.dl_rooted_over);
    let mut k = Classifier::of(&held);
    let out = k.classify(&limits(1, Some(1))).unwrap();
    assert!(out.stats.dl_attempt_over);
    assert_eq!(answer(&out, &k), horn);
    assert_ne!(horn, whole);
    // a limit at the product's choices, and none, keep the whole answer
    assert_eq!(run(1, Some(4)).1, whole);
    assert_eq!(run(1, Some(0)).1, whole);
    // without a bound on its conclusions the limit still ends the attempt, which would
    // otherwise answer without the product's conclusions
    let mut k = Classifier::of(&held);
    let out = k
        .classify(&Limits {
            threads: 1,
            dl_attempt_work: Some(0),
            dl_attempt_width: Some(3),
            ..Limits::default()
        })
        .unwrap();
    assert!(out.stats.dl_attempt_over && out.stats.dl_over_budget);
    assert_eq!(answer(&out, &k), horn);
    // and a run that asks for a partial answer takes the answer it takes past its bound
    let partial = |work: Option<usize>, width: Option<usize>| {
        let mut k = Classifier::of(&held);
        let out = k
            .classify(&Limits {
                threads: 1,
                dl_attempt_work: work,
                dl_attempt_width: width,
                partial: true,
                ..Limits::default()
            })
            .unwrap();
        let a = answer(&out, &k);
        (out.complete, out.stats.dl_attempt_over, a)
    };
    let past = partial(Some(1), Some(0));
    assert!(past.1);
    assert_eq!(partial(Some(0), Some(3)), past);
    assert_eq!(partial(None, Some(3)), past);
    // on more threads, where the width of a product depends on the schedule, the run has
    // no such limit
    for threads in [4, 8] {
        let (o, a) = run(threads, Some(3));
        assert!(!o.stats.dl_attempt_over, "{threads} threads");
        assert_eq!(a, whole, "{threads} threads");
    }
}

#[test]
fn a_kept_run_past_its_width_answers_as_a_run_that_starts_again() {
    let width = |w: Option<usize>| Limits {
        threads: 1,
        dl_attempt_width: w,
        ..Limits::default()
    };
    let mut k = Classifier::of(&axioms(UNIONS));
    let out = k.classify(&width(Some(3))).unwrap();
    assert!(!out.stats.dl_attempt_over);
    // the edit brings the product of four choices, which the kept run makes too
    k.add(&axioms(WIDE));
    let out = k.classify(&width(Some(3))).unwrap();
    assert!(out.stats.dl_attempt_over && out.stats.dl_over_budget);
    let mut held = axioms(UNIONS);
    held.extend(axioms(WIDE));
    let mut f = Classifier::of(&held);
    let want = f.classify(&width(Some(3))).unwrap();
    assert!(want.stats.dl_attempt_over);
    assert_eq!(answer(&out, &k), answer(&want, &f));
    // without the edit's product the kept run answers in full again
    k.remove(&axioms(WIDE));
    let out = k.classify(&width(Some(3))).unwrap();
    assert!(!out.stats.dl_attempt_over && !out.stats.dl_over_budget);
    assert!(out.taxonomy.is_subclass(&c("A"), &c("D")));
}

/// A run over `held` that passes the memory budget of the disjunctive rules and gets the
/// answer of the Horn rules, with its classifier: the least budget with which a run
/// answers in full is found by bisection, and a budget a little below it lets the Horn
/// rules answer.
fn past_memory(held: &[Axiom], threads: usize) -> (Classifier, Classification) {
    let run = |limit: usize| {
        let mut k = Classifier::of(held);
        let out = k.classify(&Limits {
            threads,
            max_memory: Some(limit),
            ..Limits::default()
        });
        (k, out)
    };
    let full = |limit: usize| {
        run(limit)
            .1
            .is_ok_and(|o| !o.stats.dl_over_budget && !o.stats.horn_over_budget)
    };
    let (mut low, mut high) = (0usize, 1usize << 30);
    assert!(full(high));
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if full(mid) {
            high = mid;
        } else {
            low = mid;
        }
    }
    for less in 1..4096 {
        let (k, out) = run(high - less);
        if let Ok(out) = out
            && out.stats.dl_over_budget
            && !out.stats.horn_over_budget
        {
            assert!(!out.stats.dl_attempt_over);
            return (k, out);
        }
    }
    panic!("no budget below {high} lets the Horn rules answer");
}

#[test]
fn the_attempts_of_the_fixtures_finish_within_the_default_bound() {
    let mut cases = Vec::new();
    for dir in ["dl603", "dl897"] {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(dir);
        cases.extend(
            std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
                }),
        );
    }
    cases.sort();
    assert!(cases.len() > 600, "{}", cases.len());
    let mut attempts = 0;
    for case in &cases {
        let doc = ofn_read::parse(&std::fs::read_to_string(case).unwrap()).unwrap();
        let mut k = Classifier::of(&doc.axioms);
        let out = k.classify(&limits(4, None)).unwrap();
        assert!(
            !out.stats.dl_attempt_over,
            "{}: {} conclusions",
            case.display(),
            out.stats.dl_work
        );
        attempts += usize::from(out.stats.dl_work > 0);
    }
    assert!(attempts > 100, "{attempts}");
}

/// The conclusions of a fresh run over `held`, of the kept run after the edit, and of a
/// fresh run over the edited axioms.
fn counts(threads: usize) -> (usize, usize, usize) {
    let mut held = axioms(UNIONS);
    let mut k = Classifier::of(&held);
    let first = k.classify(&limits(threads, Some(UNREACHED))).unwrap();
    let more = axioms(EDIT);
    k.add(&more);
    held.extend(more);
    let kept = k.classify(&limits(threads, Some(UNREACHED))).unwrap();
    assert!(kept.stats.incremental, "the edit's run started again");
    let mut f = Classifier::of(&held);
    let fresh = f.classify(&limits(threads, Some(UNREACHED))).unwrap();
    (first.stats.dl_work, kept.stats.dl_work, fresh.stats.dl_work)
}

/// The edit after [`EDIT`]: `EDIT` leaves and a class below `A` comes, which the case
/// analysis puts below `D`.
const SWAP: &str = "SubClassOf(:N :A)";

#[test]
fn a_kept_run_that_passes_the_bound_of_its_attempt_starts_again_in_full() {
    // one thread, whose order fixes the counts
    let threads = 1;
    let unbounded = limits(threads, Some(UNREACHED));
    // the conclusions of the first run, of the kept run after the edit and of the kept
    // run after the swap, and of a run that starts again over the axioms after the swap
    let mut k = Classifier::of(&axioms(UNIONS));
    let first = k.classify(&unbounded).unwrap().stats.dl_work;
    k.add(&axioms(EDIT));
    let added = k.classify(&unbounded).unwrap();
    assert!(added.stats.incremental);
    k.remove(&axioms(EDIT));
    k.add(&axioms(SWAP));
    let swapped = k.classify(&unbounded).unwrap();
    assert!(swapped.stats.incremental);
    let (added, swapped) = (added.stats.dl_work, swapped.stats.dl_work);
    let mut held = axioms(UNIONS);
    held.extend(axioms(SWAP));
    let fresh = Classifier::of(&held)
        .classify(&unbounded)
        .unwrap()
        .stats
        .dl_work;
    // A bound that the first two runs keep and the third passes with their conclusions,
    // while a run that starts again over the axioms of the third keeps it. The third
    // starts again in full and answers in full.
    let bound = (first + added).max(fresh);
    assert!(
        bound < first + added + swapped,
        "{first} {added} {swapped} {fresh}"
    );
    let mut k = Classifier::of(&axioms(UNIONS));
    let out = k.classify(&limits(threads, Some(bound))).unwrap();
    assert!(!out.stats.dl_attempt_over);
    k.add(&axioms(EDIT));
    let out = k.classify(&limits(threads, Some(bound))).unwrap();
    assert!(out.stats.incremental && !out.stats.dl_attempt_over);
    assert!(out.taxonomy.is_subclass(&c("M"), &c("L")));
    k.remove(&axioms(EDIT));
    k.add(&axioms(SWAP));
    let out = k.classify(&limits(threads, Some(bound))).unwrap();
    assert!(!out.stats.incremental, "the kept run did not start again");
    assert!(
        !out.stats.dl_attempt_over,
        "the run that started again fell back"
    );
    assert_eq!(out.stats.dl_work, fresh);
    let mut f = Classifier::of(&held);
    let want = f.classify(&limits(threads, Some(bound))).unwrap();
    assert_eq!(answer(&out, &k), answer(&want, &f));
    assert_eq!(out.taxonomy, want.taxonomy);
    assert!(out.taxonomy.is_subclass(&c("N"), &c("D")));
    // the run that started again counts from its own conclusions, and the next keeps
    // contexts again within the same bound
    k.add(&axioms("SubClassOf(:M2 :K)"));
    let out = k.classify(&limits(threads, Some(bound))).unwrap();
    assert!(out.stats.incremental && !out.stats.dl_attempt_over);
}

#[test]
fn kept_and_fresh_runs_agree_on_the_attempt_at_every_bound() {
    // one thread, whose order fixes the counts, so that the runs near the bound are the
    // same on every repetition
    for threads in [1] {
        let (first, more, fresh) = counts(threads);
        // at every bound up to past what both runs together derive
        let mut fell = [0usize; 2];
        for bound in 1..=first + more + fresh + 2 {
            let mut held = axioms(UNIONS);
            let mut k = Classifier::of(&held);
            k.classify(&limits(threads, Some(bound))).unwrap();
            let edit = axioms(EDIT);
            k.add(&edit);
            held.extend(edit);
            let out = k.classify(&limits(threads, Some(bound))).unwrap();
            let mut f = Classifier::of(&held);
            let want = f.classify(&limits(threads, Some(bound))).unwrap();
            assert_eq!(
                answer(&out, &k),
                answer(&want, &f),
                "{threads} threads, bound {bound}: the kept and the fresh run differ"
            );
            assert_eq!(out.taxonomy, want.taxonomy);
            fell[usize::from(want.stats.dl_attempt_over)] += 1;
        }
        // both answers occur
        assert!(fell[0] > 0 && fell[1] > 0, "{fell:?}");
    }
}
