//! Demand programs over relations (item 839's plan, Phase 4, §20.1 and §20.2): the
//! answers of query-time sessions over rule programs with relations and negation equal
//! the full run's.

use super::maintain::chain_and_key_rules;
use super::*;
use crate::extras::Extras;
use crate::facts::Base;
use crate::incremental::tests::{Vocab as TV, set_default_graph};
use crate::query::{QueryReasoner, Session};
use crate::{Inputs, Profile};
use rustc_hash::FxHashSet;
use sparkles_core::sparql::reasoning::SessionRequest;
use sparkles_core::store::Snapshot;
use std::sync::Arc;

type Triple = [u64; 3];

/// Rules over relations, made again for every session's terms.
pub(super) type Source = Arc<dyn Fn(&Terms) -> (Relations, Vec<Rule>) + Send + Sync>;

fn build(source: &Source, t: &Terms) -> Program {
    let (rels, rules) = source(t);
    Program::new(rels, rules, &Vocab::of(t), &t.budget()).unwrap()
}

pub(super) fn reasoner(source: &Source) -> QueryReasoner {
    let s = source.clone();
    QueryReasoner::new(
        Profile::Rules(String::new()),
        &Extras::default(),
        Inputs::default(),
    )
    .unwrap()
    .with_program(Arc::new(move |t: &Terms| Ok(build(&s, t))))
}

fn request(snap: &Arc<Snapshot>) -> SessionRequest {
    SessionRequest {
        snap: snap.clone(),
        readable: None,
        stop: Arc::new(|| Ok(())),
        max_memory: None,
        no_cache: true,
    }
}

/// Every fact of the full run of the program over `base`, decoded.
fn full(v: &TV, source: &Source, base: &FxHashSet<Triple>) -> BTreeSet<Fact> {
    let snap = set_default_graph(v, base);
    let terms = Terms::new(snap.clone());
    let p = build(source, &terms);
    let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
    let mut g = Graph::with_base(Arc::new(b));
    g.set_relations();
    let mut w = WorkingWarnings::new(&terms.budget());
    engine::run_program(&mut g, &p, &terms, &limits(), &mut w).unwrap();
    every(&g, &terms, &p)
}

/// The facts of `g`, both tiers, without the magic facts of a demand program.
fn every(g: &Graph, t: &Terms, p: &Program) -> BTreeSet<Fact> {
    let mut out: BTreeSet<Fact> = g
        .facts()
        .filter(|f| t.valid(f) || is_key(f[1]))
        .map(|f| decode(t, p, f))
        .collect();
    g.base()
        .unwrap()
        .for_each(None, None, None, |f| {
            out.insert((None, f.to_vec()));
            true
        })
        .unwrap();
    out
}

/// A pattern: a relation or a triple predicate (`None` for a relation's `Some`), the
/// first column or subject, and the last column or object when it is not a tail.
#[derive(Clone, Copy, Debug)]
struct Pat {
    rel: Option<RelId>,
    pred: u64,
    s: Option<u64>,
    o: Option<u64>,
}

impl Pat {
    fn ids(&self) -> [Option<u64>; 3] {
        match self.rel {
            None => [self.s, Some(self.pred), self.o],
            Some(r) => [self.s, Some(key(r)), self.o],
        }
    }

    fn matches(&self, f: &Fact) -> bool {
        match (self.rel, f.0) {
            (None, None) => {
                f.1[1] == self.pred
                    && self.s.is_none_or(|s| f.1[0] == s)
                    && self.o.is_none_or(|o| f.1[2] == o)
            }
            (Some(r), Some(fr)) if r.0 == fr => {
                self.s.is_none_or(|s| f.1.first() == Some(&s))
                    && self.o.is_none_or(|o| f.1.get(1) == Some(&o))
            }
            _ => false,
        }
    }
}

