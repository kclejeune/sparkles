//! A rooted run keeps the Horn share (roadmap item 1286). When a rooted run without
//! counting leaves out a Horn axiom that needs counting, such as a functional or
//! inverse-functional property or a minimum above one, the axioms outside OWL 2 EL go to
//! the Horn rules, as they do in a run without `Limits::rooted`, instead of trading those
//! axioms for the disjunctive ones the rooted rules take. The four fixtures of the item
//! get the default run's answer, which is sound against HermiT and finds the
//! inconsistency of `sroiq-030` and `sroiq-032`, and across the 640 fixtures of item 603 a
//! rooted run leaves out no axiom that the default run takes, apart from data axioms and
//! two cases whose default run takes the disjunctive rules without the root.

use sparkles_owl::classify::{Classification, Classifier, Consistency, Error, Expansion, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The fixtures of roadmap item 1286.
const FOUR: [&str; 4] = ["mixed-031", "sroiq-017", "sroiq-030", "sroiq-032"];

/// The fixtures whose default run takes the disjunctive rules without the root, so that a
/// rooted run that gives its axioms to the Horn rules leaves out disjunctive axioms the
/// default run takes. Each leaves out fewer axioms than the default run.
const DEFAULT_TIER_D: [&str; 2] = ["sroiq-004", "sroiq-040"];

/// The fixtures whose rooted run leaves out data axioms that the Horn rules take by
/// default, which the disjunctive rules leave out in every run (items 768 and 1303).
const DATA: [&str; 3] = ["data-015", "mixed-001", "mixed-010"];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603")
}

fn iris(line: &str) -> Vec<String> {
    line.split('<')
        .skip(1)
        .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
        .collect()
}

/// The subsumptions between `classes` that HermiT's taxonomy entails, and whether it says
/// the ontology is inconsistent.
fn reference(text: &str, classes: &BTreeSet<String>) -> (BTreeSet<(String, String)>, bool) {
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
    (out, inconsistent)
}

struct Case {
    name: String,
    axioms: Vec<Axiom>,
    classes: BTreeSet<String>,
    hermit: BTreeSet<(String, String)>,
    hermit_inconsistent: bool,
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
    let text = std::fs::read_to_string(path.with_extension("hermit.ofn")).unwrap();
    let (hermit, hermit_inconsistent) = reference(&text, &classes);
    Case {
        name: path.file_stem().unwrap().to_str().unwrap().to_string(),
        axioms: doc.axioms,
        classes,
        hermit,
        hermit_inconsistent,
    }
}

fn named(name: &str) -> Case {
    case(&fixtures().join(format!("{name}.ofn")))
}

fn all_cases() -> Vec<Case> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 640);
    paths.iter().map(|p| case(p)).collect()
}

/// What a run answers: the axioms it leaves out, its consistency, its subsumptions between
/// the declared classes and its tier.
#[derive(Debug, PartialEq)]
struct Answer {
    left_out: BTreeSet<String>,
    consistency: Consistency,
    subsumptions: BTreeSet<(String, String)>,
    tier: sparkles_owl::classify::Tier,
}

fn answer(k: &Classifier, out: &Classification, classes: &BTreeSet<String>) -> Answer {
    let mut subsumptions = BTreeSet::new();
    for a in classes {
        for b in classes {
            if out
                .taxonomy
                .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
            {
                subsumptions.insert((a.clone(), b.clone()));
            }
        }
    }
    Answer {
        left_out: k.excluded().iter().map(|e| e.describe()).collect(),
        consistency: out.consistency,
        subsumptions,
        tier: out.stats.tier,
    }
}

fn classify(axioms: &[Axiom], classes: &BTreeSet<String>, limits: &Limits) -> Answer {
    let mut k = Classifier::of(axioms);
    let out = k.classify(limits).unwrap();
    answer(&k, &out, classes)
}

fn rooted(limits: &Limits) -> Limits {
    Limits {
        rooted: true,
        ..limits.clone()
    }
}

