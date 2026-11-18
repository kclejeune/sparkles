//! Why-not answers and demand explanations through relations (item 839's plan, Phase 4,
//! §20.3, §20.4 and §21).

use super::demand::{Source, reasoner};
use super::maintain::chain_and_key_rules;
use super::*;
use crate::explain::{DemandInput, Derivation, ExplainOptions, Explained, explain_demand};
use crate::incremental::tests::{Vocab as TV, set_default_graph};
use crate::whynot::{WhyNot, WhyNotOptions, why_not};
use oxrdf::Term;
use rustc_hash::FxHashSet;
use std::sync::Arc;

type Triple = [u64; 3];

fn term(v: &TV, x: u64) -> Term {
    Terms::new(v.store.snapshot()).term(x).unwrap()
}

fn triple(v: &TV, t: Triple) -> [Term; 3] {
    [term(v, t[0]), term(v, t[1]), term(v, t[2])]
}

fn ask(v: &TV, source: &Source, base: &FxHashSet<Triple>, t: Triple) -> WhyNot {
    let snap = set_default_graph(v, base);
    let qr = reasoner(source);
    why_not(
        &DemandInput {
            reasoner: &qr,
            snap,
            readable: None,
        },
        &triple(v, t),
        &WhyNotOptions::default(),
    )
    .unwrap()
}

fn walk<'d>(d: &'d Derivation, out: &mut Vec<&'d Derivation>) {
    out.push(d);
    for p in &d.premises {
        walk(p, out);
    }
}

/// Why not a chain's triple whose fifth link is missing: the one hypothesis is that
/// link, a triple, verified with relation nodes in its tree. With the first link missing,
/// the unfolding stops at its depth before it reaches the link, and the answer is
/// incomplete and holds no relation fact.
#[test]
fn why_not_through_a_chain_names_only_triples() {
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let source: Source = Arc::new(move |_: &Terms| chain_and_key_rules(&vv));
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let goal = [ind(0), v.props[2], ind(6)];
    for gap in [4usize, 0] {
        let base: FxHashSet<Triple> = (0..6)
            .filter(|&i| i != gap)
            .map(|i| [ind(i), p(i), ind(i + 1)])
            .collect();
        let WhyNot::NotEntailed {
            hypotheses,
            complete,
            rules,
        } = ask(&v, &source, &base, goal)
        else {
            panic!("entailed")
        };
        assert!(rules.iter().any(|c| c.rule.name == "chain-end"));
        for h in &hypotheses {
            for f in &h.missing {
                assert!(matches!(f[1], Term::NamedNode(_)), "{f:?}");
            }
        }
        if gap == 4 {
            let link = triple(&v, [ind(4), p(4), ind(5)]);
            let h = hypotheses
                .iter()
                .find(|h| h.missing == vec![link.clone()])
                .unwrap_or_else(|| panic!("{hypotheses:?}"));
            let mut nodes = Vec::new();
            walk(&h.tree, &mut nodes);
            assert!(
                nodes
                    .iter()
                    .any(|d| d.relation.as_ref().is_some_and(|r| r.name == "chain"))
            );
            assert!(nodes.iter().any(|d| d.missing && d.triple == link));
        } else {
            assert!(!complete, "the first link lies past the unfolding depth");
        }
    }
}

/// A negated relation atom that blocks an instance is named as the rule wrote it, with
/// no fact, since a relation fact has no RDF form.
#[test]
fn a_negated_relation_blocks_by_its_name() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p2) = (v.props[0], v.props[1], v.props[2]);
    let source: Source = Arc::new(move |_: &Terms| {
        let (rels, ids) = rels(&[1]);
        let rules = vec![
            rule(
                "r",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(rel(ids[0], &[V(0)]))],
            ),
            rule(
                "q",
                vec![pos(tri(V(0), p1, V(1))), neg(rel(ids[0], &[V(0)]))],
                vec![head(tri(V(0), p2, V(1)))],
            ),
        ];
        (rels, rules)
    });
    let base: FxHashSet<Triple> = [[ind(0), p0, ind(1)], [ind(0), p1, ind(1)]]
        .into_iter()
        .collect();
    let WhyNot::NotEntailed { rules, .. } = ask(&v, &source, &base, [ind(0), p2, ind(1)]) else {
        panic!("entailed")
    };
    let q = rules.iter().find(|c| c.rule.name == "q").unwrap();
    assert_eq!(q.blocked_by.len(), 1, "{:?}", q.blocked_by);
    assert_eq!(
        q.blocked_by[0].call,
        format!("not r0({})", term(&v, ind(0)))
    );
    assert_eq!(q.blocked_by[0].fact, None);
}

