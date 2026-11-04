//! Tier D of roadmap item 603: the rules of disjunctive ALCHI beside the EL rules, the
//! tests of the candidate subsumers that the total order of atoms leaves as clauses, and
//! the opt-in partial answer of a run that stops, with its consistency state.

use sparkles_owl::classify::{Classification, Classifier, Consistency, Expansion, Limits, Tier};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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

/// Every subsumption between the taxonomy's classes, owl:Thing and owl:Nothing included,
/// with an unsatisfiable class below the marker `⊥` alone, since it is below every class.
fn pairs(t: &sparkles_owl::classify::Taxonomy, cs: &[Class]) -> BTreeSet<(String, String)> {
    let names: BTreeSet<&str> = cs.iter().map(|c| c.as_str()).collect();
    let mut v = BTreeSet::new();
    for x in cs {
        if t.is_unsatisfiable(x) {
            v.insert((x.as_str().to_string(), "⊥".to_string()));
            continue;
        }
        let Some(n) = t.node_of(x) else { continue };
        for k in std::iter::once(n).chain(t.ancestors(n)) {
            for y in &t.node(k).classes {
                if names.contains(y.as_str()) {
                    v.insert((x.as_str().to_string(), y.as_str().to_string()));
                }
            }
        }
    }
    v
}

/// The classes of the axioms' signature, declared or not, with owl:Thing and owl:Nothing.
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

/// Classifies at one and at eight threads, checks that the answers agree and that Tier D
/// took every axiom, and returns one of them.
fn whole(body: &str) -> Classification {
    let ax = axioms(body);
    let cs = classes(&ax);
    let mut outs = Vec::new();
    for threads in [1, 8] {
        let mut k = Classifier::of(&ax);
        let out = k
            .classify(&Limits {
                threads,
                ..Limits::default()
            })
            .unwrap();
        assert!(k.excluded().is_empty(), "{:#?}", k.excluded());
        assert_eq!(out.stats.tier, Tier::Disjunctive);
        assert!(out.stats.clauses > 0, "Tier D took no clause");
        assert!(out.complete);
        outs.push(out);
    }
    assert_eq!(
        pairs(&outs[0].taxonomy, &cs),
        pairs(&outs[1].taxonomy, &cs),
        "the answer depends on the threads"
    );
    outs.pop().unwrap()
}

fn check(body: &str, yes: &[(&str, &str)], no: &[(&str, &str)]) {
    let out = whole(body);
    for (a, b) in yes {
        assert!(
            out.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is missing"
        );
    }
    for (a, b) in no {
        assert!(
            !out.taxonomy.is_subclass(&c(a), &c(b)),
            "{a} ⊑ {b} is not entailed"
        );
    }
}

/// A disjunction that no other axiom meets, so that Tier D takes each case below.
const UNION: &str = "SubClassOf(:Z ObjectUnionOf(:Z1 :Z2))";

#[test]
fn a_case_analysis_finds_the_common_subsumer() {
    check(
        &format!(
            "SubClassOf(:A ObjectUnionOf(:B :C)) SubClassOf(:B :D) SubClassOf(:C :D)
             SubClassOf(:C :E) {UNION}"
        ),
        &[("A", "D")],
        &[("A", "E"), ("A", "B"), ("A", "C")],
    );
}

#[test]
fn a_closed_case_leaves_the_other() {
    // the case B meets B's disjointness with A, which leaves C
    check(
        "SubClassOf(:A ObjectUnionOf(:B :C)) DisjointClasses(:A :B)",
        &[("A", "C")],
        &[("A", "B")],
    );
}

#[test]
fn a_universal_meets_a_disjunctive_successor() {
    // A has an r-successor that is B or C, and every r-successor of A is not C
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r ObjectUnionOf(:B :C)))
         SubClassOf(:A ObjectAllValuesFrom(:r ObjectComplementOf(:C)))
         EquivalentClasses(:D ObjectSomeValuesFrom(:r :B))",
        &[("A", "D")],
        &[("D", "A")],
    );
}

#[test]
fn a_universal_over_an_inverse_reaches_the_predecessor() {
    // B's r⁻-successors, A's instances among them, are C or E, and E is excluded
    check(
        "SubClassOf(:A ObjectSomeValuesFrom(:r :B))
         SubClassOf(:B ObjectAllValuesFrom(ObjectInverseOf(:r) ObjectUnionOf(:C :E)))
         DisjointClasses(:A :E)",
        &[("A", "C")],
        &[("B", "C")],
    );
}

#[test]
fn a_complement_on_the_left_is_a_global_case() {
    // ¬B ⊑ C makes everything B or C
    check(
        "Declaration(Class(:A))
         SubClassOf(ObjectComplementOf(:B) :C) SubClassOf(:B :D) SubClassOf(:C :D)",
        &[("A", "D")],
        &[("A", "B"), ("A", "C")],
    );
}

#[test]
fn a_role_hierarchy_carries_the_universal() {
    check(
        &format!(
            "SubObjectPropertyOf(:s :r)
             SubClassOf(:A ObjectSomeValuesFrom(:s :B))
             SubClassOf(:A ObjectAllValuesFrom(:r :C))
             EquivalentClasses(:D ObjectSomeValuesFrom(:s ObjectIntersectionOf(:B :C))) {UNION}"
        ),
        &[("A", "D")],
        &[],
    );
}