fn schedules() -> [Limits; 4] {
    [
        (1, Expansion::Cautious),
        (4, Expansion::Cautious),
        (1, Expansion::Eager),
        (4, Expansion::Eager),
    ]
    .map(|(threads, expansion)| Limits {
        threads,
        expansion,
        ..Limits::default()
    })
}

/// The four fixtures of item 1286 give the default run's answer in a rooted run, on every
/// schedule, and so do the three that meet the Nom rule with counting. Before the item a
/// rooted run without counting left
/// out 1 to 5 more axioms than the default run, such as the functional and
/// inverse-functional properties of `sroiq-032`, and lost the inconsistency of `sroiq-030`
/// and `sroiq-032`.
#[test]
fn the_four_fixtures_keep_the_horn_share() {
    for name in FOUR {
        let c = named(name);
        for base in schedules() {
            for counting in [false, true] {
                // with counting the rooted rules take the counting axioms of mixed-031, so
                // the rule does not apply, and the three others meet the Nom rule's refusal
                if counting && name == "mixed-031" {
                    continue;
                }
                let limits = Limits {
                    counting,
                    ..base.clone()
                };
                let mut k = Classifier::of(&c.axioms);
                let out = k.classify(&rooted(&limits)).unwrap();
                let got = answer(&k, &out, &c.classes);
                let want = classify(&c.axioms, &c.classes, &limits);
                let at = format!(
                    "{name}, {} threads, {:?}, counting {counting}",
                    limits.threads, limits.expansion
                );
                assert_eq!(
                    got, want,
                    "{at}: the rooted run differs from the default run"
                );
                // the Horn rules take the axioms outside OWL 2 EL, and no bound was passed
                assert_eq!(out.stats.dl_contexts, 0, "{at}");
                assert!(!out.stats.dl_rooted_over, "{at}");
                assert!(!out.stats.dl_rooted_declined, "{at}");
                assert!(!out.stats.dl_over_budget, "{at}");
                assert!(out.complete_for_taken, "{at}");
                // sound against HermiT
                let wrong: Vec<_> = got.subsumptions.difference(&c.hermit).collect();
                assert!(wrong.is_empty(), "{at}: {wrong:?}");
                if got.consistency == Consistency::Inconsistent {
                    assert!(c.hermit_inconsistent, "{at}");
                }
                if matches!(name, "sroiq-030" | "sroiq-032") {
                    assert!(c.hermit_inconsistent);
                    assert_eq!(got.consistency, Consistency::Inconsistent, "{at}");
                    assert_eq!(got.subsumptions, c.hermit, "{at}");
                }
            }
        }
    }
}

