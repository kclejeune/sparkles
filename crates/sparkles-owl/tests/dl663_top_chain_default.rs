//! Public default-dispatch checks for named domains of Top-bearing chains (item 663).
//! Inputs 01–09 reproduce the retained oracle packet. 10 and 11 reproduce the
//! reflexive and direct-bottom inputs. 12 has real prefix edges and no matching head.

use sparkles_owl::classify::{
    Changes, Classification, Classifier, Consistency, Description, Error, Excluded, Gap, Limits,
    Saturation, Tier,
};
use sparkles_owl::model::{Axiom, AxiomKind, Class, NamedIndividual, SubObjectPropertyExpression};
use sparkles_owl::ofn_read;
use std::time::Duration;

const ALL_TOP: &str = include_str!("fixtures/dl663_top_chain_default/01-all-top.ofn");
const ALL_TOP_CLASH: &str = include_str!("fixtures/dl663_top_chain_default/02-all-top-clash.ofn");
const SUPERROLES: &str =
    include_str!("fixtures/dl663_top_chain_default/03-trailing-superroles.ofn");
const ABOX: &str = include_str!("fixtures/dl663_top_chain_default/04-trailing-abox.ofn");
const CONDITIONAL: &str =
    include_str!("fixtures/dl663_top_chain_default/05-trailing-conditional.ofn");
const ORDINARY_VACUOUS: &str =
    include_str!("fixtures/dl663_top_chain_default/06-ordinary-vacuous.ofn");
const ORDINARY: &str = include_str!("fixtures/dl663_top_chain_default/07-ordinary-only.ofn");
const VACUOUS: &str = include_str!("fixtures/dl663_top_chain_default/08-vacuous-only.ofn");
const TAUTOLOGY: &str = include_str!("fixtures/dl663_top_chain_default/09-tautology-only.ofn");
const REFLEXIVE: &str = include_str!("fixtures/dl663_top_chain_default/10-reflexive-prefix.ofn");
const DIRECT_BOTTOM: &str =
    include_str!("fixtures/dl663_top_chain_default/11-direct-bottom-domain.ofn");
const NO_MATCH: &str = include_str!("fixtures/dl663_top_chain_default/12-no-match.ofn");

// The partial reason is public through Gap::Partial. The normalizer's constant is private.
const TOP_CHAIN: &str = "property chains through the top object property";

fn parse(input: &str) -> Vec<Axiom> {
    ofn_read::parse(input).unwrap().axioms
}

fn kinds(axioms: &[Axiom]) -> Vec<&AxiomKind> {
    axioms.iter().map(|a| &a.kind).collect()
}

fn class(local: &str) -> Class {
    Class::new(format!("urn:top-domain:{local}"))
}

fn individual(local: &str) -> NamedIndividual {
    NamedIndividual::new(format!("urn:top-domain:{local}"))
}

fn chain_gap(axiom: &Axiom) -> Excluded {
    assert!(matches!(
        axiom.kind,
        AxiomKind::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(_), _)
    ));
    Excluded {
        axiom: axiom.kind.clone(),
        gap: Gap::Partial(TOP_CHAIN),
    }
}

fn check_orders(
    input: &str,
    partial_chain: usize,
    mut check: impl FnMut(&[Axiom], &[Excluded], &Limits),
) {
    let mut axioms = parse(input);
    let want = [chain_gap(&axioms[partial_chain])];
    check(&axioms, &want, &Limits::default());
    for reversed in [false, true] {
        if reversed {
            axioms.reverse();
        }
        for threads in [1, 4] {
            check(
                &axioms,
                &want,
                &Limits {
                    threads,
                    ..Limits::default()
                },
            );
        }
    }
}

fn assert_partial(out: &Classification, excluded: &[Excluded], want: &[Excluded]) {
    assert_eq!(excluded, want);
    assert_eq!(out.stats.tier, Tier::El);
    assert!(out.complete_for_taken);
    assert!(!out.complete);
}

