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
            whole = k.excluded().is_empty();
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
        }
        if answers[0] != answers[1] {
            failures.push(format!("{stem}: the answer depends on the threads"));
        }
        if answers[1] != answers[2] {
            failures.push(format!(
                "{stem}: the answer depends on the expansion strategy"
            ));
        }
        let got = &answers[0];
        let extra: Vec<_> = got.difference(&want).collect();
        if !extra.is_empty() {
            failures.push(format!("{stem}: not entailed {extra:?}"));
        }
        if whole {
            let missing: Vec<_> = want.difference(got).collect();
            if !missing.is_empty() {
                failures.push(format!("{stem}: missing {missing:?}"));
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