#[test]
fn a_transitive_role_carries_a_negative_existential() {
    // the r-path A → B → C, with ∃r.C ⊑ D through the transitive closure
    check(
        &format!(
            "TransitiveObjectProperty(:r)
             SubClassOf(:A ObjectSomeValuesFrom(:r :B))
             SubClassOf(:B ObjectSomeValuesFrom(:r :C))
             SubClassOf(ObjectSomeValuesFrom(:r :C) :D) {UNION}"
        ),
        &[("A", "D"), ("B", "D")],
        &[("C", "D")],
    );
}

#[test]
fn a_transitive_role_carries_a_universal() {
    check(
        &format!(
            "TransitiveObjectProperty(:r)
             SubClassOf(:A ObjectAllValuesFrom(:r :E))
             SubClassOf(:A ObjectSomeValuesFrom(:r :B))
             SubClassOf(:B ObjectSomeValuesFrom(:r :C))
             DisjointClasses(:C :E) {UNION}"
        ),
        &[("A", "Nothing")],
        &[("B", "Nothing")],
    );
}

#[test]
fn a_chain_carries_a_negative_existential() {
    check(
        &format!(
            "SubObjectPropertyOf(ObjectPropertyChain(:p :q) :t)
             SubClassOf(:A ObjectSomeValuesFrom(:p :B))
             SubClassOf(:B ObjectSomeValuesFrom(:q :C))
             SubClassOf(ObjectSomeValuesFrom(:t :C) :D) {UNION}"
        ),
        &[("A", "D")],
        &[("B", "D")],
    );
}

#[test]
fn the_range_of_a_transitive_role_holds_along_its_paths() {
    check(
        &format!(
            "TransitiveObjectProperty(:r)
             SubObjectPropertyOf(:s :r)
             ObjectPropertyRange(:r :R)
             SubClassOf(:A ObjectSomeValuesFrom(:s :B))
             SubClassOf(:B ObjectSomeValuesFrom(:s :C))
             EquivalentClasses(:D ObjectSomeValuesFrom(:s ObjectSomeValuesFrom(:s ObjectIntersectionOf(:C :R))))
             {UNION}"
        ),
        &[("A", "D")],
        &[],
    );
}

/// The ontology of the partial answers: chains of cases whose common subsumers hold, over
/// existential restrictions, universals and an inverse role, large enough that a run
/// stops part way at many points.
fn many(n: usize, inconsistent: bool) -> String {
    let mut s = String::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = |m: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % m as u64) as usize
    };
    for i in 0..n {
        s.push_str(&format!("Declaration(Class(:C{i}))\n"));
    }
    for i in 0..n {
        let (b, d) = (next(n), next(n));
        // Ci ⊑ Bi ⊔ Ei with Bi and Ei both below Ci's common subsumer Ki
        s.push_str(&format!(
            "SubClassOf(:C{i} ObjectUnionOf(:B{i} :E{i}))
             SubClassOf(:B{i} :K{i}) SubClassOf(:E{i} :K{i}) SubClassOf(:B{i} :C{b})
             SubClassOf(:K{i} ObjectSomeValuesFrom(:r :C{d}))
             SubClassOf(:E{i} ObjectAllValuesFrom(ObjectInverseOf(:r) :F{d}))
             SubClassOf(ObjectSomeValuesFrom(:r :F{i}) :G{b})\n"
        ));
    }
    if inconsistent {
        // every element has a successor that is a case of two empty classes
        s.push_str(
            "SubClassOf(owl:Thing ObjectSomeValuesFrom(:s ObjectUnionOf(:N1 :N2)))
             SubClassOf(:N1 owl:Nothing) SubClassOf(:N2 :N3) DisjointClasses(:N3 :N2)\n",
        );
    }
    s
}

/// Checks a partial answer against the full one: its subsumptions are among the full
/// answer's, a class it reports complete has its full answer, and an undecided
/// consistency leaves every class open.
fn check_partial(part: &Classification, full: &Classification, cs: &[Class]) {
    let got = pairs(&part.taxonomy, cs);
    let want = pairs(&full.taxonomy, cs);
    // a class that the full answer makes unsatisfiable is below every class
    let extra: Vec<_> = got
        .difference(&want)
        .filter(|(x, _)| !want.contains(&(x.clone(), "⊥".to_string())))
        .collect();
    assert!(extra.is_empty(), "a partial answer has {extra:?}");
    if part.complete {
        assert_eq!(got, want);
        return;
    }
    let open: BTreeSet<&Class> = part.incomplete.iter().collect();
    if part.consistency == Consistency::Unknown {
        for x in cs {
            if *x != Class::thing() && *x != Class::nothing() {
                assert!(
                    open.contains(x),
                    "{x:?} is complete with the consistency unknown"
                );
            }
        }
    }
    let wrong = if full.consistent {
        Consistency::Inconsistent
    } else {
        Consistency::Consistent
    };
    assert_ne!(
        part.consistency, wrong,
        "the partial answer contradicts the consistency"
    );
    for x in cs {
        if open.contains(x) || *x == Class::thing() || *x == Class::nothing() {
            continue;
        }
        let above = |a: &BTreeSet<(String, String)>| -> BTreeSet<String> {
            a.iter()
                .filter(|(p, _)| p == x.as_str())
                .map(|(_, q)| q.clone())
                .collect()
        };
        assert_eq!(above(&got), above(&want), "{x:?} is reported complete");
    }
}

