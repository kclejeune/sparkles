//! Partial classification answers for EL (`Plain`, `Full`) and Horn (`Horn`, `HornFull`)
//! tiers (roadmap item 714).

use sparkles_owl::classify::{
    Classification, Classifier, Consistency, Error, Limits, Tier, Unmapped,
};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const EX: &str = "http://ex.org/";

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\n\
         Prefix(owl:=<http://www.w3.org/2002/07/owl#>)\n\
         Prefix(xsd:=<http://www.w3.org/2001/XMLSchema#>)\n\
         Ontology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

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

/// Checks soundness, consistency, and exact per-class completeness of a partial answer
/// against the full classification of the same ontology.
fn check_partial(part: &Classification, full: &Classification, cs: &[Class]) {
    let got = pairs(&part.taxonomy, cs);
    let want = pairs(&full.taxonomy, cs);
    if part.complete_for_taken {
        assert_eq!(part.consistent, full.consistent);
        assert_eq!(part.consistency, full.consistency);
        if full.complete {
            assert!(part.incomplete.is_empty(), "{:?}", part.incomplete);
        }
        assert_eq!(got, want);
        return;
    }
    assert!(!part.complete);
    let open: BTreeSet<&Class> = part.incomplete.iter().collect();
    // Soundness: every emitted subsumption and unsatisfiability must hold in full.
    if full.consistent {
        for p in &got {
            if p.1 == "⊥" {
                assert!(
                    want.contains(p),
                    "partial answer marks satisfiable class unsatisfiable: {p:?}"
                );
            } else {
                let x = Class::new(p.0.clone());
                if !full.taxonomy.is_unsatisfiable(&x) {
                    assert!(
                        want.contains(p),
                        "partial answer adds unentailed subsumption: {p:?}"
                    );
                }
            }
        }
    }
    // Consistency must never contradict the full run.
    match (full.consistent, part.consistency) {
        (true, Consistency::Inconsistent) => {
            panic!("partial answer claims inconsistent for consistent ontology")
        }
        (false, Consistency::Consistent) => {
            panic!("partial answer claims consistent for inconsistent ontology")
        }
        (_, Consistency::Inconsistent) => {
            assert!(!part.consistent);
            assert!(
                part.incomplete.is_empty(),
                "inconsistent partial answer left incomplete classes: {:?}",
                part.incomplete
            );
            for x in cs {
                assert!(part.taxonomy.is_unsatisfiable(x));
            }
            return;
        }
        (_, Consistency::Unknown) => {
            assert!(part.consistent);
            for x in cs {
                if *x != Class::thing() && *x != Class::nothing() {
                    assert!(
                        open.contains(x),
                        "{x:?} reported complete while consistency is Unknown"
                    );
                }
            }
            return;
        }
        (true, Consistency::Consistent) => {
            assert!(part.consistent);
        }
    }
    // Completeness: every class outside `incomplete` must have its exact superclass set.
    for x in cs {
        if *x == Class::thing() || *x == Class::nothing() || open.contains(x) {
            continue;
        }
        let above = |set: &BTreeSet<(String, String)>| -> Vec<String> {
            set.iter()
                .filter(|(a, _)| a == x.as_str())
                .map(|(_, b)| b.clone())
                .collect()
        };
        assert_eq!(
            above(&got),
            above(&want),
            "{x:?} reported complete but subsumers differ (got {:?} vs want {:?})",
            above(&got),
            above(&want)
        );
    }
}

fn polls_of(ax: &[Axiom], threads: usize, el_only: bool) -> usize {
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    Classifier::of(ax)
        .classify(&Limits {
            threads,
            el_only,
            cancelled: Some(Arc::new(move || {
                p.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        })
        .unwrap();
    polls.load(Ordering::Relaxed)
}

fn cancelled_at(k: usize, threads: usize, el_only: bool) -> Limits {
    let polls = Arc::new(AtomicUsize::new(0));
    Limits {
        threads,
        el_only,
        partial: true,
        cancelled: Some(Arc::new(move || polls.fetch_add(1, Ordering::Relaxed) >= k)),
        ..Limits::default()
    }
}

/// Pure EL (`Plain`) ontology with deep chains, existential trees, property chains,
/// transitivity, self loops, and disjointness.
fn el_plain_ontology(n: usize, inconsistent: bool) -> String {
    let mut s = String::from(
        "TransitiveObjectProperty(:part)\n\
         SubObjectPropertyOf(ObjectPropertyChain(:r1 :r2) :s)\n\
         SubClassOf(:SelfA ObjectHasSelf(:loop))\n\
         SubClassOf(ObjectHasSelf(:loop) :SelfB)\n",
    );
    for i in 0..n {
        let next = (i + 1) % n;
        let skip = (7 * i + 3) % n;
        s.push_str(&format!(
            "SubClassOf(:A{i} :A{next})\n\
             SubClassOf(:A{i} ObjectSomeValuesFrom(:r1 :B{i}))\n\
             SubClassOf(:B{i} ObjectSomeValuesFrom(:r2 :C{next}))\n\
             SubClassOf(ObjectSomeValuesFrom(:s :C{i}) :D{i})\n\
             SubClassOf(:C{i} ObjectSomeValuesFrom(:part :P{skip}))\n\
             SubClassOf(:P{i} ObjectSomeValuesFrom(:part :P{next}))\n\
             SubClassOf(ObjectSomeValuesFrom(:part :P{i}) :Q{i})\n\
             SubClassOf(:D{i} :E{i})\n",
        ));
    }
    // Independent component that can finish even when other components are stopped.
    s.push_str(
        "SubClassOf(:Iso0 :Iso1)\n\
         SubClassOf(:Iso1 :Iso2)\n\
         DisjointClasses(:DisjA :DisjB)\n\
         SubClassOf(:UnsatX ObjectIntersectionOf(:DisjA :DisjB))\n",
    );
    if inconsistent {
        s.push_str(
            "SubClassOf(owl:Thing :Inc0)\n\
             SubClassOf(:Inc0 ObjectSomeValuesFrom(:r1 :Inc1))\n\
             SubClassOf(:Inc1 ObjectSomeValuesFrom(:r2 :Inc2))\n\
             SubClassOf(ObjectSomeValuesFrom(:s :Inc2) :DisjA)\n\
             SubClassOf(ObjectSomeValuesFrom(:s :Inc2) :DisjB)\n",
        );
    }
    s
}

/// EL `Full` ontology with individuals, functional data properties, and Phase 3 witnesses.
fn el_full_ontology(n: usize, inconsistent_via_individual: bool) -> String {
    let mut s = String::from(
        "FunctionalDataProperty(:code)\n\
         TransitiveObjectProperty(:chain)\n\
         SubClassOf(:BadData DataHasValue(:code \"1\"^^xsd:integer))\n\
         SubClassOf(:BadData DataHasValue(:code \"2\"^^xsd:integer))\n\
         ClassAssertion(:IndBase :a0)\n\
         SubClassOf(:IndBase :IndSuper)\n",
    );
    for i in 0..n {
        let next = (i + 1) % n;
        // `:WorldDerived{i}` cannot be derived globally: `:Wit{i}` is non-real during
        // global saturation, so it never pushes `:Tag{i}` to `{:a0}` until `:W{i}`'s
        // Phase 3 world marks `:Wit{i}` real. Only then does `{:a0}` gain `:Tag{i}` and
        // propagate `∃:probe.:Tag{i}` back to `:W{i}` via `:W{i} ⊑ ∃:probe.{:a0}`.
        s.push_str(&format!(
            "SubClassOf(:W{i} ObjectSomeValuesFrom(:link :Mid{i}))\n\
             SubClassOf(:Mid{i} ObjectSomeValuesFrom(:link :Wit{i}))\n\
             SubClassOf(:Wit{i} ObjectOneOf(:a0))\n\
             SubClassOf(:Wit{i} :Tag{i})\n\
             SubClassOf(:W{i} ObjectSomeValuesFrom(:probe ObjectOneOf(:a0)))\n\
             SubClassOf(ObjectSomeValuesFrom(:probe :Tag{i}) :WorldDerived{i})\n\
             SubClassOf(:Chain{i} :Chain{next})\n\
             SubClassOf(:Chain{i} ObjectSomeValuesFrom(:chain :Chain{next}))\n",
        ));
    }
    // Isolated component with no path to any witness.
    s.push_str(
        "SubClassOf(:Clean0 :Clean1)\n\
         SubClassOf(:Clean1 :Clean2)\n",
    );
    if inconsistent_via_individual {
        s.push_str(
            "SubClassOf(:IndBase ObjectSomeValuesFrom(:p :BadInd))\n\
             SubClassOf(:BadInd :BadData)\n",
        );
    }
    s
}

/// Horn-SHIQ (`Horn`, no individuals) ontology with inverses, universals, `≤1`, `≥2`,
/// transitive chains under universals, and disjoint/asymmetric roles.
fn horn_plain_ontology(n: usize, inconsistent: bool) -> String {
    let mut s = String::from(
        "InverseObjectProperties(:r :rinv)\n\
         TransitiveObjectProperty(:t)\n\
         DisjointObjectProperties(:d1 :d2)\n\
         AsymmetricObjectProperty(:asym)\n",
    );
    for i in 0..n {
        let b = (7 * i + 3) % n;
        let d = (11 * i + 5) % n;
        s.push_str(&format!(
            "SubClassOf(:H{i} ObjectSomeValuesFrom(:r :M{i}))\n\
             SubClassOf(:H{i} ObjectAllValuesFrom(:r :F{b}))\n\
             SubClassOf(:H{i} ObjectMaxCardinality(1 :r owl:Thing))\n\
             SubClassOf(:H{i} ObjectSomeValuesFrom(:r :G{d}))\n\
             SubClassOf(:M{i} ObjectAllValuesFrom(:rinv :Back{i}))\n\
             SubClassOf(ObjectIntersectionOf(:F{i} :G{i}) :Join{i})\n\
             SubClassOf(:H{i} ObjectSomeValuesFrom(:t :T{b}))\n\
             SubClassOf(:H{i} ObjectAllValuesFrom(:t :TU{d}))\n\
             SubClassOf(:H{i} ObjectMinCardinality(2 :m :Min{i}))\n",
        ));
    }
    // Isolated component.
    s.push_str(
        "SubClassOf(:Solo0 :Solo1)\n\
         SubClassOf(:Solo1 :Solo2)\n",
    );
    if inconsistent {
        s.push_str(
            "SubClassOf(owl:Thing :Clash0)\n\
             SubClassOf(:Clash0 ObjectSomeValuesFrom(:d1 :Target))\n\
             SubClassOf(:Clash0 ObjectSomeValuesFrom(:d2 :Target))\n\
             SubClassOf(:Clash0 ObjectMaxCardinality(1 :s owl:Thing))\n\
             SubObjectPropertyOf(:d1 :s)\n\
             SubObjectPropertyOf(:d2 :s)\n",
        );
    }
    s
}

/// Horn with individuals (`HornFull`) triggering R7 (`Same`), R8 clashes, real-context
/// `send_named`, and Phase 3 conditional worlds over conjunction contexts.
fn horn_full_ontology(n: usize, inconsistent: bool) -> String {
    let mut s = String::from(
        "InverseObjectProperties(:r :rinv)\n\
         InverseFunctionalObjectProperty(:r)\n\
         DisjointObjectProperties(:d1 :d2)\n\
         ClassAssertion(:Seed :i0)\n\
         ClassAssertion(:Seed :i1)\n\
         SubClassOf(:Seed ObjectSomeValuesFrom(:r ObjectOneOf(:hub)))\n\
         SubClassOf(:Seed ObjectAllValuesFrom(:r :HubProp))\n\
         SubClassOf(:HubProp :HubSuper)\n",
    );
    for i in 0..n {
        let next = (i + 1) % n;
        // `:HWorldDerived{i}` cannot be derived globally: `:HM{i}` is non-real during
        // global saturation, so it pushes `:Tag{i}` to `{:hub}` only inside `:HF{i}`'s
        // Phase 3 world, which then propagates `∃:probe.:Tag{i}` back to `:HF{i}`.
        s.push_str(&format!(
            "SubClassOf(:HF{i} ObjectSomeValuesFrom(:r :HM{i}))\n\
             SubClassOf(:HF{i} ObjectAllValuesFrom(:r :HA{next}))\n\
             SubClassOf(:HM{i} ObjectOneOf(:hub))\n\
             SubClassOf(:HM{i} :Tag{i})\n\
             SubClassOf(:HM{i} ObjectAllValuesFrom(:rinv :Back{i}))\n\
             SubClassOf(:HF{i} ObjectSomeValuesFrom(:probe ObjectOneOf(:hub)))\n\
             SubClassOf(ObjectSomeValuesFrom(:probe :Tag{i}) :HWorldDerived{i})\n",
        ));
    }
    s.push_str("SubClassOf(:Pure0 :Pure1)\n");
    if inconsistent {
        s.push_str(
            "ObjectPropertyAssertion(:d1 :i0 :hub)\n\
             ObjectPropertyAssertion(:d2 :i0 :hub)\n",
        );
    }
    s
}

fn check_recovery(
    k: &mut Classifier,
    full: &Classification,
    cs: &[Class],
    threads: usize,
    tier: Tier,
) {
    let recovered = k
        .classify(&Limits {
            threads,
            ..Limits::default()
        })
        .unwrap();
    assert!(recovered.complete && recovered.incomplete.is_empty());
    assert_eq!(recovered.consistent, full.consistent);
    assert_eq!(recovered.consistency, full.consistency);
    assert_eq!(pairs(&recovered.taxonomy, cs), pairs(&full.taxonomy, cs));
    assert_eq!(recovered.stats.tier, tier);
    assert!(!recovered.stats.horn_over_budget);
}

fn sweep_cancel(body: &str, tier: Tier, inconsistent: bool, el_only: bool) {
    let ax = axioms(body);
    let cs = classes(&ax);
    let full = Classifier::of(&ax)
        .classify(&Limits {
            el_only,
            ..Limits::default()
        })
        .unwrap();
    assert!(full.complete);
    assert_eq!(full.stats.tier, tier);
    assert_eq!(full.consistent, !inconsistent);

    let mut partials = 0usize;
    let mut saw_unknown = 0usize;
    let mut saw_inconsistent = 0usize;
    let mut saw_some_complete = 0usize;

    for threads in [1, 8] {
        let total = polls_of(&ax, threads, el_only);
        assert!(total > 0, "no cancellation polls observed");
        let step = (total / 40).max(1);
        let first = (0..total).find(|&k| {
            Classifier::of(&ax)
                .classify(&cancelled_at(k, threads, el_only))
                .is_ok()
        });
        let early = first.map_or(0..0, |f| f..(f + 24).min(total));
        let mut points: BTreeSet<usize> = early.collect();
        for k in (0..total).step_by(step) {
            points.insert(k);
        }
        if let Some(last) = total.checked_sub(1) {
            points.insert(last);
        }
        for k in points {
            let mut cl = Classifier::of(&ax);
            let Ok(out) = cl.classify(&cancelled_at(k, threads, el_only)) else {
                continue;
            };
            assert_eq!(out.stats.tier, tier);
            assert!(!out.stats.horn_over_budget);
            check_partial(&out, &full, &cs);
            if !out.complete_for_taken {
                partials += 1;
                if out.consistency == Consistency::Unknown {
                    saw_unknown += 1;
                }
                if out.consistency == Consistency::Inconsistent {
                    saw_inconsistent += 1;
                }
                if out.consistency == Consistency::Consistent && out.incomplete.len() + 2 < cs.len()
                {
                    saw_some_complete += 1;
                }
                // Verify recovery after partial answer on same Classifier instance.
                if !el_only && partials <= 4 {
                    check_recovery(&mut cl, &full, &cs, threads, tier);
                }
            }
        }
    }
    assert!(partials > 0, "tier {tier:?}: no stop gave a partial answer");
    if inconsistent {
        assert!(
            saw_inconsistent > 0,
            "tier {tier:?}: inconsistent partial stops never observed Pass 0 Consistency::Inconsistent"
        );
    } else {
        assert!(
            saw_unknown > 0 || saw_some_complete > 0,
            "tier {tier:?}: consistent partial stops saw neither Unknown nor partial completion"
        );
    }
}

#[test]
fn el_plain_partial_answers_consistent_and_inconsistent() {
    let n = if cfg!(debug_assertions) { 45 } else { 150 };
    sweep_cancel(&el_plain_ontology(n, false), Tier::El, false, false);
    sweep_cancel(&el_plain_ontology(n, true), Tier::El, true, false);
}

#[test]
fn el_full_partial_answers_consistent_and_inconsistent() {
    let n = if cfg!(debug_assertions) { 35 } else { 100 };
    sweep_cancel(&el_full_ontology(n, false), Tier::El, false, false);
    sweep_cancel(&el_full_ontology(n, true), Tier::El, true, false);
}

#[test]
fn horn_plain_partial_answers_consistent_and_inconsistent() {
    let n = if cfg!(debug_assertions) { 45 } else { 120 };
    sweep_cancel(&horn_plain_ontology(n, false), Tier::Horn, false, false);
    sweep_cancel(&horn_plain_ontology(n, true), Tier::Horn, true, false);
}

#[test]
fn horn_full_partial_answers_consistent_and_inconsistent() {
    let n = if cfg!(debug_assertions) { 30 } else { 80 };
    sweep_cancel(&horn_full_ontology(n, false), Tier::Horn, false, false);
    sweep_cancel(&horn_full_ontology(n, true), Tier::Horn, true, false);
}

#[test]
fn el_and_horn_universal_and_keys_partial_stops_are_sound() {
    // Universal role + HasKey in Phase 2: stopping before Phase 2 finishes must return
    // Unknown with all classes incomplete (or Inconsistent if already proved).
    for (body, tier) in [
        (
            "SubClassOf(:HasKeyA ObjectSomeValuesFrom(:k :Val))\n\
             HasKey(:KeyClass (:k) ())\n\
             ClassAssertion(:KeyClass :x1)\n\
             ClassAssertion(:KeyClass :x2)\n\
             ObjectPropertyAssertion(:k :x1 :v)\n\
             ObjectPropertyAssertion(:k :x2 :v)\n\
             SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :U) :AllHit)\n\
             ClassAssertion(:U :x1)\n"
                .to_string()
                + &el_plain_ontology(40, false),
            Tier::El,
        ),
        (
            "HasKey(:KeyClass (:k) ())\n\
             ClassAssertion(:KeyClass :x1)\n\
             ClassAssertion(:KeyClass :x2)\n\
             ObjectPropertyAssertion(:k :x1 :v)\n\
             ObjectPropertyAssertion(:k :x2 :v)\n\
             SubClassOf(ObjectSomeValuesFrom(owl:topObjectProperty :U) :AllHit)\n\
             ClassAssertion(:U :x1)\n"
                .to_string()
                + &horn_plain_ontology(25, false),
            Tier::Horn,
        ),
    ] {
        let ax = axioms(&body);
        let cs = classes(&ax);
        let full = Classifier::of(&ax).classify(&Limits::default()).unwrap();
        assert_eq!(full.stats.tier, tier);
        let total = polls_of(&ax, 1, false);
        let mut saw_partial = 0;
        for k in (0..total).step_by((total / 30).max(1)) {
            if let Ok(out) = Classifier::of(&ax).classify(&cancelled_at(k, 1, false)) {
                check_partial(&out, &full, &cs);
                if !out.complete_for_taken {
                    saw_partial += 1;
                }
            }
        }
        assert!(
            saw_partial > 0,
            "tier {tier:?}: no partial stop on universal/keys"
        );
    }
}

#[test]
fn phase3_worlds_partial_stop_keeps_completed_worlds() {
    // Many classes reaching a non-real witness so Phase 3 runs many worlds and polls
    // cancellation inside Phase 3, preserving completed worlds (with their world-only
    // subsumptions absent from global saturation) and marking only unfinished reaching
    // classes incomplete with Consistency::Consistent.
    let cases = [
        (
            el_full_ontology(40, false),
            40usize,
            "W",
            "WorldDerived",
            Tier::El,
        ),
        (
            horn_full_ontology(25, false),
            25usize,
            "HF",
            "HWorldDerived",
            Tier::Horn,
        ),
    ];
    for (body, count, reach_prefix, derived_prefix, tier) in cases {
        let ax = axioms(&body);
        let cs = classes(&ax);
        let full = Classifier::of(&ax).classify(&Limits::default()).unwrap();
        assert!(full.complete && full.consistent);
        assert_eq!(full.stats.tier, tier);
        // Verify each reaching class's world-only subsumption holds in the full taxonomy.
        for i in 0..count {
            let reach = Class::new(format!("{EX}{reach_prefix}{i}"));
            let derived = Class::new(format!("{EX}{derived_prefix}{i}"));
            assert!(
                full.taxonomy.is_subclass(&reach, &derived),
                "tier {tier:?}: expected world-only subsumption {reach:?} ⊑ {derived:?} in full"
            );
        }
        let total = polls_of(&ax, 1, false);
        let mut saw_phase3_partial = 0;
        // Sweep across the tail of polls where Phase 3 worlds execute one by one.
        let start = total.saturating_sub(total / 2 + 20);
        for k in start..total {
            if let Ok(out) = Classifier::of(&ax).classify(&cancelled_at(k, 1, false)) {
                check_partial(&out, &full, &cs);
                if !out.complete_for_taken && out.consistency == Consistency::Consistent {
                    let open: BTreeSet<&Class> = out.incomplete.iter().collect();
                    let mut completed_world_with_subsumption = false;
                    let mut incomplete_reaching = 0usize;
                    for i in 0..count {
                        let reach = Class::new(format!("{EX}{reach_prefix}{i}"));
                        let derived = Class::new(format!("{EX}{derived_prefix}{i}"));
                        if open.contains(&reach) {
                            incomplete_reaching += 1;
                            // Must NOT be derived globally before this class's world completes.
                            assert!(
                                !out.taxonomy.is_subclass(&reach, &derived),
                                "tier {tier:?} poll {k}: world-only subsumption {reach:?} ⊑ {derived:?} held while {reach:?} was still incomplete"
                            );
                        } else if out.taxonomy.is_subclass(&reach, &derived) {
                            completed_world_with_subsumption = true;
                        }
                    }
                    if completed_world_with_subsumption && incomplete_reaching > 0 {
                        saw_phase3_partial += 1;
                    }
                }
            }
        }
        assert!(
            saw_phase3_partial > 0,
            "tier {tier:?}: expected Phase 3 partial stop with both a completed reaching class (world-only subsumption retained) and an incomplete reaching class (world-only subsumption absent)"
        );
    }
}

#[test]
fn pre_saturation_memory_refusal_preserves_horn_and_tier_d() {
    // 1. Build a Horn ontology whose pre-saturation `horn::normalize` / `Index::build` cost
    //    greatly exceeds EL's full classification cost (using Horn-only `InverseObjectProperties`,
    //    `ObjectAllValuesFrom`, and inverse property chains that EL excludes), so there is a
    //    cap above EL's peak where `horn::normalize` / `Index::build` hits `Halt::Memory`.
    let mut horn_body = String::from("SubClassOf(:Seed0 :Seed1)\n");
    for i in 0..36 {
        let next = (i + 1) % 36;
        let next2 = (i + 2) % 36;
        horn_body.push_str(&format!(
            "InverseObjectProperties(:r{i} :ri{i})\n\
             TransitiveObjectProperty(:r{i})\n\
             SubObjectPropertyOf(ObjectPropertyChain(ObjectInverseOf(:r{i}) :r{next}) :r{next2})\n"
        ));
        for j in 0..8 {
            let d = (i * 5 + j) % 36;
            horn_body.push_str(&format!(
                "SubClassOf(:C{i} ObjectAllValuesFrom(:r{next} :D{d}))\n"
            ));
        }
    }
    let horn_ax = axioms(&horn_body);
    let el_full = Classifier::of(&horn_ax)
        .classify(&Limits {
            threads: 1,
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    let horn_full = Classifier::of(&horn_ax)
        .classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .unwrap();
    assert_eq!(horn_full.stats.tier, Tier::Horn);
    assert!(
        horn_full.stats.peak_memory > el_full.stats.peak_memory,
        "expected Horn peak ({}) > EL peak ({})",
        horn_full.stats.peak_memory,
        el_full.stats.peak_memory
    );

    // Find a cap above EL's full peak where non-partial Horn downgrades to EL
    // (`horn_over_budget == true`), and verify `partial: true` refuses with `Error::Memory`
    // rather than silently downgrading to EL.
    let lo = el_full.stats.peak_memory + 1;
    let hi = horn_full.stats.peak_memory;
    let mut checked_horn_refusal = false;
    for step in 0..=40 {
        let cap = lo + (hi - lo) * step / 40;
        let non_partial = Classifier::of(&horn_ax).classify(&Limits {
            threads: 1,
            partial: false,
            max_memory: Some(cap),
            ..Limits::default()
        });
        if let Ok(downgraded) = non_partial
            && downgraded.stats.horn_over_budget
        {
            let partial_res = Classifier::of(&horn_ax).classify(&Limits {
                threads: 1,
                partial: true,
                max_memory: Some(cap),
                ..Limits::default()
            });
            match partial_res {
                Err(Error::Memory { limit, used }) => {
                    assert_eq!(limit, cap);
                    assert!(used > cap);
                    checked_horn_refusal = true;
                    break;
                }
                Ok(out) => {
                    assert!(
                        !out.stats.horn_over_budget,
                        "cap={cap}: partial: true silently downgraded Horn to EL"
                    );
                }
                Err(e) => panic!("cap={cap}: unexpected error {e:?}"),
            }
        }
    }
    assert!(
        checked_horn_refusal,
        "expected a pre-saturation cap between EL ({lo}) and Horn ({hi}) where partial: false downgrades to EL and partial: true returns Err(Memory)"
    );

    // 2. Verify Tier D pre-saturation memory refusal preserves Tier D under `partial: true`
    //    (never silently downgrading to Horn), while `dl_attempt_work` still falls back.
    let mut dl_body = String::from("SubClassOf(:Seed0 :Seed1)\n");
    for i in 0..300usize {
        let a = i % 15;
        let b0 = (i / 15) % 24;
        let b1 = (b0 + 1 + (i % 5)) % 24;
        let b2 = (b1 + 1 + ((i / 5) % 5)) % 24;
        let b3 = (b2 + 1) % 24;
        dl_body.push_str(&format!(
            "SubClassOf(:A{a} ObjectUnionOf(:B{b0} :B{b1} :B{b2} :B{b3}))\n"
        ));
    }
    let dl_ax = axioms(&dl_body);
    let dl_full = Classifier::of(&dl_ax)
        .classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .unwrap();
    assert_eq!(dl_full.stats.tier, Tier::Disjunctive);
    let el_dl = Classifier::of(&dl_ax)
        .classify(&Limits {
            threads: 1,
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    let mut saw_dl_refusal = false;
    let lo_dl = el_dl.stats.peak_memory + 1;
    let hi_dl = dl_full.stats.peak_memory;
    for step in 0..=40 {
        let cap = lo_dl + hi_dl.saturating_sub(lo_dl) * step / 40;
        let non_partial = Classifier::of(&dl_ax).classify(&Limits {
            threads: 1,
            partial: false,
            max_memory: Some(cap),
            ..Limits::default()
        });
        if let Ok(downgraded) = non_partial
            && downgraded.stats.dl_over_budget
        {
            let res = Classifier::of(&dl_ax).classify(&Limits {
                threads: 1,
                partial: true,
                max_memory: Some(cap),
                ..Limits::default()
            });
            match res {
                Err(Error::Memory { limit, used }) => {
                    assert_eq!(limit, cap);
                    assert!(used > cap);
                    saw_dl_refusal = true;
                    break;
                }
                Ok(out) => {
                    assert!(
                        !out.stats.dl_over_budget && !out.stats.horn_over_budget,
                        "cap={cap}: partial: true silently downgraded Tier D"
                    );
                }
                Err(e) => panic!("cap={cap}: unexpected error {e:?}"),
            }
        }
    }
    assert!(
        saw_dl_refusal,
        "expected a cap between EL ({lo_dl}) and Tier D ({hi_dl}) where partial: false downgrades Tier D and partial: true returns Err(Memory)"
    );
}

#[test]
fn memory_budget_sweep_and_pool_admission_preserve_el_and_horn_tiers() {
    let cases = [
        (
            "el_plain",
            el_plain_ontology(if cfg!(debug_assertions) { 45 } else { 120 }, false),
            Tier::El,
        ),
        (
            "el_full",
            el_full_ontology(if cfg!(debug_assertions) { 40 } else { 100 }, false),
            Tier::El,
        ),
        (
            "horn_plain",
            horn_plain_ontology(if cfg!(debug_assertions) { 35 } else { 90 }, false),
            Tier::Horn,
        ),
        (
            "horn_full",
            horn_full_ontology(if cfg!(debug_assertions) { 25 } else { 70 }, false),
            Tier::Horn,
        ),
    ];

    for (name, body, tier) in cases {
        let ax = axioms(&body);
        let cs = classes(&ax);
        for threads in [1, 8] {
            let full = Classifier::of(&ax)
                .classify(&Limits {
                    threads,
                    ..Limits::default()
                })
                .unwrap();
            assert!(full.complete);
            assert_eq!(full.stats.tier, tier, "{name}");
            let peak = full.stats.peak_memory;
            let mut partials = 0usize;
            let mut refused = 0usize;
            for f in 1..20 {
                let cap = peak * f / 20;
                let mut k = Classifier::of(&ax);
                match k.classify(&Limits {
                    threads,
                    partial: true,
                    max_memory: Some(cap),
                    ..Limits::default()
                }) {
                    Err(Error::Memory { limit, used }) => {
                        assert_eq!(limit, cap, "{name} threads={threads}");
                        assert!(used > cap, "{name} threads={threads}");
                        refused += 1;
                    }
                    Err(e) => panic!("{name} threads={threads} cap={cap}: unexpected {e:?}"),
                    Ok(out) => {
                        assert_eq!(out.stats.tier, tier, "{name} cap={cap}");
                        assert!(
                            !out.stats.horn_over_budget,
                            "{name} cap={cap} silently downgraded Horn to EL"
                        );
                        assert!(
                            out.stats.memory <= cap,
                            "{name} cap={cap}: memory {} > cap",
                            out.stats.memory
                        );
                        check_partial(&out, &full, &cs);
                        if !out.complete_for_taken {
                            partials += 1;
                        }
                    }
                }
                check_recovery(&mut k, &full, &cs, threads, tier);
            }
            assert!(partials > 0, "{name} threads={threads}: no partial answer");
            assert!(refused > 0, "{name} threads={threads}: no memory refusal");

            // Cumulative memory pool (`charge`) without `max_memory`.
            let pool_cap = peak * 3 / 5;
            let accepted = Arc::new(AtomicUsize::new(0));
            let denied = Arc::new(AtomicUsize::new(0));
            let (g, r) = (accepted.clone(), denied.clone());
            let mut k = Classifier::of(&ax);
            let res = k.classify(&Limits {
                threads,
                partial: true,
                charge: Some(Arc::new(move |n| {
                    let ok = g
                        .try_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
                            old.checked_add(n).filter(|next| *next <= pool_cap)
                        })
                        .is_ok();
                    if !ok {
                        r.fetch_add(1, Ordering::SeqCst);
                    }
                    ok
                })),
                ..Limits::default()
            });
            let paid = accepted.load(Ordering::SeqCst);
            assert!(
                paid <= pool_cap,
                "{name}: paid {paid} > pool_cap {pool_cap}"
            );
            assert!(denied.load(Ordering::SeqCst) > 0, "{name}: expected denial");
            match res {
                Err(Error::Memory { limit, .. }) => assert_eq!(limit, usize::MAX),
                Err(e) => panic!("{name} pool: unexpected {e:?}"),
                Ok(out) => {
                    assert_eq!(out.stats.tier, tier);
                    assert!(!out.stats.horn_over_budget);
                    assert!(
                        out.stats.memory <= paid,
                        "{name}: memory {} > paid {paid}",
                        out.stats.memory
                    );
                    check_partial(&out, &full, &cs);
                }
            }
            check_recovery(&mut k, &full, &cs, threads, tier);
        }
    }
}

#[test]
fn tight_cap_csr_fallback_returns_sound_unknown_partial_answer() {
    // Use an EL ontology with high existential fanout so saturation builds many forward
    // edges (`step2_bytes` large relative to stripped retained state), and sweep caps
    // around the boundary where CSR admission refuses `rev_edges` yet stripping saturation
    // tables lets the conservative `(all incomplete, Unknown)` partial taxonomy fit.
    let n = if cfg!(debug_assertions) { 45 } else { 120 };
    let ax = axioms(&el_plain_ontology(n, false));
    let cs = classes(&ax);
    let full = Classifier::of(&ax)
        .classify(&Limits {
            threads: 1,
            ..Limits::default()
        })
        .unwrap();
    let peak = full.stats.peak_memory;
    let mut saw_unknown_partial = false;
    for f in 1..40 {
        let cap = peak * f / 40;
        let res = Classifier::of(&ax).classify(&Limits {
            threads: 1,
            partial: true,
            max_memory: Some(cap),
            ..Limits::default()
        });
        if let Ok(out) = res {
            assert!(out.stats.memory <= cap);
            check_partial(&out, &full, &cs);
            if !out.complete_for_taken && out.consistency == Consistency::Unknown {
                saw_unknown_partial = true;
            }
        }
    }
    assert!(
        saw_unknown_partial,
        "expected tight-cap EL partial stop with Unknown consistency"
    );
}

#[test]
fn randomized_differential_el_and_horn_partial_stops() {
    let seeds: u64 = if cfg!(debug_assertions) { 30 } else { 120 };
    let mut partial = 0usize;
    for seed in 0..seeds {
        let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = |m: u64| -> u64 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x % m
        };
        let is_horn = seed % 2 == 1;
        let with_inds = (seed / 2) % 2 == 1;
        let mut body = String::new();
        for a in 0..8 {
            body.push_str(&format!("Declaration(Class(:C{a}))\n"));
        }
        if is_horn {
            body.push_str("InverseObjectProperties(:r0 :r1)\n");
        }
        for _ in 0..14 {
            let a = next(8);
            let b = next(8);
            let r = next(2);
            match next(if is_horn { 7 } else { 5 }) {
                0 => body.push_str(&format!("SubClassOf(:C{a} :C{b})\n")),
                1 => body.push_str(&format!(
                    "SubClassOf(:C{a} ObjectSomeValuesFrom(:r{r} :C{b}))\n"
                )),
                2 => body.push_str(&format!(
                    "SubClassOf(ObjectSomeValuesFrom(:r{r} :C{a}) :C{b})\n"
                )),
                3 => {
                    let b2 = if b == a { (a + 1) % 8 } else { b };
                    let c2 = next(8);
                    body.push_str(&format!(
                        "SubClassOf(ObjectIntersectionOf(:C{a} :C{b2}) :C{c2})\n"
                    ));
                }
                4 if with_inds => {
                    let ind = next(2);
                    body.push_str(&format!("SubClassOf(:C{a} ObjectOneOf(:i{ind}))\n"));
                }
                4 => body.push_str(&format!("DisjointClasses(:C{a} :C{b})\n")),
                5 => body.push_str(&format!(
                    "SubClassOf(:C{a} ObjectAllValuesFrom(:r{r} :C{b}))\n"
                )),
                _ => body.push_str(&format!(
                    "SubClassOf(:C{a} ObjectMaxCardinality(1 :r{r} :C{b}))\n"
                )),
            }
        }
        if with_inds {
            body.push_str(&format!("ClassAssertion(:C{} :i0)\n", next(8)));
        }
        let ax = axioms(&body);
        let cs = classes(&ax);
        let mut k = Classifier::of(&ax);
        let full = k.classify(&Limits::default()).unwrap();
        if !k.excluded().is_empty() {
            continue;
        }
        for threads in [1, 8] {
            let total = polls_of(&ax, threads, false);
            for stop in 0..total {
                let Ok(out) = Classifier::of(&ax).classify(&cancelled_at(stop, threads, false))
                else {
                    continue;
                };
                check_partial(&out, &full, &cs);
                partial += (!out.complete_for_taken) as usize;
            }
        }
    }
    assert!(partial > 0, "randomized sweep produced no partial answers");
}

#[test]
fn incremental_edit_partial_stop_and_clean_recovery() {
    for (body, tier) in [
        (el_plain_ontology(80, false), Tier::El),
        (horn_plain_ontology(30, false), Tier::Horn),
    ] {
        let ax = axioms(&body);
        let mut k = Classifier::of(&ax);
        let base = k.classify(&Limits::default()).unwrap();
        assert!(base.complete);
        assert_eq!(base.stats.tier, tier);

        // Add a new axiom touching a chain component and stop mid-incremental saturation.
        let extra = axioms(
            "SubClassOf(:Extra0 :Extra1)\nSubClassOf(:Extra1 :A0)\nSubClassOf(:Extra1 :H0)\n",
        );
        k.add(extra.iter());
        let mut all = ax.clone();
        all.extend(extra);
        let cs = classes(&all);
        let full = Classifier::of(&all).classify(&Limits::default()).unwrap();

        let mut saw_partial = false;
        for stop in 0..200 {
            let mut inc = Classifier::of(&ax);
            inc.classify(&Limits::default()).unwrap();
            inc.add(all[ax.len()..].iter());
            if let Ok(out) = inc.classify(&cancelled_at(stop, 1, false)) {
                check_partial(&out, &full, &cs);
                if !out.complete_for_taken {
                    saw_partial = true;
                    // Next run resets cleanly and matches fresh full classification.
                    check_recovery(&mut inc, &full, &cs, 1, tier);
                    break;
                }
            }
        }
        assert!(
            saw_partial,
            "tier {tier:?}: incremental edit produced no partial stop"
        );
    }
}

#[test]
fn el_only_and_tier_d_fallback_to_horn_and_non_partial_saturate() {
    // 1. Limits::el_only + partial on a Horn ontology produces a partial EL answer.
    let horn_ax = axioms(&horn_plain_ontology(50, false));
    let cs = classes(&horn_ax);
    let el_full = Classifier::of(&horn_ax)
        .classify(&Limits {
            el_only: true,
            ..Limits::default()
        })
        .unwrap();
    let total = polls_of(&horn_ax, 1, true);
    let mut saw_el_partial = false;
    for k in (0..total).step_by((total / 30).max(1)) {
        if let Ok(out) = Classifier::of(&horn_ax).classify(&cancelled_at(k, 1, true)) {
            assert_eq!(out.stats.tier, Tier::Horn); // ontology's syntactic tier
            check_partial(&out, &el_full, &cs);
            if !out.complete_for_taken {
                saw_el_partial = true;
            }
        }
    }
    assert!(
        saw_el_partial,
        "el_only + partial produced no partial answer"
    );

    // 2. Tier D Stop::Work fallback to Horn + subsequent Horn cancel produces sound partial Horn.
    let mut dl_horn_body = horn_plain_ontology(35, false);
    dl_horn_body.push_str(
        "SubClassOf(:U0 ObjectUnionOf(:U1 :U2))\n\
         SubClassOf(:U1 :U3)\n\
         SubClassOf(:U2 :U3)\n",
    );
    let dl_ax = axioms(&dl_horn_body);
    let dl_cs = classes(&dl_ax);
    // Reference: Horn fallback with tiny dl_attempt_work so Tier D falls back to Horn.
    let horn_ref = Classifier::of(&dl_ax)
        .classify(&Limits {
            threads: 1,
            dl_attempt_work: Some(1),
            ..Limits::default()
        })
        .unwrap();
    assert!(horn_ref.stats.dl_over_budget);
    let polls = Arc::new(AtomicUsize::new(0));
    let p = polls.clone();
    Classifier::of(&dl_ax)
        .classify(&Limits {
            threads: 1,
            dl_attempt_work: Some(1),
            cancelled: Some(Arc::new(move || {
                p.fetch_add(1, Ordering::Relaxed);
                false
            })),
            ..Limits::default()
        })
        .unwrap();
    let total = polls.load(Ordering::Relaxed);
    let mut saw_fallback_partial = false;
    for k in (0..total).step_by((total / 40).max(1)) {
        let polls = Arc::new(AtomicUsize::new(0));
        let res = Classifier::of(&dl_ax).classify(&Limits {
            threads: 1,
            partial: true,
            dl_attempt_work: Some(1),
            cancelled: Some(Arc::new(move || polls.fetch_add(1, Ordering::Relaxed) >= k)),
            ..Limits::default()
        });
        if let Ok(out) = res
            && out.stats.dl_over_budget
            && !out.complete_for_taken
        {
            check_partial(&out, &horn_ref, &dl_cs);
            saw_fallback_partial = true;
            break;
        }
    }
    assert!(
        saw_fallback_partial,
        "Tier D Stop::Work fallback to Horn + partial produced no partial Horn answer"
    );

    // 3. Saturation::saturate (`with_taxonomy == false`) never returns partial state.
    let el_ax = axioms(&el_plain_ontology(80, false));
    let res = Classifier::of(&el_ax).saturate(&Limits {
        threads: 1,
        partial: true,
        cancelled: Some(Arc::new(|| true)),
        ..Limits::default()
    });
    assert!(matches!(res, Err(Error::Cancelled)), "{res:?}");

    // 4. EL ontology with a top-chain named-domain projection works cleanly with partial: true.
    let mut top_body = String::from(
        "SubObjectPropertyOf(ObjectPropertyChain(:r1 owl:topObjectProperty) :topHead)\n\
         ObjectPropertyDomain(:topHead :TopDom)\n",
    );
    top_body.push_str(&el_plain_ontology(40, false));
    let top_ax = axioms(&top_body);
    let top_cs = classes(&top_ax);
    let top_full = Classifier::of(&top_ax)
        .classify(&Limits::default())
        .unwrap();
    let top_total = polls_of(&top_ax, 1, false);
    let mut saw_top_partial = false;
    for k in (0..top_total).step_by((top_total / 30).max(1)) {
        if let Ok(out) = Classifier::of(&top_ax).classify(&cancelled_at(k, 1, false)) {
            check_partial(&out, &top_full, &top_cs);
            if !out.complete_for_taken {
                saw_top_partial = true;
            }
        }
    }
    assert!(
        saw_top_partial,
        "top_chain EL ontology produced no partial answer"
    );

    // 5. Deadline stop on EL and Horn returns valid partial answers.
    for (body, tier) in [
        (
            el_plain_ontology(if cfg!(debug_assertions) { 45 } else { 120 }, false),
            Tier::El,
        ),
        (
            horn_plain_ontology(if cfg!(debug_assertions) { 35 } else { 90 }, false),
            Tier::Horn,
        ),
    ] {
        let ax = axioms(&body);
        let cs = classes(&ax);
        let t = Instant::now();
        let full = Classifier::of(&ax)
            .classify(&Limits {
                threads: 1,
                ..Limits::default()
            })
            .unwrap();
        let took = t.elapsed();
        let mut d = Duration::from_micros(50);
        while d < took.mul_f64(1.5) {
            if let Ok(out) = Classifier::of(&ax).classify(&Limits {
                threads: 1,
                partial: true,
                deadline: Some(Instant::now() + d),
                ..Limits::default()
            }) {
                assert_eq!(out.stats.tier, tier);
                check_partial(&out, &full, &cs);
            }
            d = d.mul_f64(1.2);
        }
    }
}

#[test]
fn classification_public_fields_allow_literal_and_exhaustive_destructuring() {
    let ax = axioms("SubClassOf(:A :B)");
    let out = Classifier::of(&ax).classify(&Limits::default()).unwrap();
    let Classification {
        consistent,
        taxonomy,
        stats,
        complete,
        complete_for_taken,
        incomplete,
        consistency,
        unmapped,
    } = out;
    let rebuilt = Classification {
        consistent,
        taxonomy,
        stats,
        complete,
        complete_for_taken,
        incomplete,
        consistency,
        unmapped,
    };
    assert!(rebuilt.complete && rebuilt.consistent);
    assert_eq!(rebuilt.consistency, Consistency::Consistent);
}

#[test]
fn bare_coverage_mutators_preserve_memory_estimates() {
    let ax = axioms("SubClassOf(:A :B) SubClassOf(:B ObjectUnionOf(:C :D)) SubClassOf(:X :Y)");
    let limits = Limits {
        el_only: true,
        cover_later: true,
        ..Limits::default()
    };
    let mut classifier = Classifier::of(&ax);
    let mut out = classifier.classify(&limits).unwrap();
    out.incomplete.push(Class::new(format!("{EX}X")));
    let memory = out.stats.memory;
    let peak = out.stats.peak_memory;
    let kinds: Vec<_> = ax.iter().map(|a| &a.kind).collect();

    out.cover(&kinds, &classifier.excluded(), None, &limits);
    let after_cover = (out.stats.memory, out.stats.peak_memory);
    assert!(out.incomplete.contains(&Class::new(format!("{EX}A"))));
    assert!(out.incomplete.contains(&Class::new(format!("{EX}X"))));
    assert!(!out.incomplete.contains(&Class::new(format!("{EX}Y"))));

    out.leave_out_unmapped(Unmapped {
        count: 1,
        ..Unmapped::default()
    });
    assert!(out.incomplete.contains(&Class::new(format!("{EX}Y"))));
    assert_eq!(out.consistency, Consistency::Unknown);
    assert_eq!(out.stats.unmapped, 1);
    assert_eq!(after_cover, (memory, peak));
    assert_eq!((out.stats.memory, out.stats.peak_memory), (memory, peak));
}
