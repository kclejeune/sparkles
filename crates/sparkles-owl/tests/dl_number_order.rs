//! The order between number restrictions of a run of Tier D that counts without the root
//! (roadmap item 894, F24's "The order between number restrictions"), which
//! `Limits::number_order` asks for: `≥m r.F` entails `≥n s.G` when `m ≥ n`, `r ⊑ s` and
//! `F ⊑ G` are told, and contradicts `≤k s.G` when `m > k`. Each case classifies with the
//! order on one thread and on eight and without it on one thread, and where the run
//! without the order finishes its answer is the same.

use sparkles_owl::classify::{Classification, Classifier, Expansion, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    match s {
        "Nothing" => Class::nothing(),
        _ => Class::new(format!("{EX}{s}")),
    }
}

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

/// The classes of the axioms' signature with owl:Thing and owl:Nothing.
fn classes(ax: &[Axiom]) -> Vec<Class> {
    let mut cs: BTreeSet<Class> = ax
        .iter()
        .flat_map(|a| a.signature())
        .filter_map(|e| match e {
            Entity::Class(c) => Some(c),
            _ => None,
        })
        .collect();
    cs.insert(Class::thing());
    cs.insert(Class::nothing());
    cs.into_iter().collect()
}

/// Every subsumption between the classes that the taxonomy answers.
fn pairs(out: &Classification, cs: &[Class]) -> BTreeSet<(String, String)> {
    let mut v = BTreeSet::new();
    for a in cs {
        for b in cs {
            if out.taxonomy.is_subclass(a, b) {
                v.insert((a.as_str().to_string(), b.as_str().to_string()));
            }
        }
    }
    v
}

/// A run that counts, with the order or without it and with a bound on Tier D's
/// conclusions or the default one. Tier D takes every axiom.
fn run(ax: &[Axiom], order: bool, threads: usize, dl_work: Option<usize>) -> Classification {
    let mut k = Classifier::of(ax);
    let out = k
        .classify(&Limits {
            threads,
            counting: true,
            number_order: order,
            dl_work,
            ..Limits::default()
        })
        .unwrap();
    if !out.stats.dl_over_budget {
        assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert!(out.complete);
    }
    out
}

/// Classifies with the order on one thread and on eight, which must finish and agree, and
/// without it, whose answer must be the same when it finishes. Returns the run with the
/// order and the run without it, both on one thread.
fn both(body: &str) -> (Classification, Classification) {
    let ax = axioms(body);
    let cs = classes(&ax);
    let on = run(&ax, true, 1, None);
    assert!(
        !on.stats.dl_over_budget,
        "the run with the order passed its bound"
    );
    let on8 = run(&ax, true, 8, None);
    assert!(
        !on8.stats.dl_over_budget,
        "the run with the order passed its bound on eight threads"
    );
    assert_eq!(
        pairs(&on, &cs),
        pairs(&on8, &cs),
        "the answer with the order depends on the threads"
    );
    let off = run(&ax, false, 1, None);
    if !off.stats.dl_over_budget {
        assert_eq!(
            pairs(&on, &cs),
            pairs(&off, &cs),
            "the order changes the answer"
        );
    }
    (on, off)
}

fn check(
    body: &str,
    yes: &[(&str, &str)],
    no: &[(&str, &str)],
) -> (Classification, Classification) {
    let (on, off) = both(body);
    for (a, b) in yes {
        assert!(
            on.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is missing"
        );
    }
    for (a, b) in no {
        assert!(
            !on.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is not entailed"
        );
    }
    (on, off)
}

/// A disjunction that no other axiom meets, so that Tier D takes each case below.
const UNION: &str = "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))";

/// `PR_000026735`'s shape in the Protein Ontology: a class with exactly six `r`-neighbours
/// in `P` inherits exactly six in `Q`, and `P ⊑ Q` is told. Without the order the context
/// makes twelve witnesses and the at-most rule searches their matchings, which passes the
/// bound. With it the minimum over `P` entails the one over `Q`, so the context makes six.
#[test]
fn an_inherited_cardinality_over_a_wider_filler_makes_no_second_group_of_witnesses() {
    let (on, off) = check(
        &format!(
            "SubClassOf(:A ObjectExactCardinality(6 :r :P))
             SubClassOf(:B ObjectExactCardinality(6 :r :Q))
             SubClassOf(:P :Q) SubClassOf(:A :B)
             DisjointClasses(:NP :P)
             SubClassOf(:D :A)
             SubClassOf(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:Q :NP)))
             {UNION}"
        ),
        &[("D", "Nothing"), ("A", "B")],
        &[("A", "Nothing"), ("B", "A")],
    );
    assert!(
        off.stats.dl_over_budget,
        "the run without the order stayed in its bound"
    );
    assert!(on.stats.dl_work < 1 << 12);
}

