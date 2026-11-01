//! Forward rules as a program of the fixed-point operator of F13 §4.3
//! ([`sparkles_core::incr::fixpoint`]), the reasoner's plug-in to the one fixed-point
//! interface that property paths share.
//!
//! [`RuleProgram`] takes forward rules of triple patterns in Jena's syntax, as
//! [`crate::parser`] reads them, and keeps their closure over the triples of the default
//! graph. Its facts are triples. A triple of the store is a fact with the record of an
//! explicit fact, and a derived triple records the rule and the bindings of the instance
//! that first derived it. The operator then maintains the closure from each commit's
//! changes by backward and forward rederivation, and annotates each triple with the
//! witness of its recorded derivation (spec F13 §5).
//!
//! The program indexes its facts by predicate, by predicate and subject and by predicate
//! and object, and matches rule bodies against that index, so a rule premise is any fact
//! the operator holds, explicit or derived. Rules with builtins, functors, wildcards,
//! nested rules, more than [`MAX_VARS`] variables or a head variable that the body does
//! not bind are refused. The reasoner's own materialization does not use this program;
//! it is the interface through which its maintenance can move onto F13's operator.

use crate::parser::{Clause, Direction, Node, Rule};
use oxrdf::Term;
use rustc_hash::{FxHashMap, FxHashSet};
use sparkles_core::id::Id;
use sparkles_core::incr::fixpoint::{self, Program};
use sparkles_core::incr::pass::Which;
use sparkles_core::incr::persist::{Reader, Writer};
use sparkles_core::incr::prov::{Ann, Atom, FactKey, Provenance};
use sparkles_core::incr::terms::Terms;
use sparkles_core::store::Snapshot;
use sparkles_core::task::Control;
use std::sync::Arc;

/// The most variables a rule of the program binds.
pub const MAX_VARS: usize = 4;

/// The record of an explicit fact.
const EXPLICIT: u32 = u32::MAX;

/// A fact: subject, predicate, object.
pub type Triple = [Id; 3];

/// The instance that derived a fact: the rule's number and its variables' values, or an
/// explicit fact.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Record {
    pub rule: u32,
    pub vars: [Id; MAX_VARS],
}

impl Record {
    pub const EXPLICIT: Record = Record {
        rule: EXPLICIT,
        vars: [Id::UNDEF; MAX_VARS],
    };

    pub fn is_explicit(&self) -> bool {
        self.rule == EXPLICIT
    }
}

/// A position of a compiled pattern.
#[derive(Clone, Debug)]
enum Pos {
    Var(usize),
    Const(Term),
}

/// A position resolved at a pass.
#[derive(Clone, Copy, Debug)]
enum R {
    Var(usize),
    Id(Id),
    /// a position that matches nothing
    Never,
}

/// A rule's body and head resolved at a pass.
type Resolved = (Vec<[R; 3]>, Vec<[R; 3]>);

#[derive(Clone, Debug)]
struct CRule {
    body: Vec<[Pos; 3]>,
    head: Vec<[Pos; 3]>,
}

/// What a pass of the program reads: the snapshots before and after the changes, the
/// changed triples of the default graph and the task's control.
pub struct RuleCx<'a> {
    pub new: Arc<Snapshot>,
    pub old: Arc<Snapshot>,
    pub added: &'a [Triple],
    pub removed: &'a [Triple],
    pub control: &'a Control,
}

impl RuleCx<'_> {
    fn snap(&self, w: Which) -> &Snapshot {
        match w {
            Which::New => &self.new,
            Which::Old => &self.old,
        }
    }
}

/// Forward rules over the default graph's triples, as a fixed-point program.
#[derive(Clone, Debug, Default)]
pub struct RuleProgram {
    rules: Vec<CRule>,
    /// the rules' patterns resolved at the current pass
    resolved: Vec<Resolved>,
    /// the rules' constants that the store does not hold, as local ids, which the
    /// facts and their checkpoints share
    terms: Terms,
    /// the ids the rules' constants resolved to at the last pass
    consts: Vec<Id>,
    by_p: FxHashMap<Id, FxHashSet<(Id, Id)>>,
    ps: FxHashMap<(Id, Id), FxHashSet<Id>>,
    po: FxHashMap<(Id, Id), FxHashSet<Id>>,
}

