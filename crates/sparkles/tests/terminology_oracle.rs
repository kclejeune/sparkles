#![cfg(feature = "ontology")]
//! A classification answers for the whole ontology, assertions included (roadmap item
//! 1271). Wherever it calls its answer complete, and for every class it leaves outside
//! `incomplete`, the answer must be the one of a complete reasoner given the whole file:
//! HermiT's on the fixtures of items 603 and 587, and a complete run with the assertions
//! on hand-written cases. A classification of the terminology alone, the default, that
//! did not read an assertion with logical content names it as left out, is not complete,
//! has every class incomplete and keeps no consistent verdict.
//!
//! The runs go through the shipped surfaces, a file read without a store and the store,
//! so that the split between the terminology and the streamed assertions is the one the
//! commands use.

use sparkles::Dataset;
use sparkles::handles::ontology::OntologyRequest;
use sparkles::handles::ontology::classify::ClassifyRequest;
use sparkles::handles::ontology::file::OntologyFile;
use sparkles::owl::classify::{Classification, Consistency, Excluded, Gap, NOT_READ};
use sparkles::owl::model::{AxiomKind, Class, Entity};
use sparkles::owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

type Relation = BTreeSet<(String, String)>;

fn iris(line: &str) -> Vec<String> {
    line.split('<')
        .skip(1)
        .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
        .collect()
}