/// Stops the run at the `k`-th poll of its cancellation, which falls at the same point of
/// a run on one thread every time.
fn cancelled_at(k: usize, threads: usize, expansion: Expansion) -> Limits {
    let polls = Arc::new(AtomicUsize::new(0));
    Limits {
        threads,
        expansion,
        partial: true,
        cancelled: Some(Arc::new(move || polls.fetch_add(1, Ordering::Relaxed) >= k)),
        ..Limits::default()
    }
}

fn partial_answers(inconsistent: bool) {
    // a debug build runs the rules some ten times slower
    let n = if cfg!(debug_assertions) { 80 } else { 250 };
    let ax = axioms(&many(n, inconsistent));
    let cs = classes(&ax);
    let full = Classifier::of(&ax).classify(&Limits::default()).unwrap();
    assert!(full.complete);
    assert_eq!(full.stats.tier, Tier::Disjunctive);
    assert_eq!(full.consistent, !inconsistent);
    if !inconsistent {
        assert!(full.stats.dl_tests > 0, "the ontology tests no candidate");
    }
    let mut partial = 0;
    // partial answers of the early stops that left the consistency unknown
    let mut early_unknown = 0;
    let mut some_complete = 0;
    // partial answers that stopped after a round of tests
    let mut in_tests = 0;
    for (threads, expansion) in [
        (1, Expansion::Cautious),
        (8, Expansion::Cautious),
        (8, Expansion::Eager),
    ] {
        // the polls of a whole run, so that the stops spread over all of it
        let polls = Arc::new(AtomicUsize::new(0));
        let p = polls.clone();
        let again = Classifier::of(&ax)
            .classify(&Limits {
                threads,
                expansion,
                cancelled: Some(Arc::new(move || {
                    p.fetch_add(1, Ordering::Relaxed);
                    false
                })),
                ..Limits::default()
            })
            .unwrap();
        assert_eq!(pairs(&again.taxonomy, &cs), pairs(&full.taxonomy, &cs));
        let total = polls.load(Ordering::Relaxed);
        // the first stop that answers, and the stops right after it, which come before
        // every root has a context of its own
        let first = (0..total).find(|&k| {
            Classifier::of(&ax)
                .classify(&cancelled_at(k, threads, expansion))
                .is_ok()
        });
        let early = first.map_or(0..0, |f| f..(f + 64).min(total));
        for k in early.clone() {
            let out = Classifier::of(&ax)
                .classify(&cancelled_at(k, threads, expansion))
                .unwrap();
            check_partial(&out, &full, &cs);
            if !out.complete {
                partial += 1;
                early_unknown += (out.consistency == Consistency::Unknown) as usize;
            }
        }
        // deadlines from no time on, which stop before the saturation, in it before every
        // root has a context, and later
        for us in [0u64, 1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000] {
            let limits = Limits {
                threads,
                expansion,
                partial: true,
                deadline: Some(Instant::now() + Duration::from_micros(us)),
                ..Limits::default()
            };
            if let Ok(out) = Classifier::of(&ax).classify(&limits) {
                check_partial(&out, &full, &cs);
            }
        }
        for k in (0..total).step_by((total / 32).max(1)) {
            let mut kl = Classifier::of(&ax);
            let Ok(out) = kl.classify(&cancelled_at(k, threads, expansion)) else {
                // a stop before the saturation starts fails as before
                continue;
            };
            check_partial(&out, &full, &cs);
            if !out.complete {
                partial += 1;
                in_tests += (out.stats.dl_tests > 0) as usize;
                if out.incomplete.len() + 2 < cs.len() {
                    some_complete += 1;
                }
            }
        }
        // deadlines from the start of the run on
        let took = {
            let t = Instant::now();
            Classifier::of(&ax)
                .classify(&Limits {
                    threads,
                    expansion,
                    ..Limits::default()
                })
                .unwrap();
            t.elapsed()
        };
        for f in [0.05, 0.2, 0.4, 0.6, 0.8] {
            let mut kl = Classifier::of(&ax);
            let limits = Limits {
                threads,
                expansion,
                partial: true,
                deadline: Some(Instant::now() + took.mul_f64(f).max(Duration::from_micros(10))),
                ..Limits::default()
            };
            if let Ok(out) = kl.classify(&limits) {
                check_partial(&out, &full, &cs);
            }
        }
    }
    assert!(partial > 0, "no stop gave a partial answer");
    // The earliest stops leave the context of owl:Thing open, where an existential
    // restriction on every element is still to be saturated. A consistent ontology's
    // owl:Thing has no successor here and closes before any stop answers.
    if inconsistent {
        assert!(
            early_unknown > 0,
            "no early stop left the consistency unknown"
        );
    }
    if !inconsistent {
        assert!(some_complete > 0, "no partial answer had a complete class");
        assert!(in_tests > 0, "no partial answer stopped among the tests");
    }
}

#[test]
fn a_partial_answer_is_part_of_the_full_one_and_complete_where_it_says() {
    partial_answers(false);
}

#[test]
fn an_unresolved_inconsistency_leaves_every_class_open() {
    partial_answers(true);
}

/// The cases of [`many`] with the successors picked by arithmetic, as the surfaces' tests
/// of the `sparkles` crate and the bindings write them.
fn cases(n: usize) -> String {
    let mut s = String::new();
    for i in 0..n {
        let (b, d) = ((7 * i + 3) % n, (13 * i + 5) % n);
        s.push_str(&format!(
            "SubClassOf(:C{i} ObjectUnionOf(:B{i} :E{i}))
             SubClassOf(:B{i} :K{i}) SubClassOf(:B{i} :C{b}) SubClassOf(:E{i} :K{i})
             SubClassOf(:K{i} ObjectSomeValuesFrom(:r :C{d}))
             SubClassOf(:E{i} ObjectAllValuesFrom(ObjectInverseOf(:r) :F{d}))
             SubClassOf(ObjectSomeValuesFrom(:r :F{i}) :G{b})\n"
        ));
    }
    s
}