/// Why a rule cannot be part of a [`RuleProgram`].
#[derive(Debug, Clone, thiserror::Error)]
#[error("rule {rule}: {reason}")]
pub struct Unsupported {
    pub rule: String,
    pub reason: String,
}

impl RuleProgram {
    /// The program of `rules`, which must be forward rules of triple patterns.
    pub fn new(rules: &[Rule]) -> Result<RuleProgram, Unsupported> {
        let mut out = Vec::new();
        for rule in rules {
            let bad = |reason: &str| Unsupported {
                rule: rule.label(),
                reason: reason.to_string(),
            };
            if rule.direction != Direction::Forward {
                return Err(bad("a backward rule"));
            }
            let mut vars: Vec<String> = Vec::new();
            let mut compile = |clauses: &[Clause],
                               bind: bool|
             -> Result<Vec<[Pos; 3]>, Unsupported> {
                let mut pats = Vec::new();
                for c in clauses {
                    let Clause::Triple(t) = c else {
                        return Err(bad("a builtin or a nested rule"));
                    };
                    let mut pat = Vec::with_capacity(3);
                    for n in [&t.subject, &t.predicate, &t.object] {
                        pat.push(match n {
                            Node::Var(v) => match vars.iter().position(|x| x == v) {
                                Some(i) => Pos::Var(i),
                                None if bind => {
                                    vars.push(v.clone());
                                    Pos::Var(vars.len() - 1)
                                }
                                None => return Err(bad("a head variable the body does not bind")),
                            },
                            Node::Const(t) => Pos::Const(t.clone()),
                            Node::Any => return Err(bad("a wildcard")),
                            Node::Functor(..) => return Err(bad("a functor")),
                        });
                    }
                    let [s, p, o] = <[Pos; 3]>::try_from(pat).expect("three positions");
                    pats.push([s, p, o]);
                }
                Ok(pats)
            };
            let body = compile(&rule.body, true)?;
            let head = compile(&rule.head, false)?;
            if body.is_empty() || head.is_empty() {
                return Err(bad("an empty body or head"));
            }
            if vars.len() > MAX_VARS {
                return Err(bad(&format!("more than {MAX_VARS} variables")));
            }
            out.push(CRule { body, head });
        }
        Ok(RuleProgram {
            rules: out,
            ..Default::default()
        })
    }

    /// Resolve the rules' constants at the pass's new snapshot. Call it before each
    /// build or advance. A constant that the store does not hold gets a local id, in
    /// bodies and heads alike, so that a rule matches the triples that another rule
    /// derives with it. When the store comes to hold such a constant, the program's
    /// facts name the wrong id: `begin` then returns `true`, and the caller builds the
    /// fixed point again instead of advancing it.
    pub fn begin(&mut self, cx: &RuleCx) -> bool {
        let mut consts = Vec::new();
        let mut resolved = Vec::with_capacity(self.rules.len());
        for c in &self.rules {
            let mut side = |pats: &[[Pos; 3]]| -> Vec<[R; 3]> {
                pats.iter()
                    .map(|pat| {
                        let mut out = [R::Never; 3];
                        for (i, p) in pat.iter().enumerate() {
                            out[i] = match p {
                                Pos::Var(v) => R::Var(*v),
                                Pos::Const(t) => {
                                    let id = self.terms.intern(&cx.new, t);
                                    consts.push(id);
                                    R::Id(id)
                                }
                            };
                        }
                        out
                    })
                    .collect()
            };
            let b = side(&c.body);
            let h = side(&c.head);
            resolved.push((b, h));
        }
        self.resolved = resolved;
        let moved = !self.consts.is_empty() && self.consts != consts;
        self.consts = consts;
        moved
    }

