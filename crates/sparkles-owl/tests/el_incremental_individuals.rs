//! Targeted regression and randomized differential tests for incremental OWL 2 EL
//! classification with individuals, merges, keys, universal roles, and conditional
//! worlds (roadmap item 664).

use sparkles_owl::classify::{Changes, Classification, Classifier, Limits, Saturation};
use sparkles_owl::model::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn c(s: &str) -> Class {
    Class::new(format!("http://ex.org/{s}"))
}
fn ce(s: &str) -> ClassExpression {
    c(s).into()
}
fn p(s: &str) -> ObjectPropertyExpression {
    ObjectProperty::new(format!("http://ex.org/{s}")).into()
}
fn dp(s: &str) -> DataProperty {
    DataProperty::new(format!("http://ex.org/{s}"))
}
fn ind(s: &str) -> Individual {
    Individual::named(format!("http://ex.org/{s}"))
}
fn anon(s: &str) -> Individual {
    Individual::anonymous(s)
}
fn one(i: Individual) -> ClassExpression {
    ClassExpression::ObjectOneOf([i].into_iter().collect())
}
fn some(r: ObjectPropertyExpression, f: ClassExpression) -> ClassExpression {
    ClassExpression::some(r, f)
}
fn value(r: ObjectPropertyExpression, i: Individual) -> ClassExpression {
    ClassExpression::ObjectHasValue(r, i)
}
fn dval(d: &str, v: &str) -> ClassExpression {
    ClassExpression::DataHasValue(dp(d), Literal::simple(v))
}
fn dint(d: &str, v: i64) -> ClassExpression {
    ClassExpression::DataHasValue(
        dp(d),
        Literal::typed(v.to_string(), "http://www.w3.org/2001/XMLSchema#integer"),
    )
}
fn ax(k: AxiomKind) -> Axiom {
    Axiom::new(k)
}
fn sub(a: ClassExpression, b: ClassExpression) -> Axiom {
    Axiom::sub_class_of(a, b)
}
fn is_a(class: ClassExpression, i: Individual) -> Axiom {
    ax(AxiomKind::ClassAssertion(class, i))
}
fn op_assert(r: ObjectPropertyExpression, a: Individual, b: Individual) -> Axiom {
    ax(AxiomKind::ObjectPropertyAssertion(r, a, b))
}
fn dp_assert(d: DataProperty, a: Individual, lit: Literal) -> Axiom {
    ax(AxiomKind::DataPropertyAssertion(d, a, lit))
}
fn neg_op(r: ObjectPropertyExpression, a: Individual, b: Individual) -> Axiom {
    ax(AxiomKind::NegativeObjectPropertyAssertion(r, a, b))
}
fn neg_dp(d: DataProperty, a: Individual, lit: Literal) -> Axiom {
    ax(AxiomKind::NegativeDataPropertyAssertion(d, a, lit))
}
fn same_ind(is: impl IntoIterator<Item = Individual>) -> Axiom {
    ax(AxiomKind::SameIndividual(is.into_iter().collect()))
}
fn diff_ind(is: impl IntoIterator<Item = Individual>) -> Axiom {
    ax(AxiomKind::DifferentIndividuals(
        is.into_iter().collect::<Vec<_>>().into(),
    ))
}
fn has_key(
    class: ClassExpression,
    ops: impl IntoIterator<Item = ObjectPropertyExpression>,
    dps: impl IntoIterator<Item = DataProperty>,
) -> Axiom {
    ax(AxiomKind::HasKey(
        class,
        ops.into_iter().collect(),
        dps.into_iter().collect(),
    ))
}
fn range(r: ObjectPropertyExpression, cls: ClassExpression) -> Axiom {
    ax(AxiomKind::ObjectPropertyRange(r, cls))
}

fn assert_matches_fresh(
    k: &Classifier,
    inc: &Classification,
    axioms: &[Axiom],
    limits: &Limits,
    label: &str,
) {
    let mut fresh_k = Classifier::of(axioms);
    let fresh = fresh_k.classify(limits).unwrap();
    assert_eq!(inc.taxonomy, fresh.taxonomy, "{label}: taxonomy mismatch");
    assert_eq!(
        inc.consistent, fresh.consistent,
        "{label}: consistency mismatch"
    );
    assert_eq!(inc.complete, fresh.complete, "{label}: complete mismatch");
    assert_eq!(
        k.excluded(),
        fresh_k.excluded(),
        "{label}: excluded mismatch"
    );
    for cls in fresh.taxonomy.classes() {
        assert_eq!(
            k.describe(cls),
            fresh_k.describe(cls),
            "{label}: describe mismatch for {cls:?}"
        );
    }
}

