//! Weak acyclicity of a program with existential rules (item 839's plan, §1.5), after
//! Fagin, Kolaitis, Miller and Popa, "Data exchange: semantics and query answering"
//! (2005), Definition 3.7.
//!
//! A position is a column of a relation. The triple relation has a subject and an object
//! position for each constant predicate, one predicate position, the subject and object of
//! the facts a head with a variable predicate gives (`Any`), and the subject and object
//! that an atom with a variable predicate reads (`ReadAll`). Values flow from `Any` to
//! every constant predicate's column, since such a fact has some constant predicate, and
//! from every constant predicate's column and `Any` to `ReadAll`. No edge joins two
//! constant predicates by itself, so `(x p y) → ∃z (y q z)` stays acyclic.
//!
//! For each rule and each variable of both body and head, every body position of the
//! variable has a normal edge to each of its head positions, and a special edge to each
//! head position of each existential. Builtins take part through what they read and give:
//! a list builtin's list has the positions of `rdf:first` and `rdf:rest` subjects and of
//! `rdf:rest` objects, the members `listMember` and `listEntry` give have the position of
//! `rdf:first` objects, and `listForAll` reads the facts of an atom. Every other builtin
//! output is a new value made from its inputs, and `makeTemp`'s from every body position
//! of its rule. A variable's sources are its own body positions, or for a new value the
//! sources of the inputs it is made from, so a value a builtin computes from the frontier
//! carries the frontier's positions. A new value in a head gets special edges from its
//! sources, and an existential gets special edges from the sources of every other
//! variable of its rule's head. A head action gives facts whose subject (`listMapAsSubject`) or object
//! (`listMapAsObject`) is a list member, which flows from `rdf:first` objects. A program
//! is weakly acyclic when no cycle passes a special edge.

use super::{Head, IAtom, Literal, Pred, RelId, Relations, Rule, Vocab};
use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_capacity};
use crate::builtins::{BuiltinKind, HeadAction};
use crate::engine::Slot;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::fmt;
use std::sync::Arc;

/// A cycle through a special edge of rule `rule`: a value it makes at `to` from a value at
/// `from` can reach `from` again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Cycle {
    pub rule: usize,
    pub from: Position,
    pub to: Position,
}

const S: u8 = 0;
const O: u8 = 2;

/// A position of the graph.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum Position {
    /// a column of a constant predicate
    Pred(u64, u8),
    /// a column of the facts a head with a variable predicate gives
    Any(u8),
    /// a column that an atom with a variable predicate reads
    ReadAll(u8),
    /// the predicate column
    PredCol,
    Rel(RelId, u16),
}

/// Whether a builtin's outputs are values already in the facts: the members of a list.
fn selects(kind: BuiltinKind) -> bool {
    matches!(kind, BuiltinKind::ListMember | BuiltinKind::ListEntry)
}

fn reads_list(kind: BuiltinKind) -> bool {
    use BuiltinKind::*;
    matches!(
        kind,
        ListMember | ListContains | ListNotContains | ListLength | ListEntry | ListForAll
    )
}

struct Positions {
    ids: FxHashMap<Position, u32>,
    keys: Vec<Position>,
    /// `(from, to, special, rule)`
    edges: Vec<(u32, u32, bool, u32)>,
}

impl Positions {
    fn id(&mut self, p: Position) -> u32 {
        let n = self.keys.len() as u32;
        let id = *self.ids.entry(p).or_insert(n);
        if id == n {
            self.keys.push(p);
        }
        id
    }

    /// The positions of a triple pattern's column `c`, read (`head` false) or given.
    fn column(&mut self, p: Slot, c: u8, head: bool) -> u32 {
        match (p, head) {
            (Slot::Const(p), _) => self.id(Position::Pred(p, c)),
            (_, true) => self.id(Position::Any(c)),
            (_, false) => self.id(Position::ReadAll(c)),
        }
    }

    fn atom(&mut self, a: &IAtom, head: bool, out: &mut Vec<(usize, u32)>) {
        match a.pred {
            Pred::Triple => {
                for (i, c) in [(0, S), (2, O)] {
                    if let Slot::Var(v) = a.args[i] {
                        let p = self.column(a.args[1], c, head);
                        out.push((v, p));
                    }
                }
                if let Slot::Var(v) = a.args[1] {
                    let p = self.id(Position::PredCol);
                    out.push((v, p));
                }
            }
            Pred::Rel(r) => {
                for (i, s) in a.args.iter().enumerate() {
                    if let Slot::Var(v) = s {
                        let p = self.id(Position::Rel(r, i as u16));
                        out.push((*v, p));
                    }
                }
            }
        }
    }
}