fn assert_isolated_classes(out: &Classification) {
    assert!(out.consistent);
    assert!(!out.taxonomy.is_unsatisfiable(&class("A")));
    assert!(!out.taxonomy.is_unsatisfiable(&class("C")));
    assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
    assert!(!out.taxonomy.is_subclass(&class("C"), &class("D")));
}

fn assert_abox(description: Description, marker: &str) {
    assert!(description.instances.contains(&individual("a")), "{marker}");
    assert!(description.satisfiable);
    assert_eq!(description.instances, [individual("a")]);
}

#[test]
fn default_all_top_borrowed_classify() {
    check_orders(ALL_TOP, 0, |axioms, want, limits| {
        let mut sat = Saturation::new();
        let out = sat.classify(&kinds(axioms), None, limits).unwrap();
        assert!(
            out.taxonomy.is_subclass(&Class::thing(), &class("D")),
            "dl663 missing: default all-Top Thing subclass of D"
        );
        assert!(out.consistent);
        let d = sat.describe(&class("D")).unwrap();
        assert!(d.satisfiable && d.instances.is_empty());
        assert!(d.equivalents.contains(&Class::thing()));
        assert_partial(&out, sat.excluded(), want);
        let strict = Limits {
            strict: true,
            ..limits.clone()
        };
        assert_strict_refusal(
            sat.classify(&kinds(axioms), None, &strict).unwrap_err(),
            want,
        );
    });
    eprintln!("dl663 ok: default all-Top Thing subclass of D");
}

#[test]
fn default_all_top_clash_borrowed_classify() {
    check_orders(ALL_TOP_CLASH, 0, |axioms, want, limits| {
        let mut sat = Saturation::new();
        let out = sat.classify(&kinds(axioms), None, limits).unwrap();
        assert_eq!(
            out.consistency,
            Consistency::Inconsistent,
            "dl663 missing: default all-Top clash"
        );
        assert!(!out.consistent);
        assert!(out.taxonomy.is_unsatisfiable(&Class::thing()));
        assert_partial(&out, sat.excluded(), want);
    });
    eprintln!("dl663 ok: default all-Top clash");
}

#[test]
fn default_trailing_superroles_owned_classify() {
    check_orders(SUPERROLES, 0, |axioms, want, limits| {
        let mut classifier = Classifier::of(axioms);
        let out = classifier.classify(limits).unwrap();
        assert!(
            out.taxonomy.is_subclass(&class("A"), &class("D")),
            "dl663 missing: default trailing superroles A subclass of D"
        );
        assert_isolated_classes(&out);
        assert_partial(&out, &classifier.excluded(), want);
    });
    eprintln!("dl663 ok: default trailing superroles A subclass of D");
}

#[test]
fn default_trailing_abox_borrowed_saturate() {
    check_orders(ABOX, 0, |axioms, want, limits| {
        let mut sat = Saturation::new();
        let out = sat.saturate(&kinds(axioms), None, limits).unwrap();
        assert_abox(
            sat.describe(&class("D")).unwrap(),
            "dl663 missing: default borrowed trailing D(a)",
        );
        assert_eq!(
            sat.describe(&class("C")).unwrap().instances,
            [individual("c")]
        );
        assert!(out.consistent);
        assert_eq!(out.stats.tier, Tier::El);
        assert_eq!(sat.excluded(), want);
    });
    eprintln!("dl663 ok: default borrowed trailing D(a)");
}

#[test]
fn default_trailing_abox_owned_saturate() {
    check_orders(ABOX, 0, |axioms, want, limits| {
        let mut classifier = Classifier::of(axioms);
        let out = classifier.saturate(limits).unwrap();
        assert_abox(
            classifier.describe(&class("D")).unwrap(),
            "dl663 missing: default owned trailing D(a)",
        );
        assert_eq!(
            classifier.describe(&class("C")).unwrap().instances,
            [individual("c")]
        );
        assert!(out.consistent);
        assert_eq!(out.stats.tier, Tier::El);
        assert_eq!(classifier.excluded(), want);
    });
    eprintln!("dl663 ok: default owned trailing D(a)");
}