/// `PR_000024883`'s shape: the same exact cardinality stated by two axioms, beside a
/// definition by a smaller one over the same role and filler, whose alternative is the
/// minimum above it. The two minimums of equal strength have one group of witnesses in a
/// context that holds both, and the alternative, which the context's maximum contradicts,
/// leaves the definition's clause.
#[test]
fn a_cardinality_stated_twice_has_one_group_of_witnesses() {
    let (on, off) = check(
        &format!(
            "SubClassOf(:A ObjectExactCardinality(3 :r :F))
             SubClassOf(:B ObjectExactCardinality(3 :r :F))
             SubClassOf(:C ObjectIntersectionOf(:A :B :K))
             EquivalentClasses(:D ObjectIntersectionOf(:K ObjectExactCardinality(2 :r :F)))
             EquivalentClasses(:E ObjectIntersectionOf(:K ObjectExactCardinality(3 :r :F)))
             SubClassOf(:Y ObjectIntersectionOf(:K ObjectExactCardinality(2 :r :F)))
             {UNION}"
        ),
        &[("C", "E"), ("Y", "D"), ("C", "A"), ("C", "B")],
        &[
            ("C", "D"),
            ("Y", "E"),
            ("C", "Nothing"),
            ("A", "E"),
            ("D", "E"),
        ],
    );
    assert!(
        off.stats.dl_over_budget,
        "the run without the order stayed in its bound"
    );
    assert!(on.stats.dl_work < 1 << 12);
}

/// A minimum above a maximum over a wider role and filler is a contradiction that the
/// order tells, and in a context that holds the maximum the larger minimum leaves a
/// clause's head.
#[test]
fn a_minimum_above_a_maximum_is_a_told_contradiction() {
    let (on, off) = check(
        &format!(
            "SubObjectPropertyOf(:r :s) SubClassOf(:F :G)
             SubClassOf(:A ObjectMinCardinality(3 :r :F))
             SubClassOf(:B ObjectMaxCardinality(2 :s :G))
             SubClassOf(:C ObjectIntersectionOf(:A :B))
             DisjointClasses(:G1 :G2) SubClassOf(:G1 :G) SubClassOf(:G2 :G)
             SubClassOf(:K ObjectIntersectionOf(:B ObjectSomeValuesFrom(:s :G1)
               ObjectSomeValuesFrom(:s :G2)))
             EquivalentClasses(:E ObjectExactCardinality(2 :s :G))
             SubClassOf(:L ObjectIntersectionOf(:B
               ObjectUnionOf(:L1 ObjectMinCardinality(3 :r :F))))
             {UNION}"
        ),
        &[("C", "Nothing"), ("K", "E"), ("L", "L1")],
        &[
            ("A", "Nothing"),
            ("B", "Nothing"),
            ("K", "Nothing"),
            ("B", "E"),
            ("L", "Nothing"),
        ],
    );
    assert!(
        !off.stats.dl_over_budget && on.stats.dl_work < off.stats.dl_work,
        "the order saved no work: {} against {}",
        on.stats.dl_work,
        off.stats.dl_work
    );
}

/// The told subsumers of `owl:Thing` are above every filler, and three minimums whose
/// fillers are equivalent through them are ordered by name. A closure that left out what
/// is told of `owl:Thing` made each of the three block another, so that none made its
/// witnesses and the classes stayed satisfiable.
#[test]
fn equivalent_fillers_through_owl_thing_do_not_block_each_other() {
    check(
        &format!(
            "SubClassOf(owl:Thing :B) SubClassOf(:B :A)
             SubClassOf(:X0 ObjectMinCardinality(2 :r :A))
             SubClassOf(:X1 ObjectMinCardinality(2 :r owl:Thing))
             SubClassOf(:X2 ObjectMinCardinality(2 :r :B))
             SubObjectPropertyOf(:r :s) DisjointObjectProperties(:r :s)
             {UNION}"
        ),
        &[("X0", "Nothing"), ("X1", "Nothing"), ("X2", "Nothing")],
        &[("Z", "Nothing")],
    );
}

