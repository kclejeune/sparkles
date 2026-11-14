//! Rules specialized to the schema, for the demand transformation (spec F17 §3.2).
//!
//! The rules of RDFS and OWL 2 RL join a schema fact with an instance fact:
//! `(?p rdfs:domain ?c), (?x ?p ?y) -> (?x rdf:type ?c)`. Over these rules a demand on
//! `?x rdf:type ex:Student` asks for the facts of every property with a domain, since the
//! magic predicate of the instance atom holds the properties of every domain the schema
//! has. A schema pass first derives the schema in full, which is small and the same for
//! every query. Each rule's schema atoms are then joined over it, and the rule becomes one
//! rule per schema binding with those atoms gone and their variables constants:
//! `(?x ex:takesCourse ?y) -> (?x rdf:type ex:Student)`. The demand transformation then
//! splits every demand by its constant predicate and class, so a query asks only for the
//! facts of the properties and classes that can give its answers.
//!
//! The schema atoms are those of the RDFS and OWL vocabulary that [`Vocab`] names, with
//! the RDF list atoms (`rdf:first`, `rdf:rest`) whose subject such an atom binds and the
//! builtins that test or read the values they bind, such as `listMember` over an
//! `owl:intersectionOf` list. A rule whose schema join has more than [`MAX_BINDINGS`]
//! bindings is kept as it is, and so is every rule after the program reaches
//! [`MAX_RULES`] specialized rules. Its schema atoms still read the schema the pass
//! derived. The rules left out, which the reuse of tables across commits reads, are kept
//! up to [`MAX_RULES`] instances, past which a rule as written stands for its instances,
//! and the budget is charged for them.

use crate::budget::{Budget, BudgetFailure, Held};
use crate::builtins::BuiltinKind;
use crate::demand::{Closed, Vocab, clone_builtin};
use crate::engine::{self, Atom, CBuiltin, CHead, CRule, Eval, Mode, Slot};
use crate::facts::Memo;
use crate::graph::Graph;
use crate::terms::Terms;
use rustc_hash::FxHashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The most schema bindings a rule is specialized to.
pub(crate) const MAX_BINDINGS: usize = 4096;
/// The most specialized rules a program may have.
pub(crate) const MAX_RULES: usize = 20_000;

/// The builtins a schema join may evaluate: tests, and list reads that bind members.
fn schema_builtin(k: BuiltinKind) -> bool {
    use BuiltinKind::*;
    matches!(
        k,
        Equal
            | NotEqual
            | LessThan
            | GreaterThan
            | Le
            | Ge
            | IsLiteral
            | NotLiteral
            | IsBNode
            | NotBNode
            | IsDType
            | NotDType
            | IsFunctor
            | NotFunctor
            | ListMember
            | ListContains
            | ListNotContains
            | ListLength
            | ListEntry
    )
}

/// The schema join of rule `r`: its atoms and builtins, and the variables they bind.
/// `None` when the rule has no schema atom.
fn prefix(
    r: &CRule,
    closed: &Closed,
    lists: &[u64],
) -> Option<(Vec<usize>, Vec<usize>, Vec<bool>)> {
    let mut bound = vec![false; r.nvars];
    let mut atoms: Vec<usize> = Vec::new();
    let bind = |a: &Atom, bound: &mut Vec<bool>| {
        for s in [a.s, a.p, a.o] {
            if let Slot::Var(v) = s {
                bound[v] = true;
            }
        }
    };
    for (i, a) in r.atoms.iter().enumerate() {
        if closed.closes(a) {
            atoms.push(i);
            bind(a, &mut bound);
        }
    }
    if atoms.is_empty() {
        return None;
    }
    // list atoms whose subject the schema binds, to their fixpoint
    let mut grew = true;
    while grew {
        grew = false;
        for (i, a) in r.atoms.iter().enumerate() {
            if atoms.contains(&i) {
                continue;
            }
            let list = matches!(a.p, Slot::Const(p) if lists.contains(&p));
            let subject = match a.s {
                Slot::Const(_) => true,
                Slot::Var(v) => bound[v],
                Slot::Any => false,
            };
            if list && subject {
                atoms.push(i);
                bind(a, &mut bound);
                grew = true;
            }
        }
    }
    let mut builtins: Vec<usize> = Vec::new();
    let mut grew = true;
    while grew {
        grew = false;
        for (j, b) in r.builtins.iter().enumerate() {
            if builtins.contains(&j) || !schema_builtin(b.kind) {
                continue;
            }
            let args_known = b.args.iter().all(|s| match s {
                Slot::Var(v) => bound[*v] || b.outputs.contains(v),
                _ => true,
            });
            if args_known && b.inputs.iter().all(|&v| bound[v]) {
                builtins.push(j);
                for &v in &b.outputs {
                    bound[v] = true;
                }
                grew = true;
            }
        }
    }
    Some((atoms, builtins, bound))
}