#[test]
fn default_trailing_conditional_owned_classify() {
    check_orders(CONDITIONAL, 0, |axioms, want, limits| {
        let mut classifier = Classifier::of(axioms);
        let out = classifier.classify(limits).unwrap();
        assert!(
            out.taxonomy.is_subclass(&class("A"), &class("D")),
            "dl663 missing: default conditional A subclass of D"
        );
        assert!(out.taxonomy.is_subclass(&class("Q"), &class("D")));
        assert_isolated_classes(&out);
        assert!(
            classifier
                .describe(&class("D"))
                .unwrap()
                .instances
                .is_empty()
        );
        assert_partial(&out, &classifier.excluded(), want);
    });
    eprintln!("dl663 ok: default conditional A and Q subclass of D without D(a)");
}

#[test]
fn default_direct_bottom_domain_owned_saturate() {
    check_orders(DIRECT_BOTTOM, 1, |axioms, want, limits| {
        let mut classifier = Classifier::of(axioms);
        let out = classifier.saturate(limits).unwrap();
        assert!(
            !out.consistent,
            "dl663 missing: default direct bottom-domain clash"
        );
        assert!(!classifier.describe(&Class::thing()).unwrap().satisfiable);
        assert_eq!(out.stats.tier, Tier::El);
        assert_eq!(classifier.excluded(), want);
    });
    eprintln!("dl663 ok: default direct bottom-domain clash");
}

