//! Tests of relations and stratified negation in full runs (item 839's plan, Phase 2).

use super::lower::{NIL, is_key, key};
use super::*;
use crate::budget::WorkingWarnings;
use crate::builtins::BuiltinKind as B;
use crate::engine::Slot::{Any, Const as C, Var as V};
use crate::engine::{self, Limits};
use crate::graph::Graph;
use sparkles_core::id::Id;
use sparkles_core::store::{Store, StoreOptions};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

mod demand;
mod frontend;
mod maintain;
mod whynot;

/// A fact by its relation (`None` for the triple relation) and its columns.
type Fact = (Option<u32>, Vec<u64>);

fn terms() -> Terms {
    Terms::new(Store::in_memory(StoreOptions::default()).snapshot())
}

/// An inline integer term.
fn n(i: i64) -> u64 {
    Id::from_i64(i).unwrap().0
}

fn limits() -> Limits {
    Limits {
        max_iterations: 10_000,
        max_inferred: 10_000_000,
        budget: Budget::unlimited(),
        closures: false,
        magic: None,
        sampled_planning: false,
        cancel: None,
        deadline: None,
        progress: None,
    }
}

fn vocab(t: &Terms) -> Vocab {
    Vocab::of(t)
}

fn rels(arities: &[u16]) -> (Relations, Vec<RelId>) {
    let mut rels = Relations::default();
    let ids = arities
        .iter()
        .enumerate()
        .map(|(i, &a)| rels.declare(&format!("r{i}"), a, false).unwrap())
        .collect();
    (rels, ids)
}

fn rule(name: &str, body: Vec<Literal>, head: Vec<Head>) -> Rule {
    let mut nvars = 0;
    let mut see = |s: &Slot| {
        if let V(v) = s {
            nvars = nvars.max(v + 1);
        }
    };
    for l in &body {
        match l {
            Literal::Pos(a) | Literal::Neg(a) => a.args.iter().for_each(&mut see),
            Literal::Builtin(b) => b.args.iter().for_each(&mut see),
        }
    }
    for h in &head {
        match h {
            Head::Atom(a) => a.args.iter().for_each(&mut see),
            Head::Action(_, args) => args.iter().for_each(&mut see),
        }
    }
    Rule {
        name: name.into(),
        nvars,
        body,
        head,
        exists: Vec::new(),
        vars: Vec::new(),
    }
}

fn pos(a: IAtom) -> Literal {
    Literal::Pos(a)
}

fn neg(a: IAtom) -> Literal {
    Literal::Neg(a)
}

fn bi(kind: BuiltinKind, args: &[Slot]) -> Literal {
    Literal::Builtin(IBuiltin::new(kind, args))
}

fn rel(r: RelId, args: &[Slot]) -> IAtom {
    IAtom::rel(r, args)
}

fn tri(s: Slot, p: u64, o: Slot) -> IAtom {
    IAtom::triple(s, C(p), o)
}

fn head(a: IAtom) -> Head {
    Head::Atom(a)
}

/// The row of fact `f`, its tail interned.
fn row(t: &Terms, p: &Program, f: &Fact) -> [u64; 3] {
    match f.0 {
        None => [f.1[0], f.1[1], f.1[2]],
        Some(r) => {
            let r = RelId(r);
            let k = key(r);
            assert_eq!(p.relations.get(r).unwrap().arity as usize, f.1.len());
            match f.1.len() {
                0 => [NIL, k, NIL],
                1 => [f.1[0], k, NIL],
                2 => [f.1[0], k, f.1[1]],
                _ => [f.1[0], k, t.try_tail(k, &f.1[1..]).unwrap()],
            }
        }
    }
}

/// The fact of row `r`.
fn decode(t: &Terms, p: &Program, r: [u64; 3]) -> Fact {
    if !is_key(r[1]) {
        return (None, r.to_vec());
    }
    let rid = (0..p.relations.len() as u32)
        .find(|&i| key(RelId(i)) == r[1])
        .expect("a declared relation");
    let cols = match p.relations.get(RelId(rid)).unwrap().arity {
        0 => vec![],
        1 => vec![r[0]],
        2 => vec![r[0], r[2]],
        _ => {
            let (k, tail) = t.tail(r[2]).expect("a tail");
            assert_eq!(k, r[1]);
            std::iter::once(r[0]).chain(tail).collect()
        }
    };
    (Some(rid), cols)
}

fn graph(t: &Terms, p: &Program, facts: &[Fact]) -> Graph {
    let mut g = Graph::default();
    if facts.iter().any(|f| f.0.is_some()) {
        g.set_relations();
    }
    g.add_batch(facts.iter().map(|f| row(t, p, f)));
    g
}

/// Run `p` over `facts` and return every fact afterwards, with the warnings.
fn run_with(
    t: &Terms,
    p: &Program,
    facts: &[Fact],
    lim: &Limits,
) -> anyhow::Result<(BTreeSet<Fact>, Vec<String>)> {
    let mut g = graph(t, p, facts);
    let mut w = WorkingWarnings::new(&t.budget());
    engine::run_program(&mut g, p, t, lim, &mut w)?;
    let out = g.facts().map(|r| decode(t, p, r)).collect();
    Ok((out, w.as_slice().to_vec()))
}

fn run(t: &Terms, p: &Program, facts: &[Fact]) -> BTreeSet<Fact> {
    run_with(t, p, facts, &limits()).unwrap().0
}

fn program(t: &Terms, rels: Relations, rules: Vec<Rule>) -> Program {
    Program::new(rels, rules, &vocab(t), &t.budget()).unwrap()
}

// --------------------------------------------------- the reference evaluator ---

