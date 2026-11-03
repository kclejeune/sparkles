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

/// Every subsumption between the taxonomy's classes, owl:Thing and owl:Nothing included.
fn pairs(t: &sparkles_owl::classify::Taxonomy, cs: &[Class]) -> BTreeSet<(String, String)> {
    let mut v = BTreeSet::new();
    for x in cs {
        for y in cs {
            if t.is_subclass(x, y) {
                v.insert((x.as_str().to_string(), y.as_str().to_string()));
            }
        }
    }
    v
}

fn classes(ax: &[Axiom]) -> Vec<Class> {
    let mut cs: BTreeSet<Class> = ax
        .iter()
        .filter_map(|a| match &a.kind {
            AxiomKind::Declaration(Entity::Class(c)) => Some(c.clone()),
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
    let extra: Vec<_> = got.difference(&want).collect();
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
fn random_horn(seed: u64) -> (String, String) {
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
    // the mirror image of each chain, sn⁻ ∘ … ∘ s1⁻ ⊑ s⁻, which the Horn rules of
    // this phase take only when it is told
    let mut mirrors = String::new();
    for _ in 0..next(3) {
        let (p, q) = (next(3), next(3));
        let (inv, ip) = (next(2) == 0, format!("ObjectInverseOf(:r{p})"));
        match next(4) {
            0 => s.push_str(&format!("SubObjectPropertyOf(:r{p} {})\n", role(q, inv))),
            1 => {
                s.push_str(&format!("TransitiveObjectProperty(:r{p})\n"));
                mirrors.push_str(&format!("TransitiveObjectProperty({ip})\n"));
            }
            2 if p != q => s.push_str(&format!("InverseObjectProperties(:r{p} :r{q})\n")),
            _ => {
                s.push_str(&format!(
                    "SubObjectPropertyOf(ObjectPropertyChain(:r{p} {}) :r{p})\n",
                    role(q, inv)
                ));
                mirrors.push_str(&format!(
                    "SubObjectPropertyOf(ObjectPropertyChain({} {ip}) {ip})\n",
                    role(q, !inv)
                ));
            }
        }
    }
    for a in 0..6 {
        s.push_str(&format!("Declaration(Class(:A{a}))\n"));
    }
    (s, mirrors)
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
        let (body, mirrors) = random_horn(seed);
        let d = axioms(&format!("{body}{UNION}"));
        let h = axioms(&format!(
            "{body}{mirrors}SubClassOf(:Y DataSomeValuesFrom(:dp owl:real))"
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