/// Builds a disjoint chain of `n` unrelated classes (`U0 ⊑ U1 ⊑ ... ⊑ U_{n-1}`) to
/// verify that unaffected contexts are NOT cleared on localized edits.
fn unrelated_chain(prefix: &str, n: usize) -> Vec<Axiom> {
    (0..n.saturating_sub(1))
        .map(|i| {
            sub(
                ce(&format!("{prefix}{i}")),
                ce(&format!("{prefix}{}", i + 1)),
            )
        })
        .collect()
}

#[test]
fn real_nominal_merge_self_supporting_cycle_breaks_on_root_removal() {
    // Regression for the self-supporting `real` cycle:
    //   {root} ⊑ ∃r.W, W ⊑ {a}, W ⊑ ∃t.W, W ⊑ Goal
    // Removing `{root} ⊑ ∃r.W` must break `W.real` even though `a` previously
    // received `∃t.W` from `W` when `W` was real, while keeping the 20 unrelated
    // classes untouched.
    let trigger = is_a(some(p("r"), ce("W")), ind("root"));
    let mut axioms = vec![
        trigger.clone(),
        sub(ce("W"), one(ind("a"))),
        sub(ce("W"), some(p("t"), ce("W"))),
        sub(ce("W"), ce("Goal")),
        sub(one(ind("a")), ce("Check")),
        sub(ClassExpression::and([one(ind("a")), ce("Goal")]), ce("Hit")),
        Axiom::declaration(Entity::Class(c("Hit"))),
        Axiom::declaration(Entity::Class(c("Check"))),
    ];
    axioms.extend(unrelated_chain("U", 20));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert_matches_fresh(&k, &init, &axioms, &limits, "cycle init");

    // Removing the trigger must stay incremental, clear only the affected merge/real
    // cluster (NOT the 20 unrelated classes), and match fresh classification.
    k.remove([&trigger]);
    axioms.retain(|a| a != &trigger);
    let after = k.classify(&limits).unwrap();
    assert!(
        after.stats.incremental,
        "expected incremental run: {:?}",
        after.stats
    );
    assert!(
        after.stats.cleared < 10,
        "unrelated contexts should not be cleared: cleared={}",
        after.stats.cleared
    );
    assert_matches_fresh(&k, &after, &axioms, &limits, "cycle after remove");
    // Re-adding restores the merge incrementally.
    k.add([&trigger]);
    axioms.push(trigger);
    let restored = k.classify(&limits).unwrap();
    assert!(restored.stats.incremental);
    assert!(restored.stats.cleared < 10);
    assert_matches_fresh(&k, &restored, &axioms, &limits, "cycle restored");

    // Also test a `owl:Thing`-rooted self-supporting cycle (modifying `owl:Thing` affects
    // every context holding `TOP`, and must still stay sound and match fresh).
    let top_trigger = sub(
        ClassExpression::Class(Class::thing()),
        some(p("r2"), ce("W2")),
    );
    let mut top_axioms = vec![
        top_trigger.clone(),
        sub(ce("W2"), one(ind("a2"))),
        sub(ce("W2"), some(p("t2"), ce("W2"))),
        sub(ce("W2"), ce("Goal2")),
        sub(
            ClassExpression::and([one(ind("a2")), ce("Goal2")]),
            ce("Hit2"),
        ),
        Axiom::declaration(Entity::Class(c("Hit2"))),
    ];
    let mut kt = Classifier::of(&top_axioms);
    let t_init = kt.classify(&limits).unwrap();
    assert_matches_fresh(&kt, &t_init, &top_axioms, &limits, "top cycle init");
    kt.remove([&top_trigger]);
    top_axioms.retain(|a| a != &top_trigger);
    let t_after = kt.classify(&limits).unwrap();
    assert!(t_after.stats.incremental);
    assert_matches_fresh(
        &kt,
        &t_after,
        &top_axioms,
        &limits,
        "top cycle after remove",
    );
}

#[test]
fn dual_real_paths_reseed_real_without_losing_merge() {
    // Both `r1_root ⊑ ∃r1.S` and `a ⊑ ∃r2.S` make `S` real; `S ⊑ ∃s.T`, `T ⊑ {b}`,
    // `T ⊑ Extra`. Removing `r1_root ⊑ ∃r1.S` must re-seed `S.real` from `a` so `T`
    // remains real and `{b}` still receives `Extra`, without touching unrelated classes.
    let first_link = is_a(some(p("r1"), ce("S")), ind("r1_root"));
    let a_link = is_a(some(p("r2"), ce("S")), ind("a"));
    let mut axioms = vec![
        first_link.clone(),
        a_link.clone(),
        sub(ce("S"), some(p("s"), ce("T"))),
        sub(ce("T"), one(ind("b"))),
        sub(ce("T"), ce("Extra")),
        sub(ce("Probe"), one(ind("b"))),
        Axiom::declaration(Entity::Class(c("Probe"))),
        Axiom::declaration(Entity::Class(c("Extra"))),
    ];
    axioms.extend(unrelated_chain("U", 20));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert!(init.taxonomy.is_subclass(&c("Probe"), &c("Extra")));

    // Remove first real path; `a` still makes `S` real.
    k.remove([&first_link]);
    axioms.retain(|a| a != &first_link);
    let mid = k.classify(&limits).unwrap();
    assert!(mid.stats.incremental);
    assert!(mid.stats.cleared < 10, "cleared={}", mid.stats.cleared);
    assert!(mid.taxonomy.is_subclass(&c("Probe"), &c("Extra")));
    assert_matches_fresh(&k, &mid, &axioms, &limits, "dual real mid");

    // Remove second real path; now `S` and `T` are no longer real, so `b` loses `Extra`.
    k.remove([&a_link]);
    axioms.retain(|a| a != &a_link);
    let end = k.classify(&limits).unwrap();
    assert!(end.stats.incremental);
    assert!(end.stats.cleared < 10, "cleared={}", end.stats.cleared);
    assert!(!end.taxonomy.is_subclass(&c("Probe"), &c("Extra")));
    assert_matches_fresh(&k, &end, &axioms, &limits, "dual real end");
}

