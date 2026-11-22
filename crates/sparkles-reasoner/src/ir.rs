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
//! The evaluation form stays [`CRule`], which every consumer of rules reads, and
//! [`lower`] gives a program's rules in that form, by stratum, which
//! [`engine::run_program`](crate::engine::run_program) evaluates. No frontend produces a
//! program with relations yet, so only tests reach it.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod acyclic;
#[cfg(test)]
mod eval_tests;
pub(crate) mod lower;
pub(crate) mod rebuild;
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
            return Err(IrError::ReservedName);
        }
        if let Some(&id) = self.by_name.get(name) {
            let have = self.list[id.0 as usize].arity;
            if have != arity {
                return Err(IrError::Redeclared {
                    relation: id,
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
    /// the occurrence of a compiled Jena builtin in its program ([`CBuiltin::origin`]),
    /// which the lowering keeps
    pub origin: Option<usize>,
}

impl IBuiltin {
    pub(crate) fn of(b: &CBuiltin) -> IBuiltin {
        IBuiltin {
            kind: b.kind,
            args: SmallVec::from_slice(&b.args),
            inputs: SmallVec::from_slice(&b.inputs),
            outputs: SmallVec::from_slice(&b.outputs),
            origin: Some(b.origin),
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
            origin: None,
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

    fn var(&self, v: usize) -> VarName<'_> {
        VarName(self, v)
    }

    /// Every variable the rule names, with where it is.
    fn slots(&self) -> impl Iterator<Item = &Slot> + '_ {
        let body = self.body.iter().flat_map(|l| match l {
            Literal::Pos(a) | Literal::Neg(a) => a.args.iter(),
            Literal::Builtin(b) => b.args.iter(),
        });
        let head = self.head.iter().flat_map(|h| match h {
            Head::Atom(a) => a.args.iter(),
            Head::Action(_, args) => args.iter(),
        });
        body.chain(head)
    }

    fn bytes(&self) -> usize {
        self.bytes_as(false)
    }

    /// A bound of the bytes of the rule's clone, or of a copy that clones its parts: a
    /// spilled `SmallVec` clones by collecting, which rounds its capacity up to a power
    /// of two, and a `Vec` or `String` clone has the capacity of its length.
    fn clone_bytes(&self) -> usize {
        self.bytes_as(true)
    }

    fn bytes_as(&self, clone: bool) -> usize {
        let mut n = self.name.capacity()
            + self.body.capacity() * size_of::<Literal>()
            + self.head.capacity() * size_of::<Head>()
            + self.exists.capacity() * size_of::<usize>()
            + self.vars.capacity() * size_of::<String>();
        n += self.vars.iter().map(String::capacity).sum::<usize>();
        // the slots a spilled vector of `len` items and capacity `cap` holds
        let slots = |spilled: bool, len: usize, cap: usize| match (spilled, clone) {
            (false, _) => 0,
            (true, false) => cap,
            (true, true) => cap.max(len.next_power_of_two()),
        };
        let spill =
            |a: &SmallVec<[Slot; 4]>| slots(a.spilled(), a.len(), a.capacity()) * size_of::<Slot>();
        for l in &self.body {
            n += match l {
                Literal::Pos(a) | Literal::Neg(a) => spill(&a.args),
                Literal::Builtin(b) => {
                    spill(&b.args)
                        + [&b.inputs, &b.outputs]
                            .iter()
                            .map(|v| slots(v.spilled(), v.len(), v.capacity()) * size_of::<usize>())
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

/// A variable of a rule, by its name when the frontend gave one.
struct VarName<'a>(&'a Rule, usize);

impl fmt::Display for VarName<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0.vars.get(self.1) {
            Some(n) if !n.is_empty() => write!(f, "?{n}"),
            _ => write!(f, "?v{}", self.1),
        }
    }
}

/// Why a program is refused. Rules, literals and variables are named by their indexes,
/// and [`ProgramError::Invalid`] holds the message that names them, charged to the
/// program's budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IrError {
    /// an empty name, or one in [`RESERVED`]
    ReservedName,
    Redeclared {
        relation: RelId,
        arity: u16,
        again: u16,
    },
    TooMany,
    UnknownRelation {
        rule: usize,
    },
    Arity {
        rule: usize,
        relation: Option<RelId>,
        arity: u16,
        got: usize,
    },
    /// a variable number at or past the rule's `nvars`
    BadVariable {
        rule: usize,
        var: usize,
    },
    /// a variable that a head, a negated atom or a builtin needs and the body never binds
    Unsafe {
        rule: usize,
        var: usize,
        place: &'static str,
    },
    /// an existential that the body binds too
    BoundExistential {
        rule: usize,
        var: usize,
    },
    /// an existential in the predicate position of a triple head
    ExistentialPredicate {
        rule: usize,
        var: usize,
    },
    /// a builtin whose inputs no order of the body binds, with an input left unbound
    Unschedulable {
        rule: usize,
        literal: usize,
        var: usize,
    },
    NegativeCycle(strata::NegativeCycle),
    NotWeaklyAcyclic(acyclic::Cycle),
}

impl IrError {
    /// The message of the error, which names the rules, literals and variables of
    /// `rules` and the relations of `relations` that it points to.
    pub fn display<'a>(
        &'a self,
        rules: &'a [Rule],
        relations: &'a Relations,
    ) -> impl fmt::Display + 'a {
        Message(self, rules, relations)
    }
}

struct Message<'a>(&'a IrError, &'a [Rule], &'a Relations);

impl fmt::Display for Message<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Message(e, rules, relations) = self;
        let rule = |i: &usize| rules.get(*i).map_or("?", |r| r.name.as_str());
        let rel = |r: &RelId| relations.get(*r).map_or("?", |r| &*r.name);
        match e {
            IrError::ReservedName => write!(
                f,
                "a relation needs a name outside the reserved namespace {RESERVED}"
            ),
            IrError::Redeclared {
                relation,
                arity,
                again,
            } => write!(
                f,
                "relation '{}' is declared with arity {arity} and again with arity {again}",
                rel(relation)
            ),
            IrError::TooMany => write!(f, "too many relations"),
            IrError::UnknownRelation { rule: r } => {
                write!(
                    f,
                    "rule '{}' reads a relation that is not declared",
                    rule(r)
                )
            }
            IrError::Arity {
                rule: r,
                relation,
                arity,
                got,
            } => match relation {
                Some(x) => write!(
                    f,
                    "rule '{}': relation '{}' has arity {arity}, not {got}",
                    rule(r),
                    rel(x)
                ),
                None => write!(
                    f,
                    "rule '{}': a triple pattern has 3 terms, not {got}",
                    rule(r)
                ),
            },
            IrError::BadVariable { rule: r, var } => write!(
                f,
                "rule '{}': variable number {var} is past the rule's variables",
                rule(r)
            ),
            IrError::Unsafe {
                rule: r,
                var,
                place,
            } => write!(
                f,
                "rule '{}': variable {} of {place} is not bound by a positive atom or a builtin of the body",
                rule(r),
                rules[*r].var(*var)
            ),
            IrError::BoundExistential { rule: r, var } => write!(
                f,
                "rule '{}': existential variable {} is bound by the body",
                rule(r),
                rules[*r].var(*var)
            ),
            IrError::ExistentialPredicate { rule: r, var } => write!(
                f,
                "rule '{}': existential variable {} is in the predicate position of a head",
                rule(r),
                rules[*r].var(*var)
            ),
            IrError::Unschedulable {
                rule: r,
                literal,
                var,
            } => {
                let kind = match rules[*r].body.get(*literal) {
                    Some(Literal::Builtin(b)) => b.kind,
                    _ => BuiltinKind::NoValue,
                };
                write!(
                    f,
                    "rule '{}': no order of the body binds input {} of builtin {kind:?}",
                    rule(r),
                    rules[*r].var(*var)
                )
            }
            IrError::NegativeCycle(c) => {
                write!(
                    f,
                    "the rules are not stratified: rule '{}' negates literal {} of its body, which depends on the rule's own conclusions through the rules ",
                    rule(&c.rule),
                    c.literal + 1
                )?;
                for (i, r) in c.rules.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "'{}'", rule(r))?;
                }
                Ok(())
            }
            IrError::NotWeaklyAcyclic(c) => {
                write!(
                    f,
                    "the existential rules may not terminate: rule '{}' makes a new value at ",
                    rule(&c.rule)
                )?;
                c.to.write(f, relations)?;
                f.write_str(" from a value at ")?;
                c.from.write(f, relations)?;
                f.write_str(
                    ", and new values there reach it again (the program is not weakly acyclic)",
                )
            }
        }
    }
}