/// `r` without its schema join (`atoms`, `builtins`), its variables `vals` gives a value
/// for replaced by those values.
fn rest(r: &CRule, atoms: &[usize], builtins: &[usize], vals: &[Option<u64>]) -> CRule {
    let sub = |s: Slot| match s {
        Slot::Var(v) => vals[v].map_or(s, Slot::Const),
        _ => s,
    };
    CRule {
        name: r.name.clone(),
        nvars: r.nvars,
        atoms: r
            .atoms
            .iter()
            .enumerate()
            .filter(|(i, _)| !atoms.contains(i))
            .map(|(_, a)| Atom {
                s: sub(a.s),
                p: sub(a.p),
                o: sub(a.o),
            })
            .collect(),
        builtins: r
            .builtins
            .iter()
            .enumerate()
            .filter(|(j, _)| !builtins.contains(j))
            .map(|(_, b)| CBuiltin {
                origin: b.origin,
                kind: b.kind,
                args: b.args.iter().map(|&s| sub(s)).collect(),
                inputs: b
                    .inputs
                    .iter()
                    .copied()
                    .filter(|&v| vals[v].is_none())
                    .collect(),
                outputs: b
                    .outputs
                    .iter()
                    .copied()
                    .filter(|&v| vals[v].is_none())
                    .collect(),
                regex: b.regex.clone(),
            })
            .collect(),
        head: r
            .head
            .iter()
            .map(|h| match h {
                CHead::Triple(s, p, o) => CHead::Triple(sub(*s), sub(*p), sub(*o)),
                CHead::Action(a, args) => CHead::Action(*a, args.iter().map(|&s| sub(s)).collect()),
            })
            .collect(),
    }
}

/// The rules of a demand program over the schema that `g` holds in full, from
/// [`specialize`].
pub(crate) struct Specialized {
    pub rules: Vec<CRule>,
    /// the rules left out because their every head gives only facts `closed` says are
    /// complete, as written or once specialized to a binding of their schema join. A
    /// change their atoms read can change the schema, so the reuse of tables across
    /// commits reads them too (spec F17 §6.2).
    pub left_out: Vec<CRule>,
    /// the budget's charge for `left_out`, which holds at most [`MAX_RULES`] instances
    /// and the rules as written that stand for the instances past that bound
    pub held: Held,
}

/// The rules of a demand program over the schema that `g` holds in full: each rule
/// specialized to the bindings of its schema join, without the rules whose heads give
/// only facts `closed` says are complete. The joins stop when `cancel` is set, and the
/// rules made so far are then returned, which the caller drops.
pub(crate) fn specialize(
    rules: &[CRule],
    closed: &Closed,
    lists: &[u64],
    g: &Graph,
    terms: &Terms,
    budget: &Arc<Budget>,
    cancel: &AtomicBool,
) -> Result<Specialized, BudgetFailure> {
    let abort = AtomicBool::new(false);
    let mut out: Vec<CRule> = Vec::new();
    let mut dropped: Vec<CRule> = Vec::new();
    let mut held = Held::new(budget);
    let mut dropped_made = 0usize;
    let mut seen_out: FxHashSet<RuleKey> = FxHashSet::default();
    let mut made = 0usize;
    let left_out = |r: &CRule| {
        r.head
            .iter()
            .all(|h| matches!(h, CHead::Triple(..)) && closed.closes(&head_atom(h)))
    };
    for r in rules {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if left_out(r) {
            held.try_grow(rule_bytes(r))?;
            dropped.push(r.clone_shallow());
            continue;
        }
        let Some((atoms, builtins, bound)) = prefix(r, closed, lists) else {
            out.push(r.clone_shallow());
            continue;
        };
        let pre = CRule {
            name: r.name.clone(),
            nvars: r.nvars,
            atoms: atoms.iter().map(|&i| r.atoms[i].clone()).collect(),
            builtins: builtins
                .iter()
                .map(|&j| clone_builtin(&r.builtins[j]))
                .collect(),
            head: Vec::new(),
        };
        let ranges = vec![(0, g.len()); pre.atoms.len()];
        let Some(steps) = engine::plan(&pre, &ranges, Some(g)) else {
            out.push(r.clone_shallow());
            continue;
        };
        let mut ev = Eval::new(
            g,
            terms,
            &pre,
            &steps,
            g.len(),
            &abort,
            Some(cancel),
            usize::MAX,
            Mode::New,
            budget,
        );
        ev.collect_at = Some((steps.len(), MAX_BINDINGS + 1));
        let mut b = vec![0u64; r.nvars];
        ev.run(0, &mut b, None);
        if let Some(e) = ev.err.take() {
            return Err(BudgetFailure::from_anyhow(&e)
                .unwrap_or_else(|| panic!("a schema join failed: {e:#}")));
        }
        let collected = std::mem::take(&mut ev.collected);
        drop(ev);
        if collected.len() > MAX_BINDINGS || made + collected.len() > MAX_RULES {
            out.push(r.clone_shallow());
            continue;
        }
        let mut seen: FxHashSet<RuleKey> = FxHashSet::default();
        // past the bound on the instances left out, the rule as written stands for the
        // rest of them, since its atoms are more general
        let mut written = false;
        for c in collected {
            let vals: Vec<Option<u64>> = (0..r.nvars).map(|v| bound[v].then_some(c[v])).collect();
            let s = expand_lists(rest(r, &atoms, &builtins, &vals), g, terms);
            if left_out(&s) {
                if written {
                    continue;
                }
                if dropped_made >= MAX_RULES {
                    held.try_grow(rule_bytes(r))?;
                    dropped.push(r.clone_shallow());
                    written = true;
                } else if seen_out.insert(rule_key(&s)) {
                    held.try_grow(rule_bytes(&s))?;
                    dropped_made += 1;
                    dropped.push(s);
                }
                continue;
            }
            // the same rule from another binding, its atoms in another order
            if seen.insert(rule_key(&s)) {
                made += 1;
                out.push(s);
            }
        }
    }
    Ok(Specialized {
        rules: out,
        left_out: dropped,
        held,
    })
}