/// Across the 640 fixtures of item 603, a rooted run leaves out no axiom that the default
/// run takes, apart from the cases of [`DEFAULT_TIER_D`], which leave out fewer axioms than
/// the default run, and the data axioms of [`DATA`]. Every subsumption either run answers
/// is one HermiT entails.
#[test]
fn a_rooted_run_leaves_out_nothing_the_default_run_takes() {
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    for c in all_cases() {
        let mut k = Classifier::of(&c.axioms);
        let out = k.classify(&rooted(&Limits::default())).unwrap();
        let got = answer(&k, &out, &c.classes);
        let want = classify(&c.axioms, &c.classes, &Limits::default());
        for (run, a) in [("rooted", &got), ("default", &want)] {
            let wrong = a.subsumptions.difference(&c.hermit).count();
            if wrong > 0 {
                failures.push(format!("{}: the {run} run answers {wrong} wrong", c.name));
            }
        }
        let lost: Vec<_> = got.left_out.difference(&want.left_out).collect();
        if DEFAULT_TIER_D.contains(&c.name.as_str()) {
            seen.insert(c.name.clone());
            if want.left_out.len() <= got.left_out.len() {
                failures.push(format!(
                    "{}: the rooted run leaves out {} where the default run leaves out {}",
                    c.name,
                    got.left_out.len(),
                    want.left_out.len()
                ));
            }
        } else if DATA.contains(&c.name.as_str()) {
            seen.insert(c.name.clone());
            if lost.is_empty() || lost.iter().any(|a| !a.contains("Data")) {
                failures.push(format!("{}: the rooted run leaves out {lost:#?}", c.name));
            }
        } else if !lost.is_empty() {
            failures.push(format!("{}: the rooted run leaves out {lost:#?}", c.name));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    assert_eq!(seen.len(), DEFAULT_TIER_D.len() + DATA.len());
}

/// A classifier reused across changes answers as a new one does: a rooted run that the
/// disjunctive rules take, then give to the Horn rules once a functional property is
/// added, then take again once it is removed.
#[test]
fn a_reused_classifier_keeps_the_horn_share_across_changes() {
    let parse = |body: &str| {
        ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
            .unwrap()
            .axioms
    };
    let rest = parse(
        "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))
         SubClassOf(:A ObjectSomeValuesFrom(:r ObjectOneOf(:o)))
         SubClassOf(ObjectOneOf(:o) :B)
         SubClassOf(ObjectSomeValuesFrom(:r :B) :C)",
    );
    let functional = parse("FunctionalObjectProperty(:r)");
    let all: Vec<Axiom> = rest.iter().chain(&functional).cloned().collect();
    let classes: BTreeSet<String> = ["A", "B", "C", "Z", "Z1", "Z2"]
        .iter()
        .map(|c| format!("http://ex.org/{c}"))
        .chain([THING.to_string(), NOTHING.to_string()])
        .collect();
    let a_below_c = ("http://ex.org/A".to_string(), "http://ex.org/C".to_string());
    for limits in schedules() {
        let limits = rooted(&limits);
        let plain = Limits {
            rooted: false,
            ..limits.clone()
        };
        let mut k = Classifier::of(&rest);
        let first = k.classify(&limits).unwrap();
        assert!(
            first.stats.dl_contexts > 0,
            "the rooted rules take the axioms"
        );
        let first = answer(&k, &first, &classes);
        assert!(first.left_out.is_empty());
        assert!(first.subsumptions.contains(&a_below_c));
        assert_eq!(first, classify(&rest, &classes, &limits));
        k.add(&functional);
        let second = k.classify(&limits).unwrap();
        assert_eq!(
            second.stats.dl_contexts, 0,
            "the Horn rules take the axioms"
        );
        let got = answer(&k, &second, &classes);
        assert_eq!(got, classify(&all, &classes, &limits));
        assert_eq!(got, classify(&all, &classes, &plain));
        assert!(got.subsumptions.contains(&a_below_c));
        assert!(
            !got.left_out
                .iter()
                .any(|a| a.starts_with("FunctionalObjectProperty")),
            "the Horn rules take the functional property: {:?}",
            got.left_out
        );
        k.remove(&functional);
        let third = k.classify(&limits).unwrap();
        assert!(third.stats.dl_contexts > 0);
        assert_eq!(answer(&k, &third, &classes), first);
        // a run without the option on the same classifier, then the rooted run again
        k.add(&functional);
        let before = k.classify(&plain).unwrap();
        let before = answer(&k, &before, &classes);
        let again = k.classify(&limits).unwrap();
        assert_eq!(answer(&k, &again, &classes), before);
    }
}

/// A partial run of the four fixtures answers as the default partial run does.
#[test]
fn a_partial_rooted_run_keeps_the_horn_share() {
    for name in FOUR {
        let c = named(name);
        let limits = Limits {
            partial: true,
            ..Limits::default()
        };
        let mut k = Classifier::of(&c.axioms);
        let out = k.classify(&rooted(&limits)).unwrap();
        let mut plain = Classifier::of(&c.axioms);
        let want = plain.classify(&limits).unwrap();
        assert_eq!(
            answer(&k, &out, &c.classes),
            answer(&plain, &want, &c.classes),
            "{name}"
        );
        assert_eq!(out.complete, want.complete, "{name}");
        assert_eq!(out.complete_for_taken, want.complete_for_taken, "{name}");
        assert_eq!(out.incomplete, want.incomplete, "{name}");
    }
}

/// Under memory budgets stepped from an eighth of the default run's high-water mark to
/// more than it, a rooted run of the four fixtures stops where the default run stops, falls
/// back to the EL rules where it does, and otherwise gives its answer with the same flags,
/// since the refusal comes before the normalization interns anything and the rooted run's
/// high-water mark is at most the default run's. The refused run never tries Tier D, so it
/// reports no Tier D overrun, while the default run of `mixed-031`, whose individuals are
/// all outside OWL 2 EL, tries Tier D without the root and reports one where its Horn rules
/// pass the budget. A cancelled run stops before it answers.
#[test]
fn a_refused_rooted_run_stops_and_answers_as_the_default_run() {
    let (mut stopped, mut el, mut full) = (0, 0, 0);
    for name in FOUR {
        let c = named(name);
        let one = Limits {
            threads: 1,
            ..Limits::default()
        };
        let mut plain = Classifier::of(&c.axioms);
        let peak = plain.classify(&one).unwrap().stats.peak_memory;
        let mut k = Classifier::of(&c.axioms);
        let held = k.classify(&rooted(&one)).unwrap().stats.peak_memory;
        assert!(held <= peak, "{name}: {held} against {peak}");
        for step in 4..=40 {
            for partial in [false, true] {
                let budget = peak * step / 32;
                let limits = Limits {
                    threads: 1,
                    partial,
                    max_memory: Some(budget),
                    ..Limits::default()
                };
                let at = format!("{name}, budget {budget} of {peak}, partial {partial}");
                let mut k = Classifier::of(&c.axioms);
                let got = k.classify(&rooted(&limits));
                let mut plain = Classifier::of(&c.axioms);
                let want = plain.classify(&limits);
                match (&got, &want) {
                    (Ok(g), Ok(w)) => {
                        assert_eq!(
                            answer(&k, g, &c.classes),
                            answer(&plain, w, &c.classes),
                            "{at}"
                        );
                        assert_eq!(g.complete, w.complete, "{at}");
                        assert_eq!(g.complete_for_taken, w.complete_for_taken, "{at}");
                        assert_eq!(g.stats.horn_over_budget, w.stats.horn_over_budget, "{at}");
                        assert!(!g.stats.dl_over_budget, "{at}");
                        if name != "mixed-031" {
                            assert!(!w.stats.dl_over_budget, "{at}");
                        }
                        assert!(
                            !g.stats.dl_rooted_over && !g.stats.dl_rooted_declined,
                            "{at}"
                        );
                        if w.stats.horn_over_budget {
                            el += 1;
                        } else {
                            full += 1;
                        }
                    }
                    (Err(Error::Memory { .. }), Err(Error::Memory { .. })) => stopped += 1,
                    _ => panic!("{at}: {:?} against {:?}", got.err(), want.err()),
                }
            }
        }
        let cancelled = Limits {
            cancelled: Some(Arc::new(|| true)),
            ..Limits::default()
        };
        let mut k = Classifier::of(&c.axioms);
        assert!(matches!(
            k.classify(&rooted(&cancelled)),
            Err(Error::Cancelled)
        ));
    }
    assert!(
        stopped > 0 && el > 0 && full > 0,
        "the budgets stop {stopped} runs, fall back on {el} and answer {full}"
    );
}

/// A refused rooted run is not held to the rooted run's bound on derived conclusions,
/// since the Horn rules take its axioms. With a bound of one conclusion the four fixtures
/// give the default run's answer and report no fallback.
#[test]
fn a_refused_rooted_run_is_not_held_to_the_rooted_bound() {
    for name in FOUR {
        let c = named(name);
        for base in schedules() {
            let limits = Limits {
                dl_work: Some(1),
                ..base
            };
            let mut k = Classifier::of(&c.axioms);
            let out = k.classify(&rooted(&limits)).unwrap();
            assert_eq!(
                answer(&k, &out, &c.classes),
                classify(&c.axioms, &c.classes, &limits),
                "{name}"
            );
            assert!(!out.stats.dl_rooted_over, "{name}");
            assert!(!out.stats.dl_rooted_declined, "{name}");
            assert!(!out.stats.dl_over_budget, "{name}");
        }
    }
}

fn parse(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!("Prefix(:=<http://ex.org/>)\nOntology(\n{body}\n)"))
        .unwrap()
        .axioms
}

fn ex(names: &[&str]) -> BTreeSet<String> {
    names
        .iter()
        .map(|c| format!("http://ex.org/{c}"))
        .chain([THING.to_string(), NOTHING.to_string()])
        .collect()
}

fn pair(a: &str, b: &str) -> (String, String) {
    (format!("http://ex.org/{a}"), format!("http://ex.org/{b}"))
}

/// The union of the tests below: `D ⊑ E ⊔ F` with `E ⊑ G` and `F ⊑ G` gives `D ⊑ G`
/// to the rooted rules, which the Horn rules leave out.
const UNION: &str = "SubClassOf(:D ObjectUnionOf(:E :F))
     SubClassOf(:E :G)
     SubClassOf(:F :G)
     ClassAssertion(:D :d)";

/// Whether a rooted run and a run without the option give the same answer, flags and
/// stops under memory budgets from an eighth of the default run's high-water mark to more
/// than it, and at that mark itself, with partial answers or not.
fn same_under_budgets(axioms: &[Axiom], classes: &BTreeSet<String>) {
    let one = Limits {
        threads: 1,
        ..Limits::default()
    };
    let peak = Classifier::of(axioms)
        .classify(&one)
        .unwrap()
        .stats
        .peak_memory;
    let held = Classifier::of(axioms)
        .classify(&rooted(&one))
        .unwrap()
        .stats
        .peak_memory;
    // the check before the normalization interns nothing, so the rooted run holds no
    // more than the default run
    assert!(held <= peak, "{held} against {peak}");
    for budget in (4..=40).map(|s| peak * s / 32).chain([peak, peak - 1]) {
        for partial in [false, true] {
            let limits = Limits {
                threads: 1,
                partial,
                max_memory: Some(budget),
                ..Limits::default()
            };
            let at = format!("budget {budget} of {peak}, partial {partial}");
            let mut k = Classifier::of(axioms);
            let got = k.classify(&rooted(&limits));
            let mut plain = Classifier::of(axioms);
            let want = plain.classify(&limits);
            match (&got, &want) {
                (Ok(g), Ok(w)) => {
                    assert_eq!(answer(&k, g, classes), answer(&plain, w, classes), "{at}");
                    assert_eq!(g.complete, w.complete, "{at}");
                    assert_eq!(g.complete_for_taken, w.complete_for_taken, "{at}");
                    assert_eq!(g.stats.horn_over_budget, w.stats.horn_over_budget, "{at}");
                    assert_eq!(g.stats.dl_over_budget, w.stats.dl_over_budget, "{at}");
                    assert_eq!(g.stats.dl_contexts, 0, "{at}");
                }
                (Err(Error::Memory { .. }), Err(Error::Memory { .. })) => {}
                _ => panic!("{at}: {:?} against {:?}", got.err(), want.err()),
            }
        }
    }
}

/// A rooted run whose first axioms need fresh classes in the Tier D normalization and
/// whose later axiom refuses holds what the default run holds, under every budget. The
/// minimum of zero hides its filler, which needs counting, so the first axioms do not
/// refuse, and the minimum of two on `:q` does.
#[test]
fn a_refusal_after_axioms_with_fresh_classes_holds_what_the_default_run_holds() {
    let mut body = String::from(UNION);
    let mut names = vec!["D", "E", "F", "G", "Z"];
    let owned: Vec<String> = (0..24)
        .flat_map(|i| [format!("A{i}"), format!("B{i}"), format!("C{i}")])
        .collect();
    for i in 0..24 {
        body.push_str(&format!(
            "\nSubClassOf(:A{i} ObjectIntersectionOf(ObjectComplementOf(:B{i})
                 ObjectMinCardinality(0 :r ObjectMinCardinality(2 :r))))
             SubClassOf(:C{i} ObjectIntersectionOf(ObjectComplementOf(:B{i})
                 ObjectComplementOf(:A{i}) ObjectMinCardinality(2 :q{i})))"
        ));
    }
    body.push_str("\nSubClassOf(:Z ObjectMinCardinality(2 :q))");
    names.extend(owned.iter().map(String::as_str));
    let axioms = parse(&body);
    let classes = ex(&names);
    let out = Classifier::of(&axioms)
        .classify(&rooted(&Limits::default()))
        .unwrap();
    assert_eq!(out.stats.dl_contexts, 0, "the Horn rules take the axioms");
    same_under_budgets(&axioms, &classes);
}