/// Equivalent roles and a role hierarchy between counted roles: the minimums over `r` and
/// over its equivalent `t` entail each other, and both entail the minimum over `s`.
#[test]
fn equivalent_and_wider_roles_order_their_minimums() {
    let (on, off) = check(
        &format!(
            "EquivalentObjectProperties(:r :t) SubObjectPropertyOf(:r :s)
             SubClassOf(:A ObjectMinCardinality(3 :r :F))
             SubClassOf(:A ObjectMinCardinality(3 :t :F))
             SubClassOf(:A ObjectMinCardinality(2 :s :F))
             SubClassOf(:A ObjectMaxCardinality(3 :s :F))
             EquivalentClasses(:E ObjectExactCardinality(3 :s :F))
             EquivalentClasses(:E2 ObjectExactCardinality(3 :t :F))
             SubClassOf(:B ObjectIntersectionOf(:A ObjectSomeValuesFrom(:s
               ObjectIntersectionOf(:F :N))))
             SubClassOf(:F ObjectAllValuesFrom(ObjectInverseOf(:r) :HasR))
             SubClassOf(:N ObjectAllValuesFrom(ObjectInverseOf(:r) :HasN))
             {UNION}"
        ),
        &[("A", "E"), ("A", "E2"), ("A", "HasR"), ("B", "HasN")],
        &[("A", "Nothing"), ("B", "Nothing"), ("A", "HasN")],
    );
    assert!(
        off.stats.dl_over_budget,
        "the run without the order stayed in its bound"
    );
    assert!(on.stats.dl_work < 1 << 12);
}

/// Over a symmetric role an element's predecessor is one of its neighbours. Each witness
/// of the root's minimum of three is in one of the minimum's three disjoint classes and
/// has a successor in `Leaf`, which has exactly two neighbours in `F`, the witness among
/// them. `Leaf`'s minimum of two has two classes of its own, so the witness can be either
/// of its two. Names shared between the two minimums, the first two of three, would give
/// the third witness's successor three neighbours in `F`, and make it, the witness and
/// the root unsatisfiable.
#[test]
fn a_predecessor_over_a_symmetric_role_counts_for_a_smaller_minimum() {
    check(
        &format!(
            "SymmetricObjectProperty(:r)
             SubClassOf(:Root ObjectIntersectionOf(:F ObjectMinCardinality(3 :r :F)
               ObjectAllValuesFrom(:r :Mid)))
             SubClassOf(:Mid ObjectIntersectionOf(:F ObjectSomeValuesFrom(:r :Leaf)))
             SubClassOf(:Leaf ObjectExactCardinality(2 :r :F))
             SubClassOf(:Bad ObjectIntersectionOf(:Leaf ObjectMinCardinality(3 :r :F)))
             {UNION}"
        ),
        &[("Bad", "Nothing")],
        &[("Root", "Nothing"), ("Mid", "Nothing"), ("Leaf", "Nothing")],
    );
}

/// Recursive cardinalities over a symmetric role and over an explicit inverse, where the
/// predecessor is one of the counted neighbours and a minimum that the context's larger
/// minimum entails makes no witnesses of its own.
#[test]
fn recursive_cardinalities_count_the_predecessor() {
    let (on, off) = check(
        &format!(
            "SymmetricObjectProperty(:r)
             SubClassOf(:A ObjectExactCardinality(3 :r :A))
             SubClassOf(:A ObjectMinCardinality(2 :r :A))
             EquivalentClasses(:E ObjectExactCardinality(3 :r :A))
             DisjointClasses(:C1 :C2 :C3 :C4)
             SubClassOf(:B ObjectIntersectionOf(:A
               ObjectSomeValuesFrom(:r ObjectIntersectionOf(:A :C1))
               ObjectSomeValuesFrom(:r ObjectIntersectionOf(:A :C2))
               ObjectSomeValuesFrom(:r ObjectIntersectionOf(:A :C3))
               ObjectSomeValuesFrom(:r ObjectIntersectionOf(:A :C4))))
             SubClassOf(:P ObjectMinCardinality(2 :q :Q))
             SubClassOf(:Q ObjectExactCardinality(2 ObjectInverseOf(:q) :P))
             SubClassOf(:D ObjectIntersectionOf(:Q ObjectMinCardinality(3 ObjectInverseOf(:q) :P)))
             {UNION}"
        ),
        &[("A", "E"), ("B", "Nothing"), ("D", "Nothing")],
        &[("A", "Nothing"), ("P", "Nothing"), ("Q", "Nothing")],
    );
    assert!(
        off.stats.dl_over_budget,
        "the run without the order stayed in its bound"
    );
    assert!(on.stats.dl_work < 1 << 12);
}

