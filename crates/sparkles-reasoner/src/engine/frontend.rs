//! The rule program frontend (item 839's plan, §22.3): a rule text with relation atoms or
//! `not` (spec C24 §6.2) compiles to an [`ir::Program`](crate::ir::Program), with its
//! Jena rules compiled exactly as [`compile`](super::compile) compiles them.

use super::Slot;
use super::{
    BuiltinOut, CompileFailure, FlatRule, compile_builtin, compile_credit, compile_one,
    compiler_label, flatten, unsupported,
};
use crate::budget::{Budget, BudgetFailure, Held, WorkingWarnings};
use crate::builtins::HeadAction;
use crate::ir::{self, Head, IAtom, IBuiltin, Literal, Relations};
use crate::parser::{Clause, Direction, Node, Rule};
use crate::terms::Terms;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::collections::HashSet;
use std::sync::Arc;

/// A compiled rule program, with what an explanation names of each of its rules: the
/// line of the rule text and the index of the parsed rule it came from.
pub(crate) struct ProgramInfo {
    pub program: ir::Program,
    /// `(line, parsed)` of each rule of `program`, or empty when no rule text made it
    pub lines: Vec<(usize, usize)>,
    _held: Held,
}

impl ProgramInfo {
    /// A program that no rule text made, whose rules have no lines.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn bare(program: ir::Program, budget: &Arc<Budget>) -> ProgramInfo {
        ProgramInfo {
            program,
            lines: Vec::new(),
            _held: Held::new(budget),
        }
    }
}

/// Compile `rules`, a rule text that [`uses_program`](crate::parser::uses_program), over
/// `terms`, and check and stratify it as one program (Q7). Backward rules are compiled
/// only when `backward` is set, as for query-time reasoning, and are otherwise skipped
/// with the warning [`compile`](super::compile) gives. A rule without a relation atom or
/// `not` compiles as [`compile`](super::compile) compiles it, with the same warnings and
/// skips, and enters the program through [`ir::Rule::from_crule`]. A head variable that
/// the body of a rule with relations does not bind is an existential, which lowering
/// refuses until the restricted chase exists (Phase 6).
pub(crate) fn compile(
    rules: &[Rule],
    terms: &Terms,
    warnings: &mut WorkingWarnings,
    backward: bool,
) -> anyhow::Result<ProgramInfo> {
    let budget = terms.budget();
    let (bytes, emitted) = compile_credit::prewalk(rules, backward)?;
    let mut working = Held::try_new_exact(&budget, bytes)?;
    let mut flat = Vec::with_capacity(emitted);
    for (parsed, r) in rules.iter().enumerate() {
        if r.direction == Direction::Backward && !backward {
            warnings.push_fmt(format_args!(
                "rule '{}' (line {}): backward rules ('<-') are not supported by forward materialization; skipped",
                compiler_label(r)?,
                r.line
            ))?;
            continue;
        }
        if r.body.iter().any(|c| matches!(c, Clause::Rule(_))) {
            warnings.push_fmt(format_args!(
                "rule '{}': nested rule in a body; skipped",
                compiler_label(r)?
            ))?;
            continue;
        }
        flatten(r, &[], compiler_label(r)?, (r.line, parsed), &mut flat);
    }
    // the relations, in the order the rules first name them, with the parser's arities
    let mut relations = Relations::default();
    let mut ids: FxHashMap<&str, ir::RelId> = FxHashMap::default();
    for f in &flat {
        for c in f.body.iter().chain(&f.head) {
            let atom = match c {
                Clause::Relation(a) => a,
                Clause::Not(inner) => match &**inner {
                    Clause::Relation(a) => a,
                    _ => continue,
                },
                _ => continue,
            };
            if !ids.contains_key(atom.name.as_str()) {
                let id = relations
                    .declare(&atom.name, atom.args.len() as u16, false)
                    .map_err(|e| anyhow::anyhow!("relation '{}': {e:?}", atom.name))?;
                ids.insert(atom.name.as_str(), id);
            }
        }
    }
    let mut out = Vec::with_capacity(flat.len());
    let mut lines = Vec::with_capacity(flat.len());
    let mut includes: Vec<Option<Box<str>>> = Vec::with_capacity(flat.len());
    let mut occurrence = 0usize;
    for f in &flat {
        let ir_rule = f
            .body
            .iter()
            .chain(&f.head)
            .any(|c| matches!(c, Clause::Relation(_) | Clause::Not(_)));
        let compiled = if ir_rule {
            compile_ir_one(f, terms, warnings, &budget, &ids)
        } else {
            compile_one(f, terms, warnings, &budget).map(|r| {
                r.map(|(c, info)| {
                    let mut rule = ir::Rule::from_crule(&c);
                    rule.vars = info.vars;
                    rule
                })
            })
        };
        match compiled {
            Ok(Some(mut rule)) => {
                for l in &mut rule.body {
                    if let Literal::Builtin(b) = l {
                        b.origin = Some(occurrence);
                        occurrence = occurrence.checked_add(1).ok_or_else(|| {
                            BudgetFailure::parent(sparkles_budget::ParentRefused::Overflow)
                        })?;
                    }
                }
                out.push(rule);
                lines.push((f.line, f.parsed));
                includes.push(rules[f.parsed].include.as_deref().map(Box::from));
            }
            Ok(None) => {}
            Err(CompileFailure::Unsupported(msg, held)) => {
                let pushed = warnings.push_fmt(format_args!("rule '{}': {msg}; skipped", f.name));
                drop(msg);
                drop(held);
                pushed?
            }
            Err(CompileFailure::Memory(error)) => return Err(error.into()),
        }
    }
    drop(flat);
    drop(working);
    let mut held = Held::new(&budget);
    held.try_grow(lines.capacity() * size_of::<(usize, usize)>())?;
    working = held;
    let mut program =
        ir::Program::new(relations, out, &ir::Vocab::of(terms), &budget).map_err(|e| match e {
            ir::ProgramError::Memory(m) => anyhow::Error::from(m),
            e => anyhow::anyhow!("{e}"),
        })?;
    if includes.iter().any(Option::is_some) {
        program.set_includes(includes)?;
    }
    Ok(ProgramInfo {
        program,
        lines,
        _held: working,
    })
}