#[test]
fn a_deadline_before_a_class_has_a_context_leaves_it_open() {
    // A deadline can stop the saturation before it made the contexts of some classes,
    // whose answers are then not complete, nor is the consistency known while the
    // context of owl:Thing is not made.
    let n = if cfg!(debug_assertions) { 120 } else { 400 };
    let ax = axioms(&cases(n));
    let cs = classes(&ax);
    let one = Limits {
        threads: 1,
        ..Limits::default()
    };
    let t = Instant::now();
    let full = Classifier::of(&ax).classify(&one).unwrap();
    let took = t.elapsed();
    let mut partial = 0;
    let mut d = Duration::from_micros(10);
    while d < took.mul_f64(1.5) {
        let limits = Limits {
            partial: true,
            deadline: Some(Instant::now() + d),
            ..one.clone()
        };
        if let Ok(out) = Classifier::of(&ax).classify(&limits) {
            check_partial(&out, &full, &cs);
            partial += (!out.complete) as usize;
        }
        d = d.mul_f64(1.1);
    }
    assert!(partial > 0, "no deadline gave a partial answer");
}

#[test]
fn a_run_without_the_flag_fails_when_stopped() {
    let ax = axioms(&many(200, false));
    let limits = Limits {
        partial: false,
        ..cancelled_at(8, 1, Expansion::Cautious)
    };
    assert!(Classifier::of(&ax).classify(&limits).is_err());
}

#[test]
fn a_chain_holds_in_its_mirror_image_under_inverses() {
    // r0 ∘ r2⁻ ⊑ r0 gives r2 ∘ r0⁻ ⊑ r0⁻, and r0⁻ is r2: A5's r2-successor A0 has an
    // r2-successor in A3 ⊓ A2, which A5 then has as well
    check(
        &format!(
            "SubObjectPropertyOf(ObjectPropertyChain(:r0 ObjectInverseOf(:r2)) :r0)
             InverseObjectProperties(:r2 :r0)
             SubClassOf(:A5 ObjectSomeValuesFrom(:r2 :A0))
             EquivalentClasses(:A0 ObjectSomeValuesFrom(:r2 ObjectIntersectionOf(:A3 :A2)))
             {UNION}"
        ),
        &[("A5", "A0")],
        &[],
    );
}

#[test]
fn a_chain_of_three_holds_in_its_mirror_image_under_inverses() {
    // r ∘ s ∘ t ⊑ u gives t⁻ ∘ s⁻ ∘ r⁻ ⊑ u⁻, with p, q and v the inverses of t, s and r
    check(
        &format!(
            "SubObjectPropertyOf(ObjectPropertyChain(:r :s :t) :u)
             InverseObjectProperties(:p :t) InverseObjectProperties(:q :s)
             InverseObjectProperties(:v :r)
             SubClassOf(:A ObjectSomeValuesFrom(:p ObjectSomeValuesFrom(:q ObjectSomeValuesFrom(:v :B))))
             EquivalentClasses(:D ObjectSomeValuesFrom(ObjectInverseOf(:u) :B))
             {UNION}"
        ),
        &[("A", "D")],
        &[("B", "D")],
    );
}

/// A small random Horn ontology over classes `A0`…`A5` and roles `r0`…`r2`, with
/// inverses, inclusions, transitivity and chains.
fn random_horn(seed: u64) -> String {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    let role = |k: u64, inv: bool| {
        if inv {
            format!("ObjectInverseOf(:r{k})")
        } else {
            format!(":r{k}")
        }
    };
    let mut s = String::new();
    for _ in 0..8 + next(6) {
        // three distinct classes, so that no intersection or disjointness collapses
        let a = next(6);
        let b = (a + 1 + next(5)) % 6;
        let c = (0..6).map(|k| (b + 1 + k) % 6).find(|&k| k != a).unwrap();
        let r = role(next(3), next(4) == 0);
        s.push_str(&match next(7) {
            0 => format!("SubClassOf(:A{a} :A{b})\n"),
            1 => format!("SubClassOf(:A{a} ObjectSomeValuesFrom({r} :A{b}))\n"),
            2 => format!("SubClassOf(ObjectSomeValuesFrom({r} :A{a}) :A{b})\n"),
            3 => format!("SubClassOf(:A{a} ObjectAllValuesFrom({r} :A{b}))\n"),
            4 => format!("SubClassOf(ObjectIntersectionOf(:A{a} :A{b}) :A{c})\n"),
            5 => format!(
                "EquivalentClasses(:A{a} ObjectSomeValuesFrom({r} ObjectIntersectionOf(:A{b} :A{c})))\n"
            ),
            _ => format!("DisjointClasses(:A{a} :A{b})\n"),
        });
    }
    for _ in 0..next(3) {
        let (p, q) = (next(3), next(3));
        let inv = next(2) == 0;
        match next(4) {
            0 => s.push_str(&format!("SubObjectPropertyOf(:r{p} {})\n", role(q, inv))),
            1 => s.push_str(&format!("TransitiveObjectProperty(:r{p})\n")),
            2 if p != q => s.push_str(&format!("InverseObjectProperties(:r{p} :r{q})\n")),
            _ => s.push_str(&format!(
                "SubObjectPropertyOf(ObjectPropertyChain(:r{p} {}) :r{p})\n",
                role(q, inv)
            )),
        }
    }
    for a in 0..6 {
        s.push_str(&format!("Declaration(Class(:A{a}))\n"));
    }
    s
}

