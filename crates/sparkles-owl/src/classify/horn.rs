//! The Horn normalization of roadmap item 603's Tier H: the axioms outside OWL 2 EL whose
//! clauses have at most one disjunct, turned into the forms that the rules of Kazakov's
//! consequence-driven calculus for Horn-SHIQ read (*Consequence-Driven Reasoning for Horn
//! SHIQ Ontologies*, IJCAI 2009, Table 3).
//!
//! A class expression in a positive position becomes an expression `x` with `x ⊑ E`, and
//! one in a negative position an expression `y` with `E ⊑ y`, by the polarity rules of the
//! structural transformation, with fresh classes for what the index cannot read in place.
//! The forms are those of the EL index with three more nodes: `∀R.A` and `≤1 S.B` in
//! positive positions and the fresh classes. A positive `≥n S.C` becomes n existential
//! restrictions to fresh, pairwise disjoint subclasses of C, as the paper's Lemma 2 does,
//! so that the at-most rule merges two of them only to find the merge unsatisfiable. A
//! negation, a universal or a number restriction on the left side moves to the right side
//! as its dual, which keeps an axiom such as `DisjointClasses(¬A B)` Horn.
//!
//! Roles have inverses, and the role hierarchy is closed under them. A universal
//! restriction on a role that a chain or transitivity reaches is unfolded by the automaton
//! of the role (Horrocks, Kutz and Sattler, *The Even More Irresistible SROIQ*, KR 2006), a
//! fresh class per state, so that the rules propagate universals only along the links that
//! existential restrictions create. The automaton can be exponential in the depth of the
//! role hierarchy, and a run whose automata pass [`MAX_STATES`] states, or whose role
//! inclusions are not regular, takes no axiom outside OWL 2 EL.
//!
//! An axiom the normalization cannot take, because it is not Horn, names an individual,
//! the top or bottom property or a datatype outside OWL 2 EL, or breaks the restriction to
//! simple roles, stays left out and reported, which is sound.

use super::index::{
    Axioms, BOTTOM, Ex, Halt, Interner, NO_ROLE, Node, POLL_WORK, Role, RoleName, TOP,
    class_expression,
};
use crate::model::*;
use crate::vocab;
use rustc_hash::FxHashMap;
use std::collections::BTreeSet;

/// The most states the automata of a run may have together.
pub const MAX_STATES: usize = 1 << 16;
/// The largest minimum cardinality the normalization unfolds into distinct successors.
pub const MAX_AT_LEAST: u32 = 64;

/// What the Horn normalization adds to the normalized axioms.
#[derive(Default)]
pub struct HornAx {
    /// pairs of roles that cannot link one pair of elements together
    pub clash: Vec<(Role, Role)>,
    /// the fillers of at-most restrictions, which the index reads as premises
    pub premises: Vec<Ex>,
    /// the automaton of each role that a chain or transitivity reaches
    pub autos: FxHashMap<Role, Nfa>,
    /// the axioms outside OWL 2 EL the run takes
    pub taken: usize,
}

impl HornAx {
    pub fn bytes(&self) -> usize {
        self.clash.capacity() * 8
            + self.premises.capacity() * 4
            + self
                .autos
                .values()
                .map(|a| 64 + a.trans.capacity() * 16)
                .sum::<usize>()
    }
}

/// What the Horn rules read in the index.
pub struct HornIx {
    /// for each expression, [`HornIx::FILLER`] and [`HornIx::UNFOLDED`]
    pub flags: Vec<u8>,
    /// the first id of the contexts the saturation creates for conjunctions
    pub base: Ex,
}

impl HornIx {
    /// The expression is the filler of an at-most restriction, whose holders the
    /// contexts tell their neighbours.
    pub const FILLER: u8 = 1;
    /// The expression is a universal on a role with an automaton, whose told subsumer,
    /// the automaton's first state, stands for it, so that the Horn rules pass over it.
    pub const UNFOLDED: u8 = 2;

    #[inline]
    pub fn is(&self, e: Ex, flag: u8) -> bool {
        self.flags.get(e as usize).is_some_and(|&f| f & flag != 0)
    }

    pub fn bytes(&self) -> usize {
        self.flags.capacity()
    }
}

/// A nondeterministic automaton over roles, with ε-transitions as `None`.
#[derive(Clone, Debug, Default)]
pub struct Nfa {
    pub states: u32,
    pub start: u32,
    pub end: u32,
    pub trans: Vec<(u32, Option<Role>, u32)>,
}