/// The atom `a` as a pattern: its relation and argument slots.
fn pattern(a: &IAtom) -> (Option<u32>, &[Slot]) {
    match a.pred {
        Pred::Triple => (None, &a.args[..]),
        Pred::Rel(r) => (Some(r.0), &a.args[..]),
    }
}

fn matches(a: &IAtom, f: &Fact, b: &mut [u64], newly: &mut Vec<usize>) -> bool {
    let (r, args) = pattern(a);
    if r != f.0 || args.len() != f.1.len() {
        return false;
    }
    for (s, &v) in args.iter().zip(&f.1) {
        match *s {
            C(c) if c != v => return false,
            V(x) if b[x] == 0 => {
                b[x] = v;
                newly.push(x);
            }
            V(x) if b[x] != v => return false,
            _ => {}
        }
    }
    true
}

fn val(s: Slot, b: &[u64]) -> u64 {
    match s {
        V(x) => b[x],
        C(c) => c,
        Any => 0,
    }
}

/// Every binding of the positive atoms `atoms[i..]` over `facts`, then the rest of the
/// body checked, and the heads emitted to `out`.
#[allow(clippy::too_many_arguments)]
fn join(
    r: &Rule,
    heads: &[usize],
    atoms: &[&IAtom],
    i: usize,
    facts: &BTreeSet<Fact>,
    b: &mut Vec<u64>,
    out: &mut Vec<Fact>,
) {
    if i == atoms.len() {
        for l in &r.body {
            match l {
                Literal::Neg(a) => {
                    if facts
                        .iter()
                        .any(|f| matches(a, f, &mut b.clone(), &mut vec![]))
                    {
                        return;
                    }
                }
                Literal::Builtin(x) => {
                    let (l, rr) = (val(x.args[0], b), val(x.args[1], b));
                    let ok = match x.kind {
                        B::Equal => l == rr,
                        B::NotEqual => l != rr,
                        k => panic!("no reference for {k:?}"),
                    };
                    if !ok {
                        return;
                    }
                }
                Literal::Pos(_) => {}
            }
        }
        for &h in heads {
            let Head::Atom(a) = &r.head[h] else {
                panic!("no reference for head actions")
            };
            let (rel, args) = pattern(a);
            out.push((rel, args.iter().map(|s| val(*s, b)).collect()));
        }
        return;
    }
    for f in facts {
        let mut newly = Vec::new();
        if matches(atoms[i], f, b, &mut newly) {
            join(r, heads, atoms, i + 1, facts, b, out);
        }
        for x in newly {
            b[x] = 0;
        }
    }
}

/// The perfect model of `p` over `facts`: each stratum's parts naively to their fixpoint,
/// in stratum order.
fn reference(p: &Program, facts: &[Fact]) -> BTreeSet<Fact> {
    reference_by(p, facts, true)
}

/// [`reference`], or with `strata` false every part in one stratum, the reading a run
/// without strata would approach.
fn reference_by(p: &Program, facts: &[Fact], strata: bool) -> BTreeSet<Fact> {
    let mut all: BTreeSet<Fact> = facts.iter().cloned().collect();
    for s in 0..if strata { p.strata.count } else { 1 } {
        loop {
            let mut out = Vec::new();
            for part in p.strata.parts.iter().filter(|x| !strata || x.stratum == s) {
                let r = &p.rules[part.rule];
                let atoms: Vec<&IAtom> = r
                    .body
                    .iter()
                    .filter_map(|l| match l {
                        Literal::Pos(a) => Some(a),
                        _ => None,
                    })
                    .collect();
                join(
                    r,
                    &part.heads,
                    &atoms,
                    0,
                    &all,
                    &mut vec![0; r.nvars],
                    &mut out,
                );
            }
            let before = all.len();
            all.extend(out);
            if all.len() == before {
                break;
            }
        }
    }
    all
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
}

const P0: i64 = 100;