/// A refused program: memory, or an error of the program with its message.
#[derive(Debug)]
pub(crate) enum ProgramError {
    Memory(BudgetFailure),
    Invalid {
        error: IrError,
        message: String,
        _held: Held,
    },
}

impl From<BudgetFailure> for ProgramError {
    fn from(e: BudgetFailure) -> Self {
        ProgramError::Memory(e)
    }
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Memory(e) => e.fmt(f),
            ProgramError::Invalid { message, .. } => f.write_str(message),
        }
    }
}

impl ProgramError {
    /// The refusal `error` of `rules`, with its message charged to `budget` before it is
    /// formatted, with the heap that `error` holds.
    fn invalid(
        error: IrError,
        rules: &[Rule],
        relations: &Relations,
        budget: &Arc<Budget>,
    ) -> Self {
        let shown = error.display(rules, relations);
        let len = match crate::budget::format_len(format_args!("{shown}")) {
            Ok(len) => len,
            Err(e) => return ProgramError::Memory(e),
        };
        // the heap the error holds, which the error's charge covers from here on
        let heap = match &error {
            IrError::NegativeCycle(c) => c.rules.capacity() * size_of::<usize>(),
            _ => 0,
        };
        let held = match Held::try_new_exact(budget, len + heap) {
            Ok(held) => held,
            Err(e) => return ProgramError::Memory(e),
        };
        let mut message = String::with_capacity(len);
        use fmt::Write;
        write!(message, "{shown}").expect("formatting into a String");
        drop(shown);
        ProgramError::Invalid {
            error,
            message,
            _held: held,
        }
    }
}