/// An estimate of the bytes a compiled rule and the key that dedupes it hold.
fn rule_bytes(r: &CRule) -> usize {
    use std::mem::size_of;
    size_of::<CRule>()
        + r.name.len()
        + r.atoms.len() * (size_of::<Atom>() + size_of::<[Slot; 3]>())
        + r.head.len() * (size_of::<CHead>() + 64)
        + r.builtins.len() * (size_of::<CBuiltin>() + 64)
        + 64
}

/// A specialized rule as its atoms in a canonical order, its builtins and its head, so
/// that the same rule from another binding, its atoms in another order, is made once.
/// A structural key, rather than the rule's text, since every query specializes the
/// rules again (spec F17, item 911).
type RuleKey = (
    Vec<[Slot; 3]>,
    Vec<(crate::builtins::BuiltinKind, Vec<Slot>)>,
    Vec<(Option<crate::builtins::HeadAction>, Vec<Slot>)>,
);

fn rule_key(r: &CRule) -> RuleKey {
    let mut atoms: Vec<[Slot; 3]> = r.atoms.iter().map(|a| [a.s, a.p, a.o]).collect();
    atoms.sort_unstable();
    let builtins = r
        .builtins
        .iter()
        .map(|b| (b.kind, b.args.clone()))
        .collect();
    let head = r
        .head
        .iter()
        .map(|h| match h {
            CHead::Triple(s, p, o) => (None, vec![*s, *p, *o]),
            CHead::Action(a, args) => (Some(*a), args.clone()),
        })
        .collect();
    (atoms, builtins, head)
}

/// `r` with each `listForAll(L, ?y, P)` whose list and predicate are constants replaced by
/// the atoms `(?y P m)` for the members `m` of `L`, so that its facts are asked for like
/// those of any other atom.
fn expand_lists(mut r: CRule, g: &Graph, terms: &Terms) -> CRule {
    let mut kept = Vec::with_capacity(r.builtins.len());
    for b in std::mem::take(&mut r.builtins) {
        if b.kind == BuiltinKind::ListForAll
            && let [Slot::Const(l), y, p @ Slot::Const(_)] = b.args[..]
            && let Some(ms) = g.list(
                &mut Memo::default(),
                l,
                terms.rdf_first,
                terms.rdf_rest,
                terms.rdf_nil,
                g.len(),
            )
        {
            for m in ms {
                let a = Atom {
                    s: y,
                    p,
                    o: Slot::Const(m),
                };
                if !r.atoms.iter().any(|x| (x.s, x.p, x.o) == (a.s, a.p, a.o)) {
                    r.atoms.push(a);
                }
            }
            continue;
        }
        kept.push(b);
    }
    r.builtins = kept;
    r
}

fn head_atom(h: &CHead) -> Atom {
    let (s, p, o) = crate::demand::head_slots(h);
    Atom { s, p, o }
}

/// The schema of `vocab` as facts a pass derived in full.
pub(crate) fn closed(vocab: &Vocab, rdf_type: u64) -> Closed {
    Closed {
        preds: vocab.schema.clone(),
        classes: vocab.meta.clone(),
        rdf_type,
    }
}

