//! The rule IR of the n-ary Datalog core (spec F23, Phase 6, roadmap item 839).
//!
//! A program in the IR is a set of rules over the triple relation and over declared
//! relations with names and arities, with stratified negation of atoms and existential
//! head variables. Frontends produce it: the Jena frontend is [`Rule::from_crule`], which
//! takes the compiled form of a Jena rule as it is, and a rule of the C24 Datalog syntax
//! will declare relations. [`Program::new`] checks a program: the arities of its atoms,
//! that every rule is safe and has an evaluation order, its strata ([`strata`]) and, when
//! it has an existential, its weak acyclicity ([`acyclic`]).
//!
//! The evaluation form stays [`CRule`], which every consumer of rules reads. The IR is
//! analysed here, and lowering a program with relations to `CRule`s is a later phase, so
//! nothing outside this module's tests calls it yet.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod acyclic;
pub(crate) mod strata;
#[cfg(test)]
mod tests;

use crate::budget::{Budget, BudgetFailure, Held};
use crate::builtins::{BuiltinKind, HeadAction};
use crate::engine::{CBuiltin, CHead, CRule, Slot};
use crate::terms::Terms;
use rustc_hash::FxHashMap;
use smallvec::SmallVec;
use std::fmt;
use std::sync::Arc;

/// Relation names in this namespace are reserved for the keys of declared relations,
/// so no input names one.
pub(crate) const RESERVED: &str = "urn:x-sparkles:rel:";

/// A declared relation, by its index in [`Relations`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct RelId(pub u32);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Relation {
    pub name: Box<str>,
    pub arity: u16,
    /// a relation of the engine's own, such as a magic relation of a demand program
    pub hidden: bool,
}

/// The declared relations of a program, dense by id.
#[derive(Clone, Debug, Default)]
pub(crate) struct Relations {
    list: Vec<Relation>,
    by_name: FxHashMap<Box<str>, RelId>,
}

impl Relations {
    /// Declare relation `name` of `arity`, or find it when it is declared with that arity.
    pub fn declare(&mut self, name: &str, arity: u16, hidden: bool) -> Result<RelId, IrError> {
        if name.is_empty() || name.starts_with(RESERVED) {
            return Err(IrError::ReservedName(name.into()));
        }
        if let Some(&id) = self.by_name.get(name) {
            let have = self.list[id.0 as usize].arity;
            if have != arity {
                return Err(IrError::Redeclared {
                    name: name.into(),
                    arity: have,
                    again: arity,
                });
            }
            return Ok(id);
        }
        let id = RelId(u32::try_from(self.list.len()).map_err(|_| IrError::TooMany)?);
        self.list.push(Relation {
            name: name.into(),
            arity,
            hidden,
        });
        self.by_name.insert(name.into(), id);
        Ok(id)
    }

    pub fn get(&self, id: RelId) -> Option<&Relation> {
        self.list.get(id.0 as usize)
    }

    pub fn find(&self, name: &str) -> Option<RelId> {
        self.by_name.get(name).copied()
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn bytes(&self) -> usize {
        let names: usize = self.list.iter().map(|r| 2 * r.name.len()).sum();
        self.list.capacity() * size_of::<Relation>()
            + crate::budget::table_bytes(self.by_name.capacity(), size_of::<(Box<str>, RelId)>())
            + names
    }
}

/// What an atom ranges over: the triple relation, with the arguments `s, p, o`, or a
/// declared relation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Pred {
    Triple,
    Rel(RelId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IAtom {
    pub pred: Pred,
    pub args: SmallVec<[Slot; 4]>,
}

impl IAtom {
    pub fn triple(s: Slot, p: Slot, o: Slot) -> IAtom {
        IAtom {
            pred: Pred::Triple,
            args: SmallVec::from_slice(&[s, p, o]),
        }
    }

    pub fn rel(r: RelId, args: &[Slot]) -> IAtom {
        IAtom {
            pred: Pred::Rel(r),
            args: SmallVec::from_slice(args),
        }
    }

    fn vars(&self) -> impl Iterator<Item = usize> + '_ {
        self.args.iter().filter_map(|s| match s {
            Slot::Var(v) => Some(*v),
            _ => None,
        })
    }
}

/// A builtin call as the analyses read it: what [`CBuiltin`] holds without its compiled
/// pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IBuiltin {
    pub kind: BuiltinKind,
    pub args: SmallVec<[Slot; 4]>,
    /// the variables that must be bound before the call
    pub inputs: SmallVec<[usize; 4]>,
    /// the variables the call binds
    pub outputs: SmallVec<[usize; 4]>,
}