/// Qualifiers that are intersections and unions. An intersection's operands are above it
/// in the order. A union filler in a positive position is a clause, which the order does
/// not read, so the contradiction with the maximum comes from the witnesses' cases.
#[test]
fn intersection_and_union_qualifiers() {
    check(
        &format!(
            "SubClassOf(:A ObjectMinCardinality(2 :r ObjectIntersectionOf(:P :Q)))
             SubClassOf(:B ObjectMaxCardinality(1 :r :P))
             SubClassOf(:C ObjectIntersectionOf(:A :B))
             SubClassOf(:A2 ObjectMinCardinality(2 :r ObjectUnionOf(:P :Q)))
             SubClassOf(:B2 ObjectMaxCardinality(1 :r ObjectUnionOf(:P :Q)))
             SubClassOf(:C2 ObjectIntersectionOf(:A2 :B2))
             SubClassOf(:B3 ObjectMaxCardinality(1 :r :P))
             SubClassOf(:C3 ObjectIntersectionOf(:A2 :B3))
             {UNION}"
        ),
        &[("C", "Nothing"), ("C2", "Nothing")],
        &[("A", "Nothing"), ("A2", "Nothing"), ("C3", "Nothing")],
    );
}

/// An unqualified minimum is one over `owl:Thing`, which is below what the ontology tells
/// of `owl:Thing`, so it contradicts a smaller maximum over such a class.
#[test]
fn an_unqualified_minimum_is_below_the_told_subsumers_of_owl_thing() {
    check(
        &format!(
            "SubClassOf(owl:Thing :T1)
             SubClassOf(:A ObjectMinCardinality(3 :r))
             SubClassOf(:B ObjectMaxCardinality(2 :r :T1))
             SubClassOf(:C ObjectIntersectionOf(:A :B))
             SubClassOf(:A2 ObjectMinCardinality(2 :r))
             SubClassOf(:C2 ObjectIntersectionOf(:A2 :B))
             {UNION}"
        ),
        &[("C", "Nothing")],
        &[("A", "Nothing"), ("B", "Nothing"), ("C2", "Nothing")],
    );
}

/// A minimum over `owl:Nothing` is `owl:Nothing`, and a minimum over a filler that is
/// unsatisfiable by its axioms makes its class unsatisfiable through its witnesses,
/// whether a larger minimum over the same filler blocks it or not.
/// The maximum over another role makes the run count, which minimums in positive
/// positions alone do not.
#[test]
fn a_minimum_over_an_unsatisfiable_filler() {
    check(
        &format!(
            "SubClassOf(:A0 ObjectMinCardinality(2 :r owl:Nothing))
             SubClassOf(:F ObjectIntersectionOf(:F1 :F2)) DisjointClasses(:F1 :F2)
             SubClassOf(:A ObjectMinCardinality(2 :r :F))
             SubClassOf(:B ObjectMinCardinality(3 :r :F))
             SubClassOf(:C ObjectIntersectionOf(:A :B))
             SubClassOf(:G ObjectMinCardinality(2 :r :H))
             SubClassOf(:M ObjectMaxCardinality(5 :q))
             {UNION}"
        ),
        &[
            ("A0", "Nothing"),
            ("A", "Nothing"),
            ("B", "Nothing"),
            ("C", "Nothing"),
            ("F", "Nothing"),
        ],
        &[("G", "Nothing"), ("F1", "Nothing")],
    );
}