/// An automaton without ε-transitions and with only the states that its start reaches,
/// which accepts from each state what the [`Nfa`] it comes from accepts there.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EpsFree {
    pub states: u32,
    pub start: u32,
    pub finals: Vec<bool>,
    pub trans: Vec<(u32, Role, u32)>,
    /// for each state, whether a transition enters it
    pub entered: Vec<bool>,
    /// for each state, whether a transition leaves it
    pub left: Vec<bool>,
}

impl Nfa {
    /// The automaton with its ε-transitions closed over: a state takes the transitions of
    /// the states its ε-transitions reach, and is final when one of them is the end.
    pub fn eps_free(&self) -> EpsFree {
        let n = self.states as usize;
        let mut eps: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut labeled: Vec<Vec<(Role, u32)>> = vec![Vec::new(); n];
        for &(p, l, q) in &self.trans {
            match l {
                None => eps[p as usize].push(q),
                Some(l) => labeled[p as usize].push((l, q)),
            }
        }
        // the visits of the closures, marked by the state each started from
        let mut seen: Vec<u32> = vec![u32::MAX; n];
        let mut closure = |p: u32| -> Vec<u32> {
            let mut stack = vec![p];
            let mut out = Vec::new();
            while let Some(x) = stack.pop() {
                if std::mem::replace(&mut seen[x as usize], p) == p {
                    continue;
                }
                out.push(x);
                stack.extend(eps[x as usize].iter().copied());
            }
            out
        };
        // the states the start reaches, renumbered in the order they are found
        let mut id: Vec<Option<u32>> = vec![None; n];
        let mut order = vec![self.start];
        id[self.start as usize] = Some(0);
        let mut finals = Vec::new();
        let mut trans = Vec::new();
        let mut at = 0;
        while at < order.len() {
            let p = order[at];
            let c = closure(p);
            finals.push(c.contains(&self.end));
            for x in c {
                for &(l, q) in &labeled[x as usize] {
                    let q = *id[q as usize].get_or_insert_with(|| {
                        order.push(q);
                        order.len() as u32 - 1
                    });
                    trans.push((at as u32, l, q));
                }
            }
            at += 1;
        }
        trans.sort_unstable();
        trans.dedup();
        let states = order.len();
        let mut entered = vec![false; states];
        let mut left = vec![false; states];
        for &(p, _, q) in &trans {
            left[p as usize] = true;
            entered[q as usize] = true;
        }
        EpsFree {
            states: states as u32,
            start: 0,
            finals,
            trans,
            entered,
            left,
        }
    }
}

impl Nfa {
    fn state(&mut self) -> u32 {
        self.states += 1;
        self.states - 1
    }

    /// Adds a copy of `sub` between `from` and `to`.
    fn embed(&mut self, from: u32, sub: &Nfa, to: u32) {
        let off = self.states;
        self.states += sub.states;
        self.trans.push((from, None, sub.start + off));
        self.trans.push((sub.end + off, None, to));
        self.trans
            .extend(sub.trans.iter().map(|&(a, l, b)| (a + off, l, b + off)));
    }
}

/// The role box of a Horn run: the closure of the role inclusions under inverses, the
/// chains with their mirror images, which roles are simple and the automata.
struct RoleBox {
    sup: Vec<Vec<Role>>,
    chains: Vec<(Vec<Role>, Role)>,
    simple: Vec<bool>,
    autos: FxHashMap<Role, Nfa>,
    states: usize,
}

impl RoleBox {
    fn sub(&self, r: Role, s: Role) -> bool {
        r == s
            || self
                .sup
                .get(r as usize)
                .is_some_and(|v| v.binary_search(&s).is_ok())
    }

    fn equiv(&self, r: Role, s: Role) -> bool {
        self.sub(r, s) && self.sub(s, r)
    }

    fn simple(&self, r: Role) -> bool {
        self.simple.get(r as usize).copied().unwrap_or(true)
    }

