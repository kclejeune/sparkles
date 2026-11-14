//! A classifier that keeps its state between runs decides the tier of each run from the
//! axioms it holds then. The interner it keeps still holds the expressions of removed
//! axioms, and a self restriction, the top or bottom object property or an at-most-one
//! restriction among them must not keep Tier D from the disjunctions that remain.

use sparkles_owl::classify::{Classification, Classifier, Limits};
use sparkles_owl::model::*;
use sparkles_owl::ofn_read;

const EX: &str = "http://ex.org/";

fn c(s: &str) -> Class {
    Class::new(format!("{EX}{s}"))
}

fn axioms(body: &str) -> Vec<Axiom> {
    ofn_read::parse(&format!(
        "Prefix(:=<{EX}>)\nPrefix(owl:=<http://www.w3.org/2002/07/owl#>)\nOntology(\n{body}\n)"
    ))
    .unwrap()
    .axioms
}

/// The disjunction whose subsumption `B ⊑ E` only case analysis finds.
const CASES: &str = "SubClassOf(:B ObjectUnionOf(:C :D))\nSubClassOf(:C :E)\nSubClassOf(:D :E)";

/// Classifies `held` with a new classifier, the answer a kept one must match.
fn fresh(held: &[Axiom], limits: &Limits) -> Classification {
    Classifier::of(held).classify(limits).unwrap()
}

fn agree(kept: &Classification, held: &[Axiom], limits: &Limits) {
    let full = fresh(held, limits);
    assert!(full.taxonomy.is_subclass(&c("B"), &c("E")));
    assert_eq!(kept.taxonomy, full.taxonomy);
    assert_eq!(kept.consistent, full.consistent);
}

/// Classifies `first` with the disjunction, removes `first` and classifies again with
/// the kept classifier, which must find `B ⊑ E` as a fresh classifier does.
fn removal(first: &str) {
    let gone = axioms(first);
    let cases = axioms(CASES);
    let limits = Limits::default();
    let mut k = Classifier::of(gone.iter().chain(&cases));
    k.classify(&limits).unwrap();
    k.remove(&gone);
    let out = k.classify(&limits).unwrap();
    agree(&out, &cases, &limits);
    assert!(k.excluded().is_empty(), "{:?}", k.excluded());
}

#[test]
fn a_removed_self_restriction_leaves_tier_d_to_the_disjunction() {
    removal("SubClassOf(:A ObjectHasSelf(:r))");
}

#[test]
fn a_removed_top_object_property_leaves_tier_d_to_the_disjunction() {
    removal("SubClassOf(:A ObjectSomeValuesFrom(owl:topObjectProperty :F))");
}

#[test]
fn a_removed_bottom_object_property_leaves_tier_d_to_the_disjunction() {
    removal("SubClassOf(:A ObjectSomeValuesFrom(owl:bottomObjectProperty :F))");
}

/// A run by the EL rules alone removes the self restriction, and a later run adds the
/// disjunction, which Tier D must take.
#[test]
fn a_disjunction_added_after_a_removed_self_restriction_takes_tier_d() {
    let gone = axioms("SubClassOf(:A ObjectHasSelf(:r))\nSubClassOf(:A :G)");
    let cases = axioms(CASES);
    let limits = Limits::default();
    let mut k = Classifier::of(&gone);
    k.classify(&limits).unwrap();
    k.remove(&gone[..1]);
    k.classify(&limits).unwrap();
    k.add(&cases);
    let out = k.classify(&limits).unwrap();
    let held: Vec<Axiom> = gone[1..].iter().chain(&cases).cloned().collect();
    agree(&out, &held, &limits);
}

/// A run by the EL rules alone leaves the at-most restriction and the disjunction out,
/// the restriction is removed, and the next run without that limit must take the
/// disjunction with Tier D.
#[test]
fn a_removed_at_most_restriction_after_an_el_run_leaves_tier_d_to_the_disjunction() {
    let gone = axioms("SubClassOf(:A ObjectMaxCardinality(1 :r :F))");
    let cases = axioms(CASES);
    let mut k = Classifier::of(gone.iter().chain(&cases));
    k.classify(&Limits {
        el_only: true,
        ..Limits::default()
    })
    .unwrap();
    k.remove(&gone);
    let limits = Limits::default();
    let out = k.classify(&limits).unwrap();
    agree(&out, &cases, &limits);
}

/// An added axiom outside OWL 2 EL with a self restriction, whose normalization as a
/// premise of the change interns the restriction, must not keep Tier D from the
/// disjunction added beside it. A fresh classifier leaves that axiom to the rules
/// outside OWL 2 EL.
#[test]
fn an_added_self_restriction_outside_el_leaves_tier_d_to_the_disjunction() {
    let base = axioms("SubClassOf(:X :Y)");
    let added = axioms(&format!(
        "SubClassOf(ObjectHasSelf(:r) ObjectUnionOf(:P :Q))\n{CASES}"
    ));
    let limits = Limits::default();
    let mut k = Classifier::of(&base);
    k.classify(&limits).unwrap();
    k.add(&added);
    let out = k.classify(&limits).unwrap();
    let held: Vec<Axiom> = base.iter().chain(&added).cloned().collect();
    agree(&out, &held, &limits);
}

/// The same addition in a run by the EL rules alone, and a later run without that limit.
#[test]
fn an_added_self_restriction_in_an_el_run_leaves_tier_d_to_the_disjunction() {
    let base = axioms("SubClassOf(:X :Y)");
    let added = axioms(&format!(
        "SubClassOf(ObjectHasSelf(:r) ObjectUnionOf(:P :Q))\n{CASES}"
    ));
    let mut k = Classifier::of(&base);
    k.classify(&Limits::default()).unwrap();
    k.add(&added);
    k.classify(&Limits {
        el_only: true,
        ..Limits::default()
    })
    .unwrap();
    let limits = Limits::default();
    let out = k.classify(&limits).unwrap();
    let held: Vec<Axiom> = base.iter().chain(&added).cloned().collect();
    agree(&out, &held, &limits);
}

/// A run that leaves the disjunction out, since a self restriction that stays keeps Tier D
/// from it, keeps its state when a plain class axiom is removed, and agrees with a fresh
/// classifier.
#[test]
fn a_plain_removal_keeps_the_state_of_a_run_by_the_el_rules() {
    let stays = axioms(&format!("SubClassOf(:A ObjectHasSelf(:r))\n{CASES}"));
    let gone = axioms("SubClassOf(:X ObjectSomeValuesFrom(:s :Y))");
    let limits = Limits::default();
    let mut k = Classifier::of(stays.iter().chain(&gone));
    k.classify(&limits).unwrap();
    k.remove(&gone);
    let out = k.classify(&limits).unwrap();
    assert!(out.stats.incremental);
    let mut f = Classifier::of(&stays);
    let full = f.classify(&limits).unwrap();
    assert_eq!(out.taxonomy, full.taxonomy);
    assert_eq!(k.excluded(), f.excluded());
}

/// A change that touches only plain class axioms keeps the interner's state, and the
/// disjunction still goes to Tier D.
#[test]
fn a_plain_removal_beside_a_disjunction_agrees_with_a_fresh_run() {
    let gone = axioms("SubClassOf(:A ObjectSomeValuesFrom(:r :F))");
    let cases = axioms(CASES);
    let limits = Limits::default();
    let mut k = Classifier::of(gone.iter().chain(&cases));
    k.classify(&limits).unwrap();
    k.remove(&gone);
    let out = k.classify(&limits).unwrap();
    agree(&out, &cases, &limits);
}
