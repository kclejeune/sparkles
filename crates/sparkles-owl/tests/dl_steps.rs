//! Roadmap item 727: with `Limits::dl_steps` a run of the disjunctive rules that has a
//! bound on its derived conclusions derives the same conclusions on every number of
//! threads, so that it answers in full or falls back in the same way on every schedule.
//!
//! The cases are the 150 counting fixtures of item 603 and the generated ontologies of
//! item 897, with HermiT's taxonomies beside them.

use sparkles_owl::classify::{Classifier, Expansion, Limits, Stats};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

fn fixtures(dir: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(dir)
}

fn iris(line: &str) -> Vec<String> {
    line.split('<')
        .skip(1)
        .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
        .collect()
}

/// The subsumptions between `classes` that a reference taxonomy entails.
fn reference(text: &str, classes: &BTreeSet<String>) -> BTreeSet<(String, String)> {
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
    let above = |c: &str| -> BTreeSet<String> {
        let mut seen = BTreeSet::from([c.to_string(), THING.to_string()]);
        let mut stack = vec![c.to_string()];
        while let Some(x) = stack.pop() {
            for y in up.get(&x).into_iter().flatten() {
                if seen.insert(y.clone()) {
                    stack.push(y.clone());
                }
            }
        }
        seen
    };
    let inconsistent = above(THING).contains(NOTHING);
    let mut out = BTreeSet::new();
    for a in classes {
        let s = above(a);
        let unsat = inconsistent || s.contains(NOTHING);
        for b in classes {
            if unsat || s.contains(b) || b == THING || a == NOTHING {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

struct Case {
    stem: String,
    axioms: Vec<Axiom>,
    classes: BTreeSet<String>,
    want: BTreeSet<(String, String)>,
}

fn case(path: &Path) -> Case {
    let doc = ofn_read::parse(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut classes: BTreeSet<String> = doc
        .axioms
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::Class(c)) => Some(c.as_str().to_string()),
            _ => None,
        })
        .collect();
    classes.insert(THING.to_string());
    classes.insert(NOTHING.to_string());
    let want = reference(
        &std::fs::read_to_string(path.with_extension("hermit.ofn")).unwrap(),
        &classes,
    );
    Case {
        stem: path.file_stem().unwrap().to_str().unwrap().to_string(),
        axioms: doc.axioms,
        classes,
        want,
    }
}

/// The counting fixtures of item 603, the families `shq`, `shiq` and `sriq`, and the
/// generated ontologies of item 897.
fn cases() -> Vec<Case> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for (dir, families) in [
        ("dl603", &["shq-", "shiq-", "sriq-"][..]),
        ("dl897", &["qz-"][..]),
    ] {
        paths.extend(
            std::fs::read_dir(fixtures(dir))
                .unwrap()
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                        n.ends_with(".ofn")
                            && n.matches('.').count() == 1
                            && families.iter().any(|f| n.starts_with(f))
                    })
                }),
        );
    }
    paths.sort();
    assert_eq!(paths.len(), 177);
    paths.iter().map(|p| case(p)).collect()
}

/// What a run gave: the subsumptions between the case's classes, and whether it took the
/// case whole.
type Answer = (BTreeSet<(String, String)>, bool);

fn run(case: &Case, limits: &Limits) -> (Answer, Stats) {
    let mut k = Classifier::of(&case.axioms);
    let out = k.classify(limits).unwrap();
    let mut got = BTreeSet::new();
    for a in &case.classes {
        for b in &case.classes {
            if out
                .taxonomy
                .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
            {
                got.insert((a.clone(), b.clone()));
            }
        }
    }
    ((got, k.excluded().is_empty()), out.stats)
}

/// What must not depend on the threads: the answer, the conclusions Tier D derived and
/// stored, the conclusions of the saturation when Tier D gave the answer, and where the
/// run fell back. A run that the Horn rules answer, such as `sriq-018`, whose number
/// restriction is on a role that is not simple, applies a number of conclusions that
/// depends on its threads, and the option does not cover it.
fn outcome(case: &Case, limits: &Limits) -> (Answer, [usize; 3], [bool; 3]) {
    let (answer, s) = run(case, limits);
    let applied = if s.dl_work > 0 { s.conclusions } else { 0 };
    (
        answer,
        [s.dl_work, s.dl_stored, applied],
        [s.dl_over_budget, s.dl_counting_over, s.dl_rooted_over],
    )
}