    /// The role box, or why it stopped: its closure passed `limit` bytes, or `stop`
    /// said the run's deadline or cancellation came, as the index's closure of the roles
    /// checks.
    fn new(
        it: &Interner,
        role_subs: &[(Role, Role)],
        chains: &[(Vec<Role>, Role)],
        limit: Option<usize>,
        stop: &dyn Fn() -> bool,
    ) -> Result<RoleBox, Halt> {
        let n = it.roles.len();
        let inv = inverses(it);
        let mut direct: Vec<Vec<Role>> = vec![Vec::new(); n];
        for &(r, s) in role_subs {
            direct[r as usize].push(s);
        }
        let mut bytes = 0usize;
        let mut work = 0usize;
        let mut sup: Vec<Vec<Role>> = Vec::with_capacity(n);
        for r in 0..n as Role {
            let mut seen: BTreeSet<Role> = BTreeSet::new();
            let mut stack = vec![r];
            while let Some(x) = stack.pop() {
                work += 1;
                if work.is_multiple_of(POLL_WORK) && stop() {
                    return Err(Halt::Stopped);
                }
                if seen.insert(x) {
                    stack.extend(direct[x as usize].iter().copied());
                }
            }
            bytes += 48 + seen.len() * 8;
            if limit.is_some_and(|l| bytes > l) {
                return Err(Halt::Memory(bytes));
            }
            sup.push(seen.into_iter().collect());
        }
        let mut all: Vec<(Vec<Role>, Role)> = Vec::with_capacity(2 * chains.len());
        for (ops, t) in chains {
            all.push((ops.clone(), *t));
            let mirror: Option<Vec<Role>> = ops
                .iter()
                .rev()
                .map(|&o| Some(inv[o as usize]).filter(|&i| i != NO_ROLE))
                .collect();
            if let (Some(m), Some(i)) = (mirror, Some(inv[*t as usize]).filter(|&i| i != NO_ROLE)) {
                all.push((m, i));
            }
        }
        all.sort();
        all.dedup();
        let mut simple = vec![true; n];
        for (_, t) in &all {
            for &s in &sup[*t as usize] {
                simple[s as usize] = false;
            }
        }
        Ok(RoleBox {
            sup,
            chains: all,
            simple,
            autos: FxHashMap::default(),
            states: 0,
        })
    }

    /// The automaton of a role, whose words are the role paths that imply it, built as
    /// Horrocks, Kutz and Sattler do. `None` when the role inclusions are not regular or
    /// the automata pass [`MAX_STATES`].
    fn auto(&mut self, r: Role, stack: &mut Vec<Role>) -> Option<Nfa> {
        if let Some(a) = self.autos.get(&r) {
            return Some(a.clone());
        }
        if stack.iter().any(|&s| self.equiv(s, r)) {
            return None;
        }
        stack.push(r);
        let mut a = Nfa::default();
        a.start = a.state();
        a.end = a.state();
        a.trans.push((a.start, Some(r), a.end));
        let chains: Vec<(Vec<Role>, Role)> = self
            .chains
            .iter()
            .filter(|(_, t)| self.sub(*t, r))
            .cloned()
            .collect();
        let mut below: Vec<Role> = Vec::new();
        for (ops, t) in chains {
            if !self.equiv(t, r) {
                // a chain under a role strictly below r: the automaton of that role
                if !below.contains(&t) {
                    below.push(t);
                    let sub = self.auto(t, stack)?;
                    a.embed(a.start, &sub, a.end);
                }
                continue;
            }
            let n = ops.len();
            let first = self.equiv(ops[0], r);
            let last = self.equiv(ops[n - 1], r);
            let (from, mid, to) = match (first, last) {
                (true, true) if n == 2 => {
                    a.trans.push((a.end, None, a.start));
                    continue;
                }
                (true, true) => return None,
                (true, false) => (a.end, &ops[1..], a.end),
                (false, true) => (a.start, &ops[..n - 1], a.start),
                (false, false) => (a.start, &ops[..], a.end),
            };
            let mut cur = from;
            for (k, &op) in mid.iter().enumerate() {
                let next = if k + 1 == mid.len() { to } else { a.state() };
                if self.simple(op) {
                    a.trans.push((cur, Some(op), next));
                } else {
                    let sub = self.auto(op, stack)?;
                    a.embed(cur, &sub, next);
                }
                cur = next;
            }
        }
        stack.pop();
        self.states += a.states as usize;
        if self.states > MAX_STATES {
            return None;
        }
        self.autos.insert(r, a.clone());
        Some(a)
    }
}

/// The inverse of each role of the interner, [`NO_ROLE`] for a role without one.
pub fn inverses(it: &Interner) -> Vec<Role> {
    let mut inv = vec![NO_ROLE; it.roles.len()];
    for (r, name) in it.roles.iter().enumerate() {
        let other = match name {
            RoleName::Object(p) => it.role_id(&RoleName::Inverse(p.clone())),
            RoleName::Inverse(p) => it.role_id(&RoleName::Object(p.clone())),
            _ => None,
        };
        if let Some(o) = other {
            inv[r] = o;
        }
    }
    inv
}

