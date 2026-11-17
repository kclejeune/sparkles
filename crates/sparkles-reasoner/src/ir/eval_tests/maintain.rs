//! Maintenance of closures with relation facts (item 839's plan, Phase 3, §12.2).

use super::*;
use crate::facts::Base;
use crate::incremental::tests::{Vocab as TV, set_default_graph};
use crate::incremental::{Closure, Fallback, update_with};
use rustc_hash::FxHashSet;
use sparkles_core::store::Snapshot;
use std::sync::Arc;

type Triple = [u64; 3];

fn unlimited() -> Arc<Budget> {
    Budget::unlimited()
}

/// Program `rules` over `rels`, checked against the terms of `v`'s store.
fn checked(v: &TV, rels: Relations, rules: Vec<Rule>) -> Program {
    let t = Terms::new(v.store.snapshot());
    Program::new(rels, rules, &Vocab::of(&t), &unlimited()).unwrap()
}

/// The closure of `p` over `base` by a full run.
fn full(v: &TV, p: &Program, base: &FxHashSet<Triple>) -> Closure {
    let snap = set_default_graph(v, base);
    let terms = Terms::new(snap.clone());
    let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
    let mut g = Graph::with_base(Arc::new(b));
    g.set_relations();
    let mut w = WorkingWarnings::new(&terms.budget());
    engine::run_program(&mut g, p, &terms, &limits(), &mut w).unwrap();
    Closure::new(g, terms).unwrap()
}

/// Every fact of `c`, both tiers, decoded.
fn render(c: &Closure, p: &Program) -> BTreeSet<Fact> {
    let mut out: BTreeSet<Fact> = c.live().map(|(t, _)| decode(&c.terms, p, t)).collect();
    c.graph
        .base()
        .unwrap()
        .for_each(None, None, None, |t| {
            out.insert((None, t.to_vec()));
            true
        })
        .unwrap();
    out
}

/// `c` as it reads back from the dataset at `snap`: the new input graphs as its base
/// tier, its derived triples and no relation facts, with terms of its own.
fn read_back(c: &Closure, snap: &Arc<Snapshot>) -> Closure {
    let terms = Terms::new(snap.clone());
    let b = Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
    let mut g = Graph::with_base(Arc::new(b));
    g.add_batch(
        c.live()
            .filter(|(t, flag)| !flag && !is_key(t[1]))
            .map(|(t, _)| t),
    );
    Closure::new(g, terms).unwrap()
}

/// Bring `c` to `base`, the input after `inserted` and `removed`, kept in memory or read
/// back from the dataset.
fn step(
    v: &TV,
    p: &Program,
    c: Closure,
    base: &FxHashSet<Triple>,
    inserted: &[Triple],
    removed: &[Triple],
    kept: bool,
) -> anyhow::Result<Closure> {
    let snap = set_default_graph(v, base);
    let mut c = if kept {
        let mut c = c;
        let moved = c.terms.rebase(snap.clone()).unwrap();
        c.remap(&moved).unwrap();
        c.set_inputs(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
        c
    } else {
        read_back(&c, &snap)
    };
    let lowered = p.lower(&unlimited()).unwrap();
    assert_eq!(lowered.strata.len(), 1, "a monotone program");
    let rules = &lowered.strata[0];
    assert!(
        rules
            .iter()
            .all(|r| engine::incremental_blocker(r, &c.terms).is_none())
    );
    let part = p.relation_part(&Vocab::of(&c.terms), &unlimited()).unwrap();
    let rebuild = if kept { None } else { part.as_ref() };
    update_with(&mut c, rules, &limits(), inserted, removed, None, rebuild)?;
    c.compact_if_needed(false).unwrap();
    Ok(c)
}

/// A random monotone program over relations of arities 0 to 5, base predicates `p0` and
/// `p1` and derived predicates `p2` and `p3`, whose constants and facts are four
/// individuals of `v`.
fn random_monotone(v: &TV, rng: &mut Rng) -> Option<Program> {
    let arities = [0u16, 1, 2, 3, 5];
    let (rels, ids) = rels(&arities);
    let ind = |rng: &mut Rng| v.inds[rng.below(4)];
    let mut rules = Vec::new();
    for ri in 0..3 + rng.below(6) {
        let mut vars: Vec<usize> = Vec::new();
        let mut body = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let mut slot = |rng: &mut Rng| -> Slot {
                if rng.below(6) == 0 {
                    C(ind(rng))
                } else {
                    let x = rng.below(4);
                    vars.push(x);
                    V(x)
                }
            };
            let i = rng.below(arities.len() + 2);
            body.push(pos(if i >= arities.len() {
                let s = slot(rng);
                let o = slot(rng);
                // the base predicates, and the derived ones now and then
                let p = v.props[if rng.below(3) == 0 {
                    2 + rng.below(2)
                } else {
                    i - arities.len()
                }];
                tri(s, p, o)
            } else {
                let args: Vec<Slot> = (0..arities[i]).map(|_| slot(rng)).collect();
                rel(ids[i], &args)
            }));
        }
        vars.sort_unstable();
        vars.dedup();
        let bound = |rng: &mut Rng| -> Slot {
            if vars.is_empty() || rng.below(5) == 0 {
                C(ind(rng))
            } else {
                V(vars[rng.below(vars.len())])
            }
        };
        let i = rng.below(arities.len() + 2);
        let h = if i >= arities.len() {
            let s = bound(rng);
            let o = bound(rng);
            tri(s, v.props[2 + (i - arities.len())], o)
        } else {
            let args: Vec<Slot> = (0..arities[i]).map(|_| bound(rng)).collect();
            rel(ids[i], &args)
        };
        rules.push(rule(&format!("m{ri}"), body, vec![head(h)]));
    }
    let t = Terms::new(v.store.snapshot());
    let p = Program::new(rels, rules, &Vocab::of(&t), &unlimited()).ok()?;
    (p.shape() == super::super::lower::Shape::Monotone).then_some(p)
}