fn steps(threads: usize) -> Limits {
    Limits {
        threads,
        counting: true,
        dl_steps: true,
        ..Limits::default()
    }
}

/// Every counting case derives the same conclusions and gives the same answer on one,
/// two, four and eight threads with the option, whether it stays within the default bound
/// or passes it. A run that stays within it and takes the case whole has HermiT's
/// answer, and every other answer is among HermiT's subsumptions.
#[test]
fn a_run_in_steps_derives_the_same_on_every_number_of_threads() {
    let mut failures = Vec::new();
    let mut within = 0;
    for case in &cases() {
        let first = outcome(case, &steps(1));
        for threads in [2, 4, 8] {
            if outcome(case, &steps(threads)) != first {
                failures.push(format!("{}: {threads} threads differ from one", case.stem));
            }
        }
        let ((got, whole), _, [over, counting_over, _]) = first;
        if !got.is_subset(&case.want) {
            failures.push(format!("{}: not entailed", case.stem));
        }
        if !over && !counting_over {
            within += 1;
            if whole && got != case.want {
                failures.push(format!("{}: not HermiT's answer", case.stem));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    // the cases that stay within the default bound, so that a change of the steps shows
    assert_eq!(within, WITHIN);
}

/// The cases of [`cases`] that a run in steps finishes within the default bound.
const WITHIN: usize = 171;

/// One case run many times on one, four and eight threads gives one outcome, at the
/// default bound, which it keeps, at a bound equal to the conclusions it derives, which
/// it keeps, and at one conclusion less, which it passes on every run.
#[test]
fn one_case_run_many_times_gives_one_outcome() {
    let case = case(&fixtures("dl897").join("qz-0048.ofn"));
    let first = outcome(&case, &steps(1));
    let ((got, whole), [work, ..], [over, counting_over, _]) = first.clone();
    assert!(whole && !over && !counting_over && got == case.want);
    for dl_work in [None, Some(work), Some(work - 1)] {
        let mut outcomes = BTreeSet::new();
        for _ in 0..12 {
            for threads in [1, 4, 8] {
                outcomes.insert(outcome(
                    &case,
                    &Limits {
                        dl_work,
                        ..steps(threads)
                    },
                ));
            }
        }
        assert_eq!(outcomes.len(), 1, "bound {dl_work:?}");
        let (_, [w, ..], [_, counting_over, _]) = outcomes.pop_first().unwrap();
        assert_eq!(
            counting_over,
            dl_work == Some(work - 1),
            "bound {dl_work:?}"
        );
        if !counting_over {
            assert_eq!(w, work);
        }
    }
}

/// The option changes no answer of a run that stays within its bound: on the cases that
/// both drivers finish, the answers are equal.
#[test]
fn a_run_in_steps_answers_as_the_default_run_does() {
    for case in cases().iter().step_by(3) {
        let limits = |dl_steps: bool| Limits {
            threads: 4,
            counting: true,
            dl_steps,
            dl_work: Some(1 << 21),
            ..Limits::default()
        };
        let (a, sa) = run(case, &limits(true));
        let (b, sb) = run(case, &limits(false));
        if !sa.dl_counting_over && !sb.dl_counting_over {
            assert_eq!(a, b, "{}", case.stem);
        }
    }
}

/// A run with the eager strategy and a rooted run take one thread with the option, so
/// that they too give one outcome on every number of threads.
#[test]
fn eager_and_rooted_runs_in_steps_give_one_outcome() {
    let all = cases();
    for case in all.iter().step_by(15) {
        let limits = |threads: usize| Limits {
            expansion: Expansion::Eager,
            ..steps(threads)
        };
        let first = outcome(case, &limits(1));
        for threads in [4, 8] {
            assert_eq!(
                outcome(case, &limits(threads)),
                first,
                "{} eager",
                case.stem
            );
        }
    }
    let mut shoi: Vec<PathBuf> = std::fs::read_dir(fixtures("dl603"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("shoi-") && n.ends_with(".ofn") && n.matches('.').count() == 1
            })
        })
        .collect();
    shoi.sort();
    for path in shoi.iter().step_by(5) {
        let case = case(path);
        let limits = |threads: usize| Limits {
            rooted: true,
            ..steps(threads)
        };
        let first = outcome(&case, &limits(1));
        for threads in [4, 8] {
            assert_eq!(
                outcome(&case, &limits(threads)),
                first,
                "{} rooted",
                case.stem
            );
        }
    }
}
