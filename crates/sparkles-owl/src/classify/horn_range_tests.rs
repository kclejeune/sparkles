//! The range universals that the Horn normalization leaves out when the ranges of the
//! links that end a role's paths already give them (roadmap item 693). The fixtures of
//! `tests/fixtures/horn_ranges` carry HermiT's taxonomy beside them, and each is
//! classified with the universals left out as the rule says, with every one kept, and,
//! for the cases that need one, with none, which must lose an answer.

use super::horn::{RangeUniversals, with_range_universals};
use super::{Classifier, Limits};
use crate::model::*;
use crate::ofn_read;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

fn dir(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn iris(line: &str) -> Vec<String> {
    line.split('<')
        .skip(1)
        .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
        .collect()
}

/// The classes a HermiT taxonomy names and the subsumptions between them it entails,
/// closed under transitivity.
fn hermit(text: &str) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    let mut classes = BTreeSet::new();
    let mut up: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let ns = iris(line);
        // the output declares only some of the classes it names
        classes.extend(ns.iter().cloned());
        if line.starts_with("SubClassOf(") && ns.len() == 2 {
            up.push((ns[0].clone(), ns[1].clone()));
        } else if line.starts_with("EquivalentClasses(") {
            for a in &ns {
                for b in &ns {
                    up.push((a.clone(), b.clone()));
                }
            }
        }
    }
    let mut out = BTreeSet::new();
    for a in &classes {
        let mut seen = BTreeSet::from([a.clone(), THING.to_string()]);
        let mut stack = vec![a.clone()];
        while let Some(x) = stack.pop() {
            for (s, t) in &up {
                if *s == x && seen.insert(t.clone()) {
                    stack.push(t.clone());
                }
            }
        }
        let unsat = seen.contains(NOTHING);
        for b in &classes {
            if unsat || seen.contains(b) {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    (classes, out)
}

/// The subsumptions between `classes` of a classification, and the range universals its
/// normalization left out.
fn answer(
    k: &mut Classifier,
    limits: &Limits,
    classes: &BTreeSet<String>,
    mode: RangeUniversals,
) -> (BTreeSet<(String, String)>, usize) {
    let out = with_range_universals(mode, || k.classify(limits)).unwrap();
    let t = &out.taxonomy;
    let mut pairs = BTreeSet::new();
    for a in classes {
        for b in classes {
            if t.is_subclass(&Class::new(a.clone()), &Class::new(b.clone())) {
                pairs.insert((a.clone(), b.clone()));
            }
        }
    }
    (pairs, out.stats.horn_ranges_left_out)
}

fn fixture(name: &str) -> (Vec<Axiom>, BTreeSet<String>, BTreeSet<(String, String)>) {
    let d = dir("horn_ranges");
    let doc =
        ofn_read::parse(&std::fs::read_to_string(d.join(format!("{name}.ofn"))).unwrap()).unwrap();
    let text = std::fs::read_to_string(d.join(format!("{name}.hermit.ofn"))).unwrap();
    let (classes, want) = hermit(&text);
    (doc.axioms, classes, want)
}

/// Each fixture, how many range universals the rule leaves out, and whether leaving out
/// every one loses an answer.
const CASES: &[(&str, usize, bool)] = &[
    // a chain whose last link has no range keeps its result's range universal, the
    // shape of ore_ont_9663's role 207
    ("last-unranged", 0, true),
    // a superrole of the last link holds one range and not the other
    ("superrole", 1, true),
    // a transitive role and its inverse, with links both ways and a loop
    ("transitive-inverse", 1, false),
    // a chain whose last operand is an inverse, whose range is a domain
    ("chain-inverse-last", 1, true),
    // an inverse functional identification gives the predecessor the successor's range
    ("identify", 1, false),
    // property assertions both ways
    ("assertions", 1, false),
    // a successor that is an individual
    ("nominal-successor", 1, false),
    // a run whose only universal was the range's
    ("only-ranges", 1, false),
];

#[test]
fn the_range_universals_left_out_change_no_answer() {
    for &(name, covered, needed) in CASES {
        let (axioms, classes, want) = fixture(name);
        for threads in [1, 4] {
            let limits = Limits {
                threads,
                ..Limits::default()
            };
            let mut k = Classifier::of(&axioms);
            let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
            assert_eq!(got, want, "{name}: the rule's answer is HermiT's");
            assert_eq!(n, covered, "{name}: the universals the rule leaves out");
            assert!(k.excluded().is_empty(), "{name}: {:#?}", k.excluded());
            let mut k = Classifier::of(&axioms);
            let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Keep);
            assert_eq!(
                got, want,
                "{name}: every universal kept gives HermiT's answer"
            );
            assert_eq!(n, 0);
            let mut k = Classifier::of(&axioms);
            let (got, _) = answer(&mut k, &limits, &classes, RangeUniversals::Drop);
            assert!(
                got.is_subset(&want),
                "{name}: dropping universals stays sound"
            );
            assert_eq!(
                got != want,
                needed,
                "{name}: whether the answer needs a range universal, lost {:?}",
                want.difference(&got).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn a_removed_covering_range_brings_the_universal_back() {
    let (axioms, classes, want) = fixture("superrole");
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
    assert_eq!((got, n), (want.clone(), 1));
    // without the range of the last link's superrole, r's range of D holds at the end of
    // a chain only through its universal
    let gone: Vec<Axiom> = axioms
        .iter()
        .filter(|a| {
            matches!(&a.kind, AxiomKind::ObjectPropertyRange(ObjectPropertyExpression::Property(p), _)
                if p.as_str() == "http://ex.org/t")
        })
        .cloned()
        .collect();
    assert_eq!(gone.len(), 1);
    k.remove(&gone);
    let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
    assert_eq!(n, 0);
    assert_eq!(got, want, "A ⊑ X holds by r's range alone");
}

/// The cases of the differential fixtures, with the disjunctive attempt bounded so that
/// the Horn rules answer whatever they take.
#[test]
fn the_dl603_fixtures_answer_the_same_with_and_without_the_rule() {
    let d = dir("dl603");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&d)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    cases.sort();
    let limits = Limits {
        threads: 1,
        dl_attempt_work: Some(1),
        ..Limits::default()
    };
    let (mut skipped, mut files) = (0, 0);
    for case in &cases {
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
        let mut k = Classifier::of(&doc.axioms);
        let (a, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
        let mut k = Classifier::of(&doc.axioms);
        let (b, _) = answer(&mut k, &limits, &classes, RangeUniversals::Keep);
        assert_eq!(a, b, "{}", case.display());
        skipped += n;
        files += (n > 0) as usize;
    }
    assert!(files > 0, "no fixture exercises the rule");
    eprintln!("{files} fixtures left out {skipped} range universals");
}