fn random_triple(v: &TV, rng: &mut Rng) -> Triple {
    [
        v.inds[rng.below(4)],
        v.props[rng.below(2)],
        v.inds[rng.below(4)],
    ]
}

/// Random histories of insertions and deletions over random monotone programs with
/// relations, with the closure kept and read back, each step equal to a full run.
#[test]
fn random_histories_match_full_runs() {
    let v = TV::new();
    let (mut steps, mut programs, mut relation_rows) = (0, 0, 0usize);
    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let Some(p) = random_monotone(&v, &mut rng) else {
            continue;
        };
        programs += 1;
        for kept in [true, false] {
            let mut rng = Rng(seed ^ 0x5555 | 1);
            let mut base: FxHashSet<Triple> =
                (0..10).map(|_| random_triple(&v, &mut rng)).collect();
            let mut c = full(&v, &p, &base);
            for s in 0..6 {
                let mut removed = Vec::new();
                for _ in 0..rng.below(3) {
                    if let Some(&t) = base.iter().nth(rng.below(base.len().max(1)))
                        && base.remove(&t)
                    {
                        removed.push(t);
                    }
                }
                let mut inserted = Vec::new();
                for _ in 0..rng.below(3) {
                    let t = random_triple(&v, &mut rng);
                    if !base.contains(&t) && !removed.contains(&t) && base.insert(t) {
                        inserted.push(t);
                    }
                }
                c = step(&v, &p, c, &base, &inserted, &removed, kept).unwrap_or_else(|e| {
                    panic!("seed {seed} step {s} kept {kept}: {e:#}");
                });
                let want = full(&v, &p, &base);
                let (a, b) = (render(&c, &p), render(&want, &p));
                if a != b {
                    let extra: Vec<_> = a.difference(&b).collect();
                    let missing: Vec<_> = b.difference(&a).collect();
                    panic!(
                        "seed {seed} step {s} kept {kept}: removed {removed:?} inserted {inserted:?}\nrules {:?}\nextra {extra:?}\nmissing {missing:?}",
                        p.rules
                    );
                }
                relation_rows += c.live().filter(|(t, _)| is_key(t[1])).count();
                steps += 1;
            }
        }
    }
    eprintln!("programs {programs} steps {steps} relation rows {relation_rows}");
    assert!(programs > 150, "{programs} programs");
    assert!(relation_rows > 1000, "{relation_rows} relation rows");
}

/// A chain of six properties through `chain/4` and a key of four values through
/// `keyed/3`, after `owl2rl.rls`: `chain-end` derives `(x p2 y)` and `key-end` `(x p3 y)`.
fn chain_and_key(v: &TV) -> Program {
    let mut rels = Relations::default();
    // chain(list, i, x, y): the chain's first i properties lead from x to y
    let chain = rels.declare("chain", 4, false).unwrap();
    // keyed(x, i, k): x has the key's first i values k (one value per property here)
    let keyed = rels.declare("keyed", 3, false).unwrap();
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let mut rules = vec![rule(
        "chain-0",
        vec![pos(tri(V(0), p(0), V(1)))],
        vec![head(rel(chain, &[C(ind(7)), C(n(1)), V(0), V(1)]))],
    )];
    for i in 1..6 {
        rules.push(rule(
            &format!("chain-{i}"),
            vec![
                pos(rel(chain, &[C(ind(7)), C(n(i as i64)), V(0), V(1)])),
                pos(tri(V(1), p(i), V(2))),
            ],
            vec![head(rel(
                chain,
                &[C(ind(7)), C(n(i as i64 + 1)), V(0), V(2)],
            ))],
        ));
    }
    rules.push(rule(
        "chain-end",
        vec![pos(rel(chain, &[C(ind(7)), C(n(6)), V(0), V(1)]))],
        vec![head(tri(V(0), v.props[2], V(1)))],
    ));
    // keys over p0 four times by position, standing for four properties
    rules.push(rule(
        "key-0",
        vec![pos(tri(V(0), p(0), V(1)))],
        vec![head(rel(keyed, &[V(0), C(n(1)), V(1)]))],
    ));
    for i in 1..4 {
        rules.push(rule(
            &format!("key-{i}"),
            vec![
                pos(rel(keyed, &[V(0), C(n(i)), V(1)])),
                pos(tri(V(0), p(1), V(2))),
            ],
            vec![head(rel(keyed, &[V(0), C(n(i + 1)), V(1)]))],
        ));
    }
    rules.push(rule(
        "key-end",
        vec![
            pos(rel(keyed, &[V(0), C(n(4)), V(2)])),
            pos(rel(keyed, &[V(1), C(n(4)), V(2)])),
        ],
        vec![head(tri(V(0), v.props[3], V(1)))],
    ));
    checked(v, rels, rules)
}