/// A minimum that the Horn rules unfold refuses, while one above their limit of 64, which
/// both rules leave out, does not, and the rooted run keeps the union and `D ⊑ G`.
#[test]
fn a_minimum_the_horn_rules_leave_out_does_not_refuse() {
    let classes = ex(&["A", "D", "E", "F", "G"]);
    let d_below_g = pair("D", "G");
    for n in [2, 64, 65] {
        let axioms = parse(&format!(
            "{UNION}\nSubClassOf(:A ObjectMinCardinality({n} :r))"
        ));
        let mut k = Classifier::of(&axioms);
        let out = k.classify(&rooted(&Limits::default())).unwrap();
        let got = answer(&k, &out, &classes);
        let want = classify(&axioms, &classes, &Limits::default());
        if n <= 64 {
            assert_eq!(got, want, "{n}");
            assert_eq!(out.stats.dl_contexts, 0, "{n}");
            assert!(
                !got.left_out.iter().any(|a| a.contains("MinCardinality")),
                "{n}: {:?}",
                got.left_out
            );
            assert!(!got.subsumptions.contains(&d_below_g), "{n}");
        } else {
            assert!(out.stats.dl_contexts > 0, "the rooted rules take the union");
            assert!(got.subsumptions.contains(&d_below_g));
            assert_eq!(got.left_out.len(), 1, "{:?}", got.left_out);
            assert!(got.left_out.iter().all(|a| a.contains("MinCardinality(65")));
            // the default run leaves out the union too, and the minimum
            assert!(want.left_out.len() > got.left_out.len());
        }
    }
}