    /// The bytes of the fixed point `fp` of a program, with the term table of its local
    /// constants. A checkpoint must use it, or [`Self::decode`]'s counterpart, rather
    /// than a writer over other terms, because the facts name the program's local ids.
    pub fn encode(
        fp: &fixpoint::FixedPoint<RuleProgram>,
        snap: &Snapshot,
    ) -> sparkles_core::Result<Vec<u8>> {
        let mut w = Writer::new(snap, &fp.program.terms);
        fp.save(&mut w)?;
        Ok(w.finish())
    }

    /// Load into `fp` the state that [`Self::encode`] wrote, mapping its terms into the
    /// id space of `snap` and the program's fresh local terms. Call [`Self::begin`]
    /// before the next pass, as for any other.
    pub fn decode(
        fp: &mut fixpoint::FixedPoint<RuleProgram>,
        data: &[u8],
        snap: &Snapshot,
    ) -> sparkles_core::Result<()> {
        let mut terms = Terms::default();
        let mut r = Reader::new(data, snap, &mut terms)?;
        fp.program.terms = terms;
        fp.program.consts.clear();
        fp.program.resolved.clear();
        fp.load(&mut r)
    }

    /// The term of an id of the program's facts at `snap`.
    pub fn term(&self, snap: &Snapshot, id: Id) -> Option<Term> {
        self.terms.term(snap, id)
    }

    /// Bind `t` into `b` by pattern `pat`, or `false` when it does not match.
    fn bind(pat: &[R; 3], t: &Triple, b: &mut [Id; MAX_VARS]) -> bool {
        for (r, &x) in pat.iter().zip(t.iter()) {
            match *r {
                R::Never => return false,
                R::Id(c) if c != x => return false,
                R::Id(_) => {}
                R::Var(v) if b[v].is_undef() => b[v] = x,
                R::Var(v) if b[v] != x => return false,
                R::Var(_) => {}
            }
        }
        true
    }

    fn value(r: &R, b: &[Id; MAX_VARS]) -> Option<Id> {
        match *r {
            R::Id(c) => Some(c),
            R::Var(v) if !b[v].is_undef() => Some(b[v]),
            _ => None,
        }
    }

    fn instance(pat: &[R; 3], b: &[Id; MAX_VARS]) -> Option<Triple> {
        Some([
            Self::value(&pat[0], b)?,
            Self::value(&pat[1], b)?,
            Self::value(&pat[2], b)?,
        ])
    }

    /// The facts of the index that match `pat` under `b`.
    fn candidates(&self, pat: &[R; 3], b: &[Id; MAX_VARS]) -> Vec<Triple> {
        if pat.iter().any(|r| matches!(r, R::Never)) {
            return Vec::new();
        }
        let (s, p, o) = (
            Self::value(&pat[0], b),
            Self::value(&pat[1], b),
            Self::value(&pat[2], b),
        );
        match (s, p, o) {
            (Some(s), Some(p), Some(o)) => {
                if self.ps.get(&(p, s)).is_some_and(|x| x.contains(&o)) {
                    vec![[s, p, o]]
                } else {
                    vec![]
                }
            }
            (Some(s), Some(p), None) => self
                .ps
                .get(&(p, s))
                .map(|x| x.iter().map(|&o| [s, p, o]).collect())
                .unwrap_or_default(),
            (None, Some(p), Some(o)) => self
                .po
                .get(&(p, o))
                .map(|x| x.iter().map(|&s| [s, p, o]).collect())
                .unwrap_or_default(),
            (_, Some(p), _) => self
                .by_p
                .get(&p)
                .map(|x| {
                    x.iter()
                        .filter(|(xs, xo)| s.is_none_or(|s| s == *xs) && o.is_none_or(|o| o == *xo))
                        .map(|&(xs, xo)| [xs, p, xo])
                        .collect()
                })
                .unwrap_or_default(),
            (_, None, _) => self
                .by_p
                .iter()
                .flat_map(|(&p, x)| x.iter().map(move |&(xs, xo)| [xs, p, xo]))
                .filter(|t| s.is_none_or(|s| s == t[0]) && o.is_none_or(|o| o == t[2]))
                .collect(),
        }
    }