/// The facts a session derives for `pats` at once, with linear recursion rewritten or
/// not.
fn session_facts(
    v: &TV,
    source: &Source,
    base: &FxHashSet<Triple>,
    pats: &[Pat],
    linear: bool,
) -> BTreeSet<Fact> {
    let snap = set_default_graph(v, base);
    let qr = reasoner(source);
    let mut s: Session = qr.test_session(request(&snap), linear).unwrap();
    let ids: Vec<[Option<u64>; 3]> = pats.iter().map(Pat::ids).collect();
    s.derive_ids(&ids).unwrap();
    let p = build(source, s.terms());
    every(s.graph().unwrap(), s.terms(), &p)
}

/// The session's facts are facts of the full run, and every fact of the full run that a
/// pattern matches is the session's.
fn check(v: &TV, source: &Source, base: &FxHashSet<Triple>, pats: &[Pat], what: &str) {
    let want = full(v, source, base);
    for linear in [true, false] {
        let got = session_facts(v, source, base, pats, linear);
        let extra: Vec<&Fact> = got.difference(&want).collect();
        assert!(
            extra.is_empty(),
            "{what} {pats:?}: not in the full run {extra:?}"
        );
        for p in pats {
            let missing: Vec<&Fact> = want
                .iter()
                .filter(|f| p.matches(f) && !got.contains(*f))
                .collect();
            assert!(
                missing.is_empty(),
                "{what} {p:?} linear {linear}: missing {missing:?}"
            );
        }
    }
}

fn tp(pred: u64, s: Option<u64>, o: Option<u64>) -> Pat {
    Pat {
        rel: None,
        pred,
        s,
        o,
    }
}

fn rp(r: u32, s: Option<u64>, o: Option<u64>) -> Pat {
    Pat {
        rel: Some(RelId(r)),
        pred: 0,
        s,
        o,
    }
}

/// Demand over Phase 3's chain of six properties and key of four values, for every
/// binding of the derived triples' subject and object, and of the relations' first
/// column, equals the full run.
#[test]
fn demand_over_chains_and_keys_equals_the_full_run() {
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let source: Source = Arc::new(move |_: &Terms| chain_and_key_rules(&vv));
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let mut base: FxHashSet<Triple> = (0..6).map(|i| [ind(i), p(i), ind(i + 1)]).collect();
    base.insert([ind(2), p(1), ind(2)]);
    base.insert([ind(5), p(1), ind(5)]);
    let mut values: Vec<Option<u64>> = (0..8).map(|i| Some(ind(i))).collect();
    values.push(None);
    for &pred in &v.props[..4] {
        for &s in &values {
            check(&v, &source, &base, &[tp(pred, s, None)], "subject");
            check(&v, &source, &base, &[tp(pred, None, s)], "object");
        }
        check(
            &v,
            &source,
            &base,
            &[tp(pred, Some(ind(0)), Some(ind(6)))],
            "both",
        );
    }
    // the relations by their first column: chain(list, i, x, y) and keyed(x, i, k)
    for r in 0..2 {
        for &s in &values {
            check(&v, &source, &base, &[rp(r, s, None)], "relation");
        }
    }
    // several patterns in one program
    check(
        &v,
        &source,
        &base,
        &[
            tp(v.props[2], Some(ind(0)), None),
            tp(v.props[3], None, Some(ind(5))),
            rp(1, Some(ind(2)), None),
        ],
        "together",
    );
}

/// A demand with every position free asks for every fact the rules give, and gives no
/// relation fact to a query, since none is valid RDF.
#[test]
fn free_demands_return_no_relation_fact() {
    use sparkles_core::sparql::reasoning::{Demand, DemandTerm, ReasoningSession};
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let source: Source = Arc::new(move |_: &Terms| chain_and_key_rules(&vv));
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let mut base: FxHashSet<Triple> = (0..6).map(|i| [ind(i), p(i), ind(i + 1)]).collect();
    base.insert([ind(2), p(1), ind(2)]);
    let want = full(&v, &source, &base);
    let snap = set_default_graph(&v, &base);
    let qr = reasoner(&source);
    let mut s: Session = qr.test_session(request(&snap), true).unwrap();
    let free = Demand {
        s: DemandTerm::Var,
        p: DemandTerm::Var,
        o: DemandTerm::Var,
    };
    s.prepare(&[free], false).unwrap();
    let out = s.derive(vec![(0, Vec::new())]).unwrap();
    assert!(!out.facts.is_empty());
    for f in &out.facts {
        assert!(!is_key(f[1]), "{f:?}");
        assert!(want.contains(&(None, f.to_vec())), "{f:?}");
    }
    let derived: BTreeSet<&Fact> = want
        .iter()
        .filter(|f| f.0.is_none() && !base.contains(&[f.1[0], f.1[1], f.1[2]]))
        .collect();
    let got: BTreeSet<Fact> = out.facts.iter().map(|f| (None, f.to_vec())).collect();
    for f in derived {
        assert!(got.contains(f), "{f:?} missing");
    }
}