#[test]
fn tier_d_answers_horn_ontologies_as_the_horn_rules_do() {
    // a union that meets nothing brings Tier D in, and a data restriction that meets
    // nothing keeps it out, so that the Horn rules classify the same Horn axioms
    let mut compared = 0;
    let seeds: u64 = std::env::var("DL_DIFF_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 200 } else { 1000 });
    for seed in 0..seeds {
        let body = random_horn(seed);
        let d = axioms(&format!("{body}{UNION}"));
        let h = axioms(&format!(
            "{body}SubClassOf(:Y DataSomeValuesFrom(:dp owl:real))"
        ));
        let cs: Vec<Class> = (0..6)
            .map(|a| c(&format!("A{a}")))
            .chain([Class::thing(), Class::nothing()])
            .collect();
        let mut kd = Classifier::of(&d);
        let od = kd.classify(&Limits::default()).unwrap();
        let mut kh = Classifier::of(&h);
        let oh = kh.classify(&Limits::default()).unwrap();
        if !kd.excluded().is_empty() || !kh.excluded().is_empty() {
            continue;
        }
        assert_eq!(od.stats.tier, Tier::Disjunctive, "{body}");
        assert!(od.stats.clauses > 0, "{body}");
        compared += 1;
        let (pd, ph) = (pairs(&od.taxonomy, &cs), pairs(&oh.taxonomy, &cs));
        // both are sound and complete on Horn axioms, so their answers agree
        let missing: Vec<_> = ph.difference(&pd).collect();
        let extra: Vec<_> = pd.difference(&ph).collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "seed {seed}: Tier D misses {missing:?} and adds {extra:?}\n{body}"
        );
        // and Tier D is the same on every thread count and strategy
        for (threads, expansion) in [(8, Expansion::Cautious), (4, Expansion::Eager)] {
            let o = Classifier::of(&d)
                .classify(&Limits {
                    threads,
                    expansion,
                    ..Limits::default()
                })
                .unwrap();
            assert_eq!(pairs(&o.taxonomy, &cs), pd, "seed {seed}\n{body}");
        }
    }
    assert!(
        compared > seeds * 3 / 4,
        "only {compared} cases were taken whole"
    );
}

/// A random ALCHI ontology: a random Horn one with a few unions in the heads.
fn random_dl(seed: u64) -> String {
    let mut s = random_horn(seed);
    let mut x = seed.wrapping_mul(0xD1B5_4A32_D192_ED03) | 1;
    let mut next = |m: u64| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x % m
    };
    for _ in 0..1 + next(3) {
        let a = next(6);
        let b = (a + 1 + next(5)) % 6;
        let c = (0..6).map(|k| (b + 1 + k) % 6).find(|&k| k != a).unwrap();
        s.push_str(&match next(3) {
            0 => format!("SubClassOf(:A{a} ObjectUnionOf(:A{b} :A{c}))\n"),
            1 => format!(
                "SubClassOf(:A{a} ObjectSomeValuesFrom(:r{} ObjectUnionOf(:A{b} :A{c})))\n",
                next(3)
            ),
            _ => format!("EquivalentClasses(:A{a} ObjectUnionOf(:A{b} :A{c}))\n"),
        });
    }
    s
}

/// The polls of the cancellation in a whole run.
fn polls_of(ax: &[Axiom], threads: usize) -> usize {
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    Classifier::of(ax)
        .classify(&Limits {
            threads,
            cancelled: Some(Arc::new(move || {
                p.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        })
        .unwrap();
    polls.load(Ordering::Relaxed)
}

#[test]
fn partial_answers_of_random_ontologies_are_sound_and_complete_where_they_say() {
    let seeds = if cfg!(debug_assertions) { 60 } else { 300 };
    let mut partial = 0;
    for seed in 0..seeds {
        let body = random_dl(seed);
        let ax = axioms(&body);
        let cs = classes(&ax);
        let mut k = Classifier::of(&ax);
        let full = k.classify(&Limits::default()).unwrap();
        if !k.excluded().is_empty() || full.stats.clauses == 0 {
            continue;
        }
        for threads in [1, 8] {
            let total = polls_of(&ax, threads);
            for stop in 0..total {
                let Ok(out) =
                    Classifier::of(&ax).classify(&cancelled_at(stop, threads, Expansion::Cautious))
                else {
                    continue;
                };
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    check_partial(&out, &full, &cs)
                }));
                assert!(
                    r.is_ok(),
                    "seed {seed}, stop {stop}, {threads} threads\n{body}"
                );
                partial += !out.complete as usize;
            }
        }
    }
    assert!(partial > 0);
}