/// Chains and keys written as relations, after `owl2rl.rls`: a chain of six properties
/// through `chain/4` and a key of four properties through `keyed/3`, maintained across
/// deletions that break a recursive chain and insertions, kept and read back.
#[test]
fn chains_and_keys_as_relations_are_maintained() {
    let v = TV::new();
    let prog = chain_and_key(&v);
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    for kept in [true, false] {
        // a chain i0 → i1 → … → i6 alternating p0 and p1
        let mut base: FxHashSet<Triple> = (0..6).map(|i| [ind(i), p(i), ind(i + 1)]).collect();
        base.insert([ind(1), p(0), ind(2)]);
        base.insert([ind(2), p(1), ind(2)]);
        let mut c = full(&v, &prog, &base);
        assert!(render(&c, &prog).contains(&(None, vec![ind(0), v.props[2], ind(6)])));
        let history: [(&[Triple], &[Triple]); 4] = [
            (&[], &[[ind(3), p(3), ind(4)]]),
            (&[[ind(3), p(3), ind(4)]], &[]),
            (&[[ind(5), p(0), ind(3)]], &[[ind(0), p(0), ind(1)]]),
            (&[[ind(0), p(0), ind(1)]], &[[ind(2), p(1), ind(2)]]),
        ];
        for (k, (ins, rem)) in history.iter().enumerate() {
            for t in *rem {
                assert!(base.remove(t));
            }
            for t in *ins {
                assert!(base.insert(*t));
            }
            c = step(&v, &prog, c, &base, ins, rem, kept).unwrap();
            assert_eq!(
                render(&c, &prog),
                render(&full(&v, &prog, &base), &prog),
                "step {k} kept {kept}"
            );
        }
    }
}

/// The review's case: `P(x) → R(x)`, `R(x) → B(x)`, with the first `P(a)` inserted into
/// a closure read back from the dataset, and the same through a relation of arity 3.
#[test]
fn a_relation_fact_of_an_inserted_fact_supports_its_consequences() {
    let v = TV::new();
    for arity in [1u16, 3] {
        let mut rels = Relations::default();
        let r = rels.declare("R", arity, false).unwrap();
        let args: Vec<Slot> = (0..arity as usize)
            .map(|i| if i == 0 { V(0) } else { C(v.inds[i]) })
            .collect();
        let prog = checked(
            &v,
            rels,
            vec![
                rule(
                    "p-r",
                    vec![pos(tri(V(0), v.props[0], C(v.inds[0])))],
                    vec![head(rel(r, &args))],
                ),
                rule(
                    "r-b",
                    vec![pos(rel(r, &args))],
                    vec![head(tri(V(0), v.props[2], C(v.inds[0])))],
                ),
            ],
        );
        let mut base = FxHashSet::default();
        let c = full(&v, &prog, &base);
        let a = [v.inds[1], v.props[0], v.inds[0]];
        base.insert(a);
        let c = step(&v, &prog, c, &base, &[a], &[], false).unwrap();
        let got = render(&c, &prog);
        assert!(
            got.contains(&(None, vec![v.inds[1], v.props[2], v.inds[0]])),
            "arity {arity}: {got:?}"
        );
        assert_eq!(got, render(&full(&v, &prog, &base), &prog));
        // and the deletion of the same fact from a read-back closure
        base.remove(&a);
        let c = step(&v, &prog, c, &base, &[], &[a], false).unwrap();
        assert_eq!(render(&c, &prog), render(&full(&v, &prog, &base), &prog));
    }
}

/// A local IRI that only a tail holds, made by `uriConcat`, which a later explicit fact
/// brings into the store: the tail moves with the term, and a join on its column matches.
#[test]
fn a_tail_moves_with_its_local_terms() {
    let v = TV::new();
    // IRIs of the store, the same in every terms of the store
    let (pre, suf) = (v.inds[0], v.inds[1]);
    let mut rels = Relations::default();
    let r = rels.declare("R", 3, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "make",
                vec![
                    pos(tri(V(0), v.props[0], V(1))),
                    bi(B::UriConcat, &[C(pre), C(suf), V(2)]),
                ],
                vec![head(rel(r, &[V(0), V(2), V(1)]))],
            ),
            rule(
                "join",
                vec![
                    pos(rel(r, &[V(0), V(2), V(1)])),
                    pos(tri(V(2), v.props[1], V(3))),
                ],
                vec![head(tri(V(0), v.props[2], V(3)))],
            ),
        ],
    );
    let mut base: FxHashSet<Triple> = [[v.inds[0], v.props[0], v.inds[1]]].into_iter().collect();
    let c = full(&v, &prog, &base);
    let local = c
        .live()
        .find(|(t, _)| is_key(t[1]))
        .and_then(|(t, _)| c.terms.tail(t[2]).map(|(_, mut cols)| cols.next().unwrap()))
        .unwrap();
    assert_eq!(Id(local).tag(), sparkles_core::id::Tag::Local);
    // the store learns the IRI through an explicit fact about it
    let new = {
        let mut txn = v.store.write();
        let id = txn
            .intern_key(&sparkles_core::id::iri_key(&format!(
                "{0}i0{0}i1",
                crate::incremental::tests::EX
            )))
            .unwrap();
        txn.commit().unwrap();
        id.0
    };
    let t = [new, v.props[1], v.inds[2]];
    base.insert(t);
    let c = step(&v, &prog, c, &base, &[t], &[], true).unwrap();
    let got = render(&c, &prog);
    assert!(
        got.contains(&(None, vec![v.inds[0], v.props[2], v.inds[2]])),
        "{got:?}"
    );
    assert!(got.contains(&(Some(0), vec![v.inds[0], new, v.inds[1]])));
    assert_eq!(got, render(&full(&v, &prog, &base), &prog));
}