#[test]
fn non_real_nominal_edit_does_not_clear_nominal_or_unrelated_classes() {
    // `C ⊑ {a}` (non-real witness) and `Other ⊑ {a}` plus 25 unrelated classes.
    // Editing `C ⊑ X` must NOT clear `{a}` or the unrelated classes, and must keep
    // `Other`'s world reused!
    let edit = sub(ce("C"), ce("X"));
    let mut axioms = vec![
        sub(ce("C"), one(ind("a"))),
        sub(ce("C"), ce("CProp")),
        sub(ce("Other"), one(ind("b"))),
        sub(ce("Other"), ce("OProp")),
        sub(
            ClassExpression::and([one(ind("b")), ce("OProp")]),
            ce("OWin"),
        ),
        is_a(ce("AType"), ind("a")),
        is_a(ce("BType"), ind("b")),
        Axiom::declaration(Entity::Class(c("OWin"))),
        Axiom::declaration(Entity::Class(c("X"))),
    ];
    axioms.extend(unrelated_chain("U", 25));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert!(init.taxonomy.is_subclass(&c("Other"), &c("OWin")));
    assert!(init.stats.worlds_run >= 2, "{:?}", init.stats);

    k.add([&edit]);
    axioms.push(edit.clone());
    let inc = k.classify(&limits).unwrap();
    assert!(inc.stats.incremental);
    assert!(inc.stats.cleared <= 3, "cleared={}", inc.stats.cleared);
    assert!(
        inc.stats.worlds_reused >= 1,
        "Other's world must be reused: {:?}",
        inc.stats
    );
    assert_matches_fresh(&k, &inc, &axioms, &limits, "non-real edit add");

    k.remove([&edit]);
    axioms.retain(|a| a != &edit);
    let inc2 = k.classify(&limits).unwrap();
    assert!(inc2.stats.incremental);
    assert!(inc2.stats.cleared <= 3, "cleared={}", inc2.stats.cleared);
    assert!(inc2.stats.worlds_reused >= 1, "{:?}", inc2.stats);
    assert_matches_fresh(&k, &inc2, &axioms, &limits, "non-real edit remove");
}

#[test]
fn universal_fact_grounded_witnesses_and_cycle_breaking() {
    // 1. Two independent witnesses `a` and `b` for `∃top.F ⊑ G`, plus self-loop `∃top.F ⊑ F`.
    // 2. Unrelated terminology edit keeps universal fact and is incremental with few clears.
    // 3. Removing witness `a` still has pre-broadcast witness `b` -> incremental!
    // 4. Removing witness `b` breaks all grounded supports -> restarts fresh and breaks
    //    the self-supporting `∃top.F ⊑ F` cycle!
    let wa = is_a(ce("F"), ind("a"));
    let wb = is_a(ce("F"), ind("b"));
    let mut axioms = vec![
        wa.clone(),
        wb.clone(),
        sub(some(ObjectProperty::top().into(), ce("F")), ce("F")),
        sub(some(ObjectProperty::top().into(), ce("F")), ce("G")),
        Axiom::declaration(Entity::Class(c("G"))),
    ];
    axioms.extend(unrelated_chain("U", 25));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert!(init.taxonomy.is_subclass(&c("U0"), &c("G")));

    // Unrelated edit stays incremental and clears only U0.
    let u_edit = sub(ce("U0"), ce("UExtra"));
    k.add([&u_edit]);
    axioms.push(u_edit);
    let step1 = k.classify(&limits).unwrap();
    assert!(step1.stats.incremental);
    assert!(step1.stats.cleared <= 2, "cleared={}", step1.stats.cleared);
    assert_matches_fresh(&k, &step1, &axioms, &limits, "univ unrelated edit");

    // Removing first witness `a` still keeps grounded witness `b` -> stays incremental!
    k.remove([&wa]);
    axioms.retain(|a| a != &wa);
    let step2 = k.classify(&limits).unwrap();
    assert!(
        step2.stats.incremental,
        "second witness keeps incremental: {:?}",
        step2.stats
    );
    assert!(step2.stats.cleared <= 4, "cleared={}", step2.stats.cleared);
    assert!(step2.taxonomy.is_subclass(&c("U0"), &c("G")));
    assert_matches_fresh(&k, &step2, &axioms, &limits, "univ remove first witness");

    // Removing last witness `b` must NOT self-support `F` via `∃top.F ⊑ F`!
    k.remove([&wb]);
    axioms.retain(|a| a != &wb);
    let step3 = k.classify(&limits).unwrap();
    assert!(
        !step3.stats.incremental,
        "losing all universal witnesses restarts"
    );
    assert!(!step3.taxonomy.is_subclass(&c("U0"), &c("G")));
    assert_matches_fresh(&k, &step3, &axioms, &limits, "univ remove last witness");
}