#[test]
fn a_memory_stop_answers_in_part() {
    let ax = axioms(&many(if cfg!(debug_assertions) { 40 } else { 120 }, false));
    let cs = classes(&ax);
    let full = Classifier::of(&ax).classify(&Limits::default()).unwrap();
    let peak = full.stats.peak_memory;
    let mut answered = 0;
    for f in 1..20 {
        let cap = peak * f / 20;
        let out = Classifier::of(&ax).classify(&Limits {
            partial: true,
            max_memory: Some(cap),
            ..Limits::default()
        });
        match out {
            Err(sparkles_owl::classify::Error::Memory { .. }) => {}
            Err(e) => panic!("cap {cap}: {e:?}"),
            Ok(out) => {
                assert!(
                    !out.stats.dl_over_budget,
                    "cap {cap} fell back to the Horn tier"
                );
                assert_eq!(out.stats.tier, Tier::Disjunctive);
                check_partial(&out, &full, &cs);
                if !out.complete {
                    answered += 1;
                    assert!(
                        out.stats.conclusions > 0,
                        "cap {cap}: no conclusions counted"
                    );
                    // a memory stop is the only one in this run, and its peak passed the cap
                    assert!(
                        out.stats.peak_memory > cap,
                        "cap {cap}: a peak of {}",
                        out.stats.peak_memory
                    );
                }
            }
        }
    }
    assert!(answered > 0, "no cap gave a partial answer");
}

/// An intersection of `n` classes Z below a union, each of which holds of A in two cases,
/// so that Hyper has 2^n choices of premises. The other cases' classes come first, so that
/// they rank below the Z classes.
fn wide(n: usize) -> Vec<Axiom> {
    let mut s = String::new();
    for p in ["P", "Q", "Z"] {
        for i in 0..n {
            s.push_str(&format!("Declaration(Class(:{p}{i:02}))\n"));
        }
    }
    for p in ["P", "Q"] {
        for i in 0..n {
            s.push_str(&format!(
                "SubClassOf(:A ObjectUnionOf(:{p}{i:02} :Z{i:02}))\n"
            ));
        }
    }
    s.push_str("SubClassOf(ObjectIntersectionOf(");
    for i in 0..n {
        s.push_str(&format!(":Z{i:02} "));
    }
    s.push_str(") ObjectUnionOf(:D :E))\n");
    axioms(&s)
}

/// Classifies in the order of the axioms, which fixes the order of their classes.
fn in_order(
    ax: &[Axiom],
    limits: &Limits,
) -> Result<Classification, sparkles_owl::classify::Error> {
    let refs: Vec<&AxiomKind> = ax.iter().map(|a| &a.kind).collect();
    sparkles_owl::classify::Saturation::new().classify(&refs, None, limits)
}

#[test]
fn a_wide_case_analysis_stays_within_the_memory_budget() {
    let ax = wide(14);
    let cap = 500_000;
    let out = in_order(
        &ax,
        &Limits {
            max_memory: Some(cap),
            ..Limits::default()
        },
    )
    .unwrap();
    assert!(out.stats.dl_over_budget);
    assert!(
        out.stats.peak_memory <= 2 * cap,
        "a peak of {} under a cap of {cap}",
        out.stats.peak_memory
    );
    let out = in_order(
        &ax,
        &Limits {
            partial: true,
            max_memory: Some(cap),
            ..Limits::default()
        },
    );
    // with the flag, the run answers by Tier D, in part or whole
    let full = in_order(&ax, &Limits::default()).unwrap();
    let cs = classes(&ax);
    if let Ok(out) = out {
        assert!(!out.stats.dl_over_budget);
        assert!(out.stats.peak_memory <= 2 * cap);
        check_partial(&out, &full, &cs);
    }
}

/// A ⊑ B, B ⊑ A ⊔ Cᵢ for 260 classes, past one block of the bounds checks, and B ⊑ D ⊔ E with D and E below F: A's context
/// goes through the choices that are redundant, since A holds, before the one that gives F.
fn redundant_choices() -> Vec<Axiom> {
    let mut s = String::from("SubClassOf(:A :B)\n");
    for i in 0..260 {
        s.push_str(&format!("SubClassOf(:B ObjectUnionOf(:A :C{i:03}))\n"));
    }
    s.push_str("SubClassOf(:B ObjectUnionOf(:D :E))\nSubClassOf(:D :F)\nSubClassOf(:E :F)\n");
    s.push_str("Declaration(Class(:A))\nDeclaration(Class(:F))\n");
    axioms(&s)
}