/// A minimum above 64 with an empty filler, and a maximum in the body whose dual is a
/// minimum of 65, is empty for the Horn rules, while the rooted rules with counting unfold it before they
/// read its filler and leave it out past 64. It does not refuse, and the rooted run keeps
/// the union and `D ⊑ G`.
#[test]
fn a_bottom_minimum_past_the_tier_d_bound_does_not_refuse() {
    let classes = ex(&["A", "D", "E", "F", "G"]);
    for (axiom, shown) in [
        (
            "SubClassOf(:A ObjectMinCardinality(65 :r owl:Nothing))",
            "MinCardinality(65",
        ),
        (
            "SubClassOf(ObjectMaxCardinality(64 :r owl:Nothing) :A)",
            "MaxCardinality(64",
        ),
    ] {
        let axioms = parse(&format!("{UNION}\n{axiom}"));
        let mut k = Classifier::of(&axioms);
        let out = k.classify(&rooted(&Limits::default())).unwrap();
        let got = answer(&k, &out, &classes);
        assert!(
            out.stats.dl_contexts > 0,
            "{axiom}: the rooted rules take the union"
        );
        assert!(got.subsumptions.contains(&pair("D", "G")), "{axiom}");
        assert_eq!(got.left_out.len(), 1, "{axiom}: {:?}", got.left_out);
        assert!(
            got.left_out.iter().all(|a| a.contains(shown)),
            "{axiom}: {:?}",
            got.left_out
        );
    }
}