/// A minimum under a clause: an alternative that the context's maximum contradicts leaves
/// the head, and an alternative that a unit minimum entails makes no witnesses.
#[test]
fn a_minimum_in_a_disjunction() {
    let (on, off) = check(
        &format!(
            "SubClassOf(:A ObjectUnionOf(ObjectMinCardinality(2 :r :F) ObjectMinCardinality(3 :r :F)))
             SubClassOf(:A ObjectMaxCardinality(1 :r :F))
             SubClassOf(:B ObjectUnionOf(:B1 ObjectMinCardinality(3 :r :F)))
             SubClassOf(:B ObjectExactCardinality(2 :r :F))
             SubClassOf(:C ObjectUnionOf(:C1 ObjectMinCardinality(2 :r :F)))
             SubClassOf(:C ObjectMinCardinality(3 :r :F))
             SubClassOf(:C ObjectMaxCardinality(3 :r :F))
             {UNION}"
        ),
        &[("A", "Nothing"), ("B", "B1")],
        &[("B", "Nothing"), ("C", "Nothing"), ("C", "C1")],
    );
    assert!(
        !off.stats.dl_over_budget && on.stats.dl_work < off.stats.dl_work,
        "the order saved no work: {} against {}",
        on.stats.dl_work,
        off.stats.dl_work
    );
}

/// A minimum under a clause whose name is not the representative of its strength: `X`
/// states the minimum first, so its name stands for the one in `A`'s union. In `A` neither
/// holds as a unit, so nothing blocks the alternative, whose witnesses over the
/// unsatisfiable filler refute it and leave `P`.
#[test]
fn a_minimum_under_a_clause_makes_its_witnesses_without_its_representative() {
    check(
        &format!(
            "SubClassOf(:X ObjectMinCardinality(2 :r :F))
             SubClassOf(:A ObjectUnionOf(:P ObjectMinCardinality(2 :r :F)))
             SubClassOf(:F ObjectIntersectionOf(:F1 :F2)) DisjointClasses(:F1 :F2)
             SubClassOf(:M ObjectMaxCardinality(5 :q))
             {UNION}"
        ),
        &[("A", "P"), ("X", "Nothing"), ("F", "Nothing")],
        &[("A", "Nothing"), ("P", "Nothing")],
    );
}

/// A minimum that another can entail waits in its context until the context has taken
/// its units, and a run that stops meanwhile has not finished that context. `A` holds a
/// minimum over an unsatisfiable filler, which the larger minimum of `B` can entail, and
/// thousands of told subsumers, so that the run's polls fall inside its context. Every
/// stop that answers in part reports `A` open or unsatisfiable.
#[test]
fn a_stopped_run_leaves_the_class_of_a_waiting_minimum_open() {
    let mut body = String::from(
        "DisjointObjectProperties(:t :u)
         SubClassOf(:A ObjectMinCardinality(2 :r :F))
         SubClassOf(:B ObjectMinCardinality(3 :r :F))
         SubClassOf(:F owl:Nothing)\n",
    );
    for j in 0..6000 {
        body.push_str(&format!("SubClassOf(:A :S{j})\n"));
    }
    body.push_str(UNION);
    let ax = axioms(&body);
    let limits = |stop_at: usize, polls: Arc<AtomicUsize>| Limits {
        threads: 1,
        counting: true,
        number_order: true,
        partial: true,
        cancelled: Some(Arc::new(move || {
            polls.fetch_add(1, Ordering::Relaxed) >= stop_at
        })),
        ..Limits::default()
    };
    let polls = Arc::new(AtomicUsize::new(0));
    let full = Classifier::of(&ax)
        .classify(&limits(usize::MAX, polls.clone()))
        .unwrap();
    assert!(full.complete);
    assert!(full.taxonomy.is_subclass(&c("A"), &c("Nothing")));
    let total = polls.load(Ordering::Relaxed);
    let mut stopped = 0;
    for k in 0..total {
        let Ok(out) = Classifier::of(&ax).classify(&limits(k, Arc::new(AtomicUsize::new(0))))
        else {
            continue;
        };
        if out.complete {
            continue;
        }
        stopped += 1;
        assert!(
            out.incomplete.contains(&c("A")) || out.taxonomy.is_subclass(&c("A"), &c("Nothing")),
            "a stop at poll {k} of {total} reports A complete and satisfiable"
        );
    }
    assert!(stopped > 0, "no stop answered in part");
}

const THING: &str = "http://www.w3.org/2002/07/owl#Thing";
const NOTHING: &str = "http://www.w3.org/2002/07/owl#Nothing";