/// The backward search of a deletion matches a tuple head without making the tails of
/// the instances it tries.
#[test]
fn a_search_does_not_grow_the_terms() {
    let v = TV::new();
    let mut rels = Relations::default();
    let r = rels.declare("R", 3, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![rule(
            "r",
            vec![
                pos(tri(V(0), v.props[0], V(1))),
                pos(tri(V(1), v.props[1], V(2))),
            ],
            vec![head(rel(r, &[V(0), V(1), V(2)]))],
        )],
    );
    let i = |k: usize| v.inds[k];
    let mut base: FxHashSet<Triple> = FxHashSet::default();
    for k in 1..5 {
        base.insert([i(0), v.props[0], i(k)]);
        for m in 5..8 {
            base.insert([i(k), v.props[1], i(m)]);
        }
    }
    let c = full(&v, &prog, &base);
    let gone = [i(1), v.props[1], i(5)];
    base.remove(&gone);
    let full_c = step(
        &v,
        &prog,
        full(
            &v,
            &prog,
            &FxHashSet::from_iter(base.iter().copied().chain([gone])),
        ),
        &base,
        &[],
        &[gone],
        true,
    )
    .unwrap();
    assert_eq!(
        render(&full_c, &prog),
        render(&full(&v, &prog, &base), &prog)
    );
    // over terms that hold no tail, as a closure read back without its relation part
    // has, the consequences of the deletion try tails that are not interned
    let snap = set_default_graph(&v, &base);
    let mut c = read_back(&c, &snap);
    let before = c.terms.heap_bytes();
    let lowered = prog.lower(&unlimited()).unwrap();
    update_with(
        &mut c,
        &lowered.strata[0],
        &limits(),
        &[],
        &[gone],
        None,
        None,
    )
    .unwrap();
    assert_eq!(c.terms.heap_bytes(), before, "the search interned tails");
}

/// A program with a negated atom is not maintained, and a relation part that makes
/// terms falls back after a read back.
#[test]
fn negation_and_new_terms_are_not_maintained() {
    let v = TV::new();
    let mut rels = Relations::default();
    let r = rels.declare("R", 1, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "r",
                vec![pos(tri(V(0), v.props[0], V(1)))],
                vec![head(rel(r, &[V(0)]))],
            ),
            rule(
                "not-r",
                vec![pos(tri(V(1), v.props[0], V(0))), neg(rel(r, &[V(0)]))],
                vec![head(tri(V(0), v.props[2], V(1)))],
            ),
        ],
    );
    let t = Terms::new(v.store.snapshot());
    let lowered = prog.lower(&unlimited()).unwrap();
    assert!(
        lowered
            .strata
            .iter()
            .flatten()
            .any(|r| engine::incremental_blocker(r, &t).is_some_and(|w| w.contains("negated")))
    );

    let mut rels = Relations::default();
    let r = rels.declare("R", 2, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![rule(
            "temp",
            vec![pos(tri(V(0), v.props[0], V(1))), bi(B::MakeTemp, &[V(2)])],
            vec![head(rel(r, &[V(0), V(2)]))],
        )],
    );
    let mut base: FxHashSet<Triple> = [[v.inds[0], v.props[0], v.inds[1]]].into_iter().collect();
    let c = full(&v, &prog, &base);
    let a = [v.inds[2], v.props[0], v.inds[1]];
    base.insert(a);
    let snap = set_default_graph(&v, &base);
    let mut c = read_back(&c, &snap);
    let lowered = prog.lower(&unlimited()).unwrap();
    let part = prog
        .relation_part(&Vocab::of(&c.terms), &unlimited())
        .unwrap();
    let e = update_with(
        &mut c,
        &lowered.strata[0],
        &limits(),
        &[a],
        &[],
        None,
        part.as_ref(),
    )
    .unwrap_err();
    assert!(e.downcast_ref::<Fallback>().is_some(), "{e:#}");
}

/// Write the derived triples of `c` into the inferred graph, as a materialization does,
/// and return the snapshot and the inferred graph's id.
fn write_inferred(v: &TV, c: &Closure) -> (Arc<Snapshot>, Id) {
    let mut txn = v.store.write();
    let g = txn
        .intern_key(&sparkles_core::id::iri_key(crate::INFERRED_GRAPH))
        .unwrap();
    for (t, flag) in c.live() {
        if !flag && c.terms.valid(&t) {
            txn.insert([Id(t[0]), Id(t[1]), Id(t[2]), g]).unwrap();
        }
    }
    txn.commit().unwrap();
    (v.store.snapshot(), g)
}

fn find<'d>(
    d: &'d crate::explain::Derivation,
    f: &dyn Fn(&crate::explain::Derivation) -> bool,
    out: &mut Vec<&'d crate::explain::Derivation>,
) {
    if f(d) {
        out.push(d);
    }
    for p in &d.premises {
        find(p, f, out);
    }
}