/// [`compile_one`] for a rule with relation atoms or `not`: its body literals in text
/// order, with `noValue`'s positional wildcards, and its heads, where a head variable the
/// body does not bind is an existential.
fn compile_ir_one<'a>(
    f: &FlatRule<'a>,
    terms: &Terms,
    warnings: &mut WorkingWarnings,
    budget: &Arc<Budget>,
    ids: &FxHashMap<&str, ir::RelId>,
) -> Result<Option<ir::Rule>, CompileFailure> {
    let body_shape = compile_credit::Shape::clauses(f.body.iter().copied())?;
    let head_shape = compile_credit::Shape::clauses(f.head.iter().copied())?;
    let mut vars: FxHashMap<&'a str, usize> =
        FxHashMap::with_capacity_and_hasher(body_shape.vars + head_shape.vars, Default::default());
    let mut var = |v: &'a str| -> usize {
        let n = vars.len();
        *vars.entry(v).or_insert(n)
    };
    let slot =
        |n: &'a Node, var: &mut dyn FnMut(&'a str) -> usize| -> Result<Slot, CompileFailure> {
            Ok(match n {
                Node::Var(v) => Slot::Var(var(v)),
                Node::Const(t) => Slot::Const(terms.id_for_rule(t)?),
                Node::Any => Slot::Any,
                Node::Functor(name, _) => {
                    return Err(unsupported(
                        budget,
                        format_args!(
                            "functor term '{name}(…)' is not supported by forward materialization"
                        ),
                    ));
                }
            })
        };
    // the atom of a triple or relation clause
    let atom = |c: &'a Clause,
                var: &mut dyn FnMut(&'a str) -> usize|
     -> Result<Option<IAtom>, CompileFailure> {
        Ok(Some(match c {
            Clause::Triple(t) => IAtom::triple(
                slot(&t.subject, var)?,
                slot(&t.predicate, var)?,
                slot(&t.object, var)?,
            ),
            Clause::Relation(a) => {
                let mut args: SmallVec<[Slot; 4]> = SmallVec::with_capacity(a.args.len());
                for n in &a.args {
                    args.push(slot(n, var)?);
                }
                IAtom {
                    pred: ir::Pred::Rel(ids[a.name.as_str()]),
                    args,
                }
            }
            _ => return Ok(None),
        }))
    };
    let names_of = |c: &'a Clause| -> Vec<&'a str> {
        let nodes: Vec<&'a Node> = match c {
            Clause::Triple(t) => vec![&t.subject, &t.predicate, &t.object],
            Clause::Relation(a) => a.args.iter().collect(),
            _ => Vec::new(),
        };
        nodes
            .into_iter()
            .filter_map(|n| match n {
                Node::Var(v) => Some(v.as_str()),
                _ => None,
            })
            .collect()
    };
    let mut body = Vec::with_capacity(body_shape.clauses);
    // variables bound at the current textual position, for `noValue`'s wildcards
    let mut bound: HashSet<&str> = HashSet::with_capacity(body_shape.vars);
    for &c in &f.body {
        match c {
            Clause::Triple(_) | Clause::Relation(_) => {
                let a = atom(c, &mut var)?.expect("an atom");
                bound.extend(names_of(c));
                body.push(Literal::Pos(a));
            }
            Clause::Not(inner) => {
                let Some(a) = atom(inner, &mut var)? else {
                    return Err(unsupported(
                        budget,
                        format_args!("'not' applies to a triple pattern or a relation atom"),
                    ));
                };
                body.push(Literal::Neg(a));
            }
            Clause::Builtin(b) => {
                let mut slot_of = |n: &'a Node| slot(n, &mut var);
                match compile_builtin(b, &mut bound, &mut slot_of, budget)? {
                    BuiltinOut::Call(call) => body.push(Literal::Builtin(IBuiltin::of(&call))),
                    BuiltinOut::Holds => {}
                    BuiltinOut::Never => return Ok(None),
                }
            }
            Clause::Rule(_) => unreachable!("flattened"),
        }
    }
    let mut head = Vec::with_capacity(head_shape.clauses);
    for &c in &f.head {
        match c {
            Clause::Triple(_) | Clause::Relation(_) => {
                let a = atom(c, &mut var)?.expect("an atom");
                if a.args.contains(&Slot::Any) {
                    return Err(unsupported(budget, format_args!("wildcard in a head atom")));
                }
                head.push(Head::Atom(a));
            }
            Clause::Builtin(b) => match HeadAction::from_name(&b.name) {
                Some(Ok(action)) => {
                    let mut args = SmallVec::with_capacity(b.args.len());
                    for argument in &b.args {
                        args.push(slot(argument, &mut var)?);
                    }
                    if args.len() != 3 {
                        return Err(unsupported(
                            budget,
                            format_args!("{} expects 3 arguments", b.name),
                        ));
                    }
                    head.push(Head::Action(action, args));
                }
                Some(Err(())) if matches!(b.name.as_str(), "table" | "tableAll") => {}
                Some(Err(())) => {
                    warnings.push_fmt(format_args!(
                        "rule '{}': head builtin '{}' ignored",
                        f.name, b.name
                    ))?;
                }
                None => {
                    warnings.push_fmt(format_args!(
                        "rule '{}': unsupported head builtin '{}' ignored",
                        f.name, b.name
                    ))?;
                }
            },
            Clause::Not(_) => {
                return Err(unsupported(budget, format_args!("'not' in a head")));
            }
            Clause::Rule(_) => unreachable!("flattened"),
        }
    }
    if head.is_empty() {
        return Ok(None);
    }
    let nvars = vars.len();
    // the head variables that no positive atom or builtin output binds
    let mut binds = vec![false; nvars];
    for l in &body {
        match l {
            Literal::Pos(a) => {
                for s in &a.args {
                    if let Slot::Var(v) = s {
                        binds[*v] = true;
                    }
                }
            }
            Literal::Builtin(b) => b.outputs.iter().for_each(|&v| binds[v] = true),
            Literal::Neg(_) => {}
        }
    }
    let mut exists = Vec::new();
    for h in &head {
        let args = match h {
            Head::Atom(a) => &a.args[..],
            Head::Action(_, args) => &args[..],
        };
        for s in args {
            if let Slot::Var(v) = s
                && !binds[*v]
                && !exists.contains(v)
            {
                exists.push(*v);
            }
        }
    }
    let mut names = vec![String::new(); nvars];
    for (n, &i) in &vars {
        names[i] = (*n).to_owned();
    }
    Ok(Some(ir::Rule {
        name: f.name.clone(),
        nvars,
        body,
        head,
        exists,
        vars: names,
    }))
}
