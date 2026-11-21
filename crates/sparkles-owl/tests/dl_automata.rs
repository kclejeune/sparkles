//! The reduced automata of Tier D (roadmap item 712). Tier D unfolds a universal
//! restriction on a property that chains or transitivity reach by the property's
//! automaton, and [`Limits::dl_automata`] says how far the automaton is reduced first: 0 not
//! at all, 1 merging the states that accept the same words, and 2, the default, also
//! dropping a transition that another from the same state into the same state covers by a
//! superproperty. Every level must give the reference reasoners' answer.
//!
//! `tests/fixtures/dl712` holds hand-written cases, `hand-NAME.ofn`, in which a merge of a
//! final state with one that is not, or a dropped transition by the superproperty instead
//! of the subproperty, changes the taxonomy, through property hierarchies, inverses,
//! negative existential restrictions, chains, equivalent properties and a nominal, and 200
//! generated ones, `gen-NNN.ofn`, which its `generate.py` writes with its default seed.
//! HermiT 1.4.5.519 classified each into `NAME.hermit.ofn` and Konclude 0.7.0-1138 into
//! `NAME.konclude.ofn`, which `reference.py` writes. The expected answer is every
//! subsumption that either entails. HermiT misses `Q1 ⊑ W1` of `hand-equivalent-roles`,
//! whose property is transitive only through an equivalence, and on some runs `W0 ⊑ W1` of
//! `gen-003`, and Konclude misses two subsumptions of `hand-nominal` that its assertions
//! give.

use sparkles_owl::classify::{Classifier, Limits, Stats, Tier};
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
    /// the subsumptions of the reference taxonomies, when the case has them
    want: Option<BTreeSet<(String, String)>>,
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
    // what either reasoner entails, since each misses subsumptions that the other finds
    let want = ["hermit.ofn", "konclude.ofn"]
        .iter()
        .filter_map(|e| std::fs::read_to_string(path.with_extension(e)).ok())
        .map(|t| reference(&t, &classes))
        .reduce(|a, b| &a | &b);
    Case {
        stem: path.file_stem().unwrap().to_str().unwrap().to_string(),
        axioms: doc.axioms,
        classes,
        want,
    }
}

/// The cases of a fixture directory whose names start with one of `prefixes`.
fn cases(dir: &str, prefixes: &[&str]) -> Vec<Case> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures(dir))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.ends_with(".ofn")
                    && n.matches('.').count() == 1
                    && prefixes.iter().any(|f| n.starts_with(f))
            })
        })
        .collect();
    paths.sort();
    paths.iter().map(|p| case(p)).collect()
}

/// What a run gave: the subsumptions between the case's classes, whether it took the case
/// whole, and whether it fell back or answered in part.
struct Answer {
    got: BTreeSet<(String, String)>,
    whole: bool,
    fell: bool,
    stats: Stats,
}

fn run(case: &Case, limits: &Limits) -> Answer {
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
    Answer {
        got,
        whole: k.excluded().is_empty(),
        fell: out.stats.dl_over_budget || out.stats.dl_attempt_over || !out.complete,
        stats: out.stats,
    }
}

fn limits(level: u8, threads: usize, rooted: bool) -> Limits {
    Limits {
        threads,
        counting: true,
        rooted,
        dl_automata: Some(level),
        ..Limits::default()
    }
}

