//! The relation facts of a closure, rebuilt (item 839's plan, §12.2).
//!
//! Relation facts are rows of the derived tier but are never written, so a closure read
//! back from the dataset, and the graph an explanation reads, lack them. The relation part
//! of a program is its rules whose heads are relation atoms, each with only those heads.
//! Run over a graph whose triples are a complete closure, it gives exactly the relation
//! facts of that closure: a relation rule reads positively facts of its stratum and below
//! and negatively facts below it, every triple is present, and a monotone stratum's least
//! fixpoint over complete inputs is unique. Its strata coarsen the program's, which is
//! valid, since a subgraph of a stratified dependency graph stratifies.

use super::{Head, Pred, Program, ProgramError, Rule, Vocab};
use crate::budget::{Budget, Held, WorkingWarnings};
use crate::builtins::BuiltinKind;
use crate::engine::{self, CHead, CRule, Limits, Slot};
use crate::graph::Graph;
use crate::terms::{Kind, Terms};
use std::sync::Arc;

impl Program {
    /// The relation part of the program, or `None` when no rule heads a relation.
    pub fn relation_part(
        &self,
        vocab: &Vocab,
        budget: &Arc<Budget>,
    ) -> Result<Option<Program>, ProgramError> {
        let is_rel = |h: &Head| matches!(h, Head::Atom(a) if matches!(a.pred, Pred::Rel(_)));
        // the copy is charged before it is made: the catalog, and each rule with its
        // vector entry, which bounds its copy with fewer heads
        let mut held = Held::new(budget);
        held.try_grow(self.relations.bytes())?;
        let n = self
            .rules
            .iter()
            .filter(|r| r.head.iter().any(is_rel))
            .count();
        held.try_grow(n * size_of::<Rule>())?;
        let mut rules = Vec::with_capacity(n);
        for r in &self.rules {
            if !r.head.iter().any(is_rel) {
                continue;
            }
            held.try_grow(r.bytes())?;
            let head: Vec<Head> = r.head.iter().filter(|h| is_rel(h)).cloned().collect();
            let mut part = r.clone();
            part.head = head;
            let in_head = |v: usize| {
                part.head.iter().any(|h| match h {
                    Head::Atom(a) => a.args.contains(&Slot::Var(v)),
                    Head::Action(..) => false,
                })
            };
            part.exists = r.exists.iter().copied().filter(|&v| in_head(v)).collect();
            rules.push(part);
        }
        if rules.is_empty() {
            return Ok(None);
        }
        let relations = self.relations.clone();
        // `Program::new` charges the copy before it allocates anything, so the credit
        // passes to it with no allocation between
        drop(held);
        Program::new(relations, rules, vocab, budget).map(Some)
    }
}

/// Whether lowered rule `r` makes terms that a rerun makes again as other terms:
/// `makeTemp`, `makeSkolem` or a blank node constant in a head, a tuple head's columns
/// included.
pub(crate) fn makes_terms(r: &CRule, terms: &Terms) -> bool {
    r.builtins
        .iter()
        .any(|b| matches!(b.kind, BuiltinKind::MakeTemp | BuiltinKind::MakeSkolem))
        || blank_columns(r, terms)
        || r.head.iter().any(|h| match h {
            CHead::Triple(s, p, o) => [s, p, o].into_iter().any(|s| local_bnode(s, terms)),
            CHead::Action(..) => false,
        })
}

/// Whether a tuple head of lowered rule `r`, a relation head of arity 3 or more, holds a
/// local blank node constant among its columns.
pub(crate) fn blank_columns(r: &CRule, terms: &Terms) -> bool {
    r.builtins.iter().any(|b| {
        b.kind == BuiltinKind::RelTuple && b.args[2..].iter().any(|s| local_bnode(s, terms))
    })
}

fn local_bnode(s: &Slot, terms: &Terms) -> bool {
    match *s {
        Slot::Const(c) => {
            sparkles_core::id::Id(c).tag() == sparkles_core::id::Tag::Local
                && terms.kind(c) == Kind::BNode
        }
        _ => false,
    }
}

/// Add to `g`, whose triples are a complete closure, the relation facts of `part`, a
/// program's [`relation_part`](Program::relation_part). The facts are charged and counted
/// as a run's, under `limits`, and the run's outcome is returned.
pub(crate) fn rebuild_relations(
    g: &mut Graph,
    part: &Program,
    terms: &Terms,
    limits: &Limits,
) -> anyhow::Result<engine::Outcome> {
    g.set_relations();
    let mut warnings = WorkingWarnings::new(&limits.budget);
    engine::run_program(g, part, terms, limits, &mut warnings)
}
