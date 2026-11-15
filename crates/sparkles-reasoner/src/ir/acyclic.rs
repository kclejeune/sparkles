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
//! output is a new value, an existential whose frontier is every body variable of its
//! rule. A head action gives facts whose subject (`listMapAsSubject`) or object
//! (`listMapAsObject`) is a list member, which flows from `rdf:first` objects. A program
//! is weakly acyclic when no cycle passes a special edge.

use super::{Head, IAtom, IrError, Literal, Pred, RelId, Relations, Rule, Vocab};
use crate::budget::{Budget, BudgetFailure, Held, table_bytes, table_capacity};
use crate::builtins::{BuiltinKind, HeadAction};
use crate::engine::Slot;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::fmt;
use std::sync::Arc;

/// A cycle through a special edge: a value the rule makes at `to` from `from` can reach
/// `from` again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Cycle {
    pub rule: String,
    pub from: String,
    pub to: String,
}

impl fmt::Display for Cycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the existential rules may not terminate: rule '{}' makes a new value at {} from a value at {}, and new values at {} reach {} again (the program is not weakly acyclic)",
            self.rule, self.to, self.from, self.to, self.from
        )
    }
}

const S: u8 = 0;
const O: u8 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Position {
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

fn bound(rules: &[Rule]) -> (usize, usize) {
    let (mut positions, mut edges) = (8usize, 0usize);
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
        edges += body * head + head;
    }
    // the bridges of every column to `ReadAll` and from `Any`
    edges += 3 * positions;
    (positions, edges)
}

/// Check that `rules` are weakly acyclic, charging the position graph to `budget`.
pub(crate) fn check(
    rules: &[Rule],
    relations: &Relations,
    vocab: &Vocab,
    budget: &Arc<Budget>,
) -> Result<(), CheckError> {
    let (positions, edges) = bound(rules);
    let bytes = table_bytes(table_capacity(positions), size_of::<(Position, u32)>())
        + positions * size_of::<Position>()
        + edges * size_of::<(u32, u32, bool, u32)>()
        + positions * super::strata::SCRATCH_PER_NODE;
    let _held = Held::try_new_exact(budget, bytes)?;
    let mut g = Positions {
        ids: FxHashMap::with_capacity_and_hasher(positions, Default::default()),
        keys: Vec::with_capacity(positions),
        edges: Vec::with_capacity(edges),
    };
    let mut body: Vec<(usize, u32)> = Vec::new();
    let mut head: Vec<(usize, u32)> = Vec::new();
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
        let fresh = |v: usize| r.exists.contains(&v) || generated.contains(&v);
        let in_head = |v: usize| head.iter().any(|&(w, _)| w == v);
        for &(v, to) in &head {
            if fresh(v) {
                // a new value: from every position of the frontier, which for a builtin's
                // output is every body variable
                let all = generated.contains(&v);
                for &(w, from) in &body {
                    if all || (in_head(w) && !fresh(w)) {
                        g.edges.push((from, to, true, ri));
                    }
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
        let name = |p: Position| describe(p, relations);
        return Err(CheckError::Cycle(Cycle {
            rule: rules[ri as usize].name.clone(),
            from: name(g.keys[from as usize]),
            to: name(g.keys[to as usize]),
        }));
    }
    Ok(())
}

fn describe(p: Position, relations: &Relations) -> String {
    let col = |c: u8| if c == S { "subject" } else { "object" };
    match p {
        Position::Pred(p, c) => format!("the {} of predicate #{p}", col(c)),
        Position::Any(c) => format!("the {} of a variable predicate's head", col(c)),
        Position::ReadAll(c) => format!("the {} of a variable predicate's atom", col(c)),
        Position::PredCol => "the predicate".into(),
        Position::Rel(r, i) => match relations.get(r) {
            Some(rel) => format!("column {} of relation '{}'", i + 1, rel.name),
            None => format!("column {} of relation {}", i + 1, r.0),
        },
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

impl From<CheckError> for super::ProgramError {
    fn from(e: CheckError) -> Self {
        match e {
            CheckError::Memory(e) => super::ProgramError::Memory(e),
            CheckError::Cycle(c) => super::ProgramError::Invalid(IrError::NotWeaklyAcyclic(c)),
        }
    }
}