/// `(x p0 y) → R(x)` and `(x p1 y), not R(x) → (x p2 y)`, and the same with the negated
/// atom a triple, `(x p1 y), not (x p3 _) → (x p2 y)`: the negation reads a complete
/// lower stage.
#[test]
fn a_negation_on_demand_reads_a_complete_lower_stage() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p2, p3) = (v.props[0], v.props[1], v.props[2], v.props[3]);
    let relation: Source = Arc::new(move |_: &Terms| {
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
    let triple: Source = Arc::new(move |_: &Terms| {
        let rules = vec![
            rule(
                "r",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(tri(V(0), p3, V(1)))],
            ),
            rule(
                "q",
                vec![pos(tri(V(0), p1, V(1))), neg(tri(V(0), p3, Any))],
                vec![head(tri(V(0), p2, V(1)))],
            ),
        ];
        (Relations::default(), rules)
    });
    let base: FxHashSet<Triple> = [
        [ind(0), p0, ind(1)],
        [ind(0), p1, ind(1)],
        [ind(2), p1, ind(3)],
        [ind(4), p0, ind(4)],
        [ind(4), p1, ind(5)],
    ]
    .into_iter()
    .collect();
    for (what, source) in [("relation", &relation), ("triple", &triple)] {
        let want = full(&v, source, &base);
        assert!(want.contains(&(None, vec![ind(2), p2, ind(3)])), "{what}");
        assert!(!want.contains(&(None, vec![ind(0), p2, ind(1)])), "{what}");
        for s in [None, Some(ind(0)), Some(ind(2)), Some(ind(4))] {
            check(&v, source, &base, &[tp(p2, s, None)], what);
            check(&v, source, &base, &[tp(p2, None, s)], what);
        }
    }
}

/// A binary relation beside `(x ?p y) → (y ?p x)`: the rule with a variable predicate
/// gives no relation fact, so a demand on the relation does not answer with the reversed
/// pairs that its copy specialized to the relation's key would derive.
#[test]
fn a_variable_predicate_head_gives_no_relation_fact() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, ty, c0) = (v.props[0], v.iri[&format!("{RDF}type")], v.classes[0]);
    let source: Source = Arc::new(move |_: &Terms| {
        let (rels, ids) = rels(&[2]);
        let rules = vec![
            rule(
                "sym",
                vec![pos(IAtom::triple(V(0), V(1), V(2)))],
                vec![head(IAtom::triple(V(2), V(1), V(0)))],
            ),
            // r is not symmetric: only a typed subject starts a pair
            rule(
                "r",
                vec![pos(tri(V(0), p0, V(1))), pos(tri(V(0), ty, C(c0)))],
                vec![head(rel(ids[0], &[V(0), V(1)]))],
            ),
        ];
        (rels, rules)
    });
    let base: FxHashSet<Triple> = [[ind(0), p0, ind(1)], [ind(0), ty, c0]]
        .into_iter()
        .collect();
    let want = full(&v, &source, &base);
    assert!(want.contains(&(Some(0), vec![ind(0), ind(1)])));
    assert!(!want.contains(&(Some(0), vec![ind(1), ind(0)])));
    for s in [None, Some(ind(0)), Some(ind(1))] {
        check(&v, &source, &base, &[rp(0, s, None)], "subject");
        check(&v, &source, &base, &[rp(0, None, s)], "object");
    }
}

const RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";

/// A negated relation of arity 3 with a constant in its first tail column: the lower
/// stage completes the whole relation, since its third lowered argument is a tail column
/// and not the object.
#[test]
fn a_negated_tail_constant_does_not_narrow_the_lower_stage() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let (p0, p1, p2) = (v.props[0], v.props[1], v.props[2]);
    let five = ind(5);
    let source: Source = Arc::new(move |_: &Terms| {
        let (rels, ids) = rels(&[3]);
        let rules = vec![
            rule(
                "t",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(rel(ids[0], &[V(0), C(five), V(1)]))],
            ),
            rule(
                "q",
                vec![
                    pos(tri(V(0), p1, V(1))),
                    neg(rel(ids[0], &[V(0), C(five), V(1)])),
                ],
                vec![head(tri(V(0), p2, V(1)))],
            ),
        ];
        (rels, rules)
    });
    let base: FxHashSet<Triple> = [
        [ind(0), p0, ind(1)],
        [ind(0), p1, ind(1)],
        [ind(2), p1, ind(3)],
    ]
    .into_iter()
    .collect();
    let want = full(&v, &source, &base);
    assert!(!want.contains(&(None, vec![ind(0), p2, ind(1)])));
    for s in [None, Some(ind(0)), Some(ind(2))] {
        check(&v, &source, &base, &[tp(p2, s, None)], "subject");
    }
}

/// The constants of a random program of Phase 2 as terms of `v`'s store: the values 1 and
/// 2 as individuals, the triple predicates as properties.
fn store_rules(v: &TV, rules: &[Rule]) -> Vec<Rule> {
    let map = |s: Slot| match s {
        C(c) if c == n(1) => C(v.inds[0]),
        C(c) if c == n(2) => C(v.inds[1]),
        C(c) if c == n(P0) => C(v.props[0]),
        C(c) if c == n(P0 + 1) => C(v.props[1]),
        s => s,
    };
    let atom = |a: &IAtom| IAtom {
        pred: a.pred,
        args: a.args.iter().map(|&s| map(s)).collect(),
    };
    rules
        .iter()
        .map(|r| Rule {
            name: r.name.clone(),
            nvars: r.nvars,
            body: r
                .body
                .iter()
                .map(|l| match l {
                    Literal::Pos(a) => Literal::Pos(atom(a)),
                    Literal::Neg(a) => Literal::Neg(atom(a)),
                    Literal::Builtin(b) => Literal::Builtin(b.clone()),
                })
                .collect(),
            head: r
                .head
                .iter()
                .map(|h| match h {
                    Head::Atom(a) => Head::Atom(atom(a)),
                    Head::Action(k, args) => Head::Action(*k, args.clone()),
                })
                .collect(),
            exists: r.exists.clone(),
            vars: r.vars.clone(),
        })
        .collect()
}

