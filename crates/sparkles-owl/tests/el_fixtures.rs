//! The fixture suite of roadmap item 587: random OWL 2 EL ontologies with individuals,
//! assertions, ranges, chains, data, keys and negative assertions, which
//! `tests/fixtures/el587/generate.py` writes, each classified by HermiT 1.4.5.519 and
//! Konclude 0.7.0-1138 into `case-NNN.hermit.ofn` and `case-NNN.konclude.ofn` (the batch
//! of the lane's run record). The classifier must entail exactly the subsumptions
//! between named classes that HermiT entails, and Konclude must agree with HermiT where
//! it classified the case, except on the cases of [`KONCLUDE_DIFFERS`]. Konclude stopped
//! with a segmentation fault on ten cases (059, 090, 141, 153, 168, 229, 237, 260, 292
//! and 302), which have no Konclude file.

use sparkles_owl::classify::{Classifier, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The cases where Konclude's taxonomy differs from HermiT's, with the reason that
/// HermiT's is the right one. The classifier agrees with HermiT on each.
const KONCLUDE_DIFFERS: &[(&str, &str)] = &[
    (
        "case-074",
        "Konclude misses C3 below C2, where C3 has an xsd:token value of d0 and C2 is \
         every class with a d0 value",
    ),
    (
        "case-075",
        "Konclude puts owl:Thing below C0 = some d1 xsd:nonNegativeInteger, where the only \
         d1 value known is the owl:rational 3/2",
    ),
    (
        "case-124",
        "Konclude misses owl:Thing below C2, where some owl:topObjectProperty {a0} is below \
         C2 and every individual reaches a0 through the top property",
    ),
    (
        "case-261",
        "Konclude puts C4 below C0 = some d1 xsd:integer, where C4 has the owl:rational \
         3/2 as a d1 value",
    ),
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/el587")
}

/// The IRIs inside angle brackets of one line.
fn iris(line: &str) -> Vec<String> {
    line.split('<')
        .skip(1)
        .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
        .collect()
}

/// The subsumptions between `classes` that a reference taxonomy entails, read from the
/// direct hierarchy of `SubClassOf` and `EquivalentClasses` lines that
/// `scripts/elk-compare` writes, with the unsatisfiable classes equivalent to
/// `owl:Nothing`.
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

#[test]
fn the_classifier_entails_what_a_complete_reasoner_entails() {
    let dir = fixtures();
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.starts_with("case-") && n.ends_with(".ofn") && n.matches('.').count() == 1
            })
        })
        .collect();
    cases.sort();
    assert!(cases.len() >= 100, "{} cases", cases.len());
    let mut checked = 0;
    let mut konclude = 0;
    let mut differs = Vec::new();
    let mut failures = Vec::new();
    for case in &cases {
        let hermit = case.with_extension("hermit.ofn");
        let Ok(want) = std::fs::read_to_string(&hermit) else {
            continue;
        };
        let doc = ofn_read::parse(&std::fs::read_to_string(case).unwrap()).unwrap();
        let mut k = Classifier::of(&doc.axioms);
        let out = k.classify(&Limits::default()).unwrap();
        // every case is inside OWL 2 EL, and the classifier uses all of it
        let excluded = k.excluded();
        assert!(excluded.is_empty(), "{}: {excluded:#?}", case.display());
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
        let want = reference(&want, &classes);
        if got != want {
            failures.push(format!(
                "{}: missing {:?}, extra {:?}",
                case.display(),
                want.difference(&got).collect::<Vec<_>>(),
                got.difference(&want).collect::<Vec<_>>()
            ));
        }
        if let Ok(other) = std::fs::read_to_string(case.with_extension("konclude.ofn"))
            && !other.trim().is_empty()
        {
            konclude += 1;
            if reference(&other, &classes) != want {
                let stem = case
                    .file_stem()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default();
                differs.push(stem.to_string());
            }
        }
        checked += 1;
    }
    let known: Vec<String> = KONCLUDE_DIFFERS
        .iter()
        .map(|(c, _)| c.to_string())
        .collect();
    if differs != known {
        failures.push(format!(
            "Konclude differs from HermiT on {differs:?}, not {known:?}"
        ));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    eprintln!("{checked} cases against HermiT, {konclude} against Konclude");
}
