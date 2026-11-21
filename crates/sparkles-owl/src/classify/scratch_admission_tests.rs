//! The invalidation of a run kept by the EL rules admits its scratch before it grows
//! (roadmap item 1256): the lists of parts, of the walk and of the cleared contexts, the
//! normalized premises of the changed axioms and the disjointness maps that kept
//! contexts number again. Each test reads what the last invalidation on its thread
//! admitted and held ([`scratch_audit`]).

use super::*;

fn axioms(body: &str) -> Vec<AxiomKind> {
    crate::ofn_read::parse(&format!("Prefix(:=<urn:scratch:>)\nOntology(\n{body}\n)"))
        .expect("test ontology")
        .axioms
        .into_iter()
        .map(|a| a.kind)
        .collect()
}

fn one_thread() -> Limits {
    Limits {
        threads: 1,
        ..Limits::default()
    }
}

/// Runs `before`, then `after` with the changes between them, and answers the second
/// run's statistics and the invalidation's audit.
fn kept_run(
    before: &[AxiomKind],
    after: &[AxiomKind],
    limits: &Limits,
) -> (Stats, scratch_audit::Audit) {
    let mut sat = Saturation::new();
    let all: Vec<&AxiomKind> = before.iter().collect();
    sat.saturate(&all, None, limits).unwrap();
    let added: Vec<&AxiomKind> = after.iter().filter(|k| !before.contains(k)).collect();
    let removed: Vec<&AxiomKind> = before.iter().filter(|k| !after.contains(k)).collect();
    let all: Vec<&AxiomKind> = after.iter().collect();
    scratch_audit::take();
    let out = sat
        .saturate(
            &all,
            Some(Changes {
                added: &added,
                removed: &removed,
            }),
            limits,
        )
        .unwrap();
    let audit = scratch_audit::take().expect("the run invalidated its kept contexts");
    (out.stats, audit)
}

/// The validation's case: removing `owl:Thing ⊑ C2000 ⊓ … ⊓ C2031` beside 1,024
/// classes clears every context, so the queue of cleared contexts holds more than a
/// thousand ids, which the run's peak counts.
#[test]
fn the_peak_counts_the_lists_of_the_invalidation() {
    let mut body: String = (0..1024)
        .map(|n| format!("Declaration(Class(:C{n}))\n"))
        .collect();
    let conj: String = (2000..2032).map(|n| format!(" :C{n}")).collect();
    body.push_str(&format!(
        "SubClassOf(owl:Thing ObjectIntersectionOf({conj}))\n"
    ));
    let before = axioms(&body);
    let after = before[..before.len() - 1].to_vec();
    let (stats, audit) = kept_run(&before, &after, &one_thread());
    assert!(stats.cleared > 1024, "{stats:?}");
    assert!(audit.live <= audit.lists, "{audit:?}");
    // the queue holds each cleared context once
    assert!(audit.lists >= 4 * stats.cleared, "{audit:?}");
    assert!(
        stats.peak_memory >= audit.held && audit.peak >= audit.held,
        "{stats:?} {audit:?}"
    );
}

/// A changed disjointness set counts its operands among the premises.
#[test]
fn the_premises_count_the_operands_of_a_changed_disjointness_set() {
    let before = axioms("SubClassOf(:A :B)");
    let mut after = before.clone();
    let members: String = (0..600).map(|n| format!(" :D{n}")).collect();
    after.extend(axioms(&format!("DisjointClasses({members})")));
    let (_, audit) = kept_run(&before, &after, &one_thread());
    assert!(audit.premises >= 600 * 4, "{audit:?}");
}

/// An unsatisfiable kept context holds `owl:Nothing` and the subsumer that gave it,
/// whose memberships the saturation never recorded. Numbering its sets again keeps
/// none, so its map does not grow outside the budget. Here 128 classes `A_i` are
/// unsatisfiable, each in a set with `Z` and `owl:Nothing` in sets with `X_i`, and the
/// axioms come in another order, which numbers the sets again without a change.
#[test]
fn renumbered_sets_grow_no_map_of_an_unsatisfiable_context() {
    let mut body = String::new();
    for i in 0..128 {
        body.push_str(&format!(
            "DisjointClasses(owl:Nothing :X{i})\nDisjointClasses(:A{i} :Z)\nSubClassOf(:A{i} owl:Nothing)\n"
        ));
    }
    let before = axioms(&body);
    let mut after = before.clone();
    after.rotate_left(1);
    let (stats, audit) = kept_run(&before, &after, &one_thread());
    assert!(stats.incremental, "{stats:?}");
    assert!(audit.grown <= audit.maps, "{audit:?}");
    assert_eq!(audit.grown, 0, "{audit:?}");
}

/// A kept context that numbers its sets again takes no more entries than it held, so
/// its map grows within the admitted bytes, and the taxonomy is a fresh run's.
#[test]
fn renumbered_sets_of_satisfiable_contexts_stay_within_the_admitted_maps() {
    let mut body = String::new();
    for i in 0..64 {
        body.push_str(&format!(
            "DisjointClasses(:A{i} :B{i})\nSubClassOf(:C{i} :A{i})\nSubClassOf(:C{i} :E{i})\n"
        ));
    }
    let before = axioms(&body);
    let after = before[3..].to_vec();
    let (stats, audit) = kept_run(&before, &after, &one_thread());
    assert!(stats.incremental, "{stats:?}");
    assert!(audit.grown <= audit.maps, "{audit:?}");
    let all: Vec<&AxiomKind> = after.iter().collect();
    let fresh = Saturation::new()
        .classify(&all, None, &one_thread())
        .unwrap();
    let mut sat = Saturation::new();
    let first: Vec<&AxiomKind> = before.iter().collect();
    sat.classify(&first, None, &one_thread()).unwrap();
    let removed: Vec<&AxiomKind> = before[..3].iter().collect();
    let kept = sat
        .classify(
            &all,
            Some(Changes {
                added: &[],
                removed: &removed,
            }),
            &one_thread(),
        )
        .unwrap();
    assert_eq!(kept.taxonomy, fresh.taxonomy);
}