/// Random programs of Phase 2, relations of arities 0 to 5 with recursion and negated
/// atoms, over the store: the relations' input facts come from triples of their own
/// predicates. For random patterns, each in a session of its own, the session's facts
/// are the full run's and hold every fact of the full run that the pattern matches, and
/// no program that stratifies is refused by the demand transformation.
#[test]
fn random_programs_on_demand_equal_their_full_runs() {
    let v = Arc::new(TV::new());
    let (in1, in2) = (v.props[2], v.props[3]);
    let mut checked = 0;
    let mut negated = 0;
    for seed in 1..=600u64 {
        let t = super::terms();
        let Some((p, facts)) = random_program(&t, seed) else {
            continue;
        };
        let vv = v.clone();
        let source: Source = Arc::new(move |_: &Terms| {
            let t = super::terms();
            let (p, _) = random_program(&t, seed).expect("a program");
            let mut rules = store_rules(&vv, &p.rules);
            // the input facts of relations 1 and 2, from triples
            rules.push(rule(
                "in1",
                vec![pos(tri(V(0), in1, V(0)))],
                vec![head(rel(RelId(1), &[V(0)]))],
            ));
            rules.push(rule(
                "in2",
                vec![pos(tri(V(0), in2, V(1)))],
                vec![head(rel(RelId(2), &[V(0), V(1)]))],
            ));
            (p.relations, rules)
        });
        let val = |x: u64| if x == n(1) { v.inds[0] } else { v.inds[1] };
        let base: FxHashSet<Triple> = facts
            .iter()
            .map(|f| match f.0 {
                None => [val(f.1[0]), v.props[0], val(f.1[2])],
                Some(1) => [val(f.1[0]), in1, val(f.1[0])],
                Some(_) => [val(f.1[0]), in2, val(f.1[1])],
            })
            .collect();
        negated += usize::from(p.strata.has_negation);
        let mut rng = super::Rng(seed | 1);
        let value = |rng: &mut super::Rng| match rng.below(3) {
            0 => None,
            1 => Some(v.inds[0]),
            _ => Some(v.inds[1]),
        };
        for _ in 0..3 {
            let pat = match rng.below(7) {
                i @ 0..5 => {
                    let s = value(&mut rng);
                    // the last column is a pattern position only up to arity 2
                    let o = if [0u16, 1, 2, 3, 5][i] == 2 {
                        value(&mut rng)
                    } else {
                        None
                    };
                    rp(i as u32, s, o)
                }
                i => tp(v.props[i - 5], value(&mut rng), value(&mut rng)),
            };
            check(&v, &source, &base, &[pat], &format!("seed {seed}"));
        }
        checked += 1;
    }
    eprintln!("checked {checked} negated {negated}");
    assert!(checked > 50, "{checked} programs checked");
    assert!(negated > 20, "{negated} programs had a negation");
}

/// No magic rule of a demand program over a relation of arity three or more interns a
/// tail.
#[test]
fn magic_rules_intern_no_tail() {
    use crate::demand::{Key, KeyP, Pass, transform};
    let v = TV::new();
    let (rels, rules) = chain_and_key_rules(&v);
    let t = Terms::new(v.store.snapshot());
    let p = Program::new(rels, rules, &Vocab::of(&t), &t.budget()).unwrap();
    let lowered = p.lower(&t.budget()).unwrap();
    let rules: Vec<engine::CRule> = lowered
        .strata
        .iter()
        .flatten()
        .map(engine::CRule::clone_shallow)
        .collect();
    let est = |_: &engine::Atom, _: &[bool]| (1.0, 1.0);
    for (pred, s, o) in [
        (v.props[2], true, false),
        (v.props[2], false, true),
        (v.props[3], true, false),
    ] {
        let root = Key::new(s, KeyP::Const(pred), o);
        let prog = transform(&rules, &t, &[root], &est, Pass::Plain, true).unwrap();
        let mut tuples = 0;
        for st in &prog.stages {
            for r in &st.rules {
                let magic = matches!(r.head.first(), Some(engine::CHead::Triple(_, C(m), _)) if prog.magic_preds.contains(m));
                if magic {
                    assert!(
                        r.builtins.iter().all(|b| b.kind != B::RelTuple),
                        "{}",
                        r.name
                    );
                } else {
                    tuples += r.builtins.iter().filter(|b| b.kind == B::RelTuple).count();
                }
            }
        }
        assert!(tuples > 0, "the adorned copies keep their tuples");
    }
}