/// `rules` without those that read `owl:sameAs` (`same_as`) when no fact of it can hold:
/// the graph has none and no rule gives one without reading one. Equality then needs
/// no stage of its own, and the `eq-rep-*` rules, whose variable predicates would answer
/// every demand, are left out.
pub(crate) fn without_equality(rules: Vec<CRule>, same_as: u64, g: &Graph) -> Vec<CRule> {
    let reads = |r: &CRule| r.atoms.iter().any(|a| a.p == Slot::Const(same_as));
    let seeds = rules.iter().any(|r| {
        !reads(r)
            && r.head.iter().any(|h| {
                matches!(crate::demand::head_slots(h).1, Slot::Var(_) | Slot::Any)
                    || crate::demand::head_slots(h).1 == Slot::Const(same_as)
            })
    });
    if seeds || g.count_p(same_as, 0, g.len()) > 0 {
        return rules;
    }
    rules.into_iter().filter(|r| !reads(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rule whose head predicate a schema atom binds, to a relation the proof closed,
    /// is left out once specialized, and [`Specialized::left_out`] holds that instance
    /// with its other atoms, which the reuse of tables across commits reads (Codex review
    /// qtr-step2, round 2).
    #[test]
    fn a_rule_closed_once_specialized_is_kept_as_left_out() {
        let store =
            sparkles_core::store::Store::in_memory(sparkles_core::store::StoreOptions::default());
        let terms = Terms::new(store.snapshot());
        let mut graph = Graph::with_capacity(0);
        // `8 6 7`: the schema atom `(?p 6 7)` binds ?p to 8, a closed relation
        graph.add_batch([[8, 6, 7]]);
        let closed = Closed {
            preds: [6, 8].into_iter().collect(),
            classes: FxHashSet::default(),
            rdf_type: 1,
        };
        let rule = CRule {
            name: "schema producer".into(),
            nvars: 1,
            atoms: vec![
                Atom {
                    s: Slot::Var(0),
                    p: Slot::Const(6),
                    o: Slot::Const(7),
                },
                Atom {
                    s: Slot::Const(20),
                    p: Slot::Const(21),
                    o: Slot::Const(22),
                },
            ],
            builtins: vec![],
            head: vec![CHead::Triple(
                Slot::Const(30),
                Slot::Var(0),
                Slot::Const(31),
            )],
        };
        let s = specialize(
            &[rule],
            &closed,
            &[],
            &graph,
            &terms,
            &Budget::unlimited(),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(s.rules.is_empty());
        assert_eq!(s.left_out.len(), 1);
        let a = &s.left_out[0].atoms;
        assert_eq!(a.len(), 1);
        assert_eq!(
            (a[0].s, a[0].p, a[0].o),
            (Slot::Const(20), Slot::Const(21), Slot::Const(22))
        );
    }

    /// Past [`MAX_RULES`] instances left out, a rule as written stands for the rest of
    /// its instances, so the list stays bounded and the reuse check reads atoms that are
    /// only more general (Codex review qtr-step2, round 3).
    #[test]
    fn instances_left_out_stop_at_the_bound() {
        let store =
            sparkles_core::store::Store::in_memory(sparkles_core::store::StoreOptions::default());
        let terms = Terms::new(store.snapshot());
        let mut graph = Graph::with_capacity(0);
        let subjects: Vec<u64> = (1000..1000 + MAX_BINDINGS as u64).collect();
        graph.add_batch(subjects.iter().map(|&p| [p, 6, 7]));
        let closed = Closed {
            preds: subjects.iter().copied().chain([6]).collect(),
            classes: FxHashSet::default(),
            rdf_type: 1,
        };
        let rule = |k: u64| CRule {
            name: format!("producer {k}"),
            nvars: 1,
            atoms: vec![
                Atom {
                    s: Slot::Var(0),
                    p: Slot::Const(6),
                    o: Slot::Const(7),
                },
                Atom {
                    s: Slot::Const(20 + k),
                    p: Slot::Const(21),
                    o: Slot::Const(22),
                },
            ],
            builtins: vec![],
            head: vec![CHead::Triple(
                Slot::Const(30),
                Slot::Var(0),
                Slot::Const(31),
            )],
        };
        let n = MAX_RULES / MAX_BINDINGS + 2;
        let rules: Vec<CRule> = (0..n as u64).map(rule).collect();
        let budget = Budget::unlimited();
        let s = specialize(
            &rules,
            &closed,
            &[],
            &graph,
            &terms,
            &budget,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(s.rules.is_empty());
        let written = s.left_out.iter().filter(|r| r.atoms.len() == 2).count();
        assert_eq!(s.left_out.len() - written, MAX_RULES);
        assert_eq!(written, 2);
        assert!(budget.used() > 0);
        drop(s);
        assert_eq!(budget.used(), 0);
    }
}