/// `Why` through a chain of six properties and a key of four values: relation premises
/// are nodes of their own with their rules, no tail handle among the bindings, the
/// chain's triples as the leaves, and a relation node cut as a `ref` at the depth limit.
#[test]
fn explanations_show_relation_nodes() {
    use crate::explain::{DerivationKind as K, ExplainOptions, Explained, explain_program};
    let v = TV::new();
    let prog = chain_and_key(&v);
    let p = |i: usize| v.props[i % 2];
    let ind = |i: usize| v.inds[i];
    let mut base: FxHashSet<Triple> = (0..6).map(|i| [ind(i), p(i), ind(i + 1)]).collect();
    // a second value property for the key
    base.insert([ind(2), p(1), ind(2)]);
    let c = full(&v, &prog, &base);
    let (snap, inferred) = write_inferred(&v, &c);
    let term = |x: u64| c.terms.term(x).unwrap();
    let explain = |t: Triple, depth: u32| {
        let opts = ExplainOptions {
            max_depth: depth,
            ..Default::default()
        };
        let triple = [term(t[0]), term(t[1]), term(t[2])];
        match explain_program(
            snap.clone(),
            &prog,
            &[Id::DEFAULT_GRAPH],
            Some(inferred),
            &[],
            &triple,
            &opts,
        )
        .unwrap()
        {
            Explained::Found { trees, .. } => trees.into_iter().next().unwrap(),
            e => panic!("{t:?}: {e:?}"),
        }
    };
    let tree = explain([ind(0), v.props[2], ind(6)], 32);
    assert_eq!(tree.rule.as_ref().unwrap().name, "chain-end");
    let top = &tree.premises[0];
    let atom = top.relation.as_ref().expect("a relation node");
    assert_eq!(atom.name, "chain");
    assert_eq!(atom.args.len(), 4);
    assert_eq!(top.kind, K::Derived);
    assert_eq!(top.rule.as_ref().unwrap().name, "chain-5");
    assert!(!top.generalized);
    // no tail handle and no lowered builtin is shown
    let mut all = Vec::new();
    find(&tree, &|_| true, &mut all);
    for d in &all {
        assert!(d.bindings.len() <= 3, "{:?}", d.bindings);
        assert!(d.builtins.is_empty(), "{:?}", d.builtins);
    }
    let mut leaves = Vec::new();
    find(&tree, &|d| d.kind == K::Asserted, &mut leaves);
    let got: BTreeSet<String> = leaves.iter().map(|d| format!("{:?}", d.triple)).collect();
    let want: BTreeSet<String> = (0..6)
        .map(|i| format!("{:?}", [term(ind(i)), term(p(i)), term(ind(i + 1))]))
        .collect();
    assert_eq!(got, want);
    let mut rels = Vec::new();
    find(&tree, &|d| d.relation.is_some(), &mut rels);
    assert_eq!(rels.len(), 6, "one node per chain step");
    // a depth limit cuts a relation node as a ref, which keeps its fact
    let cut = explain([ind(0), v.props[2], ind(6)], 3);
    let mut refs = Vec::new();
    find(&cut, &|d| d.kind == K::Ref, &mut refs);
    assert!(
        refs.iter()
            .any(|d| d.relation.as_ref().is_some_and(|r| r.name == "chain"))
    );
    // the key: two individuals with the same four values
    let key = explain([ind(2), v.props[3], ind(2)], 32);
    assert_eq!(key.rule.as_ref().unwrap().name, "key-end");
    assert!(
        key.premises
            .iter()
            .all(|d| d.relation.as_ref().is_some_and(|r| r.name == "keyed"))
    );
}

/// The witnesses of a program with relations equal those of the same program written
/// with tuple triples, and the report counts no relation fact.
#[test]
fn witnesses_pass_through_relations() {
    use crate::witness::Witnesses;
    let v = TV::new();
    let (p0, p1, p2, p3, p4) = (v.props[0], v.props[1], v.props[2], v.props[3], v.props[4]);
    let mut rels = Relations::default();
    let ch = rels.declare("chain", 3, false).unwrap();
    let with_rel = checked(
        &v,
        rels,
        vec![
            rule(
                "s1",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(rel(ch, &[C(n(1)), V(0), V(1)]))],
            ),
            rule(
                "s2",
                vec![
                    pos(rel(ch, &[C(n(1)), V(0), V(1)])),
                    pos(tri(V(1), p1, V(2))),
                ],
                vec![head(rel(ch, &[C(n(2)), V(0), V(2)]))],
            ),
            rule(
                "s3",
                vec![
                    pos(rel(ch, &[C(n(2)), V(0), V(1)])),
                    pos(tri(V(1), p0, V(2))),
                ],
                vec![head(tri(V(0), p2, V(2)))],
            ),
        ],
    );
    let with_triples = checked(
        &v,
        Relations::default(),
        vec![
            rule(
                "s1",
                vec![pos(tri(V(0), p0, V(1)))],
                vec![head(tri(V(0), p3, V(1)))],
            ),
            rule(
                "s2",
                vec![pos(tri(V(0), p3, V(1))), pos(tri(V(1), p1, V(2)))],
                vec![head(tri(V(0), p4, V(2)))],
            ),
            rule(
                "s3",
                vec![pos(tri(V(0), p4, V(1))), pos(tri(V(1), p0, V(2)))],
                vec![head(tri(V(0), p2, V(2)))],
            ),
        ],
    );
    let i = |k: usize| v.inds[k];
    let base: FxHashSet<Triple> = [
        [i(0), p0, i(1)],
        [i(1), p1, i(2)],
        [i(2), p0, i(3)],
        [i(0), p0, i(4)],
        [i(4), p1, i(2)],
        [i(1), p1, i(5)],
        [i(5), p0, i(3)],
    ]
    .into_iter()
    .collect();
    let mut out = Vec::new();
    for prog in [&with_rel, &with_triples] {
        let mut c = full(&v, prog, &base);
        let lowered = prog.lower(&unlimited()).unwrap();
        let w = Witnesses::build(&mut c, &lowered.strata[0], 8, &limits()).unwrap();
        let ends: Vec<Triple> = c.live().map(|x| x.0).filter(|t| t[1] == p2).collect();
        let report: BTreeSet<String> = ends
            .iter()
            .map(|t| format!("{t:?} {:?}", w.report_of(&c, t).unwrap()))
            .collect();
        let derived = c.live().filter(|(t, f)| !f && !is_key(t[1])).count() as u64;
        assert_eq!(w.report.facts, derived);
        out.push(report);
    }
    assert_eq!(out[0], out[1]);
    assert!(!out[0].is_empty());
}