    /// Every binding that extends `b` to match the patterns `rest` with held facts.
    fn solve(
        &self,
        pats: &[[R; 3]],
        rest: &mut Vec<usize>,
        b: [Id; MAX_VARS],
        held: &dyn Fn(&Triple) -> bool,
        first: bool,
        out: &mut Vec<[Id; MAX_VARS]>,
    ) {
        if first && !out.is_empty() {
            return;
        }
        let Some(&i) = rest.last() else {
            out.push(b);
            return;
        };
        rest.pop();
        for t in self.candidates(&pats[i], &b) {
            if !held(&t) {
                continue;
            }
            let mut nb = b;
            if Self::bind(&pats[i], &t, &mut nb) {
                self.solve(pats, rest, nb, held, first, out);
            }
        }
        rest.push(i);
    }

    fn index(&mut self, t: &Triple, add: bool) {
        let [s, p, o] = *t;
        if add {
            self.by_p.entry(p).or_default().insert((s, o));
            self.ps.entry((p, s)).or_default().insert(o);
            self.po.entry((p, o)).or_default().insert(s);
        } else {
            if let Some(x) = self.by_p.get_mut(&p) {
                x.remove(&(s, o));
                if x.is_empty() {
                    self.by_p.remove(&p);
                }
            }
            for (m, k, v) in [(&mut self.ps, (p, s), o), (&mut self.po, (p, o), s)] {
                if let Some(x) = m.get_mut(&k) {
                    x.remove(&v);
                    if x.is_empty() {
                        m.remove(&k);
                    }
                }
            }
        }
    }

    fn explicit(cx: &RuleCx, w: Which, t: &Triple) -> sparkles_core::Result<bool> {
        cx.snap(w).contains(&[t[0], t[1], t[2], Id::DEFAULT_GRAPH])
    }
}

impl Program for RuleProgram {
    type Fact = Triple;
    type Record = Record;
    type Cx<'c> = RuleCx<'c>;

    fn premises(&self, _f: &Triple, r: &Record, out: &mut Vec<Triple>) {
        if r.is_explicit() {
            return;
        }
        let (body, _) = &self.resolved[r.rule as usize];
        for pat in body {
            if let Some(t) = Self::instance(pat, &r.vars) {
                out.push(t);
            }
        }
    }

    fn axioms(
        &mut self,
        cx: &mut RuleCx,
        out: &mut Vec<(Triple, Record)>,
    ) -> sparkles_core::Result<()> {
        cx.new.for_each_quad_in(&[Id::DEFAULT_GRAPH.0], |q| {
            out.push(([q[0], q[1], q[2]], Record::EXPLICIT));
            Ok(())
        })
    }