/// A relation beside a rule whose head has a variable predicate: the unfolding of a
/// missing relation fact does not take that head, so the answer gives the RDF support of
/// the relation without an unknown term.
#[test]
fn a_wildcard_head_does_not_unfold_a_relation_fact() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p2, declared, source_p) = (v.props[0], v.props[2], v.props[3], v.props[4]);
    let source: Source = Arc::new(move |_: &Terms| {
        let (rels, ids) = rels(&[2]);
        let rules = vec![
            rule(
                "r",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(rel(ids[0], &[V(0), V(1)]))],
            ),
            rule(
                "q",
                vec![pos(rel(ids[0], &[V(0), V(1)]))],
                vec![head(tri(V(0), p2, V(1)))],
            ),
            rule(
                "wild",
                vec![
                    pos(IAtom::triple(V(0), C(declared), V(1))),
                    pos(tri(V(2), source_p, V(1))),
                ],
                vec![head(IAtom::triple(V(2), V(0), V(1)))],
            ),
        ];
        (rels, rules)
    });
    let base = FxHashSet::default();
    let WhyNot::NotEntailed { hypotheses, .. } = ask(&v, &source, &base, [ind(0), p2, ind(1)])
    else {
        panic!("entailed")
    };
    let support = triple(&v, [ind(0), p0, ind(1)]);
    assert!(
        hypotheses
            .iter()
            .any(|h| h.missing == vec![support.clone()]),
        "{hypotheses:?}"
    );
}

/// A rule that gives a relation fact with a rule-local blank node in its tail makes
/// terms, so its hypotheses are not verified and the answer is incomplete, as for a
/// binary relation, whose blank node is in the lowered head itself.
#[test]
fn a_blank_node_in_a_tuple_head_leaves_the_answer_incomplete() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p2) = (v.props[0], v.props[2]);
    let i0 = ind(0);
    for arity in [2u16, 3] {
        let source: Source = Arc::new(move |t: &Terms| {
            let blank = t
                .id_for_rule(&Term::BlankNode(oxrdf::BlankNode::new_unchecked("b")))
                .unwrap();
            assert!(t.is_rule_bnode(blank));
            let (rels, ids) = rels(&[arity]);
            let cols: Vec<Slot> = if arity == 2 {
                vec![V(0), C(blank)]
            } else {
                vec![V(0), C(i0), C(blank)]
            };
            let rules = vec![rule(
                "both",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(rel(ids[0], &cols)), head(tri(V(0), p2, V(1)))],
            )];
            (rels, rules)
        });
        let base = FxHashSet::default();
        let WhyNot::NotEntailed { complete, .. } = ask(&v, &source, &base, [ind(1), p2, ind(2)])
        else {
            panic!("entailed")
        };
        assert!(!complete, "arity {arity}");
    }
}

/// A placeholder that a hypothesis names is renamed in a relation node's columns too.
#[test]
fn renaming_reaches_relation_columns() {
    use sparkles_core::explain::RelationAtom;
    let from = Term::BlankNode(oxrdf::BlankNode::new_unchecked("verify1"));
    let to = Term::BlankNode(oxrdf::BlankNode::new_unchecked("y1"));
    let mut d = Derivation::asserted([from.clone(), from.clone(), from.clone()], None);
    d.relation = Some(RelationAtom {
        name: "r".into(),
        args: vec![from.clone(), to.clone()],
    });
    crate::whynot::rename_for_tests(&mut d, &[(from, to.clone())]);
    assert_eq!(d.relation.unwrap().args, vec![to.clone(), to]);
}