/// Why an axiom stays left out.
type Why = &'static str;

/// What one axiom adds, kept apart until the whole axiom is normalized.
#[derive(Default)]
struct Out {
    subs: Vec<(Ex, Ex)>,
    disjoint: Vec<Vec<Ex>>,
    ranges: Vec<(Role, Ex)>,
    role_subs: Vec<(Role, Role)>,
    chains: Vec<(Vec<Role>, Role)>,
    reflexive: Vec<Role>,
    clash: Vec<(Role, Role)>,
    premises: Vec<Ex>,
}

impl Out {
    fn commit(self, ax: &mut Axioms, h: &mut HornAx) {
        ax.subs.extend(self.subs);
        ax.disjoint.extend(self.disjoint);
        ax.ranges.extend(self.ranges);
        ax.role_subs.extend(self.role_subs);
        ax.chains.extend(self.chains);
        ax.reflexive.extend(self.reflexive);
        h.clash.extend(self.clash);
        h.premises.extend(self.premises);
    }
}

struct Norm<'a> {
    it: &'a mut Interner,
    rb: Option<&'a RoleBox>,
    out: Out,
}

impl Norm<'_> {
    /// The role of an object property expression, with its inverse interned beside it.
    fn role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        let named = p.named();
        if matches!(
            named.as_str(),
            vocab::OWL_TOP_OBJECT_PROPERTY | vocab::OWL_BOTTOM_OBJECT_PROPERTY
        ) {
            return Err("the top and bottom object properties");
        }
        let r = self.it.role(RoleName::Object(named.clone()));
        let i = self.it.role(RoleName::Inverse(named.clone()));
        Ok(match p {
            ObjectPropertyExpression::Property(_) => r,
            ObjectPropertyExpression::InverseOf(_) => i,
        })
    }

    /// The role of an expression that OWL 2 DL requires to be simple.
    fn simple_role(&mut self, p: &ObjectPropertyExpression) -> Result<Role, Why> {
        let r = self.role(p)?;
        match self.rb {
            Some(rb) if !rb.simple(r) => Err("a role that is not simple where OWL 2 DL needs one"),
            _ => Ok(r),
        }
    }

    fn conj(&mut self, mut es: Vec<Ex>) -> Ex {
        es.sort_unstable();
        es.dedup();
        es.retain(|&e| e != TOP);
        if es.contains(&BOTTOM) {
            return BOTTOM;
        }
        match es.len() {
            0 => TOP,
            1 => es[0],
            _ => {
                let mut acc = es[0];
                for &e in &es[1..] {
                    acc = self.it.conj(acc, e);
                }
                acc
            }
        }
    }

    /// An expression of the EL index, for data restrictions inside a Horn axiom.
    fn el(&mut self, c: &ClassExpression) -> Result<Ex, Why> {
        match class_expression(c, self.it, 0)? {
            (e, None) => Ok(e),
            (_, Some(why)) => Err(why),
        }
    }

    /// `x` with `x ⊑ c`.
    fn pos(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                let es = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.pos(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => return Err("a disjunction in a positive position"),
                }
            }
            C::ObjectComplementOf(f) => {
                let g = self.neg(f, d)?;
                match g {
                    TOP => BOTTOM,
                    BOTTOM => TOP,
                    _ => {
                        let x = self.it.fresh_class();
                        let both = self.it.conj(x, g);
                        self.out.subs.push((both, BOTTOM));
                        x
                    }
                }
            }
            C::ObjectSomeValuesFrom(p, f) => {
                let r = self.role(p)?;
                let f = self.pos(f, d)?;
                if f == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, f))
                }
            }
            C::ObjectAllValuesFrom(p, f) => {
                let r = self.role(p)?;
                let f = self.pos(f, d)?;
                if f == TOP {
                    TOP
                } else {
                    self.it.node(Node::Forall(r, f))
                }
            }
            C::ObjectHasSelf(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.it.node(Node::SelfLoop(r))
            }
            C::ObjectMinCardinality(n, p, f) => self.at_least(*n, p, f.as_deref(), d)?,
            C::ObjectMaxCardinality(n, p, f) => self.at_most(*n, p, f.as_deref(), d)?,
            C::ObjectExactCardinality(n, p, f) => {
                let a = self.at_least(*n, p, f.as_deref(), d)?;
                let b = self.at_most(*n, p, f.as_deref(), d)?;
                self.conj(vec![a, b])
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::DataSomeValuesFrom(..) | C::DataHasValue(..) => self.el(c)?,
            _ => return Err("a data restriction outside OWL 2 EL"),
        })
    }

    fn at_least(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        if n == 0 {
            return Ok(TOP);
        }
        let r = if n == 1 {
            self.role(p)?
        } else {
            self.simple_role(p)?
        };
        let f = match f {
            Some(f) => self.pos(f, d)?,
            None => TOP,
        };
        if f == BOTTOM {
            return Ok(BOTTOM);
        }
        if n == 1 {
            return Ok(self.it.node(Node::Exist(r, f)));
        }
        if n > MAX_AT_LEAST {
            return Err("a minimum cardinality above the limit the normalization unfolds");
        }
        // n distinct successors: fresh, pairwise disjoint subclasses of the filler
        let mut parts = Vec::with_capacity(n as usize);
        let mut fresh = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let b = self.it.fresh_class();
            if f != TOP {
                self.out.subs.push((b, f));
            }
            fresh.push(b);
            parts.push(self.it.node(Node::Exist(r, b)));
        }
        fresh.sort_unstable();
        self.out.disjoint.push(fresh);
        Ok(self.conj(parts))
    }

    fn at_most(
        &mut self,
        n: u32,
        p: &ObjectPropertyExpression,
        f: Option<&ClassExpression>,
        d: usize,
    ) -> Result<Ex, Why> {
        let r = self.simple_role(p)?;
        let g = match f {
            Some(f) => self.neg(f, d)?,
            None => TOP,
        };
        if g == BOTTOM {
            return Ok(TOP);
        }
        match n {
            0 => {
                // ≤0 r.F is ¬∃r.F
                let x = self.it.fresh_class();
                let e = self.it.node(Node::Exist(r, g));
                let both = self.it.conj(x, e);
                self.out.subs.push((both, BOTTOM));
                Ok(x)
            }
            1 => {
                self.out.premises.push(g);
                Ok(self.it.node(Node::AtMost1(r, g)))
            }
            _ => Err("a maximum cardinality above one in a positive position"),
        }
    }

    /// `y` with `c ⊑ y`.
    fn neg(&mut self, c: &ClassExpression, depth: usize) -> Result<Ex, Why> {
        use ClassExpression as C;
        if depth > 256 {
            return Err("class expressions nested too deeply");
        }
        let d = depth + 1;
        Ok(match c {
            C::Class(c) => self.it.class(c),
            C::ObjectIntersectionOf(cs) => {
                let es = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                self.conj(es)
            }
            C::ObjectUnionOf(cs) => {
                let mut es: Vec<Ex> = cs
                    .iter()
                    .map(|c| self.neg(c, d))
                    .collect::<Result<_, _>>()?;
                if es.contains(&TOP) {
                    return Ok(TOP);
                }
                es.retain(|&e| e != BOTTOM);
                es.sort_unstable();
                es.dedup();
                match es.len() {
                    0 => BOTTOM,
                    1 => es[0],
                    _ => {
                        let x = self.it.fresh_class();
                        for e in es {
                            self.out.subs.push((e, x));
                        }
                        x
                    }
                }
            }
            C::ObjectSomeValuesFrom(p, f) => {
                let r = self.role(p)?;
                let g = self.neg(f, d)?;
                if g == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, g))
                }
            }
            C::ObjectMinCardinality(0, ..) => TOP,
            C::ObjectMinCardinality(1, p, f) => {
                let r = self.role(p)?;
                let g = match f {
                    Some(f) => self.neg(f, d)?,
                    None => TOP,
                };
                if g == BOTTOM {
                    BOTTOM
                } else {
                    self.it.node(Node::Exist(r, g))
                }
            }
            C::ObjectHasSelf(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.it.node(Node::SelfLoop(r))
            }
            C::ObjectOneOf(_) | C::ObjectHasValue(..) => return Err("individuals"),
            C::DataSomeValuesFrom(..) | C::DataHasValue(..) => self.el(c)?,
            C::DataAllValuesFrom(..)
            | C::DataMinCardinality(..)
            | C::DataMaxCardinality(..)
            | C::DataExactCardinality(..) => return Err("a data restriction outside OWL 2 EL"),
            _ => return Err("a class expression that is not Horn in a negative position"),
        })
    }

    /// `⊓lhs ⊑ ⊔rhs`, Horn once the duals of what cannot stand on its side have moved.
    fn sub(&mut self, lhs: Vec<ClassExpression>, rhs: Vec<ClassExpression>) -> Result<(), Why> {
        use ClassExpression as C;
        let mut left: Vec<ClassExpression> = Vec::new();
        let mut right: Vec<ClassExpression> = Vec::new();
        let mut todo_l = lhs;
        let mut todo_r = rhs;
        let mut steps = 0usize;
        while !todo_l.is_empty() || !todo_r.is_empty() {
            steps += 1;
            if steps > 4096 {
                return Err("class expressions nested too deeply");
            }
            if let Some(c) = todo_l.pop() {
                match c {
                    C::ObjectIntersectionOf(cs) => todo_l.extend(cs),
                    C::ObjectComplementOf(f) => todo_r.push(*f),
                    C::ObjectAllValuesFrom(p, f) => todo_r.push(C::ObjectSomeValuesFrom(
                        p,
                        Box::new(C::ObjectComplementOf(f)),
                    )),
                    C::ObjectMaxCardinality(n, p, f) => {
                        let Some(m) = n.checked_add(1) else {
                            return Err("a cardinality too large");
                        };
                        todo_r.push(C::ObjectMinCardinality(m, p, f))
                    }
                    C::ObjectMinCardinality(n, p, f) if n >= 2 => {
                        todo_r.push(C::ObjectMaxCardinality(n - 1, p, f))
                    }
                    C::Class(ref k) if k.as_str() == vocab::OWL_THING => {}
                    c => left.push(c),
                }
                continue;
            }
            if let Some(c) = todo_r.pop() {
                match c {
                    C::ObjectUnionOf(cs) => todo_r.extend(cs),
                    C::ObjectComplementOf(f) => todo_l.push(*f),
                    C::Class(ref k) if k.as_str() == vocab::OWL_NOTHING => {}
                    c => right.push(c),
                }
            }
        }
        let y = {
            let es = left
                .iter()
                .map(|c| self.neg(c, 0))
                .collect::<Result<Vec<_>, _>>()?;
            self.conj(es)
        };
        let mut xs = right
            .iter()
            .map(|c| self.pos(c, 0))
            .collect::<Result<Vec<_>, _>>()?;
        if xs.contains(&TOP) || y == BOTTOM {
            return Ok(());
        }
        xs.retain(|&x| x != BOTTOM);
        xs.sort_unstable();
        xs.dedup();
        let x = match xs.len() {
            0 => BOTTOM,
            1 => xs[0],
            _ => return Err("a disjunction in a positive position"),
        };
        if x != y {
            self.out.subs.push((y, x));
        }
        Ok(())
    }

    /// Whether an axiom is a role axiom that needs no simple roles.
    fn is_role_axiom(ax: &AxiomKind) -> bool {
        use AxiomKind as K;
        matches!(
            ax,
            K::SubObjectPropertyOf(..)
                | K::EquivalentObjectProperties(_)
                | K::InverseObjectProperties(..)
                | K::SymmetricObjectProperty(_)
                | K::TransitiveObjectProperty(_)
                | K::ReflexiveObjectProperty(_)
        )
    }

    /// A role axiom of [`Norm::is_role_axiom`].
    fn role_axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        match ax {
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Expression(p), s) => {
                let (r, s) = (self.role(p)?, self.role(s)?);
                self.out.role_subs.push((r, s));
            }
            K::SubObjectPropertyOf(SubObjectPropertyExpression::Chain(ps), s) => {
                let s = self.role(s)?;
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                match rs.len() {
                    0 => {}
                    1 => self.out.role_subs.push((rs[0], s)),
                    _ => self.out.chains.push((rs, s)),
                }
            }
            K::EquivalentObjectProperties(ps) => {
                let rs = ps
                    .iter()
                    .map(|p| self.role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                for w in 0..rs.len() {
                    self.out.role_subs.push((rs[w], rs[(w + 1) % rs.len()]));
                }
            }
            K::InverseObjectProperties(p, q) => {
                // p is q⁻, both ways
                let (p, q) = (self.role(p)?, self.role(&q.inverse())?);
                self.out.role_subs.push((p, q));
                self.out.role_subs.push((q, p));
            }
            K::SymmetricObjectProperty(p) => {
                let (r, i) = (self.role(p)?, self.role(&p.inverse())?);
                self.out.role_subs.push((r, i));
            }
            K::TransitiveObjectProperty(p) => {
                let r = self.role(p)?;
                self.out.chains.push((vec![r, r], r));
            }
            K::ReflexiveObjectProperty(p) => {
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                self.out.reflexive.push(r);
            }
            _ => return Err("not a role axiom"),
        }
        Ok(())
    }

    /// A class axiom or a role axiom that needs simple roles.
    fn axiom(&mut self, ax: &AxiomKind) -> Result<(), Why> {
        use AxiomKind as K;
        use ClassExpression as C;
        match ax {
            K::SubClassOf(c, d) => self.sub(vec![c.clone()], vec![d.clone()]),
            K::EquivalentClasses(cs) => {
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                for w in 0..cs.len() {
                    let next = cs[(w + 1) % cs.len()];
                    if cs[w] != next {
                        self.sub(vec![cs[w].clone()], vec![next.clone()])?;
                    }
                }
                Ok(())
            }
            K::DisjointClasses(cs) => {
                let cs: Vec<&ClassExpression> = cs.iter().collect();
                // operands with a negative form make one disjointness set
                if let Ok(mut es) = cs
                    .iter()
                    .map(|c| self.neg(c, 0))
                    .collect::<Result<Vec<_>, _>>()
                {
                    es.sort_unstable();
                    for w in es.windows(2) {
                        if w[0] == w[1] {
                            self.out.subs.push((w[0], BOTTOM));
                        }
                    }
                    es.dedup();
                    es.retain(|&e| e != BOTTOM);
                    if es.len() >= 2 {
                        self.out.disjoint.push(es);
                    }
                    return Ok(());
                }
                for i in 0..cs.len() {
                    for j in i + 1..cs.len() {
                        self.sub(vec![cs[i].clone(), cs[j].clone()], vec![])?;
                    }
                }
                Ok(())
            }
            K::DisjointUnion(c, ds) if ds.len() <= 1 => {
                let d = ds
                    .iter()
                    .next()
                    .cloned()
                    .unwrap_or(C::Class(Class::nothing()));
                let c = C::Class(c.clone());
                self.sub(vec![c.clone()], vec![d.clone()])?;
                self.sub(vec![d], vec![c])
            }
            K::ObjectPropertyDomain(p, c) => self.sub(
                vec![C::ObjectSomeValuesFrom(
                    p.clone(),
                    Box::new(C::Class(Class::thing())),
                )],
                vec![c.clone()],
            ),
            K::ObjectPropertyRange(p, c) => {
                let r = self.role(p)?;
                let c = self.pos(c, 0)?;
                if c != TOP {
                    self.out.ranges.push((r, c));
                }
                Ok(())
            }
            K::DataPropertyDomain(d, c) => self.sub(
                vec![C::DataSomeValuesFrom(
                    vec![d.clone()],
                    DataRange::Datatype(Datatype::new(vocab::RDFS_LITERAL)),
                )],
                vec![c.clone()],
            ),
            K::FunctionalObjectProperty(p) => {
                let r = self.simple_role(p)?;
                let e = self.it.node(Node::AtMost1(r, TOP));
                self.out.premises.push(TOP);
                self.out.subs.push((TOP, e));
                Ok(())
            }
            K::InverseFunctionalObjectProperty(p) => {
                let r = self.simple_role(&p.inverse())?;
                let e = self.it.node(Node::AtMost1(r, TOP));
                self.out.premises.push(TOP);
                self.out.subs.push((TOP, e));
                Ok(())
            }
            K::IrreflexiveObjectProperty(p) => {
                self.simple_role(p)?;
                let r = self.role(&ObjectPropertyExpression::Property(p.named().clone()))?;
                let e = self.it.node(Node::SelfLoop(r));
                self.out.subs.push((e, BOTTOM));
                Ok(())
            }
            K::AsymmetricObjectProperty(p) => {
                let r = self.simple_role(p)?;
                let i = self.simple_role(&p.inverse())?;
                self.out.clash.push((r, i));
                self.out.clash.push((i, r));
                Ok(())
            }
            K::DisjointObjectProperties(ps) => {
                let rs = ps
                    .iter()
                    .map(|p| self.simple_role(p))
                    .collect::<Result<Vec<_>, _>>()?;
                for &a in &rs {
                    for &b in &rs {
                        if a != b {
                            self.out.clash.push((a, b));
                        }
                    }
                }
                Ok(())
            }
            _ => Err("an axiom the Horn rules do not take"),
        }
    }
}

