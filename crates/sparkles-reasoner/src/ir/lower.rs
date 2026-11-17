//! Lowering a program of the IR to the evaluation form (item 839's plan, §1.2 and §10.2).
//!
//! A relation lives in the derived tier as triples over its key, a reserved id that no
//! input can name. A fact `r(c0, c1)` of arity 2 is the row `(c0, key, c1)`, one of arity
//! 1 is `(c0, key, NIL)`, and one of arity 0 is `(NIL, key, NIL)`. A fact of arity 3 or
//! more is `(c0, key, h)`, where `h` is the handle of its tail `c1 … cn-1`, which
//! [`Terms::try_tail`] interns. So a lowered relation atom is a triple atom with a constant
//! predicate, and the semi-naive ranges, the hash index, the budget and `max_inferred`
//! hold for relation facts unchanged.
//!
//! A body atom of arity 3 or more lowers to `(c0, key, ?h)` and the builtin
//! [`RelTail`](BuiltinKind::RelTail), a head atom of arity 3 or more to the head
//! `(c0, key, ?h)` and the body builtin [`RelTuple`](BuiltinKind::RelTuple), and a negated
//! atom to [`RelAbsent`](BuiltinKind::RelAbsent). Every relation row is an invalid triple,
//! since its predicate is no IRI, and [`Graph::is_hidden`](crate::graph::Graph::is_hidden)
//! hides it from every atom and `noValue` with a variable predicate.

use super::{Head, IAtom, Literal, Pred, Program, RelId, Rule};
use crate::budget::{Budget, BudgetFailure, Held};
use crate::builtins::BuiltinKind;
use crate::engine::{Atom, CBuiltin, CHead, CRule, Slot};
use sparkles_core::id::{Id, Tag};
use std::sync::Arc;

/// The payload of the first relation key under the `Special` tag. The store uses the
/// `Special` tag only for the default graph, payload 0.
const KEY_BASE: u64 = 1 << 58;
/// The id of relation 0's key: relation `r`'s is `KEY0 + r`.
const KEY0: u64 = Id::new(Tag::Special, KEY_BASE).0;

/// The sentinel of the missing columns of a relation fact of arity 0 or 1, an id that no
/// term has.
pub(crate) const NIL: u64 = Id::new(Tag::Special, KEY_BASE - 1).0;

/// The key of relation `r`.
pub(crate) fn key(r: RelId) -> u64 {
    KEY0 + r.0 as u64
}

/// Whether `p` is the key of a relation.
#[inline(always)]
pub(crate) fn is_key(p: u64) -> bool {
    p.wrapping_sub(KEY0) < 1 << 32
}

/// The relation whose key is `p`, when `p` is one.
pub(crate) fn rel_of(p: u64) -> Option<RelId> {
    is_key(p).then(|| RelId((p - KEY0) as u32))
}

/// How a program runs (§1.3's policy).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shape {
    /// no IR feature: the rules as Jena compiles them, one stratum, Jena's `noValue`
    Jena,
    /// relations and no negation: the lowered rules in one stratum
    Monotone,
    /// an IR feature and a negation: by strata
    Strata,
}

/// A lowered program: the rules of each stratum in evaluation form, charged to a budget
/// while it lives.
pub(crate) struct Lowered {
    pub shape: Shape,
    pub strata: Vec<Vec<CRule>>,
    /// the rules without an IR feature that have a `noValue`, by index, when the program
    /// runs by strata: their `noValue` reads complete lower strata (Q7)
    pub stratified_novalue: Vec<usize>,
    _held: Held,
}

impl Lowered {
    /// The bytes charged for the lowered rules.
    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self._held.bytes()
    }
}

/// The first origin of the builtins that no compiled Jena rule gave an origin, above any
/// that a compiled program numbers.
const ORIGIN_BASE: usize = 1 << 40;

/// Why a program cannot be lowered yet.
#[derive(Debug)]
pub(crate) enum LowerError {
    /// an existential head, which the chase evaluates (Phase 6)
    Existential {
        rule: String,
    },
    /// a rule that reads lists and calls `makeTemp`, which a replay of its list reads
    /// would fire again for old instances (roadmap 1006)
    TempOverLists {
        rule: String,
    },
    Memory(BudgetFailure),
}