/// A demand explanation through the chain shows the relation nodes that the explanation
/// of the materialized closure shows (Phase 3's `explain_program`).
#[test]
fn a_demand_explanation_shows_relation_nodes() {
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let source: Source = Arc::new(move |_: &Terms| chain_and_key_rules(&vv));
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let base: FxHashSet<Triple> = (0..6).map(|i| [ind(i), p(i), ind(i + 1)]).collect();
    let snap = set_default_graph(&v, &base);
    let qr = reasoner(&source);
    let found = explain_demand(
        &DemandInput {
            reasoner: &qr,
            snap,
            readable: None,
        },
        &triple(&v, [ind(0), v.props[2], ind(6)]),
        &ExplainOptions::default(),
    )
    .unwrap();
    let Explained::Found { trees, .. } = found else {
        panic!("{found:?}")
    };
    let tree = &trees[0];
    assert_eq!(tree.rule.as_ref().unwrap().name, "chain-end");
    let mut nodes = Vec::new();
    walk(tree, &mut nodes);
    let chain: Vec<&&Derivation> = nodes
        .iter()
        .filter(|d| d.relation.as_ref().is_some_and(|r| r.name == "chain"))
        .collect();
    assert_eq!(chain.len(), 6, "one node per chain step");
    for d in &nodes {
        assert!(d.builtins.is_empty(), "{:?}", d.builtins);
    }
    let leaves: BTreeSet<String> = nodes
        .iter()
        .filter(|d| d.premises.is_empty() && d.relation.is_none())
        .map(|d| format!("{:?}", d.triple))
        .collect();
    let want: BTreeSet<String> = (0..6)
        .map(|i| format!("{:?}", triple(&v, [ind(i), p(i), ind(i + 1)])))
        .collect();
    assert_eq!(leaves, want);
}

/// `R(x, y, z), S(k, y, z) → (x out k)`, in both body orders, with `R` missing and `S`
/// holding: the columns that `S`'s tail binds make the missing `R` ground, so that it is
/// unfolded to the triple that gives it.
#[test]
fn a_missing_tail_waits_for_a_later_binder() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, q, out) = (v.props[0], v.props[1], v.props[2]);
    let c = ind(7);
    for r_first in [true, false] {
        let source: Source = Arc::new(move |_: &Terms| {
            let (rels, ids) = rels(&[3, 3]);
            let (r, s) = (ids[0], ids[1]);
            let ra = pos(rel(r, &[V(0), V(1), V(2)]));
            let sa = pos(rel(s, &[V(3), V(1), V(2)]));
            let body = if r_first { vec![ra, sa] } else { vec![sa, ra] };
            let rules = vec![
                rule(
                    "r",
                    vec![pos(tri(V(0), p0, V(1)))],
                    vec![head(rel(r, &[V(0), V(1), C(c)]))],
                ),
                rule(
                    "s",
                    vec![pos(tri(V(0), q, V(1)))],
                    vec![head(rel(s, &[V(0), V(1), C(c)]))],
                ),
                rule("out", body, vec![head(tri(V(0), out, V(3)))]),
            ];
            (rels, rules)
        });
        let base: FxHashSet<Triple> = [[ind(3), q, ind(1)]].into_iter().collect();
        let WhyNot::NotEntailed { hypotheses, .. } = ask(&v, &source, &base, [ind(0), out, ind(3)])
        else {
            panic!("entailed")
        };
        let support = triple(&v, [ind(0), p0, ind(1)]);
        assert!(
            hypotheses
                .iter()
                .any(|h| h.missing == vec![support.clone()]),
            "r first {r_first}: {hypotheses:?}"
        );
    }
}

