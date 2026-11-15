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

    /// The program's rules in evaluation form, by stratum, charged to `budget` before
    /// each is built and while they live.
    pub fn lower(&self, budget: &Arc<Budget>) -> Result<Lowered, LowerError> {
        let shape = self.shape();
        let mut held = Held::new(budget);
        let mut stratified_novalue = Vec::new();
        for r in &self.rules {
            if !r.exists.is_empty() {
                return Err(LowerError::Existential {
                    rule: r.name.clone(),
                });
            }
        }
        // the first origin of each rule's builtins that have none
        let mut origins = Vec::with_capacity(self.rules.len());
        let mut next = ORIGIN_BASE;
        for r in &self.rules {
            origins.push(next);
            next += r.body.len() + r.head.len();
        }
        let mut lower = |ri: usize, heads: &[usize]| -> Result<CRule, LowerError> {
            held.try_grow(lowered_bytes(self, &self.rules[ri], heads))?;
            lower_rule(self, ri, heads, shape, origins[ri])
        };
        let strata = match shape {
            Shape::Jena | Shape::Monotone => {
                let mut rules = Vec::new();
                for (i, r) in self.rules.iter().enumerate() {
                    let heads: Vec<usize> = (0..r.head.len()).collect();
                    rules.push(lower(i, &heads)?);
                }
                vec![rules]
            }
            Shape::Strata => {
                let mut strata: Vec<Vec<CRule>> =
                    (0..self.strata.count).map(|_| Vec::new()).collect();
                for part in &self.strata.parts {
                    let r = &self.rules[part.rule];
                    strata[part.stratum as usize].push(lower(part.rule, &part.heads)?);
                    let novalue = r.body.iter().any(
                        |l| matches!(l, Literal::Builtin(b) if b.kind == BuiltinKind::NoValue),
                    );
                    if novalue && !r.uses_ir() && stratified_novalue.last() != Some(&part.rule) {
                        stratified_novalue.push(part.rule);
                    }
                }
                strata
            }
        };
        held.try_grow(
            (strata.len() * size_of::<Vec<CRule>>())
                + strata
                    .iter()
                    .map(|s| s.capacity() * size_of::<CRule>())
                    .sum::<usize>()
                + stratified_novalue.capacity() * size_of::<usize>(),
        )?;
        Ok(Lowered {
            shape,
            strata,
            stratified_novalue,
            _held: held,
        })
    }
}

/// The heap of rule `r` lowered with the heads `heads`, an upper bound: its atoms, its
/// builtins with their arguments, inputs and outputs, one tail builtin per atom and head of
/// arity 3 or more, and its heads.
fn lowered_bytes(p: &Program, r: &Rule, heads: &[usize]) -> usize {
    let arity = |a: &IAtom| a.args.len() + 2;
    let mut atoms = 0;
    let mut builtins = 0;
    let mut slots = 0;
    for l in &r.body {
        match l {
            Literal::Pos(a) => {
                atoms += 1;
                builtins += usize::from(matches!(a.pred, Pred::Rel(_)) && a.args.len() > 2);
                slots += 2 * arity(a);
            }
            Literal::Neg(a) => {
                builtins += 1;
                slots += 2 * arity(a);
            }
            Literal::Builtin(b) => {
                builtins += 1;
                slots += b.args.len() + b.inputs.len() + b.outputs.len();
            }
        }
    }
    for &h in heads {
        match &r.head[h] {
            Head::Atom(a) => {
                builtins += usize::from(matches!(a.pred, Pred::Rel(_)) && a.args.len() > 2);
                slots += 2 * arity(a);
            }
            Head::Action(_, args) => slots += args.len(),
        }
    }
    let _ = p;
    r.name.capacity()
        + atoms * size_of::<Atom>()
        + builtins * size_of::<CBuiltin>()
        + heads.len() * size_of::<CHead>()
        + slots * size_of::<Slot>()
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
    let inputs = kind
        .input_positions(args.len())
        .iter()
        .filter_map(var)
        .collect();
    let outputs = kind
        .output_positions(args.len())
        .iter()
        .filter_map(var)
        .collect();
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
    let mut atoms = Vec::new();
    let mut builtins = Vec::new();
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
                        let mut args = vec![h, Slot::Const(k)];
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
                        let mut args = vec![s, Slot::Const(k)];
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
                        let mut args = vec![h, Slot::Const(k)];
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