/// Each hand-written case is taken whole by Tier D at every level, on one thread and on
/// four, with the reference answer, and the reduced automata derive less than the unreduced
/// ones. A merge that ignored which states are final, or a covered transition dropped by
/// the superproperty instead of the subproperty, changes the answer of every case. The run
/// with the default limits answers as level 2 does.
#[test]
fn the_hand_cases_give_the_reference_answer_at_every_level() {
    let cases = cases("dl712", &["hand-"]);
    assert_eq!(cases.len(), 7);
    let mut failures = Vec::new();
    for case in &cases {
        let want = case.want.as_ref().unwrap();
        let rooted = case.stem == "hand-nominal";
        let mut work = Vec::new();
        for level in 0..=2 {
            for threads in [1, 4] {
                let a = run(case, &limits(level, threads, rooted));
                let at = format!("{} at level {level} on {threads} threads", case.stem);
                if a.stats.tier != Tier::Disjunctive || !a.whole || a.fell {
                    failures.push(format!("{at}: not taken whole by Tier D"));
                }
                if a.got != *want {
                    let extra: Vec<_> = a.got.difference(want).collect();
                    let missing: Vec<_> = want.difference(&a.got).collect();
                    failures.push(format!("{at}: not entailed {extra:?}, missing {missing:?}"));
                }
                if threads == 1 {
                    work.push(a.stats.conclusions);
                }
            }
        }
        if work[2] >= work[0] {
            failures.push(format!("{}: work by level {work:?}", case.stem));
        }
        let default = run(
            case,
            &Limits {
                threads: 1,
                counting: true,
                rooted,
                ..Limits::default()
            },
        );
        if default.got != *want || default.stats.conclusions != work[2] {
            failures.push(format!("{}: the default is not level 2", case.stem));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The generated cases at levels 0 and 2 on one thread: every answer is one that the
/// references entail, an answer of a case taken whole that did not fall back is theirs,
/// and the two levels give one answer when neither fell back. The counts show what the
/// cases exercise.
#[test]
fn the_generated_cases_give_the_reference_answer_at_levels_0_and_2() {
    let cases = cases("dl712", &["gen-"]);
    assert_eq!(cases.len(), 200);
    let mut failures = Vec::new();
    // cases taken whole by Tier D at both levels, and those whose work the reduction
    // changed
    let (mut whole, mut changed) = (0, 0);
    for case in &cases {
        let want = case.want.as_ref().unwrap();
        let answers: Vec<Answer> = [0, 2]
            .iter()
            .map(|&l| run(case, &limits(l, 1, false)))
            .collect();
        for (a, level) in answers.iter().zip([0, 2]) {
            let extra: Vec<_> = a.got.difference(want).collect();
            if !extra.is_empty() {
                failures.push(format!(
                    "{} at level {level}: not entailed {extra:?}",
                    case.stem
                ));
            }
            let missing: Vec<_> = want.difference(&a.got).collect();
            if a.whole && !a.fell && !missing.is_empty() {
                failures.push(format!(
                    "{} at level {level}: missing {missing:?}",
                    case.stem
                ));
            }
        }
        let [a, b] = &answers[..] else { unreachable!() };
        if !a.fell && !b.fell && a.got != b.got {
            failures.push(format!("{}: the levels answer differently", case.stem));
        }
        if answers
            .iter()
            .all(|a| a.whole && !a.fell && a.stats.tier == Tier::Disjunctive)
        {
            whole += 1;
            changed += (a.stats.conclusions != b.stats.conclusions) as usize;
        }
    }
    eprintln!("{whole} taken whole by Tier D, {changed} with other conclusions");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!((whole, changed), (WHOLE, CHANGED));
}

/// The generated cases that Tier D takes whole at both levels, and those of them whose
/// conclusions the reduction changes.
const WHOLE: usize = 200;
const CHANGED: usize = 107;

/// The cases of the corpus whose conclusions the reduction changes: 107 generated cases
/// and the hand-written ones other than `hand-nominal`, which falls back without the
/// rooted rules. None of the fixtures of items 603 and 897 has an automaton that the
/// reduction changes.
const CORPUS_CHANGED: usize = 113;

/// Levels 0 and 2 give one answer on the fixtures of items 603, 897 and 712 on one thread when
/// neither run falls back, and the run that falls back answers no more than the other.
#[test]
fn levels_0_and_2_answer_alike_across_the_fixture_corpus() {
    let mut all = cases("dl603", &[""]);
    all.extend(cases("dl897", &["qz-"]));
    all.extend(cases("dl712", &[""]));
    assert_eq!(all.len(), 640 + 27 + 207);
    let mut failures = Vec::new();
    let mut changed = 0;
    for case in &all {
        let a = run(case, &limits(0, 1, false));
        let b = run(case, &limits(2, 1, false));
        match (a.fell, b.fell) {
            (false, false) if a.got != b.got => {
                failures.push(format!("{}: the levels answer differently", case.stem))
            }
            (true, false) if !a.got.is_subset(&b.got) => {
                failures.push(format!("{}: level 0 fell back to more", case.stem))
            }
            (false, true) if !b.got.is_subset(&a.got) => {
                failures.push(format!("{}: level 2 fell back to more", case.stem))
            }
            _ => {}
        }
        changed += (a.stats.conclusions != b.stats.conclusions) as usize;
    }
    eprintln!("{changed} cases with other work");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(changed, CORPUS_CHANGED);
}