#[test]
fn key_merges_track_exact_context_dependencies_and_stay_local() {
    // Two independent keys:
    //   K1 = HasKey(Person, (hasSSN), (age)) merging (alice, alice2) via ssn1 & 30
    //   K2 = HasKey(Car, (hasVIN), ()) merging (car1, car2) via vin1
    // Plus 25 unrelated classes.
    let k1 = has_key(ce("Person"), [p("hasSSN")], [dp("age")]);
    let k2 = has_key(ce("Car"), [p("hasVIN")], []);
    let alice_ssn = op_assert(p("hasSSN"), ind("alice"), ind("ssn1"));
    let mut axioms = vec![
        k1.clone(),
        k2.clone(),
        is_a(ce("Person"), ind("alice")),
        is_a(ce("Person"), ind("alice2")),
        is_a(ce("AliceOnly"), ind("alice")),
        sub(ce("ProbeAlice2"), one(ind("alice2"))),
        alice_ssn.clone(),
        op_assert(p("hasSSN"), ind("alice2"), ind("ssn1")),
        dp_assert(
            dp("age"),
            ind("alice"),
            Literal::typed("30", "http://www.w3.org/2001/XMLSchema#integer"),
        ),
        dp_assert(
            dp("age"),
            ind("alice2"),
            Literal::typed("30", "http://www.w3.org/2001/XMLSchema#integer"),
        ),
        is_a(ce("Car"), ind("car1")),
        is_a(ce("Car"), ind("car2")),
        is_a(ce("Car1Only"), ind("car1")),
        sub(ce("ProbeCar2"), one(ind("car2"))),
        op_assert(p("hasVIN"), ind("car1"), ind("vin1")),
        op_assert(p("hasVIN"), ind("car2"), ind("vin1")),
        Axiom::declaration(Entity::Class(c("AliceOnly"))),
        Axiom::declaration(Entity::Class(c("Car1Only"))),
    ];
    axioms.extend(unrelated_chain("U", 25));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert!(
        init.taxonomy
            .is_subclass(&c("ProbeAlice2"), &c("AliceOnly"))
    );
    assert!(init.taxonomy.is_subclass(&c("ProbeCar2"), &c("Car1Only")));

    // 1. Remove `alice`'s SSN assertion -> breaks K1 merge (alice, alice2), keeps K2 and U0..U24!
    k.remove([&alice_ssn]);
    axioms.retain(|a| a != &alice_ssn);
    let step1 = k.classify(&limits).unwrap();
    assert!(step1.stats.incremental);
    assert!(
        step1.stats.cleared < 15,
        "unrelated classes and K2 cluster must not be cleared: cleared={}",
        step1.stats.cleared
    );
    assert!(
        !step1
            .taxonomy
            .is_subclass(&c("ProbeAlice2"), &c("AliceOnly"))
    );
    assert!(step1.taxonomy.is_subclass(&c("ProbeCar2"), &c("Car1Only")));
    assert_matches_fresh(&k, &step1, &axioms, &limits, "key break assertion");

    // 2. Re-add `alice`'s SSN assertion -> re-merges (alice, alice2) incrementally!
    k.add([&alice_ssn]);
    axioms.push(alice_ssn);
    let step2 = k.classify(&limits).unwrap();
    assert!(step2.stats.incremental);
    assert!(step2.stats.cleared < 15, "cleared={}", step2.stats.cleared);
    assert!(
        step2
            .taxonomy
            .is_subclass(&c("ProbeAlice2"), &c("AliceOnly"))
    );
    assert_matches_fresh(&k, &step2, &axioms, &limits, "key restore assertion");

    // 3. Remove K1 key axiom itself -> breaks (alice, alice2), keeps K2 and U0..U24!
    k.remove([&k1]);
    axioms.retain(|a| a != &k1);
    let step3 = k.classify(&limits).unwrap();
    assert!(step3.stats.incremental);
    assert!(step3.stats.cleared < 15, "cleared={}", step3.stats.cleared);
    assert!(
        !step3
            .taxonomy
            .is_subclass(&c("ProbeAlice2"), &c("AliceOnly"))
    );
    assert!(step3.taxonomy.is_subclass(&c("ProbeCar2"), &c("Car1Only")));
    assert_matches_fresh(&k, &step3, &axioms, &limits, "key remove axiom");

    // 4. Add K1 back -> re-merges incrementally!
    k.add([&k1]);
    axioms.push(k1);
    let step4 = k.classify(&limits).unwrap();
    assert!(step4.stats.incremental);
    assert!(step4.stats.cleared < 15, "cleared={}", step4.stats.cleared);
    assert!(
        step4
            .taxonomy
            .is_subclass(&c("ProbeAlice2"), &c("AliceOnly"))
    );
    assert_matches_fresh(&k, &step4, &axioms, &limits, "key re-add axiom");
}