impl From<BudgetFailure> for LowerError {
    fn from(e: BudgetFailure) -> Self {
        LowerError::Memory(e)
    }
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LowerError::Existential { rule } => write!(
                f,
                "rule '{rule}' has an existential head variable, which the reasoner does not evaluate"
            ),
            LowerError::TempOverLists { rule } => write!(
                f,
                "rule '{rule}' reads RDF lists and calls makeTemp, which a program with relations or negation does not support"
            ),
            LowerError::Memory(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for LowerError {}

impl Program {
    /// How the program runs.
    pub fn shape(&self) -> Shape {
        if !self.uses_ir() {
            Shape::Jena
        } else if self.strata.has_negation {
            Shape::Strata
        } else {
            Shape::Monotone
        }
    }

    /// The IR rule of each rule [`lower`](Self::lower) gives, by stratum.
    /// The vectors have exact capacities, so a caller can charge them before it asks.
    pub fn lowered_origins(&self) -> Vec<Vec<usize>> {
        match self.shape() {
            Shape::Jena | Shape::Monotone => vec![(0..self.rules.len()).collect()],
            Shape::Strata => {
                // each stratum's vector is allocated for exactly its rules
                let mut sizes = vec![0usize; self.strata.count as usize];
                for part in &self.strata.parts {
                    sizes[part.stratum as usize] += 1;
                }
                let mut v: Vec<Vec<usize>> = sizes.iter().map(|&n| Vec::with_capacity(n)).collect();
                for part in &self.strata.parts {
                    v[part.stratum as usize].push(part.rule);
                }
                v
            }
        }
    }

    /// The program's rules in evaluation form, by stratum, charged to `budget` before
    /// each is built and while they live.
    pub fn lower(&self, budget: &Arc<Budget>) -> Result<Lowered, LowerError> {
        let shape = self.shape();
        let mut held = Held::new(budget);
        for r in &self.rules {
            if !r.exists.is_empty() {
                return Err(LowerError::Existential {
                    rule: r.name.clone(),
                });
            }
        }
        // the rules of each stratum, which the outer vectors are allocated for exactly
        let count = match shape {
            Shape::Jena | Shape::Monotone => 1,
            Shape::Strata => self.strata.count as usize,
        };
        held.try_grow(count * (size_of::<Vec<CRule>>() + size_of::<usize>()))?;
        let mut sizes = vec![0usize; count];
        match shape {
            Shape::Jena | Shape::Monotone => sizes[0] = self.rules.len(),
            Shape::Strata => {
                for part in &self.strata.parts {
                    sizes[part.stratum as usize] += 1;
                }
            }
        }
        held.try_grow(sizes.iter().sum::<usize>() * size_of::<CRule>())?;
        let mut strata: Vec<Vec<CRule>> = sizes.iter().map(|&n| Vec::with_capacity(n)).collect();
        drop(sizes);
        held.shrink(count * size_of::<usize>());
        // the first origin of each rule's builtins that have none
        held.try_grow(self.rules.len() * size_of::<usize>())?;
        let mut origins = Vec::with_capacity(self.rules.len());
        let mut next = ORIGIN_BASE;
        for r in &self.rules {
            origins.push(next);
            next += r.body.len() + r.head.len();
        }
        match shape {
            Shape::Jena | Shape::Monotone => {
                for (i, r) in self.rules.iter().enumerate() {
                    // the indices of every head, scratch charged while they live
                    let n = r.head.len() * size_of::<usize>();
                    held.try_grow(n)?;
                    let heads: Vec<usize> = (0..r.head.len()).collect();
                    let rule = lower_charged(self, &mut held, i, &heads, shape, origins[i]);
                    drop(heads);
                    held.shrink(n);
                    strata[0].push(rule?);
                }
            }
            Shape::Strata => {
                for part in &self.strata.parts {
                    let ri = part.rule;
                    let rule = lower_charged(self, &mut held, ri, &part.heads, shape, origins[ri]);
                    strata[part.stratum as usize].push(rule?);
                }
            }
        }
        drop(origins);
        held.shrink(self.rules.len() * size_of::<usize>());
        let novalue = |i: &usize| shape == Shape::Strata && jena_novalue(&self.rules[*i]);
        let n = (0..self.rules.len()).filter(novalue).count();
        held.try_grow(n * size_of::<usize>())?;
        let mut stratified_novalue = Vec::with_capacity(n);
        stratified_novalue.extend((0..self.rules.len()).filter(novalue));
        Ok(Lowered {
            shape,
            strata,
            stratified_novalue,
            _held: held,
        })
    }
}

/// Rule `ri` with the heads `heads` lowered, charged to `held`: a bound of its size before
/// it is built, and then the size it keeps.
fn lower_charged(
    p: &Program,
    held: &mut Held,
    ri: usize,
    heads: &[usize],
    shape: Shape,
    origin: usize,
) -> Result<CRule, LowerError> {
    let bound = lowered_bound(&p.rules[ri], heads);
    held.try_grow(bound)?;
    match lower_rule(p, ri, heads, shape, origin) {
        Ok(rule) => {
            let kept = crule_bytes(&rule);
            debug_assert!(kept <= bound, "{kept} > {bound}");
            held.shrink(bound.saturating_sub(kept));
            Ok(rule)
        }
        Err(e) => {
            held.shrink(bound);
            Err(e)
        }
    }
}

/// Whether `r` is a rule of Jena's form with a `noValue`.
fn jena_novalue(r: &Rule) -> bool {
    !r.uses_ir()
        && r.body
            .iter()
            .any(|l| matches!(l, Literal::Builtin(b) if b.kind == BuiltinKind::NoValue))
}

/// The heap a lowered rule keeps: its name, its atoms, its builtins with their arguments,
/// inputs and outputs, and its heads with their arguments.
pub(crate) fn crule_bytes(r: &CRule) -> usize {
    let builtins: usize = r
        .builtins
        .iter()
        .map(|b| {
            b.args.capacity() * size_of::<Slot>()
                + (b.inputs.capacity() + b.outputs.capacity()) * size_of::<usize>()
        })
        .sum();
    let heads: usize = r
        .head
        .iter()
        .map(|h| match h {
            CHead::Action(_, args) => args.capacity() * size_of::<Slot>(),
            CHead::Triple(..) => 0,
        })
        .sum();
    r.name.capacity()
        + r.atoms.capacity() * size_of::<Atom>()
        + r.builtins.capacity() * size_of::<CBuiltin>()
        + r.head.capacity() * size_of::<CHead>()
        + builtins
        + heads
}

/// A bound of [`crule_bytes`] for rule `r` lowered with the heads `heads`: an atom per
/// positive literal, a builtin per body literal and per head, each with the arguments of
/// a whole lowered atom and as many inputs and outputs, and the heads.
fn lowered_bound(r: &Rule, heads: &[usize]) -> usize {
    let mut atoms = 0;
    let mut builtins = 0;
    let mut slots = 0;
    let args = |a: &IAtom| a.args.len().max(2) + 1;
    for l in &r.body {
        match l {
            Literal::Pos(a) => {
                atoms += 1;
                builtins += 1;
                slots += args(a);
            }
            Literal::Neg(a) => {
                builtins += 1;
                slots += args(a);
            }
            Literal::Builtin(b) => {
                builtins += 1;
                slots += b.args.len();
            }
        }
    }
    for &h in heads {
        match &r.head[h] {
            Head::Atom(a) => {
                builtins += 1;
                slots += args(a);
            }
            Head::Action(_, a) => slots += a.len(),
        }
    }
    r.name.len()
        + atoms * size_of::<Atom>()
        + builtins * size_of::<CBuiltin>()
        + heads.len() * size_of::<CHead>()
        + slots * (size_of::<Slot>() + 2 * size_of::<usize>())
}

/// The slots of the lowered atom `a`: the triple's, and the tail's columns for a relation
/// of arity 3 or more.
fn slots(p: &Program, a: &IAtom) -> ([Slot; 3], Option<(u64, Vec<Slot>)>) {
    match a.pred {
        Pred::Triple => ([a.args[0], a.args[1], a.args[2]], None),
        Pred::Rel(r) => {
            let k = Slot::Const(key(r));
            let nil = Slot::Const(NIL);
            debug_assert_eq!(
                p.relations.get(r).map(|x| x.arity as usize),
                Some(a.args.len())
            );
            match a.args.len() {
                0 => ([nil, k, nil], None),
                1 => ([a.args[0], k, nil], None),
                2 => ([a.args[0], k, a.args[1]], None),
                _ => (
                    [a.args[0], k, Slot::Any],
                    Some((key(r), a.args[1..].to_vec())),
                ),
            }
        }
    }
}

fn builtin(kind: BuiltinKind, args: Vec<Slot>, origin: usize) -> CBuiltin {
    let var = |i: &usize| match args[*i] {
        Slot::Var(v) => Some(v),
        _ => None,
    };
    let exact = |positions: Vec<usize>| -> Vec<usize> {
        let mut v = Vec::with_capacity(positions.iter().filter_map(var).count());
        v.extend(positions.iter().filter_map(var));
        v
    };
    let inputs = exact(kind.input_positions(args.len()));
    let outputs = exact(kind.output_positions(args.len()));
    CBuiltin {
        origin,
        kind,
        args,
        inputs,
        outputs,
        regex: None,
    }
}

/// Rule `ri` with the heads `heads`, in evaluation form.
fn lower_rule(
    p: &Program,
    ri: usize,
    heads: &[usize],
    shape: Shape,
    first_origin: usize,
) -> Result<CRule, LowerError> {
    let r: &Rule = &p.rules[ri];
    let mut nvars = r.nvars;
    let mut fresh = || {
        nvars += 1;
        Slot::Var(nvars - 1)
    };
    let pos = r
        .body
        .iter()
        .filter(|l| matches!(l, Literal::Pos(_)))
        .count();
    let tails = |a: &IAtom| usize::from(matches!(a.pred, Pred::Rel(_)) && a.args.len() > 2);
    let calls = r
        .body
        .iter()
        .map(|l| match l {
            Literal::Pos(a) => tails(a),
            _ => 1,
        })
        .sum::<usize>()
        + heads
            .iter()
            .map(|&h| match &r.head[h] {
                Head::Atom(a) => tails(a),
                Head::Action(..) => 0,
            })
            .sum::<usize>();
    let mut atoms = Vec::with_capacity(pos);
    let mut builtins = Vec::with_capacity(calls);
    for (li, l) in r.body.iter().enumerate() {
        let origin = first_origin + li;
        match l {
            Literal::Pos(a) => {
                let ([s, pr, o], tail) = slots(p, a);
                match tail {
                    None => atoms.push(Atom { s, p: pr, o }),
                    Some((k, cols)) => {
                        let h = fresh();
                        atoms.push(Atom { s, p: pr, o: h });
                        let mut args = Vec::with_capacity(2 + cols.len());
                        args.extend([h, Slot::Const(k)]);
                        args.extend(cols);
                        builtins.push(builtin(BuiltinKind::RelTail, args, origin));
                    }
                }
            }
            Literal::Neg(a) => {
                let ([s, pr, o], tail) = slots(p, a);
                let args = match tail {
                    None => vec![s, pr, o],
                    Some((k, cols)) => {
                        let mut args = Vec::with_capacity(2 + cols.len());
                        args.extend([s, Slot::Const(k)]);
                        args.extend(cols);
                        args
                    }
                };
                builtins.push(builtin(BuiltinKind::RelAbsent, args, origin));
            }
            Literal::Builtin(b) => {
                builtins.push(builtin(b.kind, b.args.to_vec(), b.origin.unwrap_or(origin)));
            }
        }
    }
    let mut head = Vec::with_capacity(heads.len());
    for &hi in heads {
        let origin = first_origin + r.body.len() + hi;
        match &r.head[hi] {
            Head::Atom(a) => {
                let ([s, pr, o], tail) = slots(p, a);
                match tail {
                    None => head.push(CHead::Triple(s, pr, o)),
                    Some((k, cols)) => {
                        let h = fresh();
                        head.push(CHead::Triple(s, pr, h));
                        let mut args = Vec::with_capacity(2 + cols.len());
                        args.extend([h, Slot::Const(k)]);
                        args.extend(cols);
                        builtins.push(builtin(BuiltinKind::RelTuple, args, origin));
                    }
                }
            }
            Head::Action(a, args) => head.push(CHead::Action(*a, args.to_vec())),
        }
    }
    let rule = CRule {
        name: r.name.clone(),
        nvars,
        atoms,
        builtins,
        head,
    };
    if shape != Shape::Jena
        && reads_lists(&rule)
        && rule
            .builtins
            .iter()
            .any(|b| b.kind == BuiltinKind::MakeTemp)
    {
        return Err(LowerError::TempOverLists {
            rule: r.name.clone(),
        });
    }
    Ok(rule)
}

/// Whether a rule reads RDF lists, through a list builtin or a head action.
pub(crate) fn reads_lists(r: &CRule) -> bool {
    crate::engine::reads_lists(r) || r.head.iter().any(|h| matches!(h, CHead::Action(..)))
}