/// Normalizes the axioms outside OWL 2 EL that the Horn rules can take, after the EL
/// axioms are in `ax`. The answer says which were taken. Nothing is taken when the role
/// inclusions are not regular, their automata are too large or no axiom is Horn, and
/// then `ax` is as it was, the interner aside. The closure of the roles counts against
/// `limit` and polls `stop`, as the index's does.
pub fn normalize(
    axioms: &[&AxiomKind],
    it: &mut Interner,
    ax: &mut Axioms,
    limit: Option<usize>,
    stop: &dyn Fn() -> bool,
) -> Result<Vec<bool>, Halt> {
    let mut taken = vec![false; axioms.len()];
    let mut h = HornAx::default();
    // every object property has its inverse
    let named: Vec<ObjectProperty> = it
        .roles
        .iter()
        .filter_map(|n| match n {
            RoleName::Object(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    for p in named {
        it.role(RoleName::Inverse(p));
    }
    // the role axioms first, so that the role box is whole before the class axioms ask
    // which roles are simple
    let mut roles = Out::default();
    let mut role_taken = Vec::new();
    for (i, k) in axioms.iter().enumerate() {
        let mut n = Norm {
            it,
            rb: None,
            out: Out::default(),
        };
        if Norm::is_role_axiom(k) && n.role_axiom(k).is_ok() {
            let o = n.out;
            roles.role_subs.extend(o.role_subs);
            roles.chains.extend(o.chains);
            roles.reflexive.extend(o.reflexive);
            role_taken.push(i);
        }
    }
    let mut role_subs: Vec<(Role, Role)> = ax.role_subs.clone();
    role_subs.extend(roles.role_subs.iter().copied());
    let inv = inverses(it);
    let mirror = |subs: &mut Vec<(Role, Role)>| {
        let n = subs.len();
        for k in 0..n {
            let (r, s) = subs[k];
            if let (Some(&a), Some(&b)) = (inv.get(r as usize), inv.get(s as usize))
                && a != NO_ROLE
                && b != NO_ROLE
            {
                subs.push((a, b));
            }
        }
        subs.sort_unstable();
        subs.dedup();
    };
    mirror(&mut role_subs);
    let mut chains = ax.chains.clone();
    chains.extend(roles.chains.iter().cloned());
    let mut rb = RoleBox::new(it, &role_subs, &chains, limit, stop)?;
    let nonsimple: Vec<Role> = (0..rb.simple.len() as Role)
        .filter(|&r| !rb.simple(r))
        .collect();
    for r in nonsimple {
        if rb.auto(r, &mut Vec::new()).is_none() {
            return Ok(vec![false; axioms.len()]);
        }
    }
    for i in role_taken {
        taken[i] = true;
    }
    roles.role_subs = role_subs;
    // the class axioms and the role axioms that need simple roles, which go to `ax` only
    // once one is taken
    let mut class_out: Vec<Out> = Vec::new();
    for (i, k) in axioms.iter().enumerate() {
        if taken[i] {
            continue;
        }
        let mut n = Norm {
            it,
            rb: Some(&rb),
            out: Out::default(),
        };
        if Norm::is_role_axiom(k) {
            continue;
        }
        if n.axiom(k).is_ok() {
            class_out.push(n.out);
            taken[i] = true;
        }
    }
    if !taken.contains(&true) {
        return Ok(taken);
    }
    ax.role_subs.clear();
    for o in class_out {
        o.commit(ax, &mut h);
    }
    roles.commit(ax, &mut h);
    // a domain of r is a range of r⁻, which the successors of r⁻ take at once
    let inv = inverses(it);
    let domains: Vec<(Role, Ex)> = ax
        .subs
        .iter()
        .filter_map(|&(a, c)| match it.nodes[a as usize] {
            Node::Exist(r, TOP) => Some((inv[r as usize], c)).filter(|&(i, _)| i != NO_ROLE),
            _ => None,
        })
        .collect();
    ax.ranges.extend(domains);
    // a range of a role that a chain reaches holds along the chain's paths too
    let ranges: Vec<(Role, Ex)> = ax
        .ranges
        .iter()
        .copied()
        .filter(|&(r, _)| !rb.simple(r))
        .collect();
    for (r, c) in ranges {
        let e = it.node(Node::Forall(r, c));
        ax.subs.push((TOP, e));
    }
    h.taken = taken.iter().filter(|t| **t).count();
    h.autos = std::mem::take(&mut rb.autos);
    ax.horn = Some(Box::new(h));
    Ok(taken)
}