#[test]
fn conditional_worlds_reuse_unaffected_and_invalidate_on_world_local_subsumer_and_keys() {
    // Module A: classes A0..A9 reach non-real witness WA -> {a}, where WA has AExtra
    //           and `a` is a Person with hasSSN -> ssn1.
    // Module B: classes B0..B9 reach non-real witness WB -> {b}, where WB has BExtra
    //           and `b` is a Car with hasVIN -> vin1.
    // Keys: K_person = HasKey(Person, (hasSSN), ()), K_car = HasKey(Car, (hasVIN), ()).
    let mut axioms = vec![
        has_key(ce("Person"), [p("hasSSN")], []),
        has_key(ce("Car"), [p("hasVIN")], []),
        sub(ce("WA"), one(ind("a"))),
        sub(ce("WA"), ce("AExtra")),
        sub(ce("WA"), some(p("hasSSN"), one(ind("ssn1")))),
        sub(
            ClassExpression::and([one(ind("a")), ce("AExtra")]),
            ce("ALocal"),
        ),
        is_a(ce("Person"), ind("a")),
        is_a(ce("Person"), ind("a_twin")),
        is_a(ce("TwinTag"), ind("a_twin")),
        sub(ce("WB"), one(ind("b"))),
        sub(ce("WB"), ce("BExtra")),
        sub(
            ClassExpression::and([one(ind("b")), ce("BExtra")]),
            ce("BLocal"),
        ),
        is_a(ce("Car"), ind("b")),
        Axiom::declaration(Entity::Class(c("TwinTag"))),
        Axiom::declaration(Entity::Class(c("LateHit"))),
    ];
    for i in 0..8 {
        axioms.push(sub(ce(&format!("A{i}")), some(p("pa"), ce("WA"))));
        axioms.push(sub(ce(&format!("B{i}")), some(p("pb"), ce("WB"))));
    }
    axioms.extend(unrelated_chain("U", 15));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let init = k.classify(&limits).unwrap();
    assert!(init.stats.worlds_run >= 16, "{:?}", init.stats);
    assert_matches_fresh(&k, &init, &axioms, &limits, "worlds init");

    // 1. Pure terminology edit on U0 -> ALL worlds reused, 0 worlds re-run!
    let u_edit = sub(ce("U0"), ce("UTag"));
    k.add([&u_edit]);
    axioms.push(u_edit);
    let step1 = k.classify(&limits).unwrap();
    assert!(step1.stats.incremental);
    assert_eq!(step1.stats.worlds_run, 0, "{:?}", step1.stats);
    assert!(step1.stats.worlds_reused >= 16, "{:?}", step1.stats);
    assert_matches_fresh(&k, &step1, &axioms, &limits, "worlds unrelated edit");

    // 2. Add `ALocal ⊑ LateHit`: `ALocal` was ONLY derived inside Module A's worlds (not
    //    globally!). Module A's worlds MUST invalidate and re-run to derive `LateHit`,
    //    while Module B's worlds MUST stay reused!
    let local_edit = sub(ce("ALocal"), ce("LateHit"));
    k.add([&local_edit]);
    axioms.push(local_edit);
    let step2 = k.classify(&limits).unwrap();
    assert!(step2.stats.incremental);
    assert!(
        step2.stats.worlds_run >= 8,
        "Module A worlds must re-run: {:?}",
        step2.stats
    );
    assert!(
        step2.stats.worlds_reused >= 8,
        "Module B worlds must be reused: {:?}",
        step2.stats
    );
    assert_matches_fresh(&k, &step2, &axioms, &limits, "worlds local subsumer edit");

    // 3. Add `hasSSN(a_twin, ssn1)`: inside Module A's worlds, `a` has `hasSSN -> ssn1`
    //    from `WA`, so now `K_person` fires inside Module A's worlds and merges `a` with
    //    `a_twin`, giving `TwinTag` to `a` inside Module A's worlds! Module A's worlds
    //    MUST invalidate and re-run, while Module B's worlds (watching only K_car) stay reused!
    let twin_ssn = op_assert(p("hasSSN"), ind("a_twin"), ind("ssn1"));
    k.add([&twin_ssn]);
    axioms.push(twin_ssn.clone());
    let step3 = k.classify(&limits).unwrap();
    assert!(step3.stats.incremental);
    assert!(
        step3.stats.worlds_run >= 8,
        "Module A worlds must re-run on K_person: {:?}",
        step3.stats
    );
    assert!(
        step3.stats.worlds_reused >= 8,
        "Module B worlds must stay reused: {:?}",
        step3.stats
    );
    assert_matches_fresh(&k, &step3, &axioms, &limits, "worlds key trigger edit");

    // 4. Remove `hasSSN(a_twin, ssn1)`: Module A's worlds revert, Module B stays reused!
    k.remove([&twin_ssn]);
    axioms.retain(|a| a != &twin_ssn);
    let step4 = k.classify(&limits).unwrap();
    assert!(step4.stats.incremental);
    assert!(step4.stats.worlds_reused >= 8, "{:?}", step4.stats);
    assert_matches_fresh(&k, &step4, &axioms, &limits, "worlds key remove edit");
}