/// A maximum of 64 with an empty filler inside the filler of a minimum of two in the head
/// is in a positive position, where the rooted rules with counting take it, and so do the
/// Horn rules. The axiom refuses, and the rooted run answers as the default run does.
#[test]
fn a_bottom_maximum_inside_a_minimum_refuses() {
    let classes = ex(&["A", "D", "E", "F", "G"]);
    let axiom = "SubClassOf(:A ObjectMinCardinality(2 :r ObjectMaxCardinality(64 :s owl:Nothing)))";
    let axioms = parse(&format!("{UNION}\n{axiom}"));
    let mut k = Classifier::of(&axioms);
    let out = k.classify(&rooted(&Limits::default())).unwrap();
    let got = answer(&k, &out, &classes);
    let want = classify(&axioms, &classes, &Limits::default());
    assert_eq!(got, want);
    assert_eq!(out.stats.dl_contexts, 0, "the Horn rules take the axioms");
    assert!(!got.subsumptions.contains(&pair("D", "G")));
    assert!(
        !got.left_out.iter().any(|a| a.contains("Cardinality")),
        "{:?}",
        got.left_out
    );
}

/// Disjuncts that the Horn rules intern as one expression are one disjunct, such as an
/// existential restriction and the minimum of one with its filler, or two maximums of one
/// whose fillers differ by `owl:Thing`. The axiom refuses, and the rooted run answers as
/// the default run does.
#[test]
fn disjuncts_the_horn_rules_intern_as_one_refuse() {
    let classes = ex(&["A", "B", "D", "E", "F", "G"]);
    for axiom in [
        "SubClassOf(:A ObjectIntersectionOf(ObjectMinCardinality(2 :s)
             ObjectUnionOf(ObjectSomeValuesFrom(:r :B) ObjectMinCardinality(1 :r :B))))",
        "SubClassOf(:A ObjectUnionOf(ObjectMaxCardinality(1 :r :B)
             ObjectMaxCardinality(1 :r ObjectIntersectionOf(:B owl:Thing))))",
    ] {
        let axioms = parse(&format!("{UNION}\n{axiom}"));
        let mut k = Classifier::of(&axioms);
        let out = k.classify(&rooted(&Limits::default())).unwrap();
        let got = answer(&k, &out, &classes);
        let want = classify(&axioms, &classes, &Limits::default());
        assert_eq!(got, want, "{axiom}");
        assert_eq!(out.stats.dl_contexts, 0, "{axiom}");
        assert!(!got.subsumptions.contains(&pair("D", "G")), "{axiom}");
        assert!(
            !got.left_out.iter().any(|a| a.contains("Cardinality")),
            "{axiom}: {:?}",
            got.left_out
        );
    }
}