/// A rule with a negated atom gives no complete support (round 9's case): with `A(a)` and
/// `T(a)`, `B(a)` follows, but not from `A(a)` alone.
#[test]
fn witnesses_under_a_negated_atom_are_not_complete() {
    use crate::witness::Witnesses;
    let v = TV::new();
    let (a, t, b) = (v.props[0], v.props[1], v.props[2]);
    let mut rels = Relations::default();
    let r = rels.declare("R", 1, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "r",
                vec![pos(tri(V(0), a, V(1))), neg(tri(V(0), t, Any))],
                vec![head(rel(r, &[V(0)]))],
            ),
            rule(
                "b",
                vec![pos(tri(V(0), a, V(1))), neg(rel(r, &[V(0)]))],
                vec![head(tri(V(0), b, V(1)))],
            ),
            rule(
                "down",
                vec![pos(tri(V(0), b, V(1)))],
                vec![head(tri(V(1), b, V(0)))],
            ),
        ],
    );
    let i = |k: usize| v.inds[k];
    let base: FxHashSet<Triple> = [[i(0), a, i(1)], [i(0), t, i(1)]].into_iter().collect();
    let mut c = full(&v, &prog, &base);
    let lowered = prog.lower(&unlimited()).unwrap();
    let mut lowered = lowered;
    let rules: Vec<CRule> = std::mem::take(&mut lowered.strata)
        .into_iter()
        .flatten()
        .collect();
    let w = Witnesses::build(&mut c, &rules, 8, &limits()).unwrap();
    for f in [[i(0), b, i(1)], [i(1), b, i(0)]] {
        let ann = w.report_of(&c, &f).unwrap();
        assert!(ann.truncated, "{f:?}: {ann:?}");
    }
}

/// A blank node constant among a tuple head's columns is a new blank node in each run,
/// so the program is not maintained.
#[test]
fn a_blank_node_in_a_tuple_head_blocks_maintenance() {
    let v = TV::new();
    let t = Terms::new(v.store.snapshot());
    let blank = t.try_fresh_bnode().unwrap();
    let mut rels = Relations::default();
    let r = rels.declare("R", 3, false).unwrap();
    let prog = Program::new(
        rels,
        vec![
            rule(
                "seed",
                vec![pos(tri(V(0), v.props[0], V(1)))],
                vec![head(rel(r, &[V(0), C(v.inds[0]), C(blank)]))],
            ),
            rule(
                "p",
                vec![pos(rel(r, &[V(0), C(v.inds[0]), V(1)]))],
                vec![
                    head(tri(V(0), v.props[2], V(1))),
                    head(tri(V(0), v.props[3], V(1))),
                ],
            ),
        ],
        &Vocab::of(&t),
        &unlimited(),
    )
    .unwrap();
    let lowered = prog.lower(&unlimited()).unwrap();
    let seed = &lowered.strata[0][0];
    assert!(
        engine::incremental_blocker(seed, &t).is_some_and(|w| w.contains("blank node")),
        "{:?}",
        engine::incremental_blocker(seed, &t)
    );
    assert!(super::super::rebuild::makes_terms(seed, &t));
}

/// The inferred facts of the rebuild and of the insertion count against one limit, so a
/// read-back update is refused where the kept and the full run are.
#[test]
fn a_rebuild_and_its_insertion_share_the_limit_of_inferred_facts() {
    let v = TV::new();
    let mut rels = Relations::default();
    let r = rels.declare("R", 1, false).unwrap();
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "p-r",
                vec![pos(tri(V(0), v.props[0], C(v.inds[0])))],
                vec![head(rel(r, &[V(0)]))],
            ),
            rule(
                "r-b",
                vec![pos(rel(r, &[V(0)]))],
                vec![head(tri(V(0), v.props[2], C(v.inds[0])))],
            ),
        ],
    );
    let lowered = prog.lower(&unlimited()).unwrap();
    let a = [v.inds[1], v.props[0], v.inds[0]];
    let base: FxHashSet<Triple> = [a].into_iter().collect();
    // the limits of inferred facts, refused and accepted, and a limit of iterations that
    // the full run, the kept update and the read-back update all meet
    for (max, iterations, ok) in [(1usize, 10_000, false), (2, 10_000, true), (2, 3, true)] {
        let lim = Limits {
            max_inferred: max,
            max_iterations: iterations,
            ..limits()
        };
        // full
        let snap = set_default_graph(&v, &base);
        let terms = Terms::new(snap.clone());
        let mut g = Graph::with_base(Arc::new(
            Base::new(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap(),
        ));
        let mut w = WorkingWarnings::new(&terms.budget());
        assert_eq!(
            engine::run_program(&mut g, &prog, &terms, &lim, &mut w).is_ok(),
            ok,
            "full {max}"
        );
        for kept in [true, false] {
            let c = full(&v, &prog, &FxHashSet::default());
            let snap = set_default_graph(&v, &base);
            let mut c = if kept {
                let mut c = c;
                c.set_inputs(snap.clone(), &[Id::DEFAULT_GRAPH]).unwrap();
                c
            } else {
                read_back(&c, &snap)
            };
            let part = prog
                .relation_part(&Vocab::of(&c.terms), &unlimited())
                .unwrap();
            let rebuild = if kept { None } else { part.as_ref() };
            let got = update_with(&mut c, &lowered.strata[0], &lim, &[a], &[], None, rebuild);
            assert_eq!(got.is_ok(), ok, "kept {kept} max {max}: {:?}", got.err());
        }
    }
}