#[test]
fn asserted_object_property_range_toggling_matches_fresh() {
    // Regression for `Builder::asserted`: `r(a, b)` changes `∃r.{b}`'s successor between
    // `{b} ⊓ R` and `{b}` and adds/removes told `({b}, R)`.
    let r_ab = op_assert(p("r"), ind("a"), ind("b"));
    let mut axioms = vec![
        range(p("r"), ce("R")),
        sub(ce("C"), value(p("r"), ind("b"))),
        sub(ClassExpression::and([one(ind("b")), ce("R")]), ce("Hit")),
        sub(ce("ProbeB"), one(ind("b"))),
        Axiom::declaration(Entity::Class(c("Hit"))),
        Axiom::declaration(Entity::Class(c("ProbeB"))),
    ];
    axioms.extend(unrelated_chain("U", 15));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let s0 = k.classify(&limits).unwrap();
    assert_matches_fresh(&k, &s0, &axioms, &limits, "asserted range 0");

    k.add([&r_ab]);
    axioms.push(r_ab.clone());
    let s1 = k.classify(&limits).unwrap();
    assert!(s1.stats.incremental);
    assert!(s1.stats.cleared < 12, "cleared={}", s1.stats.cleared);
    assert_matches_fresh(&k, &s1, &axioms, &limits, "asserted range add");

    k.remove([&r_ab]);
    axioms.retain(|a| a != &r_ab);
    let s2 = k.classify(&limits).unwrap();
    assert!(s2.stats.incremental);
    assert!(s2.stats.cleared < 12, "cleared={}", s2.stats.cleared);
    assert_matches_fresh(&k, &s2, &axioms, &limits, "asserted range remove");
}

#[test]
fn anonymous_individuals_interchangeable_and_top_data_property_edits() {
    let a1 = is_a(ce("C"), anon("b1"));
    let a2 = is_a(ce("C"), anon("b2"));
    let a2_extra = is_a(ce("D"), anon("b2"));
    let td = sub(
        ClassExpression::DataSomeValuesFrom(
            vec![DataProperty::top()],
            DataRange::Datatype(Datatype::new("http://www.w3.org/2001/XMLSchema#string")),
        ),
        ce("HasStr"),
    );
    let mut axioms = vec![
        a1.clone(),
        a2.clone(),
        sub(ce("C"), ce("E")),
        Axiom::declaration(Entity::Class(c("HasStr"))),
    ];
    axioms.extend(unrelated_chain("U", 15));
    let limits = Limits::default();
    let mut k = Classifier::of(&axioms);
    let s0 = k.classify(&limits).unwrap();
    assert_matches_fresh(&k, &s0, &axioms, &limits, "anon init");

    // Removing the kept representative swaps `interchangeable` to `b2`.
    k.remove([&a1]);
    axioms.retain(|a| a != &a1);
    let s1 = k.classify(&limits).unwrap();
    assert!(s1.stats.incremental);
    assert!(s1.stats.cleared < 12, "cleared={}", s1.stats.cleared);
    assert_matches_fresh(&k, &s1, &axioms, &limits, "anon remove rep");

    // Re-add `a1` and promote `b2` to non-lone.
    k.add([&a1, &a2_extra]);
    axioms.push(a1);
    axioms.push(a2_extra);
    let s2 = k.classify(&limits).unwrap();
    assert!(s2.stats.incremental);
    assert_matches_fresh(&k, &s2, &axioms, &limits, "anon promote");

    // Add `topDataProperty` axiom and then edit unrelated class beside it.
    k.add([&td]);
    axioms.push(td);
    let s3 = k.classify(&limits).unwrap();
    assert_matches_fresh(&k, &s3, &axioms, &limits, "top data add");

    let u_edit = sub(ce("U0"), ce("UExtra"));
    k.add([&u_edit]);
    axioms.push(u_edit);
    let s4 = k.classify(&limits).unwrap();
    assert!(s4.stats.incremental);
    assert!(s4.stats.cleared <= 3, "cleared={}", s4.stats.cleared);
    assert_matches_fresh(&k, &s4, &axioms, &limits, "top data unrelated edit");
}