impl IBuiltin {
    fn of(b: &CBuiltin) -> IBuiltin {
        IBuiltin {
            kind: b.kind,
            args: SmallVec::from_slice(&b.args),
            inputs: SmallVec::from_slice(&b.inputs),
            outputs: SmallVec::from_slice(&b.outputs),
        }
    }

    /// A call of `kind` over `args`, with the inputs and outputs its positions give.
    pub fn new(kind: BuiltinKind, args: &[Slot]) -> IBuiltin {
        let var = |i: &usize| match args[*i] {
            Slot::Var(v) => Some(v),
            _ => None,
        };
        IBuiltin {
            kind,
            args: SmallVec::from_slice(args),
            inputs: kind
                .input_positions(args.len())
                .iter()
                .filter_map(var)
                .collect(),
            outputs: kind
                .output_positions(args.len())
                .iter()
                .filter_map(var)
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Literal {
    Pos(IAtom),
    /// a negated atom, which a program evaluates against complete lower strata
    Neg(IAtom),
    Builtin(IBuiltin),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Head {
    Atom(IAtom),
    /// an additive head action over a list (`listMapAsSubject`, `listMapAsObject`)
    Action(HeadAction, SmallVec<[Slot; 3]>),
}

/// A rule of the IR.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Rule {
    pub name: String,
    pub nvars: usize,
    pub body: Vec<Literal>,
    pub head: Vec<Head>,
    /// the head variables that the body does not bind, each standing for a new node
    pub exists: Vec<usize>,
    /// the names of the variables by number, when the frontend has them
    pub vars: Vec<String>,
}

impl Rule {
    /// The Jena frontend: the IR of a compiled Jena rule. Its atoms become positive
    /// triple atoms and its builtins, `noValue` among them, and heads are kept, so the
    /// rule has no relation, negation or existential.
    pub fn from_crule(r: &CRule) -> Rule {
        let mut body = Vec::with_capacity(r.atoms.len() + r.builtins.len());
        body.extend(
            r.atoms
                .iter()
                .map(|a| Literal::Pos(IAtom::triple(a.s, a.p, a.o))),
        );
        body.extend(r.builtins.iter().map(|b| Literal::Builtin(IBuiltin::of(b))));
        let head = r
            .head
            .iter()
            .map(|h| match h {
                CHead::Triple(s, p, o) => Head::Atom(IAtom::triple(*s, *p, *o)),
                CHead::Action(a, args) => Head::Action(*a, SmallVec::from_slice(args)),
            })
            .collect();
        Rule {
            name: r.name.clone(),
            nvars: r.nvars,
            body,
            head,
            exists: Vec::new(),
            vars: Vec::new(),
        }
    }

    /// Whether the rule uses what only the IR has: a declared relation, a negated atom
    /// or an existential.
    pub fn uses_ir(&self) -> bool {
        !self.exists.is_empty()
            || self.body.iter().any(|l| match l {
                Literal::Pos(a) => a.pred != Pred::Triple,
                Literal::Neg(_) => true,
                Literal::Builtin(_) => false,
            })
            || self
                .head
                .iter()
                .any(|h| matches!(h, Head::Atom(a) if a.pred != Pred::Triple))
    }

    pub(crate) fn var_name(&self, v: usize) -> String {
        match self.vars.get(v) {
            Some(n) if !n.is_empty() => format!("?{n}"),
            _ => format!("?v{v}"),
        }
    }

    fn bytes(&self) -> usize {
        let mut n = self.name.capacity()
            + self.body.capacity() * size_of::<Literal>()
            + self.head.capacity() * size_of::<Head>()
            + self.exists.capacity() * size_of::<usize>()
            + self.vars.capacity() * size_of::<String>();
        n += self.vars.iter().map(String::capacity).sum::<usize>();
        let spill = |a: &SmallVec<[Slot; 4]>| {
            if a.spilled() {
                a.capacity() * size_of::<Slot>()
            } else {
                0
            }
        };
        for l in &self.body {
            n += match l {
                Literal::Pos(a) | Literal::Neg(a) => spill(&a.args),
                Literal::Builtin(b) => {
                    spill(&b.args)
                        + [&b.inputs, &b.outputs]
                            .iter()
                            .filter(|v| v.spilled())
                            .map(|v| v.capacity() * size_of::<usize>())
                            .sum::<usize>()
                }
            };
        }
        for h in &self.head {
            if let Head::Atom(a) = h {
                n += spill(&a.args);
            }
        }
        n
    }
}

/// The terms the analyses name: `rdf:type`, whose constant classes are nodes of their
/// own, and the list vocabulary.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Vocab {
    pub rdf_type: Option<u64>,
    pub rdf_first: u64,
    pub rdf_rest: u64,
}

impl Vocab {
    pub fn of(terms: &Terms) -> Vocab {
        Vocab {
            rdf_type: terms
                .try_id_for(&oxrdf::Term::NamedNode(
                    oxrdf::vocab::rdf::TYPE.into_owned(),
                ))
                .ok(),
            rdf_first: terms.rdf_first,
            rdf_rest: terms.rdf_rest,
        }
    }
}

/// Why a program is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IrError {
    ReservedName(String),
    Redeclared {
        name: String,
        arity: u16,
        again: u16,
    },
    TooMany,
    UnknownRelation {
        rule: String,
    },
    Arity {
        rule: String,
        relation: String,
        arity: u16,
        got: usize,
    },
    /// a variable that a head, a negated atom or a builtin needs and the body never binds
    Unsafe {
        rule: String,
        var: String,
        place: &'static str,
    },
    /// an existential that the body binds too
    BoundExistential {
        rule: String,
        var: String,
    },
    /// an existential in the predicate position of a triple head
    ExistentialPredicate {
        rule: String,
        var: String,
    },
    /// builtins whose inputs no order of the body binds
    Unschedulable {
        rule: String,
        builtins: Vec<String>,
    },
    NegativeCycle(strata::NegativeCycle),
    NotWeaklyAcyclic(acyclic::Cycle),
}

impl fmt::Display for IrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IrError::ReservedName(n) if n.is_empty() => write!(f, "a relation needs a name"),
            IrError::ReservedName(n) => {
                write!(
                    f,
                    "relation name '{n}' is in the reserved namespace {RESERVED}"
                )
            }
            IrError::Redeclared { name, arity, again } => write!(
                f,
                "relation '{name}' is declared with arity {arity} and again with arity {again}"
            ),
            IrError::TooMany => write!(f, "too many relations"),
            IrError::UnknownRelation { rule } => {
                write!(f, "rule '{rule}' reads a relation that is not declared")
            }
            IrError::Arity {
                rule,
                relation,
                arity,
                got,
            } => write!(
                f,
                "rule '{rule}': relation '{relation}' has arity {arity}, not {got}"
            ),
            IrError::Unsafe { rule, var, place } => write!(
                f,
                "rule '{rule}': variable {var} of {place} is not bound by a positive atom or a builtin of the body"
            ),
            IrError::BoundExistential { rule, var } => write!(
                f,
                "rule '{rule}': existential variable {var} is bound by the body"
            ),
            IrError::ExistentialPredicate { rule, var } => write!(
                f,
                "rule '{rule}': existential variable {var} is in the predicate position of a head"
            ),
            IrError::Unschedulable { rule, builtins } => write!(
                f,
                "rule '{rule}': no order of the body binds the inputs of {}",
                builtins.join(", ")
            ),
            IrError::NegativeCycle(c) => c.fmt(f),
            IrError::NotWeaklyAcyclic(c) => c.fmt(f),
        }
    }
}