/// A minimum of zero holds nothing, so a maximum in its filler does not refuse beside a
/// self restriction or a data restriction, which the rooted rules leave out for their own
/// reasons, and the rooted run keeps the union.
#[test]
fn a_zero_minimum_beside_a_self_or_data_restriction_does_not_refuse() {
    let classes = ex(&["A", "D", "E", "F", "G"]);
    for head in [
        "ObjectHasSelf(:t)",
        "DataSomeValuesFrom(:dp <http://www.w3.org/2001/XMLSchema#integer>)",
    ] {
        let axioms = parse(&format!(
            "{UNION}\nSubClassOf(ObjectMinCardinality(0 :r ObjectMaxCardinality(1 :s :A)) {head})"
        ));
        let mut k = Classifier::of(&axioms);
        let out = k.classify(&rooted(&Limits::default())).unwrap();
        let got = answer(&k, &out, &classes);
        assert!(
            out.stats.dl_contexts > 0,
            "{head}: the rooted rules take the union"
        );
        assert!(got.subsumptions.contains(&pair("D", "G")), "{head}");
        assert!(
            !got.left_out.iter().any(|a| a.contains("ObjectUnionOf")),
            "{head}: {:?}",
            got.left_out
        );
    }
}

/// A rooted run over item 805's rewriting that gives its axioms to the Horn rules falls
/// back to the original axioms, which go to the Horn rules too, and holds, stops and
/// answers as the default run does under every budget (row 1304's rewritten axiom).
#[test]
fn a_refusal_over_the_rewriting_answers_as_the_default_run_under_budgets() {
    let axioms = parse(
        "DisjointClasses(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             ObjectMinCardinality(2 :r ObjectMinCardinality(1 owl:topObjectProperty :A)))
         SubClassOf(
             ObjectMinCardinality(2 :r ObjectSomeValuesFrom(owl:topObjectProperty :A))
             owl:Nothing)
         ClassAssertion(:B :b)
         SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))",
    );
    same_under_budgets(&axioms, &ex(&["A", "B", "Z", "Z1", "Z2"]));
}