/// A random program over relations of arities 0 to 5 and two triple predicates, with
/// recursion and negated atoms, and random facts over four constants. The facts are of
/// relations 1 and 2 and of the triple predicate `P0`, the heads of relations 0, 1, 3 and
/// 4 and of `P1`, and most negated atoms read a head's relation.
fn random_program(t: &Terms, seed: u64) -> Option<(Program, Vec<Fact>)> {
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let arities = [0u16, 1, 2, 3, 5];
    let (rels, ids) = rels(&arities);
    // an atom over relation `i` (or a triple atom at `i == arities.len()`)
    let atom = |rng: &mut Rng, i: usize, vars: &mut Vec<usize>, bound: Option<&[usize]>, p: i64| {
        let mut slot = |rng: &mut Rng| -> Slot {
            match bound {
                Some(b) if !b.is_empty() && rng.below(4) > 0 => V(b[rng.below(b.len())]),
                Some(_) => C(n(1 + rng.below(2) as i64)),
                None if rng.below(5) == 0 => C(n(1 + rng.below(2) as i64)),
                None => {
                    let v = rng.below(4);
                    vars.push(v);
                    V(v)
                }
            }
        };
        if i == arities.len() {
            let s = slot(rng);
            let o = slot(rng);
            IAtom::triple(s, C(n(p)), o)
        } else {
            let args: Vec<Slot> = (0..arities[i]).map(|_| slot(rng)).collect();
            rel(ids[i], &args)
        }
    };
    let mut rules = Vec::new();
    let mut heads: Vec<IAtom> = Vec::new();
    for ri in 0..3 + rng.below(6) {
        let mut vars = Vec::new();
        let mut body = Vec::new();
        const HEADS: [usize; 5] = [0, 1, 3, 4, 5];
        for _ in 0..1 + rng.below(3) {
            let i = rng.below(arities.len() + 1);
            let p = P0 + rng.below(2) as i64;
            body.push(pos(atom(&mut rng, i, &mut vars, None, p)));
        }
        vars.sort_unstable();
        vars.dedup();
        if rng.below(3) > 0 && !heads.is_empty() && rng.below(4) > 0 {
            // the shape of an earlier rule's head, over this rule's variables
            let h: &IAtom = &heads[rng.below(heads.len())];
            let mut a = h.clone();
            for s in a.args.iter_mut() {
                if let V(_) = s {
                    *s = if rng.below(4) == 0 {
                        Any
                    } else if vars.is_empty() {
                        C(n(1 + rng.below(2) as i64))
                    } else {
                        V(vars[rng.below(vars.len())])
                    };
                }
            }
            body.push(neg(a));
        } else if rng.below(3) > 0 {
            let i = if rng.below(4) > 0 {
                HEADS[rng.below(HEADS.len())]
            } else {
                rng.below(arities.len() + 1)
            };
            body.push(neg(atom(&mut rng, i, &mut Vec::new(), Some(&vars), P0 + 1)));
        }
        let i = HEADS[rng.below(HEADS.len())];
        let h = atom(&mut rng, i, &mut Vec::new(), Some(&vars), P0 + 1);
        heads.push(h.clone());
        rules.push(rule(&format!("q{ri}"), body, vec![head(h)]));
    }
    let p = Program::new(rels, rules, &vocab(t), &t.budget()).ok()?;
    let mut facts = Vec::new();
    for _ in 0..30 {
        let i = [1, 2, arities.len()][rng.below(3)];
        let c = |rng: &mut Rng| n(1 + rng.below(2) as i64);
        facts.push(if i == arities.len() {
            (None, vec![c(&mut rng), n(P0), c(&mut rng)])
        } else {
            (
                Some(i as u32),
                (0..arities[i]).map(|_| c(&mut rng)).collect(),
            )
        });
    }
    Some((p, facts))
}

// ------------------------------------------------------------------ tests ---

/// Random programs with relations of arities 0 to 5, triple atoms, recursion and
/// negated atoms give the perfect model, whatever atom brings an instance's only new
/// premise.
#[test]
fn random_programs_give_the_perfect_model() {
    let mut checked = 0;
    let mut by_strata = 0;
    // the programs whose perfect model differs from the one-stratum reading
    let mut telling = 0;
    for seed in 1..=8000 {
        let t = terms();
        let Some((p, facts)) = random_program(&t, seed) else {
            continue;
        };
        let want = reference(&p, &facts);
        let got = run(&t, &p, &facts);
        assert_eq!(got, want, "seed {seed}: {:?}", p.rules);
        checked += 1;
        by_strata += usize::from(p.strata.count > 1);
        telling += usize::from(reference_by(&p, &facts, false) != want);
    }
    eprintln!("checked {checked} strata {by_strata} telling {telling}");
    assert!(checked > 600, "{checked} programs checked");
    assert!(by_strata > 300, "{by_strata} programs had several strata");
    assert!(telling > 30, "{telling} programs told the strata apart");
}

