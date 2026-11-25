//! The fixture suite of roadmap item 603: 600 random OWL 2 DL ontologies in twelve
//! families and 40 hand-written cases, which `tests/fixtures/dl603/generate.py` writes,
//! each classified by HermiT 1.4.5.519 and Konclude 0.7.0-1138 into `NAME.hermit.ofn` and
//! `NAME.konclude.ofn` (the reference batch of the lane's run record).
//!
//! Every subsumption between named classes that the classifier answers must be one that
//! HermiT entails, on every case, and on a case the classifier takes whole, with no axiom
//! left out, it must answer exactly HermiT's subsumptions. The cases of the two Horn
//! families that the classifier takes whole are counted in [`HORN_WHOLE`], so that a rule
//! that stops taking a case shows.

use sparkles_owl::classify::{Classifier, Expansion, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The cases of the families `horn-shiq` and `horn-sroiq` that the classifier takes
/// whole: all 50 of `horn-shiq` and all 50 of `horn-sroiq`, including the 15 cases with
/// at-least restrictions above one beside individuals (roadmap item 867).
const HORN_WHOLE: usize = 100;

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
                    counting: true,
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
            fell.push(out.stats.dl_over_budget || !out.complete_for_taken);
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
        let fam = family(stem);
        if matches!(fam, "horn-shiq" | "horn-sroiq") && fell.iter().any(|&f| f) {
            failures.push(format!("{stem}: Horn fixture fell back or was incomplete"));
        }
        let c = counts.entry(fam.to_string()).or_default();
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
/// taken whole by Tier D, within a raised bound on derived conclusions, on one thread and on
/// four with both strategies, with HermiT's answer.
#[test]
fn the_counting_cases_with_many_successor_equalities_finish_in_tier_d() {
    for stem in [
        "shq-004", "shq-007", "shiq-013", "shiq-038", "shiq-046", "sriq-010",
    ] {
        let path = fixtures().join(format!("{stem}.ofn"));
        let doc = ofn_read::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
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
                    counting: true,
                    dl_work: Some(1 << 23),
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                !out.stats.dl_over_budget,
                "{stem} on {threads} threads, {expansion:?}: Tier D passed its bound"
            );
            // sriq-010 names an individual, which Tier D leaves out, so its answer is sound
            // and the others' is HermiT's
            let whole = k.excluded().is_empty();
            assert!(
                whole || stem.starts_with("sriq"),
                "{stem}: an axiom is left out"
            );
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
            if whole {
                assert_eq!(got, want, "{stem} on {threads} threads, {expansion:?}");
            } else {
                assert!(
                    got.is_subset(&want),
                    "{stem} on {threads} threads, {expansion:?}"
                );
            }
        }
    }
}

/// The counting case that passes Tier D's bound on derived conclusions: each run, on one,
/// four and eight threads and with either strategy, passes it and classifies again without
/// counting, and these runs give one answer. `shiq-028` passes the bound with the order
/// between number restrictions, which is the default, and without it.
#[test]
fn the_cases_past_the_bound_fall_back_on_every_schedule() {
    for (stem, number_order) in [("shiq-028", false), ("shiq-028", true)] {
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
                        counting: true,
                        number_order,
                        ..Limits::default()
                    })
                    .unwrap();
                assert!(
                    out.stats.dl_over_budget,
                    "{stem} on {threads} threads, {expansion:?}, order {number_order}: \
                     Tier D stayed in its bound"
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
            "{stem}, order {number_order}: the answer depends on the schedule"
        );
    }
}