#[test]
fn reflexive_prefix_is_a_legacy_control() {
    check_orders(REFLEXIVE, 5, |axioms, want, limits| {
        let mut sat = Saturation::new();
        let kinds = kinds(axioms);
        let default = sat.classify(&kinds, None, limits).unwrap();
        assert_partial(&default, sat.excluded(), want);
        let el_only = sat
            .classify(
                &kinds,
                None,
                &Limits {
                    el_only: true,
                    ..limits.clone()
                },
            )
            .unwrap();
        assert_eq!(default.taxonomy, el_only.taxonomy);
        assert!(default.consistent && el_only.consistent);
        assert!(default.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(default.taxonomy.is_subclass(&class("C"), &class("D")));
        assert_partial(&el_only, sat.excluded(), want);
    });
    eprintln!("dl663 ok: reflexive prefix in default and EL-only");
}

#[test]
fn explicit_el_only_retains_legacy_answers() {
    let limits = Limits {
        el_only: true,
        ..Limits::default()
    };
    for (input, chain, absent_subclass) in [
        (ALL_TOP, 0, Class::thing()),
        (SUPERROLES, 0, class("A")),
        (CONDITIONAL, 0, class("A")),
    ] {
        let axioms = parse(input);
        let mut classifier = Classifier::of(&axioms);
        let out = classifier.classify(&limits).unwrap();
        assert!(out.consistent);
        assert!(!out.taxonomy.is_unsatisfiable(&absent_subclass));
        assert!(!out.taxonomy.is_subclass(&absent_subclass, &class("D")));
        assert!(
            classifier
                .describe(&class("D"))
                .unwrap()
                .instances
                .is_empty()
        );
        assert_partial(&out, &classifier.excluded(), &[chain_gap(&axioms[chain])]);
    }
    for (input, chain) in [(ALL_TOP_CLASH, 0), (DIRECT_BOTTOM, 1)] {
        let axioms = parse(input);
        let mut sat = Saturation::new();
        let out = sat.classify(&kinds(&axioms), None, &limits).unwrap();
        assert!(out.consistent);
        assert_ne!(out.consistency, Consistency::Inconsistent);
        assert_partial(&out, sat.excluded(), &[chain_gap(&axioms[chain])]);
    }
    let axioms = parse(ABOX);
    let mut sat = Saturation::new();
    assert!(
        sat.saturate(&kinds(&axioms), None, &limits)
            .unwrap()
            .consistent
    );
    assert!(sat.describe(&class("D")).unwrap().instances.is_empty());
    assert_eq!(
        sat.describe(&class("C")).unwrap().instances,
        [individual("c")]
    );
    assert_eq!(sat.excluded(), [chain_gap(&axioms[0])]);
    eprintln!("dl663 ok: explicit EL-only legacy class, individual and clash answers");
}

#[test]
fn ordinary_vacuous_and_tautological_defaults() {
    for (input, partial_chain, ordinary) in [
        (ORDINARY_VACUOUS, Some(3), true),
        (ORDINARY, None, true),
        (VACUOUS, Some(0), false),
        (TAUTOLOGY, None, false),
    ] {
        let axioms = parse(input);
        let mut sat = Saturation::new();
        let out = sat
            .classify(&kinds(&axioms), None, &Limits::default())
            .unwrap();
        assert!(out.consistent);
        assert!(!out.taxonomy.is_unsatisfiable(&class("C")));
        assert!(!out.taxonomy.is_subclass(&Class::thing(), &class("D")));
        assert!(!out.taxonomy.is_subclass(&class("C"), &class("D")));
        if ordinary {
            assert!(out.taxonomy.is_subclass(&class("A"), &class("D")));
            assert!(!out.taxonomy.is_unsatisfiable(&class("A")));
        }
        if let Some(chain) = partial_chain {
            assert_partial(&out, sat.excluded(), &[chain_gap(&axioms[chain])]);
        } else {
            assert!(sat.excluded().is_empty());
            assert!(out.complete && out.complete_for_taken && out.incomplete.is_empty());
        }
    }
    eprintln!("dl663 ok: ordinary, vacuous and tautological defaults");
}

#[test]
fn default_without_matching_domain_retains_prefix_answers() {
    let axioms = parse(NO_MATCH);
    let mut classifier = Classifier::of(&axioms);
    let out = classifier.classify(&Limits::default()).unwrap();
    assert_isolated_classes(&out);
    assert!(!out.taxonomy.is_subclass(&class("A"), &class("D")));
    assert!(out.taxonomy.is_subclass(&class("A"), &class("HasPrefix")));
    assert!(
        classifier
            .describe(&class("D"))
            .unwrap()
            .instances
            .is_empty()
    );
    assert_eq!(
        classifier.describe(&class("HasPrefix")).unwrap().instances,
        [individual("a")]
    );
    assert_partial(&out, &classifier.excluded(), &[chain_gap(&axioms[0])]);
    eprintln!("dl663 ok: default without matching domain retains prefix answers");
}

fn assert_strict_refusal(error: Error, want: &[Excluded]) {
    let Error::Incomplete(excluded) = error else {
        panic!("expected strict refusal, got {error:?}");
    };
    assert_eq!(excluded.as_slice(), want);
}

#[test]
fn strict_and_raw_keep_sorted_original_partial_gaps() {
    let superroles = parse(SUPERROLES);
    let vacuous = parse(VACUOUS);
    let mut chains = [&superroles[0], &vacuous[0]];
    chains.sort_by(|a, b| b.kind.cmp(&a.kind));
    // The two chains share a head. Supply them in reverse diagnostic order and
    // repeat one of the original chains.
    let input: Vec<_> = chains
        .into_iter()
        .chain([chains[0]])
        .chain(superroles[1..].iter())
        .collect();
    let kinds: Vec<_> = input.iter().map(|a| &a.kind).collect();
    let want = [chain_gap(chains[1]), chain_gap(chains[0])];
    let limits = Limits {
        strict: true,
        ..Limits::default()
    };

    let mut sat = Saturation::new();
    assert_strict_refusal(sat.classify(&kinds, None, &limits).unwrap_err(), &want);
    assert_eq!(sat.excluded(), want);
    let raw = sat.saturate(&kinds, None, &limits).unwrap();
    assert!(raw.consistent);
    assert_eq!(raw.stats.coverage, Duration::ZERO);
    assert!(sat.describe(&class("A")).unwrap().satisfiable);
    assert_eq!(sat.excluded(), want);

    let mut classifier = Classifier::of(input);
    assert_strict_refusal(classifier.classify(&limits).unwrap_err(), &want);
    assert_eq!(classifier.excluded(), want);
    let raw = classifier.saturate(&limits).unwrap();
    assert!(raw.consistent);
    assert_eq!(raw.stats.coverage, Duration::ZERO);
    assert!(classifier.describe(&class("A")).unwrap().satisfiable);
    assert_eq!(classifier.excluded(), want);
    eprintln!("dl663 ok: strict refusal and raw saturation retain sorted original gaps");
}

#[test]
fn coverage_modes_keep_the_taxonomy_and_original_gap() {
    let axioms = parse(SUPERROLES);
    let kinds = kinds(&axioms);
    let want = [chain_gap(&axioms[0])];
    let mut sat = Saturation::new();
    let limits = Limits::default();
    let full = sat.classify(&kinds, None, &limits).unwrap();
    assert_partial(&full, sat.excluded(), &want);
    assert_eq!(full.incomplete, [class("A")]);
    assert_eq!(full.consistency, Consistency::Consistent);
    assert!(!full.stats.coverage_over);

    for limits in [
        Limits {
            coverage: false,
            ..Limits::default()
        },
        Limits {
            coverage_work: Some(0),
            ..Limits::default()
        },
    ] {
        let out = sat.classify(&kinds, None, &limits).unwrap();
        assert_partial(&out, sat.excluded(), &want);
        assert_eq!(out.taxonomy, full.taxonomy);
        assert_eq!(
            out.incomplete,
            [class("A"), class("B"), class("C"), class("D")]
        );
        assert_eq!(out.consistency, Consistency::Unknown);
        assert_eq!(out.stats.coverage_over, limits.coverage_work.is_some());
    }

    let mut deferred = sat
        .classify(
            &kinds,
            None,
            &Limits {
                cover_later: true,
                ..Limits::default()
            },
        )
        .unwrap();
    assert!(deferred.complete && deferred.complete_for_taken && deferred.incomplete.is_empty());
    assert_eq!(deferred.stats.coverage, Duration::ZERO);
    assert_eq!(sat.excluded(), want);
    assert!(
        deferred
            .cover(&kinds, sat.excluded(), None, &limits)
            .is_none()
    );
    assert_partial(&deferred, sat.excluded(), &want);
    assert_eq!(deferred.taxonomy, full.taxonomy);
    assert_eq!(deferred.incomplete, full.incomplete);
    assert_eq!(deferred.consistency, full.consistency);
    eprintln!("dl663 ok: full, disabled, bounded and deferred coverage");
}

#[test]
fn borrowed_all_top_can_switch_default_and_el_only() {
    let axioms = parse(ALL_TOP);
    let kinds = kinds(&axioms);
    let want = [chain_gap(&axioms[0])];
    let mut sat = Saturation::new();
    let mut first_default = None;
    for (run, el_only) in [false, true, false].into_iter().enumerate() {
        let limits = if el_only {
            Limits {
                el_only: true,
                ..Limits::default()
            }
        } else {
            Limits::default()
        };
        let changes = (run > 0).then_some(Changes::default());
        let out = sat.classify(&kinds, changes, &limits).unwrap();
        let entails = out.taxonomy.is_subclass(&Class::thing(), &class("D"));
        if el_only {
            assert!(!entails, "EL-only retained Thing subclass of D");
        } else {
            assert!(
                entails,
                "dl663 missing: borrowed default after mode selection"
            );
            if let Some(taxonomy) = &first_default {
                assert_eq!(&out.taxonomy, taxonomy);
            } else {
                first_default = Some(out.taxonomy.clone());
            }
        }
        assert_partial(&out, sat.excluded(), &want);
    }
    eprintln!("dl663 ok: borrowed all-Top default to EL-only to default");
}

#[test]
fn owned_abox_can_switch_default_and_el_only() {
    let axioms = parse(ABOX);
    let mut classifier = Classifier::of(&axioms);
    let default = Limits::default();
    let el_only = Limits {
        el_only: true,
        ..Limits::default()
    };
    for (limits, entails) in [(&default, true), (&el_only, false), (&default, true)] {
        assert!(classifier.saturate(limits).unwrap().consistent);
        let d = classifier.describe(&class("D")).unwrap();
        if entails {
            assert_abox(d, "dl663 missing: owned default D(a) after mode selection");
        } else {
            assert!(d.instances.is_empty(), "EL-only retained D(a)");
        }
        assert_eq!(classifier.excluded(), [chain_gap(&axioms[0])]);
    }
    eprintln!("dl663 ok: owned ABox default to EL-only to default");
}

#[test]
fn owned_edits_install_and_remove_the_last_matching_domain() {
    let base = parse(NO_MATCH);
    let trailing_abox = parse(ABOX);
    let domain = &trailing_abox[1];
    let edge = &base[4];
    let limits = Limits::default();
    let mut classifier = Classifier::of(&base);
    let check = |classifier: &mut Classifier, domain_present: bool, edge_present: bool| {
        let out = classifier.classify(&limits).unwrap();
        let d = classifier.describe(&class("D")).unwrap();
        if !domain_present {
            assert!(!out.taxonomy.is_subclass(&class("A"), &class("D")));
        } else {
            assert!(
                out.taxonomy.is_subclass(&class("A"), &class("D")),
                "dl663 missing: owned default after adding the matching domain"
            );
        }
        if domain_present && edge_present {
            assert_eq!(d.instances, [individual("a")]);
        } else {
            assert!(d.instances.is_empty());
        }
        let mut fresh = Classifier::of(base.iter().filter(|a| edge_present || *a != edge));
        if domain_present {
            fresh.add([domain]);
        }
        let expected = fresh.classify(&limits).unwrap();
        assert_eq!(out.taxonomy, expected.taxonomy);
        assert_eq!(out.consistency, expected.consistency);
        assert_eq!(out.incomplete, expected.incomplete);
        assert_eq!(Some(d), fresh.describe(&class("D")));
        assert_partial(&out, &classifier.excluded(), &[chain_gap(&base[0])]);
        assert_eq!(classifier.excluded(), fresh.excluded());
    };
    check(&mut classifier, false, true);
    classifier.add([domain]);
    assert!(classifier.describe(&class("D")).is_none());
    check(&mut classifier, true, true);
    classifier.remove([edge]);
    assert!(classifier.describe(&class("D")).is_none());
    check(&mut classifier, true, false);
    classifier.add([edge]);
    check(&mut classifier, true, true);
    classifier.remove([domain]);
    check(&mut classifier, false, true);
    eprintln!("dl663 ok: owned domain and edge edits agree with fresh answers");
}

#[test]
fn cancelled_owned_edit_hides_descriptions_until_retry() {
    let axioms = parse(NO_MATCH);
    let abox = parse(ABOX);
    let assertion = &abox[3];
    let limits = Limits::default();
    let mut classifier = Classifier::of(&axioms);
    classifier.saturate(&limits).unwrap();
    assert!(
        classifier
            .describe(&class("C"))
            .unwrap()
            .instances
            .is_empty()
    );
    classifier.add([assertion]);
    assert!(classifier.describe(&class("C")).is_none());
    let cancelled = Limits {
        cancelled: Some(std::sync::Arc::new(|| true)),
        partial: true,
        ..Limits::default()
    };
    assert!(matches!(
        classifier.saturate(&cancelled),
        Err(Error::Cancelled)
    ));
    assert!(classifier.describe(&class("C")).is_none());
    classifier.saturate(&limits).unwrap();
    assert_eq!(
        classifier.describe(&class("C")).unwrap().instances,
        [individual("c")]
    );
    assert!(
        classifier
            .describe(&class("D"))
            .unwrap()
            .instances
            .is_empty()
    );
    assert_eq!(classifier.excluded(), [chain_gap(&axioms[0])]);
    eprintln!("dl663 ok: cancelled owned edit hides descriptions until retry");
}