fn bound(rules: &[Rule]) -> (usize, usize, usize) {
    let (mut positions, mut edges, mut scratch) = (8usize, 0usize, 0usize);
    for r in rules {
        let mut body = 0usize;
        let mut head = 0usize;
        for l in &r.body {
            body += match l {
                Literal::Pos(a) => a.args.len(),
                Literal::Neg(_) => 0,
                Literal::Builtin(b) => b.args.len() + 3,
            };
        }
        for h in &r.head {
            head += match h {
                Head::Atom(a) => a.args.len(),
                Head::Action(_, args) => args.len() + 1,
            };
        }
        positions += body + head;
        // a normal edge, a special edge from a new value's sources and one to an
        // existential, for each pair of a body and a head position, and a list member's edge
        // for each head action
        edges += 3 * body * head + head;
        // the sources of each variable, at most one per body position, the frontier and
        // the body and head lists
        scratch = scratch.max((r.nvars + 2) * body + 2 * head);
    }
    // the bridges of every column to `ReadAll` and from `Any`
    edges += 3 * positions;
    (positions, edges, scratch)
}

/// Check that `rules` are weakly acyclic, charging the position graph to `budget`.
pub(crate) fn check(rules: &[Rule], vocab: &Vocab, budget: &Arc<Budget>) -> Result<(), CheckError> {
    let (positions, edges, scratch) = bound(rules);
    let nvars = rules.iter().map(|r| r.nvars).max().unwrap_or(0);
    let bytes = table_bytes(table_capacity(positions), size_of::<(Position, u32)>())
        + positions * size_of::<Position>()
        + edges * size_of::<(u32, u32, bool, u32)>()
        + positions * super::strata::SCRATCH_PER_NODE
        // the body and head lists, the sources and their vectors, the frontier and a
        // source list being merged, with room for a vector's doubling
        + 2 * scratch * size_of::<(usize, u32)>()
        + nvars * size_of::<Vec<u32>>();
    let _held = Held::try_new_exact(budget, bytes)?;
    let mut g = Positions {
        ids: FxHashMap::with_capacity_and_hasher(positions, Default::default()),
        keys: Vec::with_capacity(positions),
        edges: Vec::with_capacity(edges),
    };
    let mut body: Vec<(usize, u32)> = Vec::with_capacity(scratch);
    let mut head: Vec<(usize, u32)> = Vec::with_capacity(scratch);
    let mut sources: Vec<Vec<u32>> = Vec::with_capacity(nvars);
    // head positions whose values come from list members, with no body variable
    let mut members: Vec<u32> = Vec::new();
    for (ri, r) in rules.iter().enumerate() {
        let ri = ri as u32;
        body.clear();
        head.clear();
        members.clear();
        let mut generated: SmallVec<[usize; 4]> = SmallVec::new();
        for l in &r.body {
            match l {
                Literal::Pos(a) => g.atom(a, false, &mut body),
                Literal::Neg(_) => {}
                Literal::Builtin(b) => {
                    if reads_list(b.kind)
                        && let Some(Slot::Var(v)) = b.args.first()
                    {
                        for p in [
                            Position::Pred(vocab.rdf_first, S),
                            Position::Pred(vocab.rdf_rest, S),
                            Position::Pred(vocab.rdf_rest, O),
                        ] {
                            let p = g.id(p);
                            body.push((*v, p));
                        }
                    }
                    if b.kind == BuiltinKind::ListForAll {
                        let pred = b.args.get(2).copied().unwrap_or(Slot::Any);
                        if let Some(Slot::Var(v)) = b.args.get(1) {
                            let p = g.column(pred, S, false);
                            body.push((*v, p));
                        }
                        if let Slot::Var(v) = pred {
                            let p = g.id(Position::PredCol);
                            body.push((v, p));
                        }
                    }
                    if selects(b.kind) {
                        let p = g.id(Position::Pred(vocab.rdf_first, O));
                        for &v in &b.outputs {
                            body.push((v, p));
                        }
                    } else {
                        generated.extend(b.outputs.iter().copied());
                    }
                }
            }
        }
        for h in &r.head {
            match h {
                Head::Atom(a) => g.atom(a, true, &mut head),
                Head::Action(action, args) => {
                    let (member, other, oc) = match action {
                        HeadAction::ListMapAsSubject => (S, args[2], O),
                        HeadAction::ListMapAsObject => (O, args[0], S),
                    };
                    let m = g.column(args[1], member, true);
                    members.push(m);
                    if let Slot::Var(v) = other {
                        let p = g.column(args[1], oc, true);
                        head.push((v, p));
                    }
                    if let Slot::Var(v) = args[1] {
                        let p = g.id(Position::PredCol);
                        head.push((v, p));
                    }
                }
            }
        }
        let first_o = g.id(Position::Pred(vocab.rdf_first, O));
        for &m in &members {
            g.edges.push((first_o, m, false, ri));
        }
        // the sources of each variable: its body positions, and for a new value the
        // sources of the inputs it is made from, through chains of builtins
        sources.clear();
        sources.resize_with(r.nvars, Vec::new);
        for &(v, p) in &body {
            sources[v].push(p);
        }
        let mut everything: Vec<u32> = body.iter().map(|&(_, p)| p).collect();
        everything.sort_unstable();
        everything.dedup();
        for _ in 0..r.body.len() {
            let mut grew = false;
            for l in &r.body {
                let Literal::Builtin(b) = l else { continue };
                if selects(b.kind) || b.outputs.is_empty() {
                    continue;
                }
                let mut from: Vec<u32> = if b.inputs.is_empty() {
                    everything.clone()
                } else {
                    b.inputs
                        .iter()
                        .flat_map(|&v| sources[v].iter().copied())
                        .collect()
                };
                from.sort_unstable();
                from.dedup();
                for &o in &b.outputs {
                    let mut merged = from.clone();
                    merged.extend(sources[o].iter().copied());
                    merged.sort_unstable();
                    merged.dedup();
                    if merged.len() > sources[o].len() {
                        sources[o] = merged;
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        // the sources of the head's values other than its existentials
        let mut frontier: Vec<u32> = head
            .iter()
            .filter(|&&(w, _)| !r.exists.contains(&w))
            .flat_map(|&(w, _)| sources[w].iter().copied())
            .collect();
        frontier.sort_unstable();
        frontier.dedup();
        for &(v, to) in &head {
            if r.exists.contains(&v) {
                for &from in &frontier {
                    g.edges.push((from, to, true, ri));
                }
            } else if generated.contains(&v) {
                for &from in &sources[v] {
                    g.edges.push((from, to, true, ri));
                }
            }
            for &(w, from) in &body {
                if w == v {
                    g.edges.push((from, to, false, ri));
                }
            }
        }
    }
    // the bridges
    let n = g.keys.len();
    for i in 0..n {
        if let Position::Pred(_, c) = g.keys[i] {
            let any = g.id(Position::Any(c));
            let all = g.id(Position::ReadAll(c));
            g.edges.push((any, i as u32, false, u32::MAX));
            g.edges.push((i as u32, all, false, u32::MAX));
        }
    }
    for c in [S, O] {
        let any = g.id(Position::Any(c));
        let all = g.id(Position::ReadAll(c));
        g.edges.push((any, all, false, u32::MAX));
    }
    let n = g.keys.len();
    // the charge holds only while the graph stays within its bounds
    debug_assert!(
        g.edges.len() <= edges && n <= positions,
        "{} edges for {edges}, {n} positions for {positions}",
        g.edges.len()
    );
    g.edges.sort_unstable_by_key(|e| e.0);
    let mut offsets = Vec::with_capacity(n + 1);
    let mut e = 0;
    for v in 0..n as u32 {
        offsets.push(e as u32);
        while e < g.edges.len() && g.edges[e].0 == v {
            e += 1;
        }
    }
    offsets.push(e as u32);
    let edges = &g.edges;
    let (comp, _) = super::strata::tarjan(n, |v| {
        edges[offsets[v as usize] as usize..offsets[v as usize + 1] as usize]
            .iter()
            .map(|e| e.1)
    });
    if let Some(&(from, to, _, ri)) = edges
        .iter()
        .find(|e| e.2 && comp[e.0 as usize] == comp[e.1 as usize])
    {
        return Err(CheckError::Cycle(Cycle {
            rule: ri as usize,
            from: g.keys[from as usize],
            to: g.keys[to as usize],
        }));
    }
    Ok(())
}

impl Position {
    pub(super) fn write(&self, f: &mut fmt::Formatter<'_>, relations: &Relations) -> fmt::Result {
        let col = |c: u8| if c == S { "subject" } else { "object" };
        match *self {
            Position::Pred(p, c) => write!(f, "the {} of predicate #{p}", col(c)),
            Position::Any(c) => write!(f, "the {} of a variable predicate's head", col(c)),
            Position::ReadAll(c) => write!(f, "the {} of a variable predicate's atom", col(c)),
            Position::PredCol => f.write_str("the predicate"),
            Position::Rel(r, i) => match relations.get(r) {
                Some(rel) => write!(f, "column {} of relation '{}'", i + 1, rel.name),
                None => write!(f, "column {} of relation {}", i + 1, r.0),
            },
        }
    }
}

/// Why the check refuses a program.
#[derive(Debug)]
pub(crate) enum CheckError {
    Memory(BudgetFailure),
    Cycle(Cycle),
}

impl From<BudgetFailure> for CheckError {
    fn from(e: BudgetFailure) -> Self {
        CheckError::Memory(e)
    }
}