/// A labelled path relation of arity 3, recursive through itself only.
#[test]
fn a_labelled_path_relation_is_recursive() {
    let t = terms();
    let (rs, ids) = rels(&[3, 3]);
    let (edge, path) = (ids[0], ids[1]);
    let rules = vec![
        rule(
            "base",
            vec![pos(rel(edge, &[V(0), V(1), V(2)]))],
            vec![head(rel(path, &[V(0), V(1), V(2)]))],
        ),
        rule(
            "step",
            vec![
                pos(rel(path, &[V(0), V(1), V(2)])),
                pos(rel(edge, &[V(2), V(1), V(3)])),
            ],
            vec![head(rel(path, &[V(0), V(1), V(3)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    assert_eq!(p.shape(), lower::Shape::Monotone);
    let mut facts = Vec::new();
    for i in 0..20 {
        facts.push((Some(0), vec![n(i), n(7), n(i + 1)]));
        facts.push((Some(0), vec![n(i), n(8), n(i + 2)]));
    }
    let got = run(&t, &p, &facts);
    assert_eq!(got, reference(&p, &facts));
    let paths = got.iter().filter(|f| f.0 == Some(1)).count();
    // label 7: 20 + 19 + … + 1 paths; label 8: from each start, every second node
    assert_eq!(
        paths,
        210 + (1..=20).map(|i| (21 - i) / 2 + (21 - i) % 2).sum::<i64>() as usize
    );
}

/// A 5-ary join, with arities 0 and 1 beside it.
#[test]
fn a_five_ary_join_with_propositions() {
    let t = terms();
    let (rs, ids) = rels(&[5, 5, 0, 1]);
    let (a, b, go, out) = (ids[0], ids[1], ids[2], ids[3]);
    let rules = vec![rule(
        "j",
        vec![
            pos(rel(go, &[])),
            pos(rel(a, &[V(0), V(1), V(2), V(3), V(4)])),
            pos(rel(b, &[V(4), V(3), V(2), V(1), V(5)])),
        ],
        vec![head(rel(out, &[V(5)])), head(rel(go, &[]))],
    )];
    let p = program(&t, rs, rules);
    let facts = vec![
        (Some(2), vec![]),
        (Some(0), vec![n(1), n(2), n(3), n(4), n(5)]),
        (Some(0), vec![n(1), n(2), n(3), n(4), n(6)]),
        (Some(1), vec![n(5), n(4), n(3), n(2), n(50)]),
        (Some(1), vec![n(6), n(4), n(3), n(9), n(60)]),
    ];
    let got = run(&t, &p, &facts);
    assert!(got.contains(&(Some(3), vec![n(50)])));
    assert!(!got.contains(&(Some(3), vec![n(60)])));
    assert_eq!(got, reference(&p, &facts));
}

/// Relation facts never satisfy an atom or a `noValue` with a variable predicate, and
/// are invalid triples, which no write path keeps.
#[test]
fn relation_facts_are_hidden_from_variable_predicates() {
    let t = terms();
    let (rs, ids) = rels(&[2, 3, 1]);
    let (r2, r3, lonely) = (ids[0], ids[1], ids[2]);
    let seen = n(200);
    let ty = n(202);
    let rules = vec![
        rule(
            "any",
            vec![pos(IAtom::triple(V(0), V(1), V(2)))],
            vec![head(tri(V(0), seen, V(2)))],
        ),
        rule(
            "none",
            vec![
                pos(rel(r2, &[V(0), V(1)])),
                bi(B::NoValue, &[V(0), Any, V(1)]),
            ],
            vec![head(rel(lonely, &[V(0)]))],
        ),
        rule(
            "keep",
            vec![pos(rel(r2, &[V(0), V(1)]))],
            vec![head(rel(r3, &[V(0), V(1), V(1)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    let facts = vec![(Some(0), vec![n(1), n(2)]), (None, vec![n(1), ty, n(3)])];
    let mut g = graph(&t, &p, &facts);
    let mut w = WorkingWarnings::new(&t.budget());
    engine::run_program(&mut g, &p, &t, &limits(), &mut w).unwrap();
    let got: BTreeSet<Fact> = g.facts().map(|r| decode(&t, &p, r)).collect();
    // `any` sees only the triple, `none` holds for 1 since its relation facts are no
    // triples, and `keep` derives its tuple
    assert!(got.contains(&(None, vec![n(1), seen, n(3)])));
    assert!(
        !got.iter()
            .any(|f| f.0.is_none() && f.1[1] == seen && f.1[2] == n(2))
    );
    assert!(got.contains(&(Some(2), vec![n(1)])));
    assert!(got.contains(&(Some(1), vec![n(1), n(2), n(2)])));
    let mut keys = 0;
    for r in g.facts() {
        assert_eq!(is_key(r[1]), !(r[1] == seen || r[1] == ty), "{r:?}");
        if is_key(r[1]) {
            // no IRI predicate, so no write path keeps the row
            assert_ne!(t.kind(r[1]), crate::terms::Kind::Iri);
            assert!(!g.is_hidden(r[1]) || is_key(r[1]));
            keys += 1;
        }
    }
    assert_eq!(keys, 3);
}

/// `A(x) → R(x)`, `R(x) → (x p v)` and `A(x), noValue(x, p) → (x q v)` with relations
/// and no negated atom run by strata, so the `noValue` sees `(x p v)` and nothing is
/// derived for it, and the warning names the rule.
#[test]
fn novalue_beside_relations_reads_complete_lower_strata() {
    let t = terms();
    let (rs, ids) = rels(&[1, 1]);
    let (a, r) = (ids[0], ids[1]);
    let (p_, q, v) = (n(300), n(301), n(302));
    let rules = vec![
        rule(
            "mk",
            vec![pos(rel(a, &[V(0)]))],
            vec![head(rel(r, &[V(0)]))],
        ),
        rule(
            "give",
            vec![pos(rel(r, &[V(0)]))],
            vec![head(tri(V(0), p_, C(v)))],
        ),
        rule(
            "jena",
            vec![
                pos(IAtom::triple(V(0), C(n(303)), V(1))),
                bi(B::NoValue, &[V(0), C(p_)]),
            ],
            vec![head(tri(V(0), q, C(v)))],
        ),
        rule(
            "seed",
            vec![pos(rel(a, &[V(0)]))],
            vec![head(tri(V(0), n(303), V(0)))],
        ),
    ];
    let p = program(&t, rs, rules);
    assert_eq!(p.shape(), lower::Shape::Strata);
    let facts = vec![(Some(0), vec![n(1)])];
    let (got, warnings) = run_with(&t, &p, &facts, &limits()).unwrap();
    assert!(got.contains(&(None, vec![n(1), p_, v])));
    assert!(!got.contains(&(None, vec![n(1), q, v])));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("'jena'"), "{warnings:?}");
}

/// A negated atom reads a complete lower stratum.
#[test]
fn a_negation_reads_a_complete_lower_stratum() {
    let t = terms();
    let (rs, ids) = rels(&[2, 1, 1, 1]);
    let (e, reach, node, cut) = (ids[0], ids[1], ids[2], ids[3]);
    let rules = vec![
        rule(
            "start",
            vec![pos(rel(e, &[C(n(0)), V(0)]))],
            vec![head(rel(reach, &[V(0)]))],
        ),
        rule(
            "step",
            vec![pos(rel(reach, &[V(0)])), pos(rel(e, &[V(0), V(1)]))],
            vec![head(rel(reach, &[V(1)]))],
        ),
        rule(
            "cut",
            vec![pos(rel(node, &[V(0)])), neg(rel(reach, &[V(0)]))],
            vec![head(rel(cut, &[V(0)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    let mut facts: Vec<Fact> = (0..30).map(|i| (Some(2), vec![n(i)])).collect();
    facts.extend((0..20).map(|i| (Some(0), vec![n(i), n(i + 1)])));
    let got = run(&t, &p, &facts);
    let cut: BTreeSet<u64> = got
        .iter()
        .filter(|f| f.0 == Some(3))
        .map(|f| f.1[0])
        .collect();
    assert_eq!(cut, [0].into_iter().chain(21..30).map(n).collect());
    assert_eq!(got, reference(&p, &facts));
}

/// Two strata that each fit the limit of inferred facts and together exceed it.
#[test]
fn the_limit_of_inferred_facts_counts_across_strata() {
    let t = terms();
    let (rs, ids) = rels(&[1, 1, 1, 1]);
    let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
    let rules = vec![
        rule(
            "ab",
            vec![pos(rel(a, &[V(0)]))],
            vec![head(rel(b, &[V(0)]))],
        ),
        rule(
            "cd",
            vec![pos(rel(c, &[V(0)])), neg(rel(b, &[V(0)]))],
            vec![head(rel(d, &[V(0)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    assert_eq!(p.strata.count, 2);
    let mut facts: Vec<Fact> = (0..10).map(|i| (Some(0), vec![n(i)])).collect();
    facts.extend((10..20).map(|i| (Some(2), vec![n(i)])));
    let mut lim = limits();
    lim.max_inferred = 15;
    let e = run_with(&t, &p, &facts, &lim).unwrap_err();
    assert!(e.is::<crate::InferredLimitExceeded>(), "{e}");
    lim.max_inferred = 20;
    assert_eq!(run_with(&t, &p, &facts, &lim).unwrap().0.len(), 40);
}

/// The limit of iterations counts across strata.
#[test]
fn the_limit_of_iterations_counts_across_strata() {
    let t = terms();
    let (rs, ids) = rels(&[2, 2, 1, 1]);
    let (e, path, node, far) = (ids[0], ids[1], ids[2], ids[3]);
    let rules = vec![
        rule(
            "p0",
            vec![pos(rel(e, &[V(0), V(1)]))],
            vec![head(rel(path, &[V(0), V(1)]))],
        ),
        rule(
            "p1",
            vec![pos(rel(path, &[V(0), V(1)])), pos(rel(e, &[V(1), V(2)]))],
            vec![head(rel(path, &[V(0), V(2)]))],
        ),
        rule(
            "far",
            vec![pos(rel(node, &[V(0)])), neg(rel(path, &[C(n(0)), V(0)]))],
            vec![head(rel(far, &[V(0)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    let mut facts: Vec<Fact> = (0..10).map(|i| (Some(0), vec![n(i), n(i + 1)])).collect();
    facts.push((Some(2), vec![n(50)]));
    let mut g = graph(&t, &p, &facts);
    let mut w = WorkingWarnings::new(&t.budget());
    let out = engine::run_program(&mut g, &p, &t, &limits(), &mut w).unwrap();
    // the first stratum's iterations and the second's
    let lower = out.iterations - 2;
    let mut lim = limits();
    lim.max_iterations = lower + 1;
    let e = run_with(&t, &p, &facts, &lim).unwrap_err();
    assert!(e.to_string().contains("did not reach a fixpoint"), "{e}");
    lim.max_iterations = lower + 2;
    run_with(&t, &p, &facts, &lim).unwrap();
}

/// A program large enough for many work items gives the same decoded facts at 1, 4 and
/// 16 threads, with tails derived in several chunks of one iteration and tails holding
/// terms that builtins compute.
#[test]
fn relation_facts_are_the_same_at_any_thread_count() {
    let (rs, ids) = rels(&[3, 3]);
    let (e, path) = (ids[0], ids[1]);
    let rules = vec![
        rule(
            "base",
            vec![
                pos(rel(e, &[V(0), V(1), V(2)])),
                bi(B::Sum, &[V(1), C(n(1)), V(3)]),
            ],
            vec![head(rel(path, &[V(0), V(3), V(2)]))],
        ),
        rule(
            "step",
            vec![
                pos(rel(path, &[V(0), V(1), V(2)])),
                pos(rel(e, &[V(2), Any, V(3)])),
                bi(B::LessThan, &[V(1), C(n(3))]),
                bi(B::Sum, &[V(1), C(n(1)), V(4)]),
            ],
            vec![head(rel(path, &[V(0), V(4), V(3)]))],
        ),
    ];
    let mut facts = Vec::new();
    for i in 0..3000 {
        facts.push((Some(0), vec![n(i % 700), n(i % 3), n((i * 7 + 1) % 700)]));
    }
    let mut seen: Option<BTreeSet<Fact>> = None;
    for threads in [1, 4, 16] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let got = pool.install(|| {
            let t = terms();
            let p = program(&t, rs.clone(), rules.clone());
            run(&t, &p, &facts)
        });
        assert!(got.len() > 5_000, "{}", got.len());
        match &seen {
            None => seen = Some(got),
            Some(s) => assert!(*s == got, "{threads} threads"),
        }
    }
}

/// A list cell is the only new premise of `E(l), listMember(l, y) → R(y)`, with enough
/// rules and atoms that the run selects candidate rules by predicate.
#[test]
fn a_list_cell_alone_brings_a_list_instance() {
    let t = terms();
    let (rs, ids) = rels(&[1, 1, 1]);
    let (el, r, pad) = (ids[0], ids[1], ids[2]);
    let (first, rest, nil) = (t.rdf_first, t.rdf_rest, t.rdf_nil);
    let mut rules = vec![
        rule(
            "cells",
            vec![pos(rel(el, &[V(0)]))],
            vec![
                head(tri(V(0), first, C(n(9)))),
                head(tri(V(0), rest, C(nil))),
            ],
        ),
        rule(
            "member",
            vec![pos(rel(el, &[V(0)])), bi(B::ListMember, &[V(0), V(1)])],
            vec![head(rel(r, &[V(1)]))],
        ),
    ];
    // rules whose atoms outnumber the graph's predicates
    for i in 0..4 {
        rules.push(rule(
            &format!("pad{i}"),
            vec![
                pos(rel(pad, &[V(0)])),
                pos(rel(pad, &[V(0)])),
                pos(rel(pad, &[V(0)])),
            ],
            vec![head(rel(pad, &[V(0)]))],
        ));
    }
    let p = program(&t, rs, rules);
    let got = run(&t, &p, &[(Some(0), vec![n(1)]), (Some(2), vec![n(2)])]);
    assert!(got.contains(&(Some(1), vec![n(9)])), "{got:?}");
}

/// `E(l), listForAll(l, s, p) → R(l)`, where one iteration completes the list and adds an
/// unrelated `(s p b)`: the instance fires though the tested facts it reads are old.
#[test]
fn a_completed_list_fires_beside_an_unrelated_tested_fact() {
    let t = terms();
    let (rs, ids) = rels(&[1, 1, 1]);
    let (el, r, other) = (ids[0], ids[1], ids[2]);
    let (first, rest, nil) = (t.rdf_first, t.rdf_rest, t.rdf_nil);
    let (s, pp) = (n(400), n(401));
    let rules = vec![
        rule(
            "cells",
            vec![pos(rel(el, &[V(0)]))],
            vec![
                head(tri(V(0), first, C(n(9)))),
                head(tri(V(0), rest, C(nil))),
            ],
        ),
        rule(
            "unrelated",
            vec![pos(rel(other, &[V(0)]))],
            vec![head(tri(C(s), pp, V(0)))],
        ),
        rule(
            "all",
            vec![
                pos(rel(el, &[V(0)])),
                bi(B::ListForAll, &[V(0), C(s), C(pp)]),
            ],
            vec![head(rel(r, &[V(0)]))],
        ),
    ];
    let p = program(&t, rs, rules);
    let facts = vec![
        (Some(0), vec![n(1)]),
        (Some(2), vec![n(77)]),
        (None, vec![s, pp, n(9)]),
    ];
    let got = run(&t, &p, &facts);
    assert!(got.contains(&(Some(1), vec![n(1)])), "{got:?}");
}

/// A program of Jena rules alone runs as Jena's: the same facts in the same order, with
/// Jena's reading of a `noValue` that does not stratify.
#[test]
fn jena_programs_run_unchanged() {
    let text = "@prefix ex: <http://ex.org/> .
        [a: (?x ex:p ?y) -> (?y ex:p ?x)]
        [b: (?x ex:p ?y), (?y ex:p ?z), notEqual(?x, ?z) -> (?x ex:q ?z)]
        [c: (?x ex:q ?y), noValue(?y, ex:r) -> (?y ex:r ?x)]
        [d: (?x ex:r ?y) -> (?x ex:s ?y)]";
    let t = terms();
    let rules = crate::parse_rules(text).unwrap();
    let mut w = WorkingWarnings::new(&t.budget());
    let compiled = crate::engine::compile(&rules, &t, &mut w).unwrap();
    let p = Program::new(
        Relations::default(),
        compiled.iter().map(Rule::from_crule).collect(),
        &vocab(&t),
        &t.budget(),
    )
    .unwrap();
    assert_eq!(p.shape(), lower::Shape::Jena);
    let ex = |l: &str| {
        let iri = oxrdf::NamedNode::new_unchecked(format!("http://ex.org/{l}"));
        t.id_for(&iri.into()).unwrap()
    };
    let base: Vec<[u64; 3]> = (0..40)
        .map(|i| {
            [
                ex(&format!("n{i}")),
                ex("p"),
                ex(&format!("n{}", (i * 3 + 1) % 40)),
            ]
        })
        .collect();
    let mut a = Graph::default();
    a.add_batch(base.iter().copied());
    let mut b = Graph::default();
    b.add_batch(base.iter().copied());
    engine::run(&mut a, &compiled, &t, &limits(), None).unwrap();
    let mut w = WorkingWarnings::new(&t.budget());
    engine::run_program(&mut b, &p, &t, &limits(), &mut w).unwrap();
    assert_eq!(a.facts().collect::<Vec<_>>(), b.facts().collect::<Vec<_>>());
    assert!(w.is_empty());
    // the lowering keeps each builtin's occurrence, which analyses key fresh nodes by
    let lowered = p.lower(&t.budget()).unwrap();
    let origins = |rs: &[crate::engine::CRule]| -> Vec<Vec<usize>> {
        rs.iter()
            .map(|r| r.builtins.iter().map(|b| b.origin).collect())
            .collect()
    };
    assert_eq!(origins(&lowered.strata[0]), origins(&compiled));
    assert!(origins(&compiled).iter().flatten().any(|&o| o > 0));
}

/// A negated atom with a wildcard in its tail holds only when no fact of the relation
/// matches its other columns.
#[test]
fn a_wildcard_in_a_negated_tail_matches_any_value() {
    let t = terms();
    let (rs, ids) = rels(&[1, 3, 1]);
    let (a, r, out) = (ids[0], ids[1], ids[2]);
    let rules = vec![rule(
        "w",
        vec![pos(rel(a, &[V(0)])), neg(rel(r, &[V(0), Any, C(n(3))]))],
        vec![head(rel(out, &[V(0)]))],
    )];
    let p = program(&t, rs, rules);
    let facts = vec![
        (Some(0), vec![n(1)]),
        (Some(0), vec![n(2)]),
        (Some(1), vec![n(1), n(2), n(3)]),
        (Some(1), vec![n(2), n(2), n(4)]),
    ];
    let got = run(&t, &p, &facts);
    assert!(!got.contains(&(Some(2), vec![n(1)])), "{got:?}");
    assert!(got.contains(&(Some(2), vec![n(2)])), "{got:?}");
    assert_eq!(got, reference(&p, &facts));
}

/// The lowered rules are charged: a program whose IR fits a budget and whose rules split
/// into many parts, each with the whole body, is refused when the parts do not fit.
#[test]
fn the_lowered_rules_are_charged() {
    let t = terms();
    let arities = [1u16; 12];
    let (rs, ids) = rels(&arities);
    let body: Vec<Literal> = (0..40)
        .map(|i| pos(rel(ids[0], &[V(i % 4)])))
        .chain([neg(rel(ids[1], &[V(0)]))])
        .collect();
    let heads: Vec<Head> = (2..12).map(|i| head(rel(ids[i], &[V(0)]))).collect();
    let rules = vec![
        rule("wide", body, heads),
        rule(
            "b",
            vec![pos(rel(ids[0], &[V(0)]))],
            vec![head(rel(ids[1], &[V(0)]))],
        ),
    ];
    let budget = Budget::new(None);
    let p = Program::new(rs, rules, &vocab(&t), &budget).unwrap();
    let ir = budget.used();
    assert!(ir > 0);
    let all = p.lower(&budget).unwrap();
    let lowered = budget.used() - ir;
    assert!(lowered > ir, "{lowered} against {ir}");
    drop(all);
    let tight = Budget::new(Some(ir));
    assert!(matches!(p.lower(&tight), Err(lower::LowerError::Memory(_))));
    assert_eq!(tight.used(), 0);
    // the charge is what the lowered rules keep, for these and for `not R() → S()`,
    // whose lowered builtin has more arguments than its atom
    let (rs2, ids2) = rels(&[0, 0]);
    let small = rule(
        "n",
        vec![neg(rel(ids2[0], &[]))],
        vec![head(rel(ids2[1], &[]))],
    );
    let p2 = program(&t, rs2, vec![small]);
    for p in [&p, &p2] {
        let b = Budget::new(None);
        let l = p.lower(&b).unwrap();
        let kept: usize = l
            .strata
            .iter()
            .flatten()
            .map(lower::crule_bytes)
            .sum::<usize>()
            + l.strata.len() * size_of::<Vec<crate::engine::CRule>>()
            + l.strata.iter().map(|s| s.capacity()).sum::<usize>()
                * size_of::<crate::engine::CRule>()
            + l.stratified_novalue.capacity() * size_of::<usize>();
        assert_eq!(l.bytes(), kept);
    }
}

/// A program of triple rules alone over a graph that holds relation facts does not see
/// them, whoever added them.
#[test]
fn triple_rules_beside_relation_facts_do_not_see_them() {
    let t = terms();
    let (rs, ids) = rels(&[2]);
    let seen = n(200);
    let any = rule(
        "any",
        vec![pos(IAtom::triple(V(0), V(1), V(2)))],
        vec![head(tri(V(0), seen, V(2)))],
    );
    let p = program(&t, rs, vec![any]);
    assert_eq!(p.shape(), lower::Shape::Jena);
    let facts = vec![(Some(ids[0].0), vec![n(1), n(2)])];
    // a graph that the program's run marks, and one that the caller marked
    let mut g = Graph::default();
    g.add_batch(facts.iter().map(|f| row(&t, &p, f)));
    let mut w = WorkingWarnings::new(&t.budget());
    engine::run_program(&mut g, &p, &t, &limits(), &mut w).unwrap();
    assert_eq!(g.len(), 1);
    assert_eq!(run(&t, &p, &facts).len(), 1);
}

/// A rebuilt graph, such as a compaction makes, still hides the facts of relations.
#[test]
fn a_rebuilt_graph_hides_relation_facts() {
    let mut g = Graph::default();
    let k = key(RelId(0));
    assert!(!g.is_hidden(k));
    g.set_relations();
    assert!(g.is_hidden(k));
    let h = g.try_like(0, &Budget::unlimited(), true).unwrap();
    assert!(h.is_hidden(k));
    assert!(!h.is_hidden(n(5)));
}

/// An existential head and `makeTemp` in a rule that reads lists are refused by name.
#[test]
fn unsupported_rules_are_refused_by_name() {
    let t = terms();
    let (rs, ids) = rels(&[1, 2]);
    let mut r = rule(
        "ex",
        vec![pos(rel(ids[0], &[V(0)]))],
        vec![head(rel(ids[1], &[V(0), V(1)]))],
    );
    r.exists = vec![1];
    let p = program(&t, rs.clone(), vec![r]);
    let e = run_with(&t, &p, &[], &limits()).unwrap_err();
    assert!(e.to_string().contains("'ex'"), "{e}");
    let temp = rule(
        "temp",
        vec![
            pos(rel(ids[0], &[V(0)])),
            bi(B::ListMember, &[V(0), V(1)]),
            bi(B::MakeTemp, &[V(2)]),
        ],
        vec![head(rel(ids[1], &[V(1), V(2)]))],
    );
    let p = program(&t, rs, vec![temp]);
    let e = run_with(&t, &p, &[], &limits()).unwrap_err();
    assert!(
        e.to_string().contains("'temp'") && e.to_string().contains("makeTemp"),
        "{e}"
    );
}

/// A run whose tails do not fit its budget fails with a memory error, and holds nothing
/// afterwards.
#[test]
fn tails_are_charged() {
    let (rs, ids) = rels(&[3]);
    let r3 = ids[0];
    let rules = vec![rule(
        "grow",
        vec![
            pos(rel(r3, &[V(0), V(1), V(2)])),
            bi(B::LessThan, &[V(1), C(n(20_000))]),
            bi(B::Sum, &[V(1), C(n(1)), V(3)]),
        ],
        vec![head(rel(r3, &[V(0), V(3), V(2)]))],
    )];
    let budget = Budget::new(Some(4 << 20));
    let t = Terms::new_in(
        Store::in_memory(StoreOptions::default()).snapshot(),
        &budget,
    )
    .unwrap();
    let p = Program::new(rs, rules, &vocab(&t), &budget).unwrap();
    let mut lim = limits();
    lim.budget = budget.clone();
    let facts: Vec<Fact> = (0..8).map(|i| (Some(0), vec![n(i), n(0), n(i)])).collect();
    let mut g = Graph::default();
    g.set_budget(&budget).unwrap();
    for f in &facts {
        g.add(row(&t, &p, f));
    }
    let mut w = WorkingWarnings::new(&budget);
    let e = engine::run_program(&mut g, &p, &t, &lim, &mut w).unwrap_err();
    assert!(
        crate::budget::BudgetFailure::from_anyhow(&e).is_some(),
        "{e}"
    );
}

/// Cancellation stops a long run of relation iterations.
#[test]
fn a_cancelled_relation_run_stops() {
    let t = terms();
    let (rs, ids) = rels(&[3]);
    let r3 = ids[0];
    let rules = vec![rule(
        "grow",
        vec![
            pos(rel(r3, &[V(0), V(1), V(2)])),
            bi(B::Sum, &[V(1), C(n(1)), V(3)]),
        ],
        vec![head(rel(r3, &[V(0), V(3), V(2)]))],
    )];
    let p = program(&t, rs, rules);
    let cancel = std::sync::Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let mut lim = limits();
    lim.cancel = Some(cancel);
    lim.progress = Some(std::sync::Arc::new(move |_f, msg: &str| {
        if msg.starts_with("iteration 50 ") {
            c2.store(true, Ordering::Relaxed);
        }
    }));
    let e = run_with(&t, &p, &[(Some(0), vec![n(1), n(0), n(1)])], &lim).unwrap_err();
    assert!(e.to_string().contains("cancelled"), "{e}");
}

/// The probe of a tail column (item 839's plan, Q9): a binary relation and a 5-ary
/// relation joined on a column of the tail, against the same program written with
/// tuple triples. `cargo test --release -- --ignored tail_column_probe --nocapture`.
#[test]
#[ignore]
fn tail_column_probe() {
    let rounds = 5;
    let size: i64 = std::env::var("PROBE_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200_000);
    // the relation program: q(x) for each fact of `wide` whose third column `link` holds
    let time = |make: &dyn Fn(&Terms) -> (Program, Vec<Fact>)| {
        let mut best = f64::MAX;
        let mut derived = 0;
        for _ in 0..rounds {
            let t = terms();
            let (p, facts) = make(&t);
            let mut g = graph(&t, &p, &facts);
            let before = g.len();
            let mut w = WorkingWarnings::new(&t.budget());
            let start = std::time::Instant::now();
            engine::run_program(&mut g, &p, &t, &limits(), &mut w).unwrap();
            best = best.min(start.elapsed().as_secs_f64());
            derived = g.len() - before;
        }
        (best, derived)
    };
    let link = |i: i64| n(1_000_000 + i % 1000);
    let rel_prog = |arity: u16| {
        move |t: &Terms| {
            let (rs, ids) = rels(&[arity, 1, 1]);
            let (w, l, q) = (ids[0], ids[1], ids[2]);
            let cols: Vec<Slot> = (0..arity as usize).map(V).collect();
            let rules = vec![rule(
                "probe",
                vec![pos(rel(l, &[V(1)])), pos(rel(w, &cols))],
                vec![head(rel(q, &[V(0)]))],
            )];
            let p = program(t, rs, rules);
            let mut facts: Vec<Fact> = (0..size)
                .map(|i| {
                    let mut c = vec![n(i), link(i)];
                    c.extend((2..arity as i64).map(|j| n(i + j)));
                    (Some(0), c)
                })
                .collect();
            facts.extend((0..1000).step_by(10).map(|i| (Some(1), vec![link(i)])));
            (p, facts)
        }
    };
    // the triple program: a tuple node per fact with one triple per column
    let triple_prog = |arity: i64| {
        move |t: &Terms| {
            let (rs, ids) = rels(&[1, 1]);
            let (l, q) = (ids[0], ids[1]);
            let col = |j: i64| n(2_000_000 + j);
            let mut body = vec![pos(rel(l, &[V(1)]))];
            body.push(pos(tri(V(9), col(1), V(1))));
            body.push(pos(tri(V(9), col(0), V(0))));
            for j in 2..arity {
                body.push(pos(tri(V(9), col(j), V(10 + j as usize))));
            }
            let rules = vec![rule("probe", body, vec![head(rel(q, &[V(0)]))])];
            let p = program(t, rs, rules);
            let mut facts: Vec<Fact> = Vec::new();
            for i in 0..size {
                let node = n(3_000_000 + i);
                facts.push((None, vec![node, col(0), n(i)]));
                facts.push((None, vec![node, col(1), link(i)]));
                for j in 2..arity {
                    facts.push((None, vec![node, col(j), n(i + j)]));
                }
            }
            facts.extend((0..1000).step_by(10).map(|i| (Some(0), vec![link(i)])));
            (p, facts)
        }
    };
    for arity in [2u16, 5] {
        let (r, rd) = time(&rel_prog(arity));
        let (tr, td) = time(&triple_prog(arity as i64));
        assert_eq!(rd, td);
        println!(
            "tail_column_probe arity={arity} size={size} relation_s={r:.4} triples_s={tr:.4} ratio={:.3} derived={rd}",
            r / tr
        );
    }
}