/// A checked program: its relations, its rules and their strata.
pub(crate) struct Program {
    pub relations: Relations,
    pub rules: Vec<Rule>,
    pub strata: strata::Strata,
    /// the built-in rule set an `@include` brought each rule from, by rule, when the
    /// frontend knows it (empty otherwise)
    pub includes: Vec<Option<Box<str>>>,
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
    /// Check `rules` over `relations` and stratify them. The program's rules and
    /// relations are charged to `budget` while it lives, its strata with them, and the
    /// analyses' working memory while they run.
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
        let refuse = |e: IrError| ProgramError::invalid(e, &rules, &relations, budget);
        for (i, r) in rules.iter().enumerate() {
            match check_rule(i, r, &relations, budget) {
                Ok(()) => {}
                Err(Check::Memory(e)) => return Err(e.into()),
                Err(Check::Invalid(e)) => return Err(refuse(e)),
            }
        }
        let mut strata = strata::Strata::of(&rules, vocab, budget)?;
        let ir = rules.iter().any(Rule::uses_ir);
        if ir && strata.has_negation && strata.stratified.is_err() {
            // the cycle moves into the error, which charges it before the strata release
            // their charge of it
            let Err(c) = std::mem::replace(&mut strata.stratified, Ok(())) else {
                unreachable!("checked above")
            };
            let refused = refuse(IrError::NegativeCycle(c));
            drop(strata);
            return Err(refused);
        }
        if rules.iter().any(|r| !r.exists.is_empty()) {
            match acyclic::check(&rules, vocab, budget) {
                Ok(()) => {}
                Err(acyclic::CheckError::Memory(e)) => return Err(e.into()),
                Err(acyclic::CheckError::Cycle(c)) => {
                    return Err(refuse(IrError::NotWeaklyAcyclic(c)));
                }
            }
        }
        Ok(Program {
            relations,
            rules,
            strata,
            includes: Vec::new(),
            _held: held,
        })
    }

    /// Record the included rule set of each rule, charged to the program's budget.
    pub fn set_includes(&mut self, includes: Vec<Option<Box<str>>>) -> Result<(), BudgetFailure> {
        let bytes = includes.capacity() * size_of::<Option<Box<str>>>()
            + includes.iter().flatten().map(|s| s.len()).sum::<usize>();
        self._held.try_grow(bytes)?;
        self.includes = includes;
        Ok(())
    }

    /// The included rule set of rule `ri`, if it came from one.
    pub fn include(&self, ri: usize) -> Option<&str> {
        self.includes.get(ri).and_then(|s| s.as_deref())
    }

    pub fn uses_ir(&self) -> bool {
        self.rules.iter().any(Rule::uses_ir)
    }
}

enum Check {
    Memory(BudgetFailure),
    Invalid(IrError),
}