/// An explanation rebuilds relation facts from the generalized facts of the closure too,
/// given as term keys, whose local terms it interns again (review p3-r1, finding 1), and
/// stops when it is cancelled or past its deadline during the rebuild (finding 2).
#[test]
fn explanations_rebuild_from_generalized_facts() {
    use crate::explain::{ExplainOptions, Explained, explain_program};
    use sparkles_core::explain::ExplainBudgetExceeded;
    let v = TV::new();
    let mut rels = Relations::default();
    let r = rels.declare("R", 1, false).unwrap();
    // `gen` makes a literal the store does not hold, the subject of a generalized fact
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "gen",
                vec![
                    pos(tri(V(0), v.props[0], V(1))),
                    bi(B::StrConcat, &[C(v.lits[0]), C(v.lits[1]), V(2)]),
                ],
                vec![head(tri(V(2), v.props[1], V(1)))],
            ),
            rule(
                "rel",
                vec![pos(tri(V(0), v.props[1], V(1))), bi(B::IsLiteral, &[V(0)])],
                vec![head(rel(r, &[V(1)]))],
            ),
            rule(
                "out",
                vec![pos(rel(r, &[V(0)]))],
                vec![head(tri(V(0), v.props[2], V(0)))],
            ),
        ],
    );
    let base: FxHashSet<Triple> = [[v.inds[0], v.props[0], v.inds[1]]].into_iter().collect();
    let c = full(&v, &prog, &base);
    let generalized: Vec<Triple> = c
        .live()
        .map(|x| x.0)
        .filter(|t| !is_key(t[1]) && !c.terms.valid(t))
        .collect();
    assert_eq!(generalized.len(), 1);
    assert_eq!(Id(generalized[0][0]).tag(), sparkles_core::id::Tag::Local);
    let keys: Vec<[Vec<u8>; 3]> = generalized
        .iter()
        .map(|t| t.map(|x| c.terms.key_of(x).unwrap()))
        .collect();
    let (snap, inferred) = write_inferred(&v, &c);
    let term = |x: u64| c.terms.term(x).unwrap();
    let target = [term(v.inds[1]), term(v.props[2]), term(v.inds[1])];
    let run = |keys: &[[Vec<u8>; 3]], target: &[oxrdf::Term; 3], opts: &ExplainOptions| {
        explain_program(
            snap.clone(),
            &prog,
            &[Id::DEFAULT_GRAPH],
            Some(inferred),
            keys,
            target,
            opts,
        )
    };
    let opts = ExplainOptions::default();
    let found = run(&keys, &target, &opts).unwrap();
    assert!(matches!(found, Explained::Found { .. }), "{found:?}");
    assert!(matches!(
        run(&[], &target, &opts).unwrap(),
        Explained::NotFound
    ));
    let cancelled = ExplainOptions {
        cancel: Some(Arc::new(AtomicBool::new(true))),
        ..Default::default()
    };
    let e = run(&keys, &target, &cancelled).unwrap_err();
    assert!(format!("{e:#}").contains("cancelled"), "{e:#}");
    // a triple the closure lacks, which a search never reaches: only the rebuild can read
    // the passed deadline
    let absent = [term(v.inds[0]), term(v.props[2]), term(v.inds[0])];
    assert!(matches!(
        run(&keys, &absent, &opts).unwrap(),
        Explained::NotFound
    ));
    let late = ExplainOptions {
        deadline: Some(std::time::Instant::now()),
        ..Default::default()
    };
    let e = run(&keys, &absent, &late).unwrap_err();
    assert!(
        e.downcast_ref::<ExplainBudgetExceeded>()
            .is_some_and(|b| b.0.contains("timeout")),
        "{e:#}"
    );
}

/// Two facts of one relation never share a node, and a repeated one does.
#[test]
fn relation_nodes_share_by_their_fact() {
    use sparkles_core::explain::{Derivation, RelationAtom, RuleRef, finish};
    let iri = |s: &str| {
        oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(format!(
            "http://ex.org/{s}"
        )))
    };
    let rule = RuleRef {
        name: "r".into(),
        profile: "rules".into(),
        owl2rl: None,
    };
    let leaf = |s: &str| Derivation::asserted([iri(s), iri("p"), iri("o")], Some("default".into()));
    let node = |a: &str| {
        let atom = RelationAtom {
            name: "R".into(),
            args: vec![iri(a)],
        };
        let t = oxrdf::Term::NamedNode(atom.iri());
        let mut d = Derivation::derived([t.clone(), t.clone(), t], rule.clone(), vec![leaf(a)]);
        d.relation = Some(atom);
        d
    };
    let mut trees = vec![Derivation::derived(
        [iri("x"), iri("q"), iri("y")],
        rule.clone(),
        vec![node("a"), node("b"), node("a")],
    )];
    finish(&mut trees, 32);
    let ps = &trees[0].premises;
    assert_ne!(ps[0].id, ps[1].id);
    assert!(!ps[1].shared);
    assert_eq!(ps[2].id, ps[0].id);
    assert!(ps[2].shared);
}