#[test]
fn a_stop_among_redundant_choices_leaves_the_context_open() {
    let ax = redundant_choices();
    let cs = classes(&ax);
    let full = in_order(&ax, &Limits::default()).unwrap();
    assert!(full.taxonomy.is_subclass(&c("A"), &c("F")));
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    in_order(
        &ax,
        &Limits {
            cancelled: Some(Arc::new(move || {
                p.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        },
    )
    .unwrap();
    // every stop in a release build, and in a debug one, which runs the rules some ten
    // times slower, the first 64, where A's context goes through its choices, and every
    // third after them
    let step = if cfg!(debug_assertions) { 3 } else { 1 };
    for stop in (0..polls.load(Ordering::Relaxed)).filter(|&k| k < 64 || k % step == 0) {
        if let Ok(out) = in_order(&ax, &cancelled_at(stop, 1, Expansion::Cautious)) {
            check_partial(&out, &full, &cs);
        }
    }
}

/// `m` intersections of `n` classes below unions, each class holding of A in two cases.
fn wide_many(m: usize, n: usize) -> Vec<Axiom> {
    let mut s = String::new();
    for p in ["P", "Q", "Z"] {
        for k in 0..m {
            for i in 0..n {
                s.push_str(&format!("Declaration(Class(:{p}{k}_{i:02}))\n"));
            }
        }
    }
    for p in ["P", "Q"] {
        for k in 0..m {
            for i in 0..n {
                s.push_str(&format!(
                    "SubClassOf(:A ObjectUnionOf(:{p}{k}_{i:02} :Z{k}_{i:02}))\n"
                ));
            }
        }
    }
    for k in 0..m {
        s.push_str("SubClassOf(ObjectIntersectionOf(");
        for i in 0..n {
            s.push_str(&format!(":Z{k}_{i:02} "));
        }
        s.push_str(&format!(") ObjectUnionOf(:D{k} :E{k}))\n"));
    }
    axioms(&s)
}

#[test]
fn a_stop_ends_every_waiting_case_analysis() {
    // products of 2^11 choices, and of 2^8, exactly one block of the bounds' checks
    for (m, n) in [(6, 12), (12, 9)] {
        waiting_case_analyses(&wide_many(m, n));
    }
}

fn waiting_case_analyses(ax: &[Axiom]) {
    let cap = 400_000;
    for partial in [false, true] {
        let out = in_order(
            ax,
            &Limits {
                partial,
                max_memory: Some(cap),
                ..Limits::default()
            },
        );
        if let Ok(out) = out {
            assert!(
                out.stats.peak_memory <= 2 * cap,
                "a peak of {} under a cap of {cap}",
                out.stats.peak_memory
            );
            assert_eq!(out.stats.dl_over_budget, !partial);
        }
    }
}

#[test]
fn a_minimum_above_a_maximum_is_unsatisfiable() {
    check(
        &format!(
            "SubClassOf(:A ObjectMinCardinality(2 :r :B)) SubClassOf(:A ObjectMaxCardinality(1 :r))
             SubClassOf(:E ObjectMinCardinality(2 :r :B)) SubClassOf(:E ObjectMaxCardinality(2 :r))
             SubClassOf(:D :Z) {UNION}"
        ),
        &[("A", "D"), ("A", "E")],
        &[("E", "D"), ("E", "A")],
    );
}

#[test]
fn a_functional_role_merges_its_successors() {
    check(
        &format!(
            "SubClassOf(:A ObjectSomeValuesFrom(:r :B)) SubClassOf(:A ObjectSomeValuesFrom(:r :C))
             FunctionalObjectProperty(:r)
             EquivalentClasses(:D ObjectSomeValuesFrom(:r ObjectIntersectionOf(:B :C))) {UNION}"
        ),
        &[("A", "D")],
        &[("D", "A")],
    );
}

#[test]
fn an_inverse_functional_role_merges_a_successor_with_the_predecessor() {
    // the r-successor of A has at most one r-predecessor, A itself, which is then an E
    check(
        &format!(
            "SubClassOf(:A ObjectSomeValuesFrom(:r :C))
             SubClassOf(:C ObjectSomeValuesFrom(ObjectInverseOf(:r) :E))
             InverseFunctionalObjectProperty(:r) {UNION}"
        ),
        &[("A", "E")],
        &[("C", "E"), ("E", "A")],
    );
}

#[test]
fn a_maximum_on_the_left_is_met_through_a_functional_role() {
    check(
        &format!(
            "SubClassOf(:A ObjectSomeValuesFrom(:r :B)) FunctionalObjectProperty(:r)
             SubClassOf(ObjectMaxCardinality(1 :r :B) :D) {UNION}"
        ),
        &[("A", "D")],
        &[("D", "A")],
    );
}

#[test]
fn a_qualified_maximum_counts_only_its_filler() {
    // two r-successors that are B and C: at most one B leaves both, at most one C too
    check(
        &format!(
            "SubClassOf(:A ObjectMinCardinality(2 :r :B)) SubClassOf(:A ObjectMaxCardinality(1 :r :C))
             DisjointClasses(:B :C)
             SubClassOf(:F ObjectMinCardinality(2 :r :B)) SubClassOf(:F ObjectMaxCardinality(1 :r :G))
             SubClassOf(:B :G) SubClassOf(:D :Z) {UNION}"
        ),
        &[("F", "D")],
        &[("A", "D")],
    );
}

#[test]
fn disjoint_roles_with_a_common_edge_are_unsatisfiable() {
    check(
        &format!(
            "SubClassOf(:A ObjectSomeValuesFrom(:r :B)) SubObjectPropertyOf(:r :s)
             DisjointObjectProperties(:r :s)
             SubClassOf(:E ObjectSomeValuesFrom(:t :B)) DisjointObjectProperties(:r :t)
             SubClassOf(:D :Z) {UNION}"
        ),
        &[("A", "D")],
        &[("E", "D")],
    );
}

/// `body` with every existential restriction on the right of a subclass axiom read as a
/// minimum of two. Without a maximum or a functional role over its role, a minimum of two
/// has a model wherever the existential restriction has one, so the subsumptions agree.
fn with_minimums(body: &str) -> String {
    body.lines()
        .map(|l| match l.split_once(' ') {
            Some((head, rest))
                if head.starts_with("SubClassOf(:A")
                    && rest.starts_with("ObjectSomeValuesFrom(") =>
            {
                format!(
                    "{head} ObjectMinCardinality(2 {}\n",
                    &rest["ObjectSomeValuesFrom(".len()..]
                )
            }
            _ => format!("{l}\n"),
        })
        .collect()
}

#[test]
fn random_ontologies_answer_the_same_with_counting() {
    // the counting run reads each minimum of two by two successors with disjoint fillers,
    // and a functional role that nothing else names makes the run count
    let seeds: u64 = std::env::var("DL_DIFF_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 60 } else { 300 });
    let mut compared = 0;
    for seed in 0..seeds {
        let body = random_dl(seed);
        let counted = with_minimums(&body);
        if counted == body {
            continue;
        }
        let d = axioms(&format!("{body}{UNION}"));
        let n = axioms(&format!("{counted}{UNION}FunctionalObjectProperty(:q)\n"));
        let cs: Vec<Class> = (0..6)
            .map(|a| c(&format!("A{a}")))
            .chain([Class::thing(), Class::nothing()])
            .collect();
        let mut kd = Classifier::of(&d);
        let od = kd.classify(&Limits::default()).unwrap();
        if !kd.excluded().is_empty() || od.stats.dl_over_budget {
            continue;
        }
        let want = pairs(&od.taxonomy, &cs);
        for (threads, expansion) in [
            (1, Expansion::Cautious),
            (4, Expansion::Cautious),
            (4, Expansion::Eager),
        ] {
            let mut kn = Classifier::of(&n);
            let on = kn
                .classify(&Limits {
                    threads,
                    expansion,
                    ..Limits::default()
                })
                .unwrap();
            let got = pairs(&on.taxonomy, &cs);
            let extra: Vec<_> = got.difference(&want).collect();
            assert!(
                extra.is_empty(),
                "seed {seed}: counting adds {extra:?}\n{counted}"
            );
            if kn.excluded().is_empty() && !on.stats.dl_over_budget {
                let missing: Vec<_> = want.difference(&got).collect();
                assert!(
                    missing.is_empty(),
                    "seed {seed}: counting misses {missing:?}\n{counted}"
                );
                compared += 1;
            }
        }
    }
    assert!(compared > 0, "no case was compared");
    eprintln!("compared {compared} runs");
}

#[test]
fn a_functional_role_answers_as_a_maximum_of_one_on_every_class() {
    // the role axiom and the class axiom state the same, and reach the at-most rule by
    // different normalizations; both merge the successors of every random ontology's r0
    let seeds: u64 = std::env::var("DL_DIFF_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 60 } else { 300 });
    let mut compared = 0;
    for seed in 0..seeds {
        let body = with_minimums(&random_dl(seed));
        let f = axioms(&format!("{body}{UNION}FunctionalObjectProperty(:r0)\n"));
        let m = axioms(&format!(
            "{body}{UNION}SubClassOf(owl:Thing ObjectMaxCardinality(1 :r0))\n"
        ));
        let cs: Vec<Class> = (0..6)
            .map(|a| c(&format!("A{a}")))
            .chain([Class::thing(), Class::nothing()])
            .collect();
        let mut kf = Classifier::of(&f);
        let of = kf.classify(&Limits::default()).unwrap();
        let mut km = Classifier::of(&m);
        let om = km
            .classify(&Limits {
                threads: 4,
                ..Limits::default()
            })
            .unwrap();
        let whole = |k: &Classifier, o: &sparkles_owl::classify::Classification| {
            k.excluded().is_empty() && !o.stats.dl_over_budget
        };
        if whole(&kf, &of) && whole(&km, &om) {
            assert_eq!(
                pairs(&of.taxonomy, &cs),
                pairs(&om.taxonomy, &cs),
                "seed {seed}\n{body}"
            );
            compared += 1;
        }
    }
    assert!(compared > seeds / 2, "only {compared} cases were compared");
}

/// A run that either answers `a ⊑ b` or leaves an axiom out and says so.
fn entailed_or_left_out(body: &str, a: &str, b: &str) {
    let ax = axioms(body);
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    assert!(
        out.taxonomy.is_subclass(&c(a), &c(b)) || !k.excluded().is_empty(),
        "{a} ⊑ {b} is missing and no axiom is left out"
    );
}

#[test]
fn a_maximum_whose_dual_overflows_is_left_out() {
    entailed_or_left_out(
        &format!(
            "SubClassOf(ObjectMaxCardinality(4294967295 :r) :D)
             SubClassOf(:A ObjectMaxCardinality(0 :r))
             SubClassOf(ObjectExactCardinality(4294967295 :s) :E) {UNION}"
        ),
        "A",
        "D",
    );
}

#[test]
fn an_irreflexive_transitive_symmetric_role_has_no_edge() {
    entailed_or_left_out(
        &format!(
            "TransitiveObjectProperty(:r) SymmetricObjectProperty(:r)
             IrreflexiveObjectProperty(:r) SubClassOf(:A ObjectSomeValuesFrom(:r :B))
             SubClassOf(:D :Z) {UNION}"
        ),
        "A",
        "D",
    );
}

#[test]
fn a_wide_maximum_passes_the_bound_instead_of_enumerating_its_subsets() {
    let mut body = String::from("SubClassOf(:A :M) SubClassOf(:M ObjectMaxCardinality(20 :r))\n");
    for i in 0..40 {
        body.push_str(&format!("SubClassOf(:A ObjectSomeValuesFrom(:r :B{i}))\n"));
    }
    body.push_str(UNION);
    let ax = axioms(&body);
    let mut k = Classifier::of(&ax);
    let out = k.classify(&Limits::default()).unwrap();
    // the answer is sound: A is satisfiable
    assert!(!out.taxonomy.is_subclass(&c("A"), &Class::nothing()));
    assert!(out.stats.dl_over_budget || out.complete);
}