/// The generated ontologies of roadmap item 897, whose number restrictions have compound
/// qualifiers: `tests/fixtures/dl897/generate.py` wrote 300 with its default seed, and
/// these are the 25 on which a run that gave such a qualifier no name reported an answer
/// other than HermiT's as complete, with `qz-0202` and `qz-0082`, which finished then and
/// pass the bound on derived conclusions now. Each of the 25 is taken whole within the
/// default bound with HermiT's answer, on one thread, four and eight. `qz-0048` stays
/// within the bound on one thread because a context gives way to the contexts it woke:
/// the context of `C1` equates six successors under a maximum of three, and the
/// successors' contexts find that `C0` has no model. The other two fall back on every
/// schedule to an answer that HermiT entails.
#[test]
fn the_compound_qualifier_cases_give_hermits_answer_within_the_bound() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl897");
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
    assert_eq!(cases.len(), 27);
    let mut failures = Vec::new();
    for case in &cases {
        let stem = case
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let past = ["qz-0082", "qz-0202"].contains(&stem);
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
        let want = reference(
            &std::fs::read_to_string(case.with_extension("hermit.ofn")).unwrap(),
            &classes,
        );
        for threads in [1, 4, 8] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    counting: true,
                    ..Limits::default()
                })
                .unwrap();
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
            if out.stats.dl_over_budget != past {
                failures.push(format!(
                    "{stem} on {threads} threads: Tier D {} its bound",
                    if past { "stayed in" } else { "passed" }
                ));
            }
            if past {
                if !got.is_subset(&want) {
                    failures.push(format!("{stem} on {threads} threads: not entailed"));
                }
            } else if !k.excluded().is_empty() || got != want {
                failures.push(format!("{stem} on {threads} threads: not HermiT's answer"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// `shq-041` without the order between number restrictions finishes within the default
/// bound with HermiT's answer, on one, four and eight threads and with either strategy,
/// since a context gives way to the contexts it woke. Before, the run on one thread passed
/// a bound of 8 million conclusions. `tests/dl_number_order.rs` holds its answer with the
/// order.
#[test]
fn a_context_that_gives_way_keeps_shq_041_within_the_bound_without_the_order() {
    let path = fixtures().join("shq-041.ofn");
    let doc = ofn_read::parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
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
    for threads in [1, 4, 8] {
        for expansion in [Expansion::Cautious, Expansion::Eager] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    counting: true,
                    number_order: false,
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                !out.stats.dl_over_budget,
                "{threads} threads, {expansion:?}: Tier D passed its bound"
            );
            assert!(k.excluded().is_empty(), "an axiom is left out");
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
            assert_eq!(got, want, "{threads} threads, {expansion:?}");
        }
    }
}

/// With `Limits::rooted`, the rooted run of Tier D takes the `shoi` fixtures, which have
/// individuals and nominals. The run gives HermiT's taxonomy on every fixture it finishes
/// within its bound on derived conclusions, and the two fixtures that pass the bound fall
/// back to the answer of a run without the option, which is sound, on every schedule.
#[test]
fn the_rooted_run_is_exact_on_the_shoi_fixtures_within_its_bound() {
    let dir = fixtures();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("shoi-") && n.ends_with(".ofn") && n.matches('.').count() == 1
            })
        })
        .collect();
    cases.sort();
    assert_eq!(cases.len(), 50);
    let past_bound = ["shoi-022", "shoi-042"];
    let mut exact = 0;
    let mut failures = Vec::new();
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
        for (threads, expansion) in [(1, Expansion::Cautious), (4, Expansion::Eager)] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    rooted: true,
                    ..Limits::default()
                })
                .unwrap();
            assert!(
                out.complete_for_taken,
                "{stem}: a run without a deadline answered in part"
            );
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
            let extra = got.difference(&want).count();
            let missing = want.difference(&got).count();
            let fell = out.stats.dl_rooted_over;
            if extra > 0 {
                failures.push(format!("{stem} {threads} {expansion:?}: {extra} wrong"));
            }
            if fell != past_bound.contains(&stem) {
                failures.push(format!(
                    "{stem} {threads} {expansion:?}: fell back {fell}, work {}",
                    out.stats.dl_work
                ));
            }
            if fell {
                assert!(out.stats.dl_over_budget);
                assert!(
                    !k.excluded().is_empty(),
                    "{stem}: nothing reported after the fallback"
                );
                // the fallback's answer is the answer of a run without the option
                let mut plain = Classifier::of(&doc.axioms);
                let want = plain
                    .classify(&Limits {
                        threads,
                        expansion,
                        ..Limits::default()
                    })
                    .unwrap();
                let mut plain_got = BTreeSet::new();
                for a in &classes {
                    for b in &classes {
                        if want
                            .taxonomy
                            .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
                        {
                            plain_got.insert((a.clone(), b.clone()));
                        }
                    }
                }
                if got != plain_got
                    || out.consistency != want.consistency
                    || k.excluded().len() != plain.excluded().len()
                    || out.stats.tier != want.stats.tier
                {
                    failures.push(format!(
                        "{stem} {threads} {expansion:?}: the fallback differs from the default answer"
                    ));
                }
            } else {
                if !k.excluded().is_empty() {
                    failures.push(format!(
                        "{stem} {threads} {expansion:?}: {:?}",
                        k.excluded()
                    ));
                }
                if missing > 0 {
                    failures.push(format!("{stem} {threads} {expansion:?}: {missing} missing"));
                }
                exact += 1;
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(exact, 96);
}