#[test]
fn cancellation_and_tight_memory_preserve_safe_recovery() {
    let mut axioms = vec![
        has_key(ce("P"), [p("r")], []),
        is_a(ce("P"), ind("i1")),
        is_a(ce("P"), ind("i2")),
        op_assert(p("r"), ind("i1"), ind("v")),
        op_assert(p("r"), ind("i2"), ind("v")),
        sub(ce("W"), one(ind("i1"))),
        sub(ce("W"), ce("WTag")),
        sub(ce("Q"), some(p("s"), ce("W"))),
    ];
    axioms.extend(unrelated_chain("U", 15));
    let mut k = Classifier::of(&axioms);
    let base = k.classify(&Limits::default()).unwrap();
    assert_matches_fresh(&k, &base, &axioms, &Limits::default(), "recovery base");

    // Pre-cancelled incremental edit errors cleanly and next uncancelled run recovers.
    let edit = sub(ce("U0"), ce("UHit"));
    k.add([&edit]);
    axioms.push(edit);
    let flag = Arc::new(AtomicBool::new(true));
    let cancelled = Limits {
        cancelled: Some(Arc::new({
            let flag = flag.clone();
            move || flag.load(Ordering::Relaxed)
        })),
        ..Limits::default()
    };
    assert!(k.classify(&cancelled).is_err());
    let recovered = k.classify(&Limits::default()).unwrap();
    assert!(
        !recovered.stats.incremental,
        "post-error run must restart fresh"
    );
    assert_matches_fresh(
        &k,
        &recovered,
        &axioms,
        &Limits::default(),
        "recovery after cancel",
    );
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

struct IndivGen {
    rng: Rng,
}

impl IndivGen {
    fn cls(&mut self) -> ClassExpression {
        match self.rng.below(10) {
            0 => ClassExpression::Class(Class::thing()),
            1 => ClassExpression::Class(Class::nothing()),
            _ => ce(&format!("C{}", self.rng.below(6))),
        }
    }
    fn individual(&mut self) -> Individual {
        if self.rng.chance(20) {
            anon(&format!("b{}", self.rng.below(3)))
        } else {
            ind(&format!("i{}", self.rng.below(4)))
        }
    }
    fn role(&mut self) -> ObjectPropertyExpression {
        match self.rng.below(10) {
            0 => ObjectProperty::top().into(),
            1 => ObjectProperty::bottom().into(),
            _ => p(&format!("r{}", self.rng.below(3))),
        }
    }
    fn expr(&mut self, depth: usize) -> ClassExpression {
        if depth == 0 {
            return if self.rng.chance(25) {
                one(self.individual())
            } else {
                self.cls()
            };
        }
        match self.rng.below(10) {
            0..=2 => some(self.role(), self.expr(depth - 1)),
            3 | 4 => ClassExpression::and([self.expr(depth - 1), self.expr(depth - 1)]),
            5 => value(self.role(), self.individual()),
            6 => one(self.individual()),
            7 => {
                if self.rng.chance(50) {
                    dint(&format!("d{}", self.rng.below(2)), self.rng.below(3) as i64)
                } else {
                    dval(
                        &format!("d{}", self.rng.below(2)),
                        &format!("v{}", self.rng.below(3)),
                    )
                }
            }
            _ => self.cls(),
        }
    }
    fn editable_axiom(&mut self) -> Axiom {
        match self.rng.below(14) {
            0..=3 => sub(self.expr(2), self.expr(2)),
            4 => Axiom::equivalent_classes([ce(&format!("C{}", self.rng.below(6))), self.expr(1)]),
            5 => {
                let a = self.rng.below(6);
                let b = (a + 1 + self.rng.below(5)) % 6;
                Axiom::disjoint_classes([ce(&format!("C{a}")), ce(&format!("C{b}"))])
            }
            6 => is_a(self.expr(1), self.individual()),
            7 => op_assert(self.role(), self.individual(), self.individual()),
            8 => neg_op(self.role(), self.individual(), self.individual()),
            9 => {
                let lit = Literal::typed(
                    self.rng.below(3).to_string(),
                    "http://www.w3.org/2001/XMLSchema#integer",
                );
                let d = dp(&format!("d{}", self.rng.below(2)));
                if self.rng.chance(70) {
                    dp_assert(d, self.individual(), lit)
                } else {
                    neg_dp(d, self.individual(), lit)
                }
            }
            10 => same_ind([self.individual(), self.individual()]),
            11 => diff_ind([self.individual(), self.individual()]),
            12 => {
                let cls = if self.rng.chance(75) {
                    ce(&format!("C{}", self.rng.below(6)))
                } else {
                    self.expr(1)
                };
                let mut ops = Vec::new();
                let mut dps = Vec::new();
                if self.rng.chance(60) {
                    ops.push(p(&format!("r{}", self.rng.below(3))));
                }
                if self.rng.chance(40) {
                    dps.push(dp(&format!("d{}", self.rng.below(2))));
                }
                has_key(cls, ops, dps)
            }
            _ => sub(
                some(ObjectProperty::top().into(), self.cls()),
                ce(&format!("C{}", self.rng.below(6))),
            ),
        }
    }
    fn fixed(&mut self) -> Vec<Axiom> {
        let mut out = Vec::new();
        if self.rng.chance(60) {
            out.push(ax(AxiomKind::SubObjectPropertyOf(
                SubObjectPropertyExpression::Expression(p("r0")),
                p("r1"),
            )));
        }
        if self.rng.chance(40) {
            out.push(ax(AxiomKind::TransitiveObjectProperty(p("r2"))));
        }
        if self.rng.chance(50) {
            out.push(range(p("r0"), ce(&format!("C{}", self.rng.below(6)))));
        }
        if self.rng.chance(50) {
            out.push(ax(AxiomKind::FunctionalDataProperty(dp("d0"))));
        }
        for i in 0..6 {
            out.push(Axiom::declaration(Entity::Class(c(&format!("C{i}")))));
        }
        out
    }
}

#[test]
fn randomized_differential_sweep_with_individuals_keys_merges_and_universal_roles() {
    let seeds: u64 = std::env::var("OWL_INDIV_SWEEP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let mut total_changed = 0usize;
    let mut total_incremental = 0usize;
    let mut total_worlds_reused = 0usize;
    for seed in 0..seeds {
        for threads in [1usize, 3usize] {
            let mut g = IndivGen {
                rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ ((threads as u64) << 32) | 1),
            };
            let fixed = g.fixed();
            let mut edited: Vec<Axiom> = (0..5 + g.rng.below(6))
                .map(|_| g.editable_axiom())
                .collect();
            let limits = Limits {
                threads,
                ..Limits::default()
            };
            let all = |e: &[Axiom]| -> Vec<Axiom> { fixed.iter().chain(e).cloned().collect() };
            let kinds = |xs: &[Axiom]| -> BTreeSet<AxiomKind> {
                xs.iter().map(|a| a.kind.clone()).collect()
            };
            let mut k = Classifier::of(&all(&edited));
            k.classify(&limits).unwrap();
            let mut sat = Saturation::new();
            let mut before = kinds(&all(&edited));
            sat.classify(&before.iter().collect::<Vec<_>>(), None, &limits)
                .unwrap();
            for step in 0..8 {
                let mut removed = Vec::new();
                for _ in 0..g.rng.below(3) {
                    if !edited.is_empty() {
                        let i = g.rng.below(edited.len());
                        removed.push(edited.remove(i));
                    }
                }
                let added: Vec<Axiom> = (0..g.rng.below(3)).map(|_| g.editable_axiom()).collect();
                edited.extend(added.iter().cloned());
                k.remove(&removed);
                k.add(&added);
                let now_axioms = all(&edited);
                let inc = k.classify(&limits).unwrap();
                let label = format!(
                    "seed={seed} threads={threads} step={step} added={added:?} removed={removed:?}"
                );
                assert_matches_fresh(&k, &inc, &now_axioms, &limits, &label);
                let now = kinds(&now_axioms);
                let add: Vec<&AxiomKind> = now.difference(&before).collect();
                let rem: Vec<&AxiomKind> = before.difference(&now).collect();
                let changed = !add.is_empty() || !rem.is_empty();
                let held: Vec<&AxiomKind> = now.iter().collect();
                let sat_out = sat
                    .classify(
                        &held,
                        Some(Changes {
                            added: &add,
                            removed: &rem,
                        }),
                        &limits,
                    )
                    .unwrap();
                let fresh = Classifier::of(&now_axioms).classify(&limits).unwrap();
                assert_eq!(sat_out.taxonomy, fresh.taxonomy, "sat {label}");
                assert_eq!(sat_out.consistent, fresh.consistent, "sat {label}");
                assert_eq!(sat_out.complete, fresh.complete, "sat {label}");
                if changed {
                    total_changed += 1;
                    if inc.stats.incremental {
                        total_incremental += 1;
                    }
                    total_worlds_reused += inc.stats.worlds_reused;
                }
                before = now;
            }
        }
    }
    eprintln!(
        "el_incremental_individuals sweep: {total_incremental}/{total_changed} incremental, \
         {total_worlds_reused} cached worlds reused"
    );
    assert!(
        total_incremental * 2 >= total_changed,
        "expected majority incremental reuse: {total_incremental}/{total_changed}"
    );
    assert!(
        total_worlds_reused > 0,
        "expected cached world reuse during randomized sweep"
    );
}