/// A refused program: memory, or an error of the program.
#[derive(Debug)]
pub(crate) enum ProgramError {
    Memory(BudgetFailure),
    Invalid(IrError),
}

impl From<BudgetFailure> for ProgramError {
    fn from(e: BudgetFailure) -> Self {
        ProgramError::Memory(e)
    }
}

impl From<IrError> for ProgramError {
    fn from(e: IrError) -> Self {
        ProgramError::Invalid(e)
    }
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Memory(e) => e.fmt(f),
            ProgramError::Invalid(e) => e.fmt(f),
        }
    }
}

/// A checked program: its relations, its rules and their strata.
pub(crate) struct Program {
    pub relations: Relations,
    pub rules: Vec<Rule>,
    pub strata: strata::Strata,
    _held: Held,
}

impl fmt::Debug for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Program")
            .field("relations", &self.relations.len())
            .field("rules", &self.rules.len())
            .field("strata", &self.strata)
            .finish()
    }
}

impl Program {
    /// Check `rules` over `relations` and stratify them. The program's rules, relations
    /// and strata are charged to `budget` while it lives, and the analyses' working
    /// memory while they run.
    ///
    /// A program that uses an IR feature (see [`Rule::uses_ir`]) and has a negation, a
    /// negated atom or a `noValue`, is refused when it does not stratify. A program of
    /// Jena rules alone is accepted whatever its strata, since it keeps Jena's reading
    /// of `noValue`, and its strata say whether it stratifies. A program with an
    /// existential is refused unless it is weakly acyclic.
    pub fn new(
        relations: Relations,
        rules: Vec<Rule>,
        vocab: &Vocab,
        budget: &Arc<Budget>,
    ) -> Result<Program, ProgramError> {
        let mut held = Held::new(budget);
        let mut kept = relations.bytes() + rules.capacity() * size_of::<Rule>();
        for r in &rules {
            kept += r.bytes();
        }
        held.try_grow(kept)?;
        for r in &rules {
            check_rule(r, &relations)?;
        }
        let strata = strata::Strata::of(&rules, vocab, budget)?;
        held.try_grow(strata.bytes())?;
        let ir = rules.iter().any(Rule::uses_ir);
        if ir
            && strata.has_negation
            && let Err(c) = &strata.stratified
        {
            return Err(IrError::NegativeCycle(c.clone()).into());
        }
        if rules.iter().any(|r| !r.exists.is_empty()) {
            acyclic::check(&rules, &relations, vocab, budget)?;
        }
        Ok(Program {
            relations,
            rules,
            strata,
            _held: held,
        })
    }