/// The subsumptions between `classes` that a reference taxonomy in functional-style
/// syntax entails, as `tests/dl_fixtures.rs` reads it.
fn reference(text: &str, classes: &BTreeSet<String>) -> BTreeSet<(String, String)> {
    let iris = |line: &str| -> Vec<String> {
        line.split('<')
            .skip(1)
            .filter_map(|p| p.split_once('>').map(|(iri, _)| iri.to_string()))
            .collect()
    };
    let mut up: std::collections::BTreeMap<String, BTreeSet<String>> = Default::default();
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

/// The fixtures of roadmap item 603 with a minimum or exact cardinality of two or more,
/// classified with counting and the order on one thread and on four with the eager
/// strategy. Every answer is among HermiT's subsumptions, a case taken whole within the
/// bound has exactly HermiT's, and every case that stays in the bound without the order
/// stays in it with the order. `shq-041` passes the bound without the order on every
/// schedule and stays in it with the order, with HermiT's answer.
#[test]
fn the_fixtures_with_minimums_keep_their_answers_with_the_order() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dl603");
    let mut cases: Vec<std::path::PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".ofn") && n.matches('.').count() == 1)
        })
        .collect();
    cases.sort();
    let mut failures = Vec::new();
    let mut counted = 0;
    // the cases that stay in the bound without the order, and with it
    let (mut within_off, mut within_on) = (0, 0);
    for case in &cases {
        let text = std::fs::read_to_string(case).unwrap();
        let least = |op: &str| {
            text.match_indices(op).any(|(i, _)| {
                text[i + op.len()..]
                    .split(|c: char| !c.is_ascii_digit())
                    .next()
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|n| n >= 2)
            })
        };
        if !least("ObjectMinCardinality(") && !least("ObjectExactCardinality(") {
            continue;
        }
        counted += 1;
        let stem = case.file_stem().and_then(|n| n.to_str()).unwrap();
        let doc = ofn_read::parse(&text).unwrap();
        let mut names: BTreeSet<String> = doc
            .axioms
            .iter()
            .filter_map(|a| match &a.kind {
                AxiomKind::Declaration(Entity::Class(c)) => Some(c.as_str().to_string()),
                _ => None,
            })
            .collect();
        names.insert(THING.to_string());
        names.insert(NOTHING.to_string());
        let want = reference(
            &std::fs::read_to_string(case.with_extension("hermit.ofn")).unwrap(),
            &names,
        );
        let answer = |order: bool, threads: usize, expansion: Expansion| {
            let mut k = Classifier::of(&doc.axioms);
            let out = k
                .classify(&Limits {
                    threads,
                    expansion,
                    counting: true,
                    number_order: order,
                    ..Limits::default()
                })
                .unwrap();
            let mut got = BTreeSet::new();
            for a in &names {
                for b in &names {
                    if out
                        .taxonomy
                        .is_subclass(&Class::new(a.as_str()), &Class::new(b.as_str()))
                    {
                        got.insert((a.clone(), b.clone()));
                    }
                }
            }
            let fell = out.stats.dl_over_budget || !out.complete;
            (got, k.excluded().is_empty(), fell)
        };
        let (_, _, fell_off) = answer(false, 1, Expansion::Cautious);
        within_off += !fell_off as usize;
        for (threads, expansion) in [(1, Expansion::Cautious), (4, Expansion::Eager)] {
            let (got, whole, fell) = answer(true, threads, expansion);
            let extra: Vec<_> = got.difference(&want).collect();
            if !extra.is_empty() {
                failures.push(format!(
                    "{stem} on {threads} threads: not entailed {extra:?}"
                ));
            }
            if whole && !fell {
                let missing: Vec<_> = want.difference(&got).collect();
                if !missing.is_empty() {
                    failures.push(format!("{stem} on {threads} threads: missing {missing:?}"));
                }
            }
            if threads == 1 {
                within_on += !fell as usize;
                if fell && !fell_off {
                    failures.push(format!("{stem}: the order makes the run pass its bound"));
                }
            }
            if stem == "shq-041" && (!fell_off || fell || !whole) {
                failures.push(format!(
                    "shq-041 on {threads} threads: without the order past the bound \
                     {fell_off}, with it {fell}, whole {whole}"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(counted, 206);
    assert!(
        within_on > within_off,
        "{within_on} cases within the bound with the order, {within_off} without"
    );
}