/// A two-list pipeline: a rule copies the members of the stored list `l0` into a derived
/// list on `l1`, reading `l0` through `listMember`, and `listMember(l1, x)` gives `R(x)`,
/// which gives the queried triple. Completing the lists below would need the copy's own
/// list read below itself, so the program is made again with lists read in the stage,
/// which then asks for the derived cells and reads `l1` again when they arrive. Rules
/// whose atoms outnumber the graph's predicates make the engine select candidates by
/// index.
#[test]
fn a_derived_list_read_in_its_stage_gives_its_members() {
    let v = Arc::new(TV::new());
    let ind = |i: usize| v.inds[i];
    let rdf = |l: &str| v.iri[&format!("{RDF}{l}")];
    let (first, rest, nil) = (rdf("first"), rdf("rest"), rdf("nil"));
    let (l0, l1, l2) = (v.list_nodes[0], v.list_nodes[1], v.list_nodes[2]);
    let (p2, copy, uses, pad) = (v.props[2], v.props[3], v.props[4], v.props[0]);
    let source: Source = Arc::new(move |_: &Terms| {
        let (rels, ids) = rels(&[1]);
        let mut rules = vec![
            rule(
                "copy",
                vec![pos(tri(V(0), copy, V(1))), bi(B::ListMember, &[V(0), V(2)])],
                vec![head(tri(V(1), first, V(2))), head(tri(V(1), rest, C(nil)))],
            ),
            rule(
                "use",
                vec![pos(tri(V(0), uses, V(1))), bi(B::ListMember, &[V(1), V(2)])],
                vec![head(rel(ids[0], &[V(2)]))],
            ),
            rule(
                "out",
                vec![pos(rel(ids[0], &[V(0)]))],
                vec![head(tri(V(0), p2, V(0)))],
            ),
        ];
        for i in 0..4 {
            rules.push(rule(
                &format!("pad{i}"),
                vec![
                    pos(tri(V(0), pad, V(1))),
                    pos(tri(V(1), pad, V(2))),
                    pos(tri(V(2), pad, V(0))),
                ],
                vec![head(tri(V(0), pad, V(0)))],
            ));
        }
        (rels, rules)
    });
    let base: FxHashSet<Triple> = [
        [l0, first, ind(0)],
        [l0, rest, l2],
        [l2, first, ind(1)],
        [l2, rest, nil],
        [l0, copy, l1],
        [ind(5), uses, l1],
    ]
    .into_iter()
    .collect();
    let want = full(&v, &source, &base);
    // the copied cell holds both members, and a list reads its first
    assert!(want.contains(&(None, vec![ind(0), p2, ind(0)])), "{want:?}");
    for s in [None, Some(ind(0)), Some(ind(1))] {
        check(&v, &source, &base, &[tp(p2, s, None)], "subject");
        check(&v, &source, &base, &[rp(0, s, None)], "relation");
    }
}

/// The cost probe of §20.7: the facts a session derives for a chain's triple with its
/// subject or its object bound, over 40 disjoint paths, with the chain written through
/// the relation `chain(list, i, x, y)` and as one rule of its length, as the profile's
/// rules per length write it. Run with `--ignored --nocapture`.
#[test]
#[ignore]
fn demand_cost_probe() {
    let v = Arc::new(TV::new());
    let vv = v.clone();
    let relation: Source = Arc::new(move |_: &Terms| {
        let (rels, mut rules) = chain_and_key_rules(&vv);
        rules.retain(|r| r.name.starts_with("chain"));
        (rels, rules)
    });
    let (p0, p1, p2) = (v.props[0], v.props[1], v.props[2]);
    let p = move |i: usize| if i % 2 == 0 { p0 } else { p1 };
    let per_length: Source = Arc::new(move |_: &Terms| {
        let body = (0..6).map(|i| pos(tri(V(i), p(i), V(i + 1)))).collect();
        (
            Relations::default(),
            vec![rule("spo2-6", body, vec![head(tri(V(0), p2, V(6)))])],
        )
    });
    let node = |k: usize, i: usize| n(1000 + 10 * k as i64 + i as i64);
    let base: FxHashSet<Triple> = (0..40)
        .flat_map(|k| (0..6).map(move |i| [node(k, i), p(i), node(k, i + 1)]))
        .collect();
    for (what, source) in [("relation", &relation), ("per length", &per_length)] {
        let want = full(&v, source, &base);
        assert!(want.contains(&(None, vec![node(0, 0), p2, node(0, 6)])));
        for (shape, pat) in [
            ("subject", tp(p2, Some(node(0, 0)), None)),
            ("object", tp(p2, None, Some(node(0, 6)))),
        ] {
            let got = session_facts(&v, source, &base, &[pat], true);
            let derived = got.len() - base.len();
            eprintln!(
                "probe {what} {shape}: {derived} derived facts (full run {})",
                want.len() - base.len()
            );
        }
    }
}