    pub fn uses_ir(&self) -> bool {
        self.rules.iter().any(Rule::uses_ir)
    }
}

/// The checks of one rule: the arities of its atoms, safety and an evaluation order.
fn check_rule(r: &Rule, relations: &Relations) -> Result<(), IrError> {
    let arity = |a: &IAtom| -> Result<(), IrError> {
        match a.pred {
            Pred::Triple if a.args.len() == 3 => Ok(()),
            Pred::Triple => Err(IrError::Arity {
                rule: r.name.clone(),
                relation: "the triple relation".into(),
                arity: 3,
                got: a.args.len(),
            }),
            Pred::Rel(id) => {
                let Some(rel) = relations.get(id) else {
                    return Err(IrError::UnknownRelation {
                        rule: r.name.clone(),
                    });
                };
                if rel.arity as usize == a.args.len() {
                    Ok(())
                } else {
                    Err(IrError::Arity {
                        rule: r.name.clone(),
                        relation: rel.name.to_string(),
                        arity: rel.arity,
                        got: a.args.len(),
                    })
                }
            }
        }
    };
    for l in &r.body {
        if let Literal::Pos(a) | Literal::Neg(a) = l {
            arity(a)?;
        }
    }
    for h in &r.head {
        if let Head::Atom(a) = h {
            arity(a)?;
        }
    }
    // the planner's fixpoint without statistics: positive atoms bind their variables,
    // and a builtin binds its outputs once its inputs are bound
    let mut bound = vec![false; r.nvars];
    let mark = |v: usize, bound: &mut Vec<bool>| {
        if v >= bound.len() {
            bound.resize(v + 1, false);
        }
        bound[v] = true;
    };
    for l in &r.body {
        if let Literal::Pos(a) = l {
            for v in a.vars() {
                mark(v, &mut bound);
            }
        }
    }
    let is = |v: usize, bound: &[bool]| bound.get(v).copied().unwrap_or(false);
    let mut left: Vec<&IBuiltin> = r
        .body
        .iter()
        .filter_map(|l| match l {
            Literal::Builtin(b) => Some(b),
            _ => None,
        })
        .collect();
    loop {
        let before = left.len();
        let mut i = 0;
        while i < left.len() {
            if left[i].inputs.iter().all(|&v| is(v, &bound)) {
                for &v in &left[i].outputs {
                    mark(v, &mut bound);
                }
                left.swap_remove(i);
            } else {
                i += 1;
            }
        }
        if left.len() == before {
            break;
        }
    }
    if !left.is_empty() {
        return Err(IrError::Unschedulable {
            rule: r.name.clone(),
            builtins: left.iter().map(|b| format!("{:?}", b.kind)).collect(),
        });
    }
    let unsafe_var = |v: usize, place: &'static str| IrError::Unsafe {
        rule: r.name.clone(),
        var: r.var_name(v),
        place,
    };
    for l in &r.body {
        if let Literal::Neg(a) = l
            && let Some(v) = a.vars().find(|&v| !is(v, &bound))
        {
            return Err(unsafe_var(v, "a negated atom"));
        }
    }
    for &v in &r.exists {
        if is(v, &bound) {
            return Err(IrError::BoundExistential {
                rule: r.name.clone(),
                var: r.var_name(v),
            });
        }
    }
    let free = |v: usize| !is(v, &bound) && !r.exists.contains(&v);
    for h in &r.head {
        match h {
            Head::Atom(a) => {
                if a.pred == Pred::Triple
                    && let Slot::Var(v) = a.args[1]
                    && r.exists.contains(&v)
                {
                    return Err(IrError::ExistentialPredicate {
                        rule: r.name.clone(),
                        var: r.var_name(v),
                    });
                }
                if let Some(v) = a.vars().find(|&v| free(v)) {
                    return Err(unsafe_var(v, "the head"));
                }
            }
            Head::Action(_, args) => {
                for s in args {
                    if let Slot::Var(v) = *s
                        && !is(v, &bound)
                    {
                        return Err(unsafe_var(v, "a head action"));
                    }
                }
            }
        }
    }
    Ok(())
}