/// The subsumptions between `classes` that a HermiT taxonomy entails, as the fixture
/// suites of the classifier read them.
fn hermit(text: &str, classes: &BTreeSet<String>) -> Relation {
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

/// The subsumptions between `classes` that a complete classification answers.
fn relation(c: &Classification, classes: &BTreeSet<String>) -> Relation {
    let mut out = BTreeSet::new();
    let inconsistent = c.consistency == Consistency::Inconsistent;
    for a in classes {
        for b in classes {
            if inconsistent
                || a == NOTHING
                || b == THING
                || c.taxonomy
                    .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
            {
                out.insert((a.clone(), b.clone()));
            }
        }
    }
    out
}

/// The classification requests of the oracle: the default, by the OWL 2 EL rules alone,
/// and each without the modules' pass, of the terminology alone or with the assertions.
fn requests(assertions: bool) -> Vec<ClassifyRequest> {
    let mut out = Vec::new();
    for el_only in [false, true] {
        for skip_coverage in [false, true] {
            out.push(ClassifyRequest {
                el_only,
                skip_coverage,
                assertions,
                threads: 1,
                ..Default::default()
            });
        }
    }
    out
}

/// One answer: the classification, what it left out, and whether the run read the file
/// without a store.
type Answer = (Classification, Vec<Excluded>, bool);

/// The answers of a file read without a store, when the file path reads it, and of the
/// same file loaded into a store.
fn answers(path: &Path, c: &ClassifyRequest, store: bool) -> Result<Vec<Answer>, String> {
    let req = OntologyRequest::default();
    let mut out = Vec::new();
    let file = OntologyFile::read(path, &req, 1).map_err(|e| format!("read: {e}"))?;
    if let Some(file) = file {
        let f = file
            .classify(c, &Default::default())
            .map_err(|e| format!("file classify: {e}"))?;
        out.push((f.classification, f.excluded, true));
    }
    if store || out.is_empty() {
        let ds = Dataset::memory();
        let src = sparkles::io::Source::from_path(path, None).unwrap();
        ds.store().load(&[src]).map_err(|e| format!("load: {e}"))?;
        let cl = ds
            .ontology()
            .classify(&req, c, &Default::default())
            .map_err(|e| format!("store classify: {e}"))?;
        out.push(((*cl.classification).clone(), (*cl.excluded).clone(), false));
    }
    Ok(out)
}

/// Whether a classification of the terminology alone of the file leaves out an assertion
/// with logical content, as the file path streams them, or None when the file path does
/// not read the file.
fn drops_assertions(path: &Path) -> Option<bool> {
    let file = OntologyFile::read(path, &OntologyRequest::default(), 1).unwrap()?;
    let mut any = false;
    file.assertions(&Default::default(), |ax, _| {
        any = ax.is_none_or(|ax| {
            sparkles::owl::classify::logical(&ax.kind)
                && !matches!(ax.kind, AxiomKind::Declaration(_))
        });
        Ok(!any)
    })
    .unwrap();
    Some(any)
}

/// What the oracle counted, so that a branch it never reaches shows.
#[derive(Default, Debug)]
struct Counts {
    runs: usize,
    /// runs that report an assertion they did not read as left out
    witness: usize,
    /// runs whose drop of assertions the file path showed, which must then have a witness
    known_drop: usize,
    /// classes checked against the reference in runs of the terminology alone and in runs
    /// with the assertions
    decided_terminology: usize,
    decided_whole: usize,
    /// runs with the assertions that the reference calls inconsistent and that say so
    inconsistent_whole: usize,
}

/// The failures of a run against the whole ontology's answer `want` over `classes`,
/// with `inconsistent` the whole ontology's consistency, and `drops` whether the run of
/// the terminology alone left out an assertion with logical content, when known.
#[allow(clippy::too_many_arguments)]
fn check(
    run: &str,
    (got, excluded, _): &Answer,
    want: &Relation,
    inconsistent: bool,
    classes: &BTreeSet<String>,
    assertions: bool,
    drops: Option<bool>,
    n: &mut Counts,
) -> Vec<String> {
    let mut failures = Vec::new();
    n.runs += 1;
    // every case maps in full, so nothing is left out for a triple
    if got.stats.unmapped > 0 || !got.unmapped.triples.is_empty() || got.unmapped.truncated {
        failures.push(format!(
            "{run}: {} triples map to no axiom: {:?}",
            got.stats.unmapped, got.unmapped
        ));
    }
    let witness = excluded
        .iter()
        .any(|e| matches!(&e.gap, Gap::Outside { rule, .. } if rule == NOT_READ));
    if witness {
        n.witness += 1;
        if assertions {
            failures.push(format!(
                "{run}: a witness in a run that read the assertions"
            ));
        }
        if got.complete || got.consistency == Consistency::Consistent {
            failures.push(format!(
                "{run}: a witness with complete {} and {:?}",
                got.complete, got.consistency
            ));
        }
        let open: Vec<&Class> = got
            .taxonomy
            .classes()
            .filter(|c| c.as_str() != THING && c.as_str() != NOTHING)
            .filter(|c| !got.incomplete.contains(c))
            .collect();
        if !open.is_empty() {
            failures.push(format!("{run}: a witness beside complete classes {open:?}"));
        }
    }
    if !assertions && let Some(drops) = drops {
        if drops {
            n.known_drop += 1;
        }
        if drops != witness {
            failures.push(format!(
                "{run}: drops assertions {drops}, witness {witness}"
            ));
        }
    }
    if assertions && inconsistent && got.consistency == Consistency::Inconsistent {
        n.inconsistent_whole += 1;
    }
    if got.consistency != Consistency::Unknown
        && (got.consistency == Consistency::Inconsistent) != inconsistent
    {
        failures.push(format!("{run}: consistency {:?}", got.consistency));
    }
    // a class of the whole ontology that the taxonomy lacks has no answer at all
    let lacking: Vec<&String> = classes
        .iter()
        .filter(|a| *a != THING && *a != NOTHING)
        .filter(|a| got.taxonomy.node_of(&Class::new(a.as_str())).is_none())
        .collect();
    if got.complete && !lacking.is_empty() {
        failures.push(format!("{run}: complete without {lacking:?}"));
    }
    if got.complete && got.consistency == Consistency::Unknown {
        failures.push(format!("{run}: complete with the consistency unknown"));
    }
    let held =
        |a: &str| a == THING || a == NOTHING || got.taxonomy.node_of(&Class::new(a)).is_some();
    // the taxonomy's own answer: an inconsistent run answers every pair of the classes
    // it holds, and none for a class it lacks
    let sub = |a: &str, b: &str| {
        held(a)
            && held(b)
            && (got.consistency == Consistency::Inconsistent
                || a == NOTHING
                || b == THING
                || got.taxonomy.is_subclass(&Class::new(a), &Class::new(b)))
    };
    for a in classes {
        if a == THING || a == NOTHING {
            continue;
        }
        let ac = Class::new(a.as_str());
        if !got.complete && (got.incomplete.contains(&ac) || got.taxonomy.node_of(&ac).is_none()) {
            continue;
        }
        if assertions {
            n.decided_whole += 1;
        } else {
            n.decided_terminology += 1;
        }
        for b in classes {
            let w = want.contains(&(a.clone(), b.clone()));
            if w != sub(a, b) {
                failures.push(format!("{run}: {a} below {b}: {w} in the whole ontology"));
            }
            // the equivalents of a complete class, unsatisfiable ones included
            if w && want.contains(&(b.clone(), a.clone())) && !sub(b, a) {
                failures.push(format!("{run}: {b} equivalent to {a} missing"));
            }
        }
    }
    failures
}

fn cases(dir: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../sparkles-owl/tests/fixtures")
        .join(dir);
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
    cases
}

/// Whether a file holds an individual axiom, which a read may stream as an assertion.
fn has_assertions(doc: &ofn_read::Document) -> bool {
    doc.axioms.iter().any(|a| {
        matches!(
            &a.kind,
            AxiomKind::ClassAssertion(..)
                | AxiomKind::ObjectPropertyAssertion(..)
                | AxiomKind::NegativeObjectPropertyAssertion(..)
                | AxiomKind::DataPropertyAssertion(..)
                | AxiomKind::NegativeDataPropertyAssertion(..)
                | AxiomKind::SameIndividual(..)
                | AxiomKind::DifferentIndividuals(..)
        )
    })
}

/// On every case of the fixture suites of items 603 and 587 that holds assertions, the
/// classification of the terminology alone and the one with the assertions agree with
/// HermiT's classification of the whole case wherever they claim an answer, and the
/// first names an assertion it did not read exactly when the file streams one.
#[test]
fn every_run_answers_as_hermit_does_for_the_whole_case() {
    let mut n = Counts::default();
    let mut failures = Vec::new();
    let mut taken = 0usize;
    for dir in ["dl603", "el587"] {
        for case in cases(dir) {
            let text = std::fs::read_to_string(&case).unwrap();
            let doc = ofn_read::parse(&text).unwrap();
            if !has_assertions(&doc) {
                continue;
            }
            taken += 1;
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
            let reference = std::fs::read_to_string(case.with_extension("hermit.ofn")).unwrap();
            let want = hermit(&reference, &classes);
            let inconsistent = want.contains(&(THING.to_string(), NOTHING.to_string()));
            let stem = case
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let drops = drops_assertions(&case);
            for assertions in [false, true] {
                for c in requests(assertions) {
                    let run = format!(
                        "{dir}/{stem} assertions {assertions} el_only {} skip {}",
                        c.el_only, c.skip_coverage
                    );
                    let got = match answers(&case, &c, false) {
                        Ok(got) => got,
                        Err(e) => {
                            failures.push(format!("{run}: {e}"));
                            continue;
                        }
                    };
                    for got in got {
                        // the file path's stream is the store's only on the file path
                        let drops = drops.filter(|_| got.2);
                        failures.extend(check(
                            &run,
                            &got,
                            &want,
                            inconsistent,
                            &classes,
                            assertions,
                            drops,
                            &mut n,
                        ));
                    }
                }
            }
        }
    }
    eprintln!("{taken} cases, {n:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(taken > 500, "{taken}");
    assert!(n.witness > 0 && n.known_drop > 0, "{n:?}");
    assert!(n.decided_whole > 0 && n.inconsistent_whole > 0, "{n:?}");
}

/// A hand-written case: a terminology, the assertions that change its answer, and one
/// subsumption of the whole ontology that the terminology alone does not entail, or
/// `None` when the assertions make the ontology inconsistent.
struct Hand {
    name: &'static str,
    body: &'static str,
    entails: Option<(&'static str, &'static str)>,
}

const HAND: &[Hand] = &[
    // inconsistency from the assertions
    Hand {
        name: "disjoint-clash",
        body: "DisjointClasses(:A :B) SubClassOf(:C :A)
               ClassAssertion(:A :x) ClassAssertion(:B :x)",
        entails: None,
    },
    Hand {
        name: "complement",
        body: "SubClassOf(:A :B) SubClassOf(:C :B)
               ClassAssertion(ObjectComplementOf(:B) :x) ClassAssertion(:A :x)",
        entails: None,
    },
    Hand {
        name: "unsatisfiable-instance",
        body: "SubClassOf(:U owl:Nothing) SubClassOf(:A :B) ClassAssertion(:U :x)",
        entails: None,
    },
    Hand {
        name: "existential-over-unsatisfiable",
        body: "SubClassOf(:U owl:Nothing) SubClassOf(:A :B)
               ObjectPropertyRange(:p :U) ObjectPropertyAssertion(:p :x :y)",
        entails: None,
    },
    Hand {
        name: "irreflexive-reflexive",
        body: "IrreflexiveObjectProperty(:p) SubClassOf(:A :B)
               ObjectPropertyAssertion(:p :x :x) ClassAssertion(:A :x)",
        entails: None,
    },
    Hand {
        name: "data-range",
        body: "DataPropertyRange(:d xsd:integer) SubClassOf(:A :B)
               DataPropertyAssertion(:d :x \"a\")",
        entails: None,
    },
    Hand {
        name: "functional-different",
        body: "FunctionalObjectProperty(:p) SubClassOf(:A :B)
               ObjectPropertyAssertion(:p :x :y) ObjectPropertyAssertion(:p :x :z)
               DifferentIndividuals(:y :z)",
        entails: None,
    },
    Hand {
        name: "domain-disjoint",
        body: "ObjectPropertyDomain(:p :A) DisjointClasses(:A :B) SubClassOf(:C :B)
               ObjectPropertyAssertion(:p :x :y) ClassAssertion(:B :x)",
        entails: None,
    },
    // nominals in the terminology
    Hand {
        name: "one-of",
        body: "EquivalentClasses(:N ObjectOneOf(:b)) SubClassOf(:D :E)
               ClassAssertion(:D :b)",
        entails: Some(("N", "D")),
    },
    Hand {
        name: "has-value",
        body: "EquivalentClasses(:H ObjectHasValue(:p :b))
               SubClassOf(ObjectSomeValuesFrom(:p :D) :E)
               ClassAssertion(:D :b)",
        entails: Some(("H", "E")),
    },
    Hand {
        name: "one-of-same",
        body: "EquivalentClasses(:N ObjectOneOf(:b)) SubClassOf(:D :E)
               SameIndividual(:b :c) ClassAssertion(:D :c)",
        entails: Some(("N", "D")),
    },
    Hand {
        name: "one-of-different",
        body: "EquivalentClasses(:N ObjectOneOf(:b)) EquivalentClasses(:M ObjectOneOf(:c))
               SubClassOf(:N :M) SubClassOf(:D :E)
               DifferentIndividuals(:b :c)",
        entails: None,
    },
    Hand {
        name: "one-of-property",
        body: "EquivalentClasses(:N ObjectOneOf(:b))
               SubClassOf(ObjectSomeValuesFrom(:p :N) :E) SubClassOf(:D :F)
               DisjointClasses(:D :E)
               ObjectPropertyAssertion(:p :x :b) ClassAssertion(:D :x)",
        entails: None,
    },
    // the universal property, with a plain assertion and no nominal
    Hand {
        name: "top-property",
        body: "SubClassOf(:B ObjectAllValuesFrom(owl:topObjectProperty :A)) SubClassOf(:C :D)
               ClassAssertion(:B :x)",
        entails: Some(("C", "A")),
    },
    // a class that only an assertion names
    Hand {
        name: "unnamed-consistent",
        body: "SubClassOf(:A :B) ClassAssertion(:Z :x)",
        entails: Some(("Z", "Z")),
    },
    Hand {
        name: "unnamed-unsatisfiable",
        body: "SubClassOf(:A owl:Nothing) SubClassOf(:B :C) ClassAssertion(:Z :x)",
        entails: Some(("A", "Z")),
    },
    Hand {
        name: "unnamed-inconsistent",
        body: "SubClassOf(owl:Thing :A) SubClassOf(owl:Thing :B) DisjointClasses(:A :B)
               ClassAssertion(:Z :x)",
        entails: Some(("Z", "A")),
    },
];

/// Controls: assertions over a terminology without nominals, the universal property or
/// anything they can clash with. They change no answer, and a run of the terminology
/// alone still counts them as left out.
const CONTROLS: &[(&str, &str)] = &[
    (
        "positive",
        "SubClassOf(:A :B) SubClassOf(:B ObjectSomeValuesFrom(:p :C))
         ClassAssertion(:A :x) ObjectPropertyAssertion(:p :x :y) DataPropertyAssertion(:d :x \"1\")",
    ),
    (
        "same-and-different",
        "SubClassOf(:A :B) EquivalentClasses(:C ObjectIntersectionOf(:A :B))
         ClassAssertion(:A :x) SameIndividual(:x :y) DifferentIndividuals(:y :z)",
    ),
];

/// A case's file, with a declaration of every name it uses, so that the file path reads
/// it as well as the store: `:p` is an object property, `:d` a data property, a name in
/// lower case an individual and any other a class. `:Z` stays undeclared, so that only
/// an assertion names it and the store reads the case.
fn ofn(name: &str, body: &str) -> String {
    let mut names = BTreeSet::new();
    let mut rest = body;
    while let Some(i) = rest.find(':') {
        let tail = &rest[i + 1..];
        let end = tail
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(tail.len());
        let before = rest[..i].chars().last();
        if end > 0 && !before.is_some_and(|c| c.is_ascii_alphanumeric()) {
            names.insert(tail[..end].to_string());
        }
        rest = &tail[end..];
    }
    let mut decl = String::new();
    for n in names.into_iter().filter(|n| n != "Z") {
        let kind = match n.as_str() {
            "p" => "ObjectProperty",
            "d" => "DataProperty",
            _ if n.starts_with(|c: char| c.is_ascii_lowercase()) => "NamedIndividual",
            _ => "Class",
        };
        decl += &format!("Declaration({kind}(:{n}))\n");
    }
    format!(
        "Prefix(:=<http://e/#>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\nOntology(<http://e/{name}>\n\
         {decl}{body}\n)\n"
    )
}

fn iri(local: &str) -> String {
    format!("http://e/#{local}")
}

/// The whole case's answer: a complete run with the assertions, the same on the file
/// path and the store, with every class of the file in `classes`, or what keeps it from
/// being a reference. A case that leaves `:Z` undeclared has only the store's run.
fn whole(path: &Path) -> Result<(Relation, bool, BTreeSet<String>), String> {
    let c = ClassifyRequest {
        assertions: true,
        threads: 1,
        ..Default::default()
    };
    let runs = answers(path, &c, true)?;
    let undeclared = std::fs::read_to_string(path).unwrap().contains(":Z ");
    if runs.len() != 2 - usize::from(undeclared) {
        return Err("the file path does not read the case".to_string());
    }
    let mut out = None;
    for (got, excluded, _) in &runs {
        if !got.complete || !excluded.is_empty() {
            return Err(format!(
                "the run with the assertions is not complete: {:?} {excluded:?}",
                got.consistency
            ));
        }
        let mut classes: BTreeSet<String> = got
            .taxonomy
            .classes()
            .map(|c| c.as_str().to_string())
            .collect();
        classes.insert(THING.to_string());
        classes.insert(NOTHING.to_string());
        let inconsistent = got.consistency == Consistency::Inconsistent;
        let this = (relation(got, &classes), inconsistent, classes);
        if let Some(first) = &out {
            if *first != this {
                return Err("the file path and the store answer differently".to_string());
            }
        } else {
            out = Some(this);
        }
    }
    out.ok_or_else(|| "no run answered".to_string())
}

/// The runs of the terminology alone of one hand-written case, checked against `want`:
/// every one leaves an assertion out, so none is complete and none decides a class.
fn check_case(
    name: &str,
    path: &Path,
    want: &(Relation, bool, BTreeSet<String>),
    extra: &[String],
    n: &mut Counts,
) -> Vec<String> {
    let (want, inconsistent, classes) = want;
    let mut classes = classes.clone();
    classes.extend(extra.iter().cloned());
    let mut failures = Vec::new();
    let drops = drops_assertions(path);
    if drops == Some(false) {
        failures.push(format!(
            "{name}: the file path streams no assertion: {drops:?}"
        ));
    }
    for c in requests(false) {
        let run = format!("{name} el_only {} skip {}", c.el_only, c.skip_coverage);
        let got = match answers(path, &c, true) {
            Ok(got) => got,
            Err(e) => {
                failures.push(format!("{run}: {e}"));
                continue;
            }
        };
        for got in got {
            if got.0.complete {
                failures.push(format!("{run}: complete: {:?}", got.1));
            }
            failures.extend(check(
                &run,
                &got,
                want,
                *inconsistent,
                &classes,
                false,
                Some(true),
                n,
            ));
        }
    }
    failures
}

/// On hand-written cases where an assertion makes the ontology inconsistent, adds a
/// subsumption through a nominal or the universal property, or names a class the
/// terminology lacks, and on controls whose assertions change nothing, the terminology
/// alone never claims an answer: each run names an assertion it did not read.
#[test]
fn assertions_not_read_never_hide_behind_complete() {
    let dir = tempfile::tempdir().unwrap();
    let mut n = Counts::default();
    let mut failures = Vec::new();
    for h in HAND {
        let path = dir.path().join(format!("{}.ofn", h.name));
        std::fs::write(&path, ofn(h.name, h.body)).unwrap();
        let w = match whole(&path) {
            Ok(w) => w,
            Err(e) => {
                failures.push(format!("{}: {e}", h.name));
                continue;
            }
        };
        // the case must be one the assertions change
        let mut extra = Vec::new();
        match h.entails {
            None if !w.1 => {
                failures.push(format!("{}: the whole case is consistent", h.name));
                continue;
            }
            None => {}
            Some((a, b)) => {
                let pair = (iri(a), iri(b));
                extra = vec![pair.0.clone(), pair.1.clone()];
                if !w.0.contains(&pair) {
                    failures.push(format!("{}: {pair:?} not entailed", h.name));
                    continue;
                }
            }
        }
        failures.extend(check_case(h.name, &path, &w, &extra, &mut n));
    }
    for (name, body) in CONTROLS {
        let path = dir.path().join(format!("{name}.ofn"));
        std::fs::write(&path, ofn(name, body)).unwrap();
        match whole(&path) {
            Ok(w) if !w.1 => failures.extend(check_case(name, &path, &w, &[], &mut n)),
            Ok(_) => failures.push(format!("{name}: inconsistent")),
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }
    eprintln!("{n:?}");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(n.witness, n.runs, "{n:?}");
    // the file path and the store, but the store alone for the three cases of `:Z`
    assert_eq!(n.runs, 4 * (2 * (HAND.len() + CONTROLS.len()) - 3), "{n:?}");
}
