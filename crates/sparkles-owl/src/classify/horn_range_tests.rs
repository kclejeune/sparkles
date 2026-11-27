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

/// The classes a reference taxonomy names and the subsumptions between them it entails,
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
    // HermiT 1.4.5.519 misses the range of a superproperty of a chain's result, so the
    // reference of such a fixture is Konclude 0.7.0's, which entails it
    let konclude = d.join(format!("{name}.konclude.ofn"));
    let reference = if konclude.exists() {
        konclude
    } else {
        d.join(format!("{name}.hermit.ofn"))
    };
    let text = std::fs::read_to_string(reference).unwrap();
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
    // a chain of property assertions whose last link has no range
    ("chain-individuals", 0, true),
    // a chain of inverse links, walked forwards and backwards, whose last link has no range
    ("inverse-path", 0, true),
    // the last link is a chain's result whose own chain ends in a link without the range
    ("nested-last", 0, true),
    // a left-linear chain whose right operand has no range
    ("left-linear", 0, true),
    // a chain into a subproperty of the ranged property
    ("subrole-chain", 0, true),
    // a generated case in which unfolded negative existentials give links by roles that
    // no positive existential has, which take the ranges of their inverses late
    ("late-links", 1, false),
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

/// A test of the axioms an edit takes.
type Pick = Box<dyn Fn(&AxiomKind) -> bool>;

/// The axioms of a fixture that `pick` selects.
fn select(axioms: &[Axiom], pick: impl Fn(&AxiomKind) -> bool) -> Vec<Axiom> {
    axioms.iter().filter(|a| pick(&a.kind)).cloned().collect()
}

/// An edit of the axioms that cover a universal, after a run that left it out and after
/// one that kept it, answers as a fresh classifier of the edited axioms does: a domain,
/// which covers the range universal of a chain whose last operand is the inverse, and the
/// subproperty whose superproperty's range covers one.
#[test]
fn edits_of_the_covering_axioms_answer_as_a_fresh_run() {
    let ex = |n: &str| format!("http://ex.org/{n}");
    let cases: Vec<(&str, Pick)> = vec![
        (
            "chain-inverse-last",
            Box::new(move |k: &AxiomKind| {
                matches!(k, AxiomKind::ObjectPropertyDomain(ObjectPropertyExpression::Property(p), _)
                    if p.as_str() == ex("q"))
            }),
        ),
        (
            "superrole",
            Box::new(move |k: &AxiomKind| {
                matches!(k, AxiomKind::SubObjectPropertyOf(_, ObjectPropertyExpression::Property(p))
                    if p.as_str() == ex("t"))
            }),
        ),
    ];
    let limits = Limits::default();
    for (name, pick) in &cases {
        let (axioms, classes, want) = fixture(name);
        let edit = select(&axioms, |k| pick(k));
        assert_eq!(edit.len(), 1, "{name}");
        let rest = select(&axioms, |k| !pick(k));
        let fresh = |ax: &[Axiom]| {
            let mut k = Classifier::of(ax);
            answer(&mut k, &limits, &classes, RangeUniversals::Covered)
        };
        let mut k = Classifier::of(&axioms);
        let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
        assert_eq!((&got, n), (&want, 1), "{name}");
        k.remove(&edit);
        let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
        assert_eq!((got, n), fresh(&rest), "{name}: after the removal");
        assert_eq!(n, 0, "{name}: the universal comes back");
        k.add(&edit);
        let (got, n) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
        assert_eq!((&got, n), (&want, 1), "{name}: after adding it again");
    }
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

/// A domain written as a class axiom, `∃q.⊤ ⊑ D`, is a range of `q⁻` that covers the
/// range universal, and a run can keep its contexts across class axioms. Removing the
/// domain and adding it again answers as fresh classifiers of the same axioms do.
#[test]
fn a_domain_as_a_class_axiom_answers_as_a_fresh_run() {
    let body =
        "Prefix(:=<http://ex.org/>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(
SubObjectPropertyOf(ObjectPropertyChain(:a :qi) :s)
InverseObjectProperties(:q :qi)
InverseObjectProperties(:s :si)
ObjectPropertyRange(:s :D)
ObjectPropertyRange(:s :E)
SubClassOf(:A ObjectSomeValuesFrom(:a ObjectSomeValuesFrom(:qi :B)))
EquivalentClasses(:X ObjectSomeValuesFrom(:a ObjectSomeValuesFrom(:qi ObjectIntersectionOf(:B :D))))
EquivalentClasses(:Y ObjectSomeValuesFrom(:a ObjectSomeValuesFrom(:qi ObjectIntersectionOf(:B :E))))
SubClassOf(ObjectSomeValuesFrom(:q owl:Thing) :D)
SubClassOf(:Z :A)
)";
    let axioms = ofn_read::parse(body).unwrap().axioms;
    let classes: BTreeSet<String> = ["A", "B", "D", "E", "X", "Y", "Z"]
        .iter()
        .map(|c| format!("http://ex.org/{c}"))
        .chain([THING.to_string(), NOTHING.to_string()])
        .collect();
    let limits = Limits::default();
    let n = axioms.len();
    let (domain, unrelated) = (&axioms[n - 2..n - 1], &axioms[n - 1..]);
    let without = |gone: &[&[Axiom]]| -> Vec<Axiom> {
        axioms
            .iter()
            .filter(|a| !gone.iter().any(|g| g.contains(a)))
            .cloned()
            .collect()
    };
    let fresh = |ax: &[Axiom]| {
        let mut k = Classifier::of(ax);
        answer(&mut k, &limits, &classes, RangeUniversals::Covered)
    };
    let mut k = Classifier::of(&axioms);
    let (all, n0) = answer(&mut k, &limits, &classes, RangeUniversals::Covered);
    assert_eq!(n0, 1);
    assert!(all.contains(&("http://ex.org/A".into(), "http://ex.org/X".into())));
    // an unrelated edit keeps the contexts and the rule
    k.remove(unrelated);
    let out = with_range_universals(RangeUniversals::Covered, || k.classify(&limits)).unwrap();
    assert!(out.stats.incremental);
    assert_eq!(out.stats.horn_ranges_left_out, 1);
    k.remove(domain);
    assert_eq!(
        answer(&mut k, &limits, &classes, RangeUniversals::Covered),
        fresh(&without(&[domain, unrelated]))
    );
    k.add(domain);
    assert_eq!(
        answer(&mut k, &limits, &classes, RangeUniversals::Covered),
        fresh(&without(&[unrelated]))
    );
}