impl From<BudgetFailure> for Check {
    fn from(e: BudgetFailure) -> Self {
        Check::Memory(e)
    }
}

impl From<IrError> for Check {
    fn from(e: IrError) -> Self {
        Check::Invalid(e)
    }
}

/// The checks of rule `ri`: its variable numbers, the arities of its atoms, safety and
/// an evaluation order. Its working memory is charged to `budget` first.
fn check_rule(
    ri: usize,
    r: &Rule,
    relations: &Relations,
    budget: &Arc<Budget>,
) -> Result<(), Check> {
    let vars_ok = |v: usize| v < r.nvars;
    for s in r.slots() {
        if let Slot::Var(v) = *s
            && !vars_ok(v)
        {
            return Err(IrError::BadVariable { rule: ri, var: v }.into());
        }
    }
    for l in &r.body {
        if let Literal::Builtin(b) = l
            && let Some(&v) = b.inputs.iter().chain(&b.outputs).find(|&&v| !vars_ok(v))
        {
            return Err(IrError::BadVariable { rule: ri, var: v }.into());
        }
    }
    if let Some(&v) = r.exists.iter().find(|&&v| !vars_ok(v)) {
        return Err(IrError::BadVariable { rule: ri, var: v }.into());
    }
    let arity = |a: &IAtom| -> Result<(), IrError> {
        match a.pred {
            Pred::Triple if a.args.len() == 3 => Ok(()),
            Pred::Triple => Err(IrError::Arity {
                rule: ri,
                relation: None,
                arity: 3,
                got: a.args.len(),
            }),
            Pred::Rel(id) => match relations.get(id) {
                None => Err(IrError::UnknownRelation { rule: ri }),
                Some(rel) if rel.arity as usize == a.args.len() => Ok(()),
                Some(rel) => Err(IrError::Arity {
                    rule: ri,
                    relation: Some(id),
                    arity: rel.arity,
                    got: a.args.len(),
                }),
            },
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
    let _scratch = Held::try_new_exact(
        budget,
        r.nvars * size_of::<bool>() + r.body.len() * size_of::<usize>(),
    )?;
    let mut bound = vec![false; r.nvars];
    for l in &r.body {
        if let Literal::Pos(a) = l {
            for v in a.vars() {
                bound[v] = true;
            }
        }
    }
    let mut left: Vec<usize> = Vec::with_capacity(r.body.len());
    left.extend(
        r.body
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l, Literal::Builtin(_)))
            .map(|(i, _)| i),
    );
    let builtin = |i: usize| match &r.body[i] {
        Literal::Builtin(b) => b,
        _ => unreachable!("only builtins are left"),
    };
    loop {
        let before = left.len();
        let mut i = 0;
        while i < left.len() {
            let b = builtin(left[i]);
            if b.inputs.iter().all(|&v| bound[v]) {
                for &v in &b.outputs {
                    bound[v] = true;
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
    if let Some(&li) = left.iter().min() {
        let b = builtin(li);
        let var = b.inputs.iter().copied().find(|&v| !bound[v]).unwrap_or(0);
        return Err(IrError::Unschedulable {
            rule: ri,
            literal: li,
            var,
        }
        .into());
    }
    let unsafe_var = |var: usize, place: &'static str| IrError::Unsafe {
        rule: ri,
        var,
        place,
    };
    for l in &r.body {
        if let Literal::Neg(a) = l
            && let Some(v) = a.vars().find(|&v| !bound[v])
        {
            return Err(unsafe_var(v, "a negated atom").into());
        }
    }
    if let Some(&v) = r.exists.iter().find(|&&v| bound[v]) {
        return Err(IrError::BoundExistential { rule: ri, var: v }.into());
    }
    let free = |v: usize| !bound[v] && !r.exists.contains(&v);
    for h in &r.head {
        match h {
            Head::Atom(a) => {
                if a.pred == Pred::Triple
                    && let Slot::Var(v) = a.args[1]
                    && r.exists.contains(&v)
                {
                    return Err(IrError::ExistentialPredicate { rule: ri, var: v }.into());
                }
                if let Some(v) = a.vars().find(|&v| free(v)) {
                    return Err(unsafe_var(v, "the head").into());
                }
            }
            Head::Action(_, args) => {
                for s in args {
                    if let Slot::Var(v) = *s
                        && !bound[v]
                    {
                        return Err(unsafe_var(v, "a head action").into());
                    }
                }
            }
        }
    }
    Ok(())
}