/// A rebase keeps the tail keys it copies charged until it has interned the moved tails
/// (review p3-r1, finding 3): the least budget that admits it exceeds the terms it leaves
/// by at least the copies.
#[test]
fn a_rebase_charges_its_copied_tails_until_it_has_moved_them() {
    use sparkles_core::id::Tag;
    const N: u64 = 64;
    let v = TV::new();
    let mut runs = 0;
    // a rebase of `N` tails over one local IRI that the store learns, under `limit`: the
    // number of moved ids, or `None` when the limit refuses the setup or the rebase, and
    // the bytes used after it. The IRIs differ per run and have equal lengths.
    let mut run = |limit: Option<usize>| -> (Option<usize>, usize) {
        runs += 1;
        let budget = Budget::new(limit);
        let mut terms = Terms::new(v.store.snapshot());
        if terms.set_budget(&budget).is_err() {
            return (None, 0);
        }
        let iri = format!("http://ex.org/moved-{runs:04}");
        let Ok(local) = terms.try_id_for(&oxrdf::Term::NamedNode(oxrdf::NamedNode::new_unchecked(
            iri.clone(),
        ))) else {
            return (None, 0);
        };
        assert_eq!(Id(local).tag(), Tag::Local);
        for i in 0..N {
            if terms.try_tail(7, &[local, i + 1, v.inds[0]]).is_err() {
                return (None, 0);
            }
        }
        // the store learns the IRI through a fact about it
        let mut txn = v.store.write();
        let id = txn.intern_key(&sparkles_core::id::iri_key(&iri)).unwrap();
        txn.insert([id, Id(v.props[1]), Id(v.inds[2]), Id::DEFAULT_GRAPH])
            .unwrap();
        txn.commit().unwrap();
        let got = terms.rebase(v.store.snapshot()).ok().map(|m| m.len());
        (got, budget.used())
    };
    let (got, after) = run(None);
    assert_eq!(got, Some(1 + N as usize), "the IRI and every tail move");
    // the least limit that admits the setup and the rebase
    let (mut lo, mut hi) = (0usize, after * 4);
    assert!(run(Some(hi)).0.is_some());
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if run(Some(mid)).0.is_some() {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    // a copied key: the tail prefix, the relation and three columns, and its vector entry
    let copies = N as usize * (3 + 8 * 4 + size_of::<(Vec<u8>, u64)>());
    assert!(
        hi >= after + copies,
        "least limit {hi}, {after} used after the rebase, copies {copies}"
    );
}

/// A blank node constant in a relation head is one term of the closure, which a backward
/// search finds, so the witnesses below it are complete and the same whether the constant
/// sits among a tuple head's columns (arity 3) or in the triple a relation of arity 2 is
/// written as (review p3-r1, finding 5, refuted): each is the seed fact.
#[test]
fn witnesses_under_a_blank_node_in_a_relation_head_are_complete() {
    use crate::witness::Witnesses;
    let v = TV::new();
    let mut reports = Vec::new();
    for arity in [3u16, 2] {
        let t = Terms::new(v.store.snapshot());
        let blank = t.try_fresh_bnode().unwrap();
        let mut rels = Relations::default();
        let r = rels.declare("R", arity, false).unwrap();
        let (made, read): (Vec<Slot>, Vec<Slot>) = if arity == 3 {
            (
                vec![V(0), C(v.inds[0]), C(blank)],
                vec![V(0), C(v.inds[0]), V(1)],
            )
        } else {
            (vec![V(0), C(blank)], vec![V(0), V(1)])
        };
        let prog = Program::new(
            rels,
            vec![
                rule(
                    "seed",
                    vec![pos(tri(V(0), v.props[0], V(1)))],
                    vec![head(rel(r, &made))],
                ),
                rule(
                    "p",
                    vec![pos(rel(r, &read))],
                    vec![
                        head(tri(V(0), v.props[2], V(1))),
                        head(tri(V(0), v.props[3], V(1))),
                    ],
                ),
            ],
            &Vocab::of(&t),
            &unlimited(),
        )
        .unwrap();
        let base: FxHashSet<Triple> = [[v.inds[1], v.props[0], v.inds[2]]].into_iter().collect();
        let snap = set_default_graph(&v, &base);
        let b = Base::new(snap, &[Id::DEFAULT_GRAPH]).unwrap();
        let mut g = Graph::with_base(Arc::new(b));
        g.set_relations();
        let mut w = WorkingWarnings::new(&t.budget());
        engine::run_program(&mut g, &prog, &t, &limits(), &mut w).unwrap();
        let mut c = Closure::new(g, t).unwrap();
        let lowered = prog.lower(&unlimited()).unwrap();
        let w = Witnesses::build(&mut c, &lowered.strata[0], 8, &limits()).unwrap();
        let below: Vec<Triple> = c
            .live()
            .map(|x| x.0)
            .filter(|t| t[1] == v.props[2] || t[1] == v.props[3])
            .collect();
        assert_eq!(below.len(), 2, "arity {arity}");
        let mut got = BTreeSet::new();
        for f in below {
            let ann = w.report_of(&c, &f).unwrap();
            assert!(!ann.truncated, "arity {arity}, {f:?}: {ann:?}");
            assert_eq!(ann.witnesses.len(), 1, "arity {arity}: {ann:?}");
            got.insert(format!("{:?}", ann.witnesses));
        }
        // both triples have the seed fact as their one support
        assert_eq!(got.len(), 1, "arity {arity}: {got:?}");
        reports.push(got);
    }
    assert_eq!(reports[0], reports[1]);
}

/// `clone_bytes`, which `relation_part` charges before it copies a rule (review p3-r3),
/// bounds the copy: a spilled argument list clones with its capacity rounded up to a
/// power of two, past `bytes`, and each copy in the part is within its source's bound.
/// That the charge comes before the copy is checked by reading `relation_part`.
#[test]
fn a_relation_part_copy_is_within_its_clone_bound() {
    let v = TV::new();
    let mut rels = Relations::default();
    let r = rels.declare("R", 5, false).unwrap();
    let five = [V(0), V(1), V(2), V(3), V(4)];
    let prog = checked(
        &v,
        rels,
        vec![
            rule(
                "seed",
                vec![pos(tri(V(0), v.props[0], V(1)))],
                vec![head(rel(r, &[V(0), V(1), V(1), V(0), V(1)]))],
            ),
            rule(
                "again",
                vec![pos(rel(r, &five))],
                vec![
                    head(rel(r, &[V(1), V(0), V(2), V(3), V(4)])),
                    head(tri(V(0), v.props[2], V(1))),
                ],
            ),
        ],
    );
    for src in &prog.rules {
        // a list of five slots spills with capacity 5 and clones with capacity 8
        assert!(src.clone().bytes() > src.bytes(), "{}", src.name);
        assert!(src.clone().bytes() <= src.clone_bytes(), "{}", src.name);
    }
    let t = Terms::new(v.store.snapshot());
    let part = prog
        .relation_part(&Vocab::of(&t), &unlimited())
        .unwrap()
        .unwrap();
    for (copy, src) in part.rules.iter().zip(&prog.rules) {
        assert_eq!(copy.name, src.name);
        assert_eq!(copy.head.len(), 1);
        assert!(copy.bytes() <= src.clone_bytes(), "{}", src.name);
    }
}
