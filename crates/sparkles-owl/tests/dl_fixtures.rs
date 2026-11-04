//! The fixture suite of roadmap item 603: 600 random OWL 2 DL ontologies in twelve
//! families and 40 hand-written cases, which `tests/fixtures/dl603/generate.py` writes,
//! each classified by HermiT 1.4.5.519 and Konclude 0.7.0-1138 into `NAME.hermit.ofn` and
//! `NAME.konclude.ofn` (the reference batch of the lane's run record).
//!
//! Every subsumption between named classes that the classifier answers must be one that
//! HermiT entails, on every case, and on a case the classifier takes whole, with no axiom
//! left out, it must answer exactly HermiT's subsumptions. The cases of the Horn families
//! and the Horn hand cases that the Horn rules take whole are listed in
//! [`HORN_WHOLE`]'s count, so that a rule that stops taking a case shows.

use sparkles_owl::classify::{Classifier, Expansion, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The cases of the families `horn-shiq` and `horn-sroiq` that the classifier takes
/// whole. The others name individuals, which the Horn rules of this phase leave to a
/// later step.
const HORN_WHOLE: usize = 54;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603")
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

fn family(name: &str) -> &str {
    if name.starts_with("hand-") {
        "hand"
    } else {
        name.rsplit_once('-').map_or(name, |(f, _)| f)
    }
}

#[test]
fn the_classifier_is_sound_everywhere_and_complete_on_what_it_takes_whole() {
    let dir = fixtures();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 640);
    let mut failures = Vec::new();
    // by family: cases, cases taken whole
    let mut counts: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for case in &cases {
        let stem = case
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let want = std::fs::read_to_string(case.with_extension("hermit.ofn")).unwrap();
        let doc = ofn_read::parse(&std::fs::read_to_string(case).unwrap()).unwrap();
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
        let want = reference(&want, &classes);
        let mut answers = Vec::new();
        // whether each run fell back to the Horn rules or answered in part
        let mut fell = Vec::new();
        let mut whole = true;
        // both expansion strategies of Tier D give one answer
        for (threads, expansion) in [
            (1, Expansion::Cautious),
            (4, Expansion::Cautious),
            (4, Expansion::Eager),
        ] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    ..Limits::default()
                })
                .unwrap();
            let this = k.excluded().is_empty();
            if !answers.is_empty() && this != whole {
                failures.push(format!("{stem}: what is left out depends on the run"));
            }
            whole = this;
            let mut got = BTreeSet::new();
            for a in &classes {
                for b in &classes {
                    if out
                        .taxonomy
                        .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
                    {
                        got.insert((a.clone(), b.clone()));
                    }
                }
            }
            answers.push(got);
            fell.push(out.stats.dl_over_budget || !out.complete);
        }
        // the runs that stayed in Tier D's bound give one answer, and so do those that fell
        // back: a schedule can decide which of the two a run gives near the bound, and the
        // run that fell back says so
        for k in 1..answers.len() {
            if fell[k] == fell[0] && answers[k] != answers[0] {
                failures.push(format!(
                    "{stem}: the answer depends on the threads or the expansion strategy"
                ));
            }
        }
        // every run is sound, and one taken whole that did not fall back is complete
        for (got, &fell) in answers.iter().zip(&fell) {
            let extra: Vec<_> = got.difference(&want).collect();
            if !extra.is_empty() {
                failures.push(format!("{stem}: not entailed {extra:?}"));
            }
            if whole && !fell {
                let missing: Vec<_> = want.difference(got).collect();
                if !missing.is_empty() {
                    failures.push(format!("{stem}: missing {missing:?}"));
                }
            }
        }
        let c = counts.entry(family(stem).to_string()).or_default();
        c.0 += 1;
        c.1 += whole as usize;
    }
    eprintln!("{counts:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    let horn: usize = ["horn-shiq", "horn-sroiq"]
        .iter()
        .map(|f| counts.get(*f).map_or(0, |c| c.1))
        .sum();
    assert_eq!(horn, HORN_WHOLE, "{counts:?}");
}

/// The counting cases whose equalities of successor terms once grew past any bound: each is
/// taken whole by Tier D, within its bound on derived conclusions, on one thread and on
/// four with both strategies, with HermiT's answer, which the test above checks.
#[test]
fn the_counting_cases_with_many_successor_equalities_finish_in_tier_d() {
    for stem in [
        "shq-004", "shq-007", "shiq-013", "shiq-038", "shiq-046", "sriq-010",
    ] {
        let path = fixtures().join(format!("{stem}.ofn"));
        let doc = ofn_read::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for (threads, expansion) in [
            (1, Expansion::Cautious),
            (4, Expansion::Cautious),
            (4, Expansion::Eager),
        ] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    dl_work: Some(1 << 23),
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                !out.stats.dl_over_budget,
                "{stem} on {threads} threads, {expansion:?}: Tier D passed its bound"
            );
        }
    }
}

/// The counting cases that pass Tier D's bound on derived conclusions: every run, on one
/// thread or several and with either strategy, passes it and falls back to the Horn rules,
/// so that the answer is the same for every schedule.
#[test]
fn the_cases_past_the_bound_fall_back_on_every_schedule() {
    for stem in ["shq-041", "shiq-028"] {
        let path = fixtures().join(format!("{stem}.ofn"));
        let doc = ofn_read::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let mut classes: Vec<Class> = doc
            .axioms
            .iter()
            .filter_map(|a| match &a.kind {
                AxiomKind::Declaration(Entity::Class(c)) => Some(c.clone()),
                _ => None,
            })
            .collect();
        classes.push(Class::new(THING));
        classes.push(Class::new(NOTHING));
        let mut answers = BTreeSet::new();
        for threads in [1, 4, 8] {
            for expansion in [Expansion::Cautious, Expansion::Eager] {
                let mut k = Classifier::of(&doc.axioms);
                let out = k
                    .classify(&Limits {
                        threads,
                        expansion,
                        ..Limits::default()
                    })
                    .unwrap();
                assert!(
                    out.stats.dl_over_budget,
                    "{stem} on {threads} threads, {expansion:?}: Tier D stayed in its bound"
                );
                let mut got = BTreeSet::new();
                for a in &classes {
                    for b in &classes {
                        if out.taxonomy.is_subclass(a, b) {
                            got.insert((a.as_str().to_string(), b.as_str().to_string()));
                        }
                    }
                }
                answers.insert(got);
            }
        }
        assert_eq!(
            answers.len(),
            1,
            "{stem}: the answer depends on the schedule"
        );
    }
}