/// The coverage of a run that leaves axioms out (roadmap item 1271): on every case, for
/// the default run and for a run by the OWL 2 EL rules alone, each class outside
/// `incomplete` has HermiT's superclasses, and HermiT's equivalent classes when it is
/// satisfiable, and a known consistency is HermiT's.
#[test]
fn the_classes_outside_incomplete_have_hermits_answer() {
    use sparkles_owl::classify::Consistency;
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
    // runs that left something out, the classes they asked about, those they called
    // complete, and the runs whose consistency stayed known
    let (mut left_runs, mut asked, mut decided, mut known) = (0usize, 0usize, 0usize, 0usize);
    for case in &cases {
        let stem = case
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
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
        let text = std::fs::read_to_string(case.with_extension("hermit.ofn")).unwrap();
        let want = reference(&text, &classes);
        let inconsistent = want.contains(&(THING.to_string(), NOTHING.to_string()));
        for el_only in [false, true] {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads: 1,
                    el_only,
                    ..Limits::default()
                })
                .unwrap();
            let run = format!("{stem}, el_only {el_only}");
            let left = !k.excluded().is_empty();
            if out.complete != (!left && out.complete_for_taken) {
                failures.push(format!("{run}: complete {}", out.complete));
            }
            if !left || out.stats.dl_over_budget {
                continue;
            }
            left_runs += 1;
            match out.consistency {
                Consistency::Unknown => {}
                c => {
                    known += 1;
                    if (c == Consistency::Inconsistent) != inconsistent {
                        failures.push(format!("{run}: consistency {c:?}"));
                    }
                }
            }
            let sub = |a: &str, b: &str| out.taxonomy.is_subclass(&Class::new(a), &Class::new(b));
            for a in &classes {
                if a == THING || a == NOTHING {
                    continue;
                }
                asked += 1;
                if out.incomplete.contains(&Class::new(a.as_str())) {
                    continue;
                }
                decided += 1;
                let unsat = want.contains(&(a.clone(), NOTHING.to_string()));
                for b in &classes {
                    let w = want.contains(&(a.clone(), b.clone()));
                    if w != sub(a, b) {
                        failures.push(format!("{run}: {a} below {b}: {w} in HermiT"));
                    }
                    if !unsat && w && want.contains(&(b.clone(), a.clone())) && !sub(b, a) {
                        failures.push(format!("{run}: {b} equivalent to {a} missing"));
                    }
                }
            }
        }
    }
    eprintln!(
        "runs that left axioms out {left_runs}, consistency known {known}, classes {asked}, complete {decided}"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(decided > 0 && decided < asked && known > 0);
}