/// A negated atom with a wildcard that an existing fact blocks is reported, for a binary
/// relation, a relation of arity 3 and a triple.
#[test]
fn a_wildcard_negation_is_a_blocker() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p2, p3) = (v.props[0], v.props[1], v.props[2], v.props[3]);
    for arity in [0u16, 2, 3] {
        let source: Source = Arc::new(move |_: &Terms| {
            let (rels, ids) = rels(&[arity.max(2)]);
            let (given, negated) = match arity {
                0 => (tri(V(0), p3, V(1)), tri(V(0), p3, Any)),
                2 => (rel(ids[0], &[V(0), V(1)]), rel(ids[0], &[V(0), Any])),
                _ => (
                    rel(ids[0], &[V(0), V(1), V(1)]),
                    rel(ids[0], &[V(0), Any, V(2)]),
                ),
            };
            let mut body = vec![pos(tri(V(0), p1, V(2))), neg(negated)];
            if arity != 3 {
                body[0] = pos(tri(V(0), p1, V(1)));
            }
            let rules = vec![
                rule("r", vec![pos(tri(V(0), p0, V(1)))], vec![head(given)]),
                rule("q", body, vec![head(tri(V(0), p2, V(0)))]),
            ];
            (rels, rules)
        });
        let base: FxHashSet<Triple> = [[ind(0), p0, ind(1)], [ind(0), p1, ind(1)]]
            .into_iter()
            .collect();
        let WhyNot::NotEntailed { rules, .. } = ask(&v, &source, &base, [ind(0), p2, ind(0)])
        else {
            panic!("entailed")
        };
        let q = rules.iter().find(|c| c.rule.name == "q").unwrap();
        let a = term(&v, ind(0));
        let want = match arity {
            0 => format!("not ({a} {} _)", term(&v, p3)),
            2 => format!("not r0({a}, _)"),
            _ => format!("not r0({a}, _, {})", term(&v, ind(1))),
        };
        assert!(
            q.blocked_by
                .iter()
                .any(|b| b.call == want && b.fact.is_none()),
            "arity {arity}: {:?}",
            q.blocked_by
        );
    }
}

/// A negated triple's blocker keeps a literal's comma.
#[test]
fn a_negated_triple_blocker_keeps_its_literal() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p2) = (v.props[0], v.props[1], v.props[2]);
    let lit = {
        let key =
            sparkles_core::id::term_key(&Term::Literal(oxrdf::Literal::new_simple_literal("a, b")));
        let mut txn = v.store.write();
        let id = txn.intern_key(&key).unwrap().0;
        txn.commit().unwrap();
        id
    };
    let source: Source = Arc::new(move |_: &Terms| {
        let rules = vec![rule(
            "q",
            vec![pos(tri(V(0), p1, V(1))), neg(tri(V(0), p0, C(lit)))],
            vec![head(tri(V(0), p2, V(1)))],
        )];
        (Relations::default(), rules)
    });
    let base: FxHashSet<Triple> = [[ind(0), p0, lit], [ind(0), p1, ind(1)]]
        .into_iter()
        .collect();
    let WhyNot::NotEntailed { rules, .. } = ask(&v, &source, &base, [ind(0), p2, ind(1)]) else {
        panic!("entailed")
    };
    let q = rules.iter().find(|c| c.rule.name == "q").unwrap();
    assert!(
        q.blocked_by.iter().any(|b| b.call.contains("\"a, b\"")),
        "{:?}",
        q.blocked_by
    );
}

/// Why not a key's triple whose second individual lacks the key's later values: the
/// hypotheses name only triples about it, verified through `keyed` relation nodes.
#[test]
fn why_not_through_a_key_of_four_values() {
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let source: Source = Arc::new(move |_: &Terms| chain_and_key_rules(&vv));
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p3) = (v.props[0], v.props[1], v.props[3]);
    let base: FxHashSet<Triple> = [
        [ind(2), p0, ind(5)],
        [ind(2), p1, ind(2)],
        [ind(3), p0, ind(5)],
    ]
    .into_iter()
    .collect();
    let snap = set_default_graph(&v, &base);
    let qr = reasoner(&source);
    let opts = WhyNotOptions {
        max_missing: 4,
        max_unfold: 6,
        ..Default::default()
    };
    let WhyNot::NotEntailed { hypotheses, .. } = why_not(
        &DemandInput {
            reasoner: &qr,
            snap,
            readable: None,
        },
        &triple(&v, [ind(3), p3, ind(2)]),
        &opts,
    )
    .unwrap() else {
        panic!("entailed")
    };
    let h = hypotheses
        .iter()
        .find(|h| {
            h.missing
                .iter()
                .all(|f| f[0] == term(&v, ind(3)) && f[1] == term(&v, p1))
        })
        .unwrap_or_else(|| panic!("{hypotheses:?}"));
    let mut nodes = Vec::new();
    walk(&h.tree, &mut nodes);
    assert!(
        nodes
            .iter()
            .any(|d| d.relation.as_ref().is_some_and(|r| r.name == "keyed"))
    );
}