    fn consequences(
        &mut self,
        _cx: &mut RuleCx,
        _w: Which,
        f: &Triple,
        held: &dyn Fn(&Triple) -> bool,
        out: &mut Vec<(Triple, Record)>,
    ) -> sparkles_core::Result<()> {
        let mut sols = Vec::new();
        for (ri, (body, head)) in self.resolved.iter().enumerate() {
            for i in 0..body.len() {
                let mut b = [Id::UNDEF; MAX_VARS];
                if !Self::bind(&body[i], f, &mut b) {
                    continue;
                }
                let mut rest: Vec<usize> = (0..body.len()).filter(|&j| j != i).collect();
                sols.clear();
                self.solve(body, &mut rest, b, held, false, &mut sols);
                for vars in &sols {
                    let rec = Record {
                        rule: ri as u32,
                        vars: *vars,
                    };
                    for h in head {
                        if let Some(t) = Self::instance(h, vars) {
                            out.push((t, rec));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn prove(
        &mut self,
        cx: &mut RuleCx,
        f: &Triple,
        held: &dyn Fn(&Triple) -> bool,
    ) -> sparkles_core::Result<Option<Record>> {
        if Self::explicit(cx, Which::New, f)? {
            return Ok(Some(Record::EXPLICIT));
        }
        let mut sols = Vec::new();
        for (ri, (body, head)) in self.resolved.iter().enumerate() {
            for h in head {
                let mut b = [Id::UNDEF; MAX_VARS];
                if !Self::bind(h, f, &mut b) {
                    continue;
                }
                let mut rest: Vec<usize> = (0..body.len()).collect();
                sols.clear();
                self.solve(body, &mut rest, b, held, true, &mut sols);
                if let Some(vars) = sols.first() {
                    return Ok(Some(Record {
                        rule: ri as u32,
                        vars: *vars,
                    }));
                }
            }
        }
        Ok(None)
    }

    fn broken(
        &mut self,
        cx: &mut RuleCx,
        recorded: &dyn Fn(&Triple) -> Option<Record>,
        out: &mut Vec<Triple>,
    ) -> sparkles_core::Result<()> {
        for t in cx.removed {
            if recorded(t).is_some_and(|r| r.is_explicit()) {
                out.push(*t);
            }
        }
        Ok(())
    }

    fn added(
        &mut self,
        cx: &mut RuleCx,
        _held: &dyn Fn(&Triple) -> bool,
        out: &mut Vec<(Triple, Record)>,
    ) -> sparkles_core::Result<()> {
        out.extend(cx.added.iter().map(|t| (*t, Record::EXPLICIT)));
        Ok(())
    }

    fn annotate(
        &mut self,
        cx: &mut RuleCx,
        mode: Provenance,
        f: &Triple,
        r: &Record,
        premises: &[&Ann],
    ) -> sparkles_core::Result<Ann> {
        if matches!(mode, Provenance::None | Provenance::Count) {
            return Ok(Ann::derivation(mode, Vec::new()));
        }
        if r.is_explicit() {
            let key = |i: Id| self.terms.key(&cx.new, i).unwrap_or_default();
            let k = FactKey::of_keys(&key(f[0]), &key(f[1]), &key(f[2]), None);
            return Ok(Ann::derivation(mode, vec![Atom::Fact(k)]));
        }
        let atoms: Vec<Atom> = premises
            .iter()
            .flat_map(|a| fixpoint::atoms_of(a))
            .collect();
        Ok(Ann::derivation(mode, atoms))
    }

    fn tick(&mut self, cx: &mut RuleCx) -> sparkles_core::Result<()> {
        cx.control.check()
    }

    fn save(&self, w: &mut Writer, f: &Triple, r: &Record) -> sparkles_core::Result<()> {
        for &i in f {
            w.id(i)?;
        }
        w.u64(u64::from(r.rule));
        for &v in &r.vars {
            w.id(v)?;
        }
        Ok(())
    }

    fn load(&self, r: &mut Reader) -> sparkles_core::Result<(Triple, Record)> {
        let t = [r.id()?, r.id()?, r.id()?];
        let rule = u32::try_from(r.u64()?)
            .map_err(|_| sparkles_core::Error::Corrupt("a rule number out of range".into()))?;
        if rule != EXPLICIT && rule as usize >= self.rules.len() {
            return Err(sparkles_core::Error::Corrupt(
                "a fixed point's record names a rule the program does not have".into(),
            ));
        }
        let mut vars = [Id::UNDEF; MAX_VARS];
        for v in &mut vars {
            *v = r.id()?;
        }
        Ok((t, Record { rule, vars }))
    }

    fn fact_bytes(&self) -> u64 {
        // the key, the record, the map's slot overhead and three index entries
        24 + 40 + 8 + 3 * 24
    }

    fn inserted(&mut self, f: &Triple) {
        self.index(f, true);
    }

    fn removed(&mut self, f: &Triple) {
        self.index(f, false);
    }

    fn cleared(&mut self) {
        self.by_p.clear();
        self.ps.clear();
        self.po.clear();
    }
}

#[cfg(test)]
mod tests;
